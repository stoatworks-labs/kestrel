//! The frame path: one captured frame in, one packed frame per output out.

use crate::gpu::{align_up, Gpu, COPY_ALIGN, TARGET_FORMAT};
use crate::uniforms::{CropUniform, ScalarUniform};
use anyhow::{anyhow, Result};
use kestrel_core::{
    place, NormRect, OutputId, OutputPlan, Pattern, PlanSource, ScalingFilter, Size, VideoFormat,
};
use std::collections::HashMap;
use wgpu::util::DeviceExt;

const SHADER_COMMON: &str = include_str!("shaders/common.wgsl");

fn shader(device: &wgpu::Device, label: &str, body: &str) -> wgpu::ShaderModule {
    // WGSL has no include, so the shared half is concatenated on. Keeping the
    // common source first means every reported line number is off by a fixed,
    // known amount rather than varying per stage.
    device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some(label),
        source: wgpu::ShaderSource::Wgsl(format!("{SHADER_COMMON}\n{body}").into()),
    })
}

/// The decoded input frame, plus the texture it was uploaded into.
struct InputSurface {
    size: Size,
    /// Half-width RGBA holding UYVY macropixels, straight off the wire.
    upload: wgpu::Texture,
    /// Full raster RGB, what everything downstream samples.
    decoded: wgpu::Texture,
    decoded_view: wgpu::TextureView,
    bind: wgpu::BindGroup,
}

/// Everything one output port needs.
struct OutputTarget {
    size: Size,
    /// The finished picture at output raster, RGB. Also what the UI shows as
    /// this output's thumbnail — the thumbnail is therefore *the output*, not a
    /// separate render that could disagree with it.
    rgba: wgpu::Texture,
    rgba_view: wgpu::TextureView,
    /// Half-width RGBA holding UYVY macropixels, ready for the wire.
    uyvy: wgpu::Texture,
    crop_uniform: wgpu::Buffer,
    crop_bind: wgpu::BindGroup,
    fill_uniform: wgpu::Buffer,
    fill_bind: wgpu::BindGroup,
    pack_bind: wgpu::BindGroup,
    /// Readback staging, sized to the *padded* row.
    staging: wgpu::Buffer,
    row_bytes: u32,
    padded_row: u32,
    /// Where the de-padded frame ends up. Reused every frame.
    frame: Vec<u8>,
}

/// Which live texture a preview is a copy of.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum PreviewKey {
    Input,
    Output(OutputId),
}

/// A small offscreen copy of a live texture, for the UI.
struct PreviewTarget {
    size: Size,
    tex: wgpu::Texture,
    view: wgpu::TextureView,
    uniform: wgpu::Buffer,
    bind: wgpu::BindGroup,
    /// Which generation of source bindings this was built against. A preview
    /// holds a bind group pointing at a source texture, so a raster change
    /// leaves it showing the old picture forever — which reads as a frozen
    /// source rather than as a stale binding.
    generation: u64,
}

/// Thumbnails of the input and of every output, as tightly packed RGBA.
#[derive(Debug, Clone, Default)]
pub struct Previews {
    pub input_size: Size,
    pub input: Vec<u8>,
    pub output_size: Size,
    pub outputs: Vec<(OutputId, Vec<u8>)>,
}

/// A stage with one pipeline and one bind group layout.
struct Stage {
    pipeline: wgpu::RenderPipeline,
    layout: wgpu::BindGroupLayout,
}

pub struct Engine {
    pub gpu: Gpu,
    decode: Stage,
    crop: Stage,
    fill: Stage,
    pack: Stage,
    sampler: wgpu::Sampler,
    input: InputSurface,
    outputs: HashMap<OutputId, OutputTarget>,
    /// Preserves the caller's output order, which is the order the UI shows.
    order: Vec<OutputId>,
    previews: HashMap<PreviewKey, PreviewTarget>,
    /// Bumped whenever a source texture is replaced, so anything holding a
    /// bind group against one knows to rebuild.
    binding_generation: u64,
    output_format: VideoFormat,
    scaling: ScalingFilter,
    /// False until a frame has been uploaded, and again after the input is
    /// declared lost. Never "the last frame was a while ago" — see
    /// [`Engine::set_input_live`].
    input_live: bool,
}

impl Engine {
    pub fn new(gpu: Gpu, input_size: Size, output_format: VideoFormat) -> Result<Self> {
        let device = &gpu.device;

        // --- decode -----------------------------------------------------
        let decode_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("decode bgl"),
            entries: &[texture_entry(0), uniform_entry(1)],
        });
        let decode = make_stage(
            &gpu,
            "decode",
            include_str!("shaders/decode.wgsl"),
            "fs_decode",
            decode_layout,
        );

        // --- crop -------------------------------------------------------
        let crop_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("crop bgl"),
            entries: &[
                texture_entry(0),
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
                uniform_entry(2),
            ],
        });
        let crop = make_stage(
            &gpu,
            "crop",
            include_str!("shaders/crop.wgsl"),
            "fs_crop",
            crop_layout,
        );

        // --- fill -------------------------------------------------------
        let fill_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("fill bgl"),
            entries: &[uniform_entry(0)],
        });
        let fill = make_stage(
            &gpu,
            "fill",
            include_str!("shaders/fill.wgsl"),
            "fs_fill",
            fill_layout,
        );

        // --- pack -------------------------------------------------------
        let pack_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("pack bgl"),
            entries: &[texture_entry(0), uniform_entry(1)],
        });
        let pack = make_stage(
            &gpu,
            "pack",
            include_str!("shaders/pack.wgsl"),
            "fs_pack",
            pack_layout,
        );

        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("crop sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });

        let input = make_input(&gpu, &decode.layout, input_size);

        Ok(Self {
            gpu,
            decode,
            crop,
            fill,
            pack,
            sampler,
            input,
            outputs: HashMap::new(),
            order: Vec::new(),
            previews: HashMap::new(),
            binding_generation: 0,
            output_format,
            scaling: ScalingFilter::default(),
            input_live: false,
        })
    }

    pub fn input_size(&self) -> Size {
        self.input.size
    }

    pub fn output_format(&self) -> VideoFormat {
        self.output_format
    }

    pub fn input_live(&self) -> bool {
        self.input_live
    }

    /// Declare the input lost.
    ///
    /// Kept explicit rather than inferred from "no frame for a while" inside
    /// here, because only the capture side knows the difference between a
    /// source that unplugged and one that is simply slower than the output
    /// clock. Getting that wrong drops a shot off air for a frame.
    pub fn set_input_live(&mut self, live: bool) {
        self.input_live = live;
    }

    pub fn set_scaling(&mut self, s: ScalingFilter) {
        self.scaling = s;
    }

    pub fn set_input_size(&mut self, size: Size) {
        if size == self.input.size || size.w == 0 || size.h == 0 {
            return;
        }
        tracing::info!(from = ?self.input.size, to = ?size, "input raster changed");
        self.input = make_input(&self.gpu, &self.decode.layout, size);
        self.input_live = false;
        self.binding_generation += 1;
        // Every output's crop bind group points at the old decoded texture.
        let ids: Vec<_> = self.order.clone();
        self.outputs.clear();
        self.order.clear();
        self.set_outputs(&ids);
    }

    pub fn set_output_format(&mut self, fmt: VideoFormat) {
        if fmt.size == self.output_format.size {
            self.output_format = fmt;
            return;
        }
        self.output_format = fmt;
        self.binding_generation += 1;
        let ids: Vec<_> = self.order.clone();
        self.outputs.clear();
        self.order.clear();
        self.set_outputs(&ids);
    }

    /// Make the engine's targets match this list of outputs, creating and
    /// dropping as needed. Cheap to call every frame; only differences cost.
    pub fn set_outputs(&mut self, ids: &[OutputId]) {
        self.outputs.retain(|id, _| ids.contains(id));
        for id in ids {
            if !self.outputs.contains_key(id) {
                let t = make_output(
                    &self.gpu,
                    &self.crop.layout,
                    &self.fill.layout,
                    &self.pack.layout,
                    &self.sampler,
                    &self.input.decoded_view,
                    self.output_format.size,
                );
                self.outputs.insert(*id, t);
            }
        }
        self.order = ids.to_vec();
    }

    /// Upload a captured UYVY frame and decode it to RGB.
    ///
    /// `row_bytes` is the *source* stride, which DeckLink reports and which is
    /// not always `width * 2`.
    pub fn upload_input(&mut self, uyvy: &[u8], row_bytes: u32) -> Result<()> {
        let macro_w = self.input.size.w.div_ceil(2);
        let h = self.input.size.h;
        let needed = row_bytes as usize * h as usize;
        if uyvy.len() < needed {
            return Err(anyhow!(
                "short frame: {} bytes for {}x{} at stride {row_bytes}, need {needed}",
                uyvy.len(),
                self.input.size.w,
                h
            ));
        }

        self.gpu.queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &self.input.upload,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            uyvy,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                // `write_texture` is exempt from COPY_BYTES_PER_ROW_ALIGNMENT,
                // which is why the capture stride can go straight in with no
                // repacking on the CPU.
                bytes_per_row: Some(row_bytes),
                rows_per_image: Some(h),
            },
            wgpu::Extent3d {
                width: macro_w,
                height: h,
                depth_or_array_layers: 1,
            },
        );

        let mut enc = self
            .gpu
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("decode"),
            });
        {
            let mut pass = begin_pass(&mut enc, "decode", &self.input.decoded_view);
            pass.set_pipeline(&self.decode.pipeline);
            pass.set_bind_group(0, &self.input.bind, &[]);
            pass.draw(0..3, 0..1);
        }
        self.gpu.queue.submit([enc.finish()]);
        self.input_live = true;
        Ok(())
    }

    /// Render every output in the plan and hand each finished UYVY frame to
    /// `sink` as `(output, bytes, row_bytes)`.
    ///
    /// The readback is **synchronous**: submit, wait, map, copy. That costs a
    /// pipeline stall per frame, and it is the right trade here — a ring of
    /// in-flight staging buffers would hide the stall at the price of a frame
    /// or two of latency, and latency is the thing an operator watching a
    /// stage and a screen at the same time actually notices. The stall is ~1-2
    /// ms per HD output against a 20 ms budget at 50p.
    pub fn render(
        &mut self,
        plan: &[OutputPlan],
        sink: &mut dyn FnMut(OutputId, &[u8], u32),
    ) -> Result<()> {
        let ids: Vec<OutputId> = plan.iter().map(|p| p.output).collect();
        if ids != self.order {
            self.set_outputs(&ids);
        }

        let input_size = self.input.size;
        let out_size = self.output_format.size;
        let scaling = self.scaling;

        let mut enc = self
            .gpu
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("outputs"),
            });

        for p in plan {
            let Some(t) = self.outputs.get(&p.output) else {
                continue;
            };

            match p.source {
                PlanSource::Crop { rect, fit, .. } if self.input_live => {
                    let placement = place(&rect, input_size, out_size, fit);
                    let u = CropUniform::new(&placement, input_size, out_size, scaling);
                    self.gpu
                        .queue
                        .write_buffer(&t.crop_uniform, 0, bytemuck::bytes_of(&u));
                    let mut pass = begin_pass(&mut enc, "crop", &t.rgba_view);
                    pass.set_pipeline(&self.crop.pipeline);
                    pass.set_bind_group(0, &t.crop_bind, &[]);
                    pass.draw(0..3, 0..1);
                }
                // Everything else is a generated picture. A `Crop` whose input
                // has gone away lands here too, which is what makes losing the
                // source a black frame rather than a frozen one.
                other => {
                    let kind = match other {
                        PlanSource::Pattern(Pattern::Bars) => 1u32,
                        _ => 0u32,
                    };
                    self.gpu.queue.write_buffer(
                        &t.fill_uniform,
                        0,
                        bytemuck::bytes_of(&ScalarUniform::new(kind)),
                    );
                    let mut pass = begin_pass(&mut enc, "fill", &t.rgba_view);
                    pass.set_pipeline(&self.fill.pipeline);
                    pass.set_bind_group(0, &t.fill_bind, &[]);
                    pass.draw(0..3, 0..1);
                }
            }

            // Pack to UYVY, then stage for readback.
            let uyvy_view = t.uyvy.create_view(&Default::default());
            {
                let mut pass = begin_pass(&mut enc, "pack", &uyvy_view);
                pass.set_pipeline(&self.pack.pipeline);
                pass.set_bind_group(0, &t.pack_bind, &[]);
                pass.draw(0..3, 0..1);
            }
            enc.copy_texture_to_buffer(
                wgpu::TexelCopyTextureInfo {
                    texture: &t.uyvy,
                    mip_level: 0,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                },
                wgpu::TexelCopyBufferInfo {
                    buffer: &t.staging,
                    layout: wgpu::TexelCopyBufferLayout {
                        offset: 0,
                        bytes_per_row: Some(t.padded_row),
                        rows_per_image: Some(t.size.h),
                    },
                },
                wgpu::Extent3d {
                    width: t.size.w.div_ceil(2),
                    height: t.size.h,
                    depth_or_array_layers: 1,
                },
            );
        }

        self.gpu.queue.submit([enc.finish()]);

        for id in &self.order.clone() {
            let Some(t) = self.outputs.get_mut(id) else {
                continue;
            };
            let slice = t.staging.slice(..);
            slice.map_async(wgpu::MapMode::Read, |_| {});
            self.gpu.device.poll(wgpu::PollType::Wait {
                submission_index: None,
                timeout: None,
            })?;
            {
                let view = slice.get_mapped_range();
                let row = t.row_bytes as usize;
                let padded = t.padded_row as usize;
                if row == padded {
                    t.frame.copy_from_slice(&view[..row * t.size.h as usize]);
                } else {
                    // The padded case. Not an exotic path — it is every raster
                    // whose row is not a multiple of 256 — so it is a plain
                    // row-at-a-time copy rather than anything clever.
                    for y in 0..t.size.h as usize {
                        let src = y * padded;
                        let dst = y * row;
                        t.frame[dst..dst + row].copy_from_slice(&view[src..src + row]);
                    }
                }
            }
            t.staging.unmap();
            sink(*id, &t.frame, t.row_bytes);
        }

        Ok(())
    }

    /// Downscale the input and every output into small RGBA buffers for the UI.
    ///
    /// The UI is deliberately fed by *copies* rather than by sharing the live
    /// textures. Sharing them would tie the GUI's frame loop to the frame path
    /// — and the frame path is on a clock that a dragged window or an open menu
    /// must not be able to stall. This runs on the render thread at its own
    /// slower rate; a thumbnail set at 320x180 is about a megabyte, so 15 Hz
    /// costs a few percent of what the outputs already cost.
    ///
    /// Each output thumbnail is a scaled copy of *that output's own finished
    /// picture*, so a thumbnail structurally cannot disagree with what is on
    /// the wire.
    pub fn capture_previews(&mut self, thumb_height: u32) -> Previews {
        let aspect = self.output_format.size.aspect().max(0.1);
        let out_thumb = Size::new(
            ((thumb_height as f64 * aspect) as u32).max(2) & !1,
            thumb_height.max(2),
        );
        let in_aspect = self.input.size.aspect().max(0.1);
        let in_h = thumb_height * 3;
        let in_thumb = Size::new(((in_h as f64 * in_aspect) as u32).max(2) & !1, in_h);

        let input = self
            .preview_of(PreviewKey::Input, in_thumb)
            .unwrap_or_default();
        let outputs = self
            .order
            .clone()
            .into_iter()
            .map(|id| {
                let rgba = self
                    .preview_of(PreviewKey::Output(id), out_thumb)
                    .unwrap_or_default();
                (id, rgba)
            })
            .collect();

        Previews {
            input_size: in_thumb,
            input,
            output_size: out_thumb,
            outputs,
        }
    }

    fn preview_of(&mut self, key: PreviewKey, size: Size) -> Result<Vec<u8>> {
        let source_view: &wgpu::TextureView = match key {
            PreviewKey::Input => &self.input.decoded_view,
            PreviewKey::Output(id) => match self.outputs.get(&id) {
                Some(t) => &t.rgba_view,
                None => return Ok(Vec::new()),
            },
        };

        let stale = self
            .previews
            .get(&key)
            .is_none_or(|p| p.size != size || p.generation != self.binding_generation);
        if stale {
            let p = make_preview(
                &self.gpu,
                &self.crop.layout,
                &self.sampler,
                source_view,
                size,
            );
            self.previews.insert(
                key,
                PreviewTarget {
                    generation: self.binding_generation,
                    ..p
                },
            );
        }
        let p = &self.previews[&key];

        // A straight full-frame copy at the thumbnail raster; the placement
        // maths is identical to a crop, so the same shader does it.
        let src_size = match key {
            PreviewKey::Input => self.input.size,
            PreviewKey::Output(_) => self.output_format.size,
        };
        let placement = place(&NormRect::FULL, src_size, size, kestrel_core::FitMode::Fit);
        let u = CropUniform::new(&placement, src_size, size, ScalingFilter::Bilinear);
        self.gpu
            .queue
            .write_buffer(&p.uniform, 0, bytemuck::bytes_of(&u));

        let mut enc = self
            .gpu
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("preview"),
            });
        {
            let mut pass = begin_pass(&mut enc, "preview", &p.view);
            pass.set_pipeline(&self.crop.pipeline);
            pass.set_bind_group(0, &p.bind, &[]);
            pass.draw(0..3, 0..1);
        }
        self.gpu.queue.submit([enc.finish()]);

        read_texture_rgba(&self.gpu, &p.tex, size)
    }

    /// The decoded input, for anything sharing this device.
    pub fn input_view(&self) -> &wgpu::TextureView {
        &self.input.decoded_view
    }

    /// An output's finished picture, for its thumbnail.
    pub fn output_view(&self, id: OutputId) -> Option<&wgpu::TextureView> {
        self.outputs.get(&id).map(|t| &t.rgba_view)
    }

    /// Read one output's RGB back to the CPU. Test and screenshot support only
    /// — the live path never does this.
    pub fn read_output_rgba(&self, id: OutputId) -> Result<Vec<u8>> {
        let t = self
            .outputs
            .get(&id)
            .ok_or_else(|| anyhow!("no such output {id}"))?;
        read_texture_rgba(&self.gpu, &t.rgba, t.size)
    }

    /// Read the decoded input back to the CPU. Tests only.
    pub fn read_input_rgba(&self) -> Result<Vec<u8>> {
        read_texture_rgba(&self.gpu, &self.input.decoded, self.input.size)
    }
}

// --- construction helpers -------------------------------------------------

fn make_stage(
    gpu: &Gpu,
    label: &str,
    body: &str,
    entry: &str,
    layout: wgpu::BindGroupLayout,
) -> Stage {
    let module = shader(&gpu.device, label, body);
    let pl = gpu
        .device
        .create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some(label),
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
    Stage {
        pipeline: gpu.fullscreen_pipeline(label, &module, &pl, entry),
        layout,
    }
}

fn make_input(gpu: &Gpu, layout: &wgpu::BindGroupLayout, size: Size) -> InputSurface {
    let macro_w = size.w.div_ceil(2).max(1);
    let upload = gpu.device.create_texture(&wgpu::TextureDescriptor {
        label: Some("input uyvy"),
        size: wgpu::Extent3d {
            width: macro_w,
            height: size.h.max(1),
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: TARGET_FORMAT,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    let decoded = gpu.target_texture("input rgb", size.w, size.h);
    let decoded_view = decoded.create_view(&Default::default());

    let uniform = gpu
        .device
        .create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("decode u"),
            contents: bytemuck::bytes_of(&ScalarUniform::new(macro_w)),
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        });
    let bind = gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("decode bg"),
        layout,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(
                    &upload.create_view(&Default::default()),
                ),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: uniform.as_entire_binding(),
            },
        ],
    });

    InputSurface {
        size,
        upload,
        decoded,
        decoded_view,
        bind,
    }
}

fn make_output(
    gpu: &Gpu,
    crop_layout: &wgpu::BindGroupLayout,
    fill_layout: &wgpu::BindGroupLayout,
    pack_layout: &wgpu::BindGroupLayout,
    sampler: &wgpu::Sampler,
    decoded_view: &wgpu::TextureView,
    size: Size,
) -> OutputTarget {
    let device = &gpu.device;
    let rgba = gpu.target_texture("output rgb", size.w, size.h);
    let rgba_view = rgba.create_view(&Default::default());
    let macro_w = size.w.div_ceil(2).max(1);
    let uyvy = gpu.target_texture("output uyvy", macro_w, size.h);

    let crop_uniform = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("crop u"),
        size: std::mem::size_of::<CropUniform>() as u64,
        usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let crop_bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("crop bg"),
        layout: crop_layout,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(decoded_view),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: wgpu::BindingResource::Sampler(sampler),
            },
            wgpu::BindGroupEntry {
                binding: 2,
                resource: crop_uniform.as_entire_binding(),
            },
        ],
    });

    let fill_uniform = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("fill u"),
        contents: bytemuck::bytes_of(&ScalarUniform::new(0)),
        usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
    });
    let fill_bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("fill bg"),
        layout: fill_layout,
        entries: &[wgpu::BindGroupEntry {
            binding: 0,
            resource: fill_uniform.as_entire_binding(),
        }],
    });

    let pack_uniform = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("pack u"),
        contents: bytemuck::bytes_of(&ScalarUniform::new(size.w)),
        usage: wgpu::BufferUsages::UNIFORM,
    });
    let pack_bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("pack bg"),
        layout: pack_layout,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(&rgba_view),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: pack_uniform.as_entire_binding(),
            },
        ],
    });

    let row_bytes = macro_w * 4;
    let padded_row = align_up(row_bytes, COPY_ALIGN);
    let staging = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("readback"),
        size: (padded_row * size.h) as u64,
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });

    OutputTarget {
        size,
        rgba,
        rgba_view,
        uyvy,
        crop_uniform,
        crop_bind,
        fill_uniform,
        fill_bind,
        pack_bind,
        staging,
        row_bytes,
        padded_row,
        frame: vec![0u8; (row_bytes * size.h) as usize],
    }
}

fn make_preview(
    gpu: &Gpu,
    crop_layout: &wgpu::BindGroupLayout,
    sampler: &wgpu::Sampler,
    source_view: &wgpu::TextureView,
    size: Size,
) -> PreviewTarget {
    let tex = gpu.target_texture("preview", size.w, size.h);
    let view = tex.create_view(&Default::default());
    let uniform = gpu.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("preview u"),
        size: std::mem::size_of::<CropUniform>() as u64,
        usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let bind = gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("preview bg"),
        layout: crop_layout,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(source_view),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: wgpu::BindingResource::Sampler(sampler),
            },
            wgpu::BindGroupEntry {
                binding: 2,
                resource: uniform.as_entire_binding(),
            },
        ],
    });
    PreviewTarget {
        size,
        tex,
        view,
        uniform,
        bind,
        generation: 0,
    }
}

fn begin_pass<'a>(
    enc: &'a mut wgpu::CommandEncoder,
    label: &str,
    view: &'a wgpu::TextureView,
) -> wgpu::RenderPass<'a> {
    enc.begin_render_pass(&wgpu::RenderPassDescriptor {
        label: Some(label),
        color_attachments: &[Some(wgpu::RenderPassColorAttachment {
            view,
            resolve_target: None,
            depth_slice: None,
            ops: wgpu::Operations {
                load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                store: wgpu::StoreOp::Store,
            },
        })],
        depth_stencil_attachment: None,
        timestamp_writes: None,
        occlusion_query_set: None,
        multiview_mask: None,
    })
}

fn texture_entry(binding: u32) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Texture {
            sample_type: wgpu::TextureSampleType::Float { filterable: true },
            view_dimension: wgpu::TextureViewDimension::D2,
            multisampled: false,
        },
        count: None,
    }
}

fn uniform_entry(binding: u32) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Buffer {
            ty: wgpu::BufferBindingType::Uniform,
            has_dynamic_offset: false,
            min_binding_size: None,
        },
        count: None,
    }
}

/// Pull an RGBA texture back to the CPU, de-padding as it goes.
pub fn read_texture_rgba(gpu: &Gpu, tex: &wgpu::Texture, size: Size) -> Result<Vec<u8>> {
    let row = size.w * 4;
    let padded = align_up(row, COPY_ALIGN);
    let buf = gpu.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("rgba readback"),
        size: (padded * size.h) as u64,
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let mut enc = gpu
        .device
        .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
    enc.copy_texture_to_buffer(
        wgpu::TexelCopyTextureInfo {
            texture: tex,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        wgpu::TexelCopyBufferInfo {
            buffer: &buf,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(padded),
                rows_per_image: Some(size.h),
            },
        },
        wgpu::Extent3d {
            width: size.w,
            height: size.h,
            depth_or_array_layers: 1,
        },
    );
    gpu.queue.submit([enc.finish()]);

    let slice = buf.slice(..);
    slice.map_async(wgpu::MapMode::Read, |_| {});
    gpu.device.poll(wgpu::PollType::Wait {
        submission_index: None,
        timeout: None,
    })?;
    let view = slice.get_mapped_range();
    let mut out = vec![0u8; (row * size.h) as usize];
    for y in 0..size.h as usize {
        let s = y * padded as usize;
        let d = y * row as usize;
        out[d..d + row as usize].copy_from_slice(&view[s..s + row as usize]);
    }
    drop(view);
    buf.unmap();
    Ok(out)
}

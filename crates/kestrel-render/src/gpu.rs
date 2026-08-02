//! Device setup and the resources every stage shares.

use anyhow::{anyhow, Result};

/// Every intermediate target uses this. **Not** an sRGB format: these pixels
/// arrived over SDI already encoded, and a gamma re-encode on the way through
/// would shift every colour on the output. A value of 128 in must be 128 out.
pub const TARGET_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;

/// wgpu requires a buffer copy's `bytes_per_row` to be a multiple of this.
///
/// It matters more here than it looks: an HD UYVY row is 3840 bytes, which
/// happens to be 15 × 256, so the naive "just memcpy it" path works perfectly
/// at 1080p and then corrupts every frame the first time somebody selects a
/// raster whose row is not a multiple of 256. The padded path is always taken;
/// the fast case is a memcpy inside it, not a different code path.
pub const COPY_ALIGN: u32 = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;

/// A wgpu device and queue.
pub struct Gpu {
    pub device: wgpu::Device,
    pub queue: wgpu::Queue,
    pub adapter: wgpu::Adapter,
    /// Kept because **a surface belongs to the instance that created it**.
    /// Pairing a surface with a device from a *different* instance panics deep
    /// inside wgpu-core with "Surface does not exist", which reads like a
    /// lifetime bug and is not one. Owning the instance here makes it
    /// impossible to end up with two.
    pub instance: wgpu::Instance,
    pub adapter_name: String,
    pub backend: wgpu::Backend,
}

impl Gpu {
    /// A headless device — no window, no surface. Used by the CLI and by tests.
    pub async fn new() -> Result<Self> {
        Self::with_instance(wgpu::Instance::default(), None).await
    }

    /// Adopt an existing instance, optionally picking an adapter that can
    /// present to `surface`. The surface must have come from this same
    /// instance — see [`Gpu::instance`].
    pub async fn with_instance(
        instance: wgpu::Instance,
        surface: Option<&wgpu::Surface<'_>>,
    ) -> Result<Self> {
        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::HighPerformance,
                compatible_surface: surface,
                force_fallback_adapter: false,
            })
            .await
            .map_err(|e| anyhow!("no suitable GPU adapter: {e}"))?;

        let info = adapter.get_info();
        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor {
                label: Some("kestrel"),
                required_features: wgpu::Features::empty(),
                // Default limits deliberately: they keep the Windows and Linux
                // paths open on hardware nobody has tested this on yet.
                required_limits: wgpu::Limits::default(),
                memory_hints: wgpu::MemoryHints::Performance,
                experimental_features: wgpu::ExperimentalFeatures::default(),
                trace: wgpu::Trace::Off,
            })
            .await
            .map_err(|e| anyhow!("could not open the GPU: {e}"))?;

        tracing::info!(adapter = %info.name, backend = ?info.backend, "GPU ready");

        Ok(Self {
            device,
            queue,
            adapter,
            instance,
            adapter_name: info.name,
            backend: info.backend,
        })
    }

    pub fn target_texture(&self, label: &str, w: u32, h: u32) -> wgpu::Texture {
        self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some(label),
            size: wgpu::Extent3d {
                width: w.max(1),
                height: h.max(1),
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: TARGET_FORMAT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                | wgpu::TextureUsages::TEXTURE_BINDING
                | wgpu::TextureUsages::COPY_SRC
                | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        })
    }

    /// A full-screen-triangle pipeline: no vertex buffer, no index buffer, the
    /// vertex shader builds the triangle from `vertex_index`.
    pub fn fullscreen_pipeline(
        &self,
        label: &str,
        shader: &wgpu::ShaderModule,
        layout: &wgpu::PipelineLayout,
        entry: &str,
    ) -> wgpu::RenderPipeline {
        self.device
            .create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(label),
                layout: Some(layout),
                vertex: wgpu::VertexState {
                    module: shader,
                    entry_point: Some("vs_fullscreen"),
                    buffers: &[],
                    compilation_options: Default::default(),
                },
                fragment: Some(wgpu::FragmentState {
                    module: shader,
                    entry_point: Some(entry),
                    targets: &[Some(wgpu::ColorTargetState {
                        format: TARGET_FORMAT,
                        blend: None,
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                    compilation_options: Default::default(),
                }),
                primitive: wgpu::PrimitiveState::default(),
                depth_stencil: None,
                multisample: wgpu::MultisampleState::default(),
                multiview_mask: None,
                cache: None,
            })
    }
}

/// Round `n` up to the next multiple of `align`.
pub fn align_up(n: u32, align: u32) -> u32 {
    n.div_ceil(align) * align
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hd_uyvy_rows_need_no_padding_but_the_maths_still_holds() {
        assert_eq!(align_up(3840, COPY_ALIGN), 3840);
        assert_eq!(align_up(2560, COPY_ALIGN), 2560);
        // The case the fast path would corrupt: 1024x768 UYVY is 2048 (fine),
        // but 1366 wide is 2732 -> 2816.
        assert_eq!(align_up(2732, COPY_ALIGN), 2816);
        assert_eq!(align_up(1, COPY_ALIGN), 256);
    }
}

//! The window.

use crate::preview::{PreviewInput, PreviewState};
use crate::{matrix, strip, theme};
use egui::{Color32, TextureOptions};
use kestrel_app::Runtime;
use kestrel_control::{Shared, StateView};
use kestrel_core::{common_formats, IdleFill, NormRect, OutputId, RoiId, ScalingFilter, Size};
use kestrel_render::Previews;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

#[derive(PartialEq, Clone, Copy)]
enum Tab {
    Live,
    Routing,
}

pub struct App {
    shared: Arc<Shared>,
    runtime: Runtime,
    show_path: Option<PathBuf>,
    tab: Tab,
    preview: PreviewState,
    input_tex: Option<egui::TextureHandle>,
    output_tex: HashMap<OutputId, egui::TextureHandle>,
    thumb_width: f32,
    status: String,
    control_addr: String,
}

impl App {
    pub fn new(
        cc: &eframe::CreationContext<'_>,
        shared: Arc<Shared>,
        runtime: Runtime,
        show_path: Option<PathBuf>,
        control_addr: String,
    ) -> Self {
        theme::apply(&cc.egui_ctx);
        Self {
            shared,
            runtime,
            show_path,
            tab: Tab::Live,
            preview: PreviewState::default(),
            input_tex: None,
            output_tex: HashMap::new(),
            thumb_width: 208.0,
            status: String::new(),
            control_addr,
        }
    }

    /// Move the latest thumbnails into egui textures.
    ///
    /// `set` on an existing handle rather than `load_texture` every frame:
    /// loading allocates a new texture id each time, and egui only frees the
    /// old ones lazily, so a 12 Hz reload leaks steadily over a show.
    fn upload_previews(&mut self, ctx: &egui::Context, previews: &Previews) {
        if !previews.input.is_empty() {
            let img = egui::ColorImage::from_rgba_unmultiplied(
                [
                    previews.input_size.w as usize,
                    previews.input_size.h as usize,
                ],
                &previews.input,
            );
            match &mut self.input_tex {
                Some(t) => t.set(img, TextureOptions::LINEAR),
                None => {
                    self.input_tex = Some(ctx.load_texture("input", img, TextureOptions::LINEAR))
                }
            }
        }

        for (id, bytes) in &previews.outputs {
            if bytes.is_empty() {
                continue;
            }
            let img = egui::ColorImage::from_rgba_unmultiplied(
                [
                    previews.output_size.w as usize,
                    previews.output_size.h as usize,
                ],
                bytes,
            );
            match self.output_tex.get_mut(id) {
                Some(t) => t.set(img, TextureOptions::LINEAR),
                None => {
                    self.output_tex.insert(
                        *id,
                        ctx.load_texture(format!("out{}", id.0), img, TextureOptions::LINEAR),
                    );
                }
            }
        }
        // Drop textures for outputs that no longer exist, or the strip keeps
        // showing a tile for a port that was deleted.
        let live: Vec<OutputId> = previews.outputs.iter().map(|(id, _)| *id).collect();
        self.output_tex.retain(|id, _| live.contains(id));
    }

    fn save(&mut self) {
        let Some(path) = self.show_path.clone() else {
            self.status = "No show file — use Save As.".into();
            return;
        };
        let show = self.shared.show().clone();
        match kestrel_app::save(&show, &path) {
            Ok(()) => self.status = format!("Saved {}", path.display()),
            Err(e) => self.status = format!("Save failed: {e}"),
        }
    }

    fn top_bar(&mut self, ui: &mut egui::Ui, state: &StateView) {
        egui::containers::Panel::top("top")
            .frame(egui::Frame::new().fill(theme::PANEL).inner_margin(10.0))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.heading("Kestrel");
                    ui.add_space(12.0);

                    // --- output format ---
                    ui.label("Output");
                    let current = state.output_format.name.clone();
                    egui::ComboBox::from_id_salt("fmt")
                        .selected_text(&current)
                        .width(120.0)
                        .show_ui(ui, |ui| {
                            for f in common_formats() {
                                let name = f.to_string();
                                if ui.selectable_label(name == current, &name).clicked() {
                                    self.shared.edit(|show| {
                                        show.output_format = f;
                                        // Locked regions follow the new aspect,
                                        // or every one of them would silently
                                        // start producing bars.
                                        show.reapply_aspect_locks();
                                    });
                                }
                            }
                        });

                    ui.add_space(8.0);
                    ui.label("Scaling");
                    let scaling = state.scaling.clone();
                    egui::ComboBox::from_id_salt("scaling")
                        .selected_text(&scaling)
                        .width(90.0)
                        .show_ui(ui, |ui| {
                            for (name, v) in [
                                ("bicubic", ScalingFilter::Bicubic),
                                ("bilinear", ScalingFilter::Bilinear),
                            ] {
                                if ui.selectable_label(scaling == name, name).clicked() {
                                    self.shared.edit(|show| show.scaling = v);
                                }
                            }
                        })
                        .response
                        .on_hover_text(
                            "Catmull-Rom bicubic is sharper on the 2x-and-beyond blow-ups \
                             this app exists to do. Bilinear is cheaper and softer.",
                        );

                    ui.add_space(12.0);
                    ui.separator();
                    ui.add_space(12.0);

                    // --- input ---
                    let (dot, tip) = if state.input.live {
                        (theme::OK, "Input locked")
                    } else {
                        (
                            theme::LIVE,
                            "No input — every routed output is carrying black",
                        )
                    };
                    ui.colored_label(dot, "●").on_hover_text(tip);
                    ui.label(format!(
                        "{}  {}x{}",
                        state.input.device.as_deref().unwrap_or("no input"),
                        state.input.width,
                        state.input.height
                    ));

                    // --- the global kill ---
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let on = state.outputs_enabled;
                        let button = egui::Button::new(
                            egui::RichText::new(if on { "OUTPUTS ON" } else { "OUTPUTS OFF" })
                                .strong()
                                .size(15.0)
                                .color(if on { Color32::WHITE } else { Color32::BLACK }),
                        )
                        .fill(if on { theme::OK } else { theme::LIVE })
                        .min_size(egui::vec2(150.0, 34.0));
                        if ui
                            .add(button)
                            .on_hover_text(
                                "Blacks every output. The outputs keep running — the signal \
                             never stops, so nothing downstream has to re-lock.",
                            )
                            .clicked()
                        {
                            self.shared.edit(|show| show.outputs_enabled = !on);
                        }

                        ui.add_space(10.0);
                        ui.selectable_value(&mut self.tab, Tab::Routing, "  Routing  ");
                        ui.selectable_value(&mut self.tab, Tab::Live, "  Live  ");
                    });
                });
            });
    }

    fn regions_panel(&mut self, ui: &mut egui::Ui, state: &StateView) {
        ui.horizontal(|ui| {
            ui.heading("Regions");
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui
                    .button("＋")
                    .on_hover_text("Add a region in the middle of the frame")
                    .clicked()
                {
                    let n = state.rois.len() + 1;
                    let id = self.shared.edit(|show| {
                        let id = show
                            .add_roi(format!("Region {n}"), NormRect::new(0.35, 0.35, 0.3, 0.3));
                        show.reapply_aspect_locks();
                        id
                    });
                    self.preview.selected = Some(id);
                }
            });
        });
        ui.add_space(4.0);

        let mut delete: Option<RoiId> = None;
        egui::ScrollArea::vertical()
            .id_salt("regions")
            .show(ui, |ui| {
                for roi in &state.rois {
                    let selected = self.preview.selected == Some(roi.id);
                    let frame = egui::Frame::new()
                        .fill(if selected {
                            Color32::from_rgb(38, 42, 50)
                        } else {
                            theme::PANEL
                        })
                        .inner_margin(8.0)
                        .corner_radius(4.0);
                    frame.show(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.colored_label(theme::roi_colour(roi.colour), "█");
                            let mut name = roi.name.clone();
                            if ui
                                .add(
                                    egui::TextEdit::singleline(&mut name)
                                        .desired_width(f32::INFINITY)
                                        .background_color(egui::Color32::TRANSPARENT),
                                )
                                .changed()
                            {
                                self.shared.edit(|show| {
                                    if let Some(r) = show.roi_mut(roi.id) {
                                        r.name = name.clone();
                                    }
                                });
                            }
                        });
                        ui.horizontal(|ui| {
                            let mut lock = roi.lock_aspect;
                            if ui
                                .checkbox(&mut lock, "Lock aspect")
                                .on_hover_text(
                                    "Keep this region at the output's aspect while it is \
                                     resized, so it never produces bars nobody asked for.",
                                )
                                .changed()
                            {
                                self.shared.edit(|show| {
                                    let target = show.output_format.size.aspect();
                                    let input = show.input_size;
                                    if let Some(r) = show.roi_mut(roi.id) {
                                        r.lock_aspect = lock;
                                        if lock {
                                            r.rect = r.rect.with_aspect(target, input);
                                        }
                                    }
                                });
                            }
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    if ui
                                        .small_button("✕")
                                        .on_hover_text(
                                            "Delete. Any output carrying it drops to its idle \
                                         fill — it does not stop.",
                                        )
                                        .clicked()
                                    {
                                        delete = Some(roi.id);
                                    }
                                },
                            );
                        });
                        if roi.outputs.is_empty() {
                            ui.label(
                                egui::RichText::new("not routed")
                                    .color(theme::TEXT_DIM)
                                    .size(11.0),
                            );
                        } else {
                            let names: Vec<&str> = roi
                                .outputs
                                .iter()
                                .filter_map(|id| state.outputs.iter().find(|o| o.id == *id))
                                .map(|o| o.label.as_str())
                                .collect();
                            ui.label(
                                egui::RichText::new(format!("▸ {}", names.join(", ")))
                                    .color(theme::roi_colour(roi.colour))
                                    .size(11.0),
                            );
                        }
                    });
                    ui.add_space(4.0);
                }
            });

        if let Some(id) = delete {
            let _ = self.shared.edit(|show| show.remove_roi(id));
            if self.preview.selected == Some(id) {
                self.preview.selected = None;
            }
        }
    }

    fn output_settings(&mut self, ui: &mut egui::Ui, state: &StateView, id: OutputId) {
        let Some(o) = state.outputs.iter().find(|o| o.id == id) else {
            return;
        };
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new(&o.label).strong());
            ui.separator();

            ui.label("Idle");
            let idle = o.idle;
            egui::ComboBox::from_id_salt(("idle", id.0))
                .selected_text(match idle {
                    IdleFill::Black => "black",
                    IdleFill::FullInput => "full input",
                    IdleFill::Bars => "bars",
                })
                .width(100.0)
                .show_ui(ui, |ui| {
                    for (name, v) in [
                        ("black", IdleFill::Black),
                        ("full input", IdleFill::FullInput),
                        ("bars", IdleFill::Bars),
                    ] {
                        if ui.selectable_label(idle == v, name).clicked() {
                            self.shared.edit(|show| {
                                if let Some(o) = show.output_mut(id) {
                                    o.idle = v;
                                }
                            });
                        }
                    }
                })
                .response
                .on_hover_text("What this output carries when nothing is routed to it.");

            ui.label("Fit");
            let fit = o.fit.clone();
            egui::ComboBox::from_id_salt(("fit", id.0))
                .selected_text(&fit)
                .width(90.0)
                .show_ui(ui, |ui| {
                    use kestrel_core::FitMode::*;
                    for (name, v) in [("fit", Fit), ("fill", Fill), ("stretch", Stretch)] {
                        if ui.selectable_label(fit == name, name).clicked() {
                            self.shared.edit(|show| {
                                if let Some(o) = show.output_mut(id) {
                                    o.fit = v;
                                }
                            });
                        }
                    }
                });

            ui.separator();
            ui.label("Card");
            let devices = kestrel_decklink::list_devices().unwrap_or_default();
            let current = o.device.clone().unwrap_or_else(|| "none".into());
            egui::ComboBox::from_id_salt(("dev", id.0))
                .selected_text(&current)
                .width(200.0)
                .show_ui(ui, |ui| {
                    if ui.selectable_label(o.device.is_none(), "none").clicked() {
                        self.shared.edit(|show| {
                            if let Some(o) = show.output_mut(id) {
                                o.device = None;
                            }
                        });
                    }
                    for d in devices.iter().filter(|d| d.has_output) {
                        // Inactive ports are listed but not selectable, and say
                        // why: an empty menu on a card that is plainly plugged
                        // in is the thing that wastes an afternoon.
                        if !d.active {
                            ui.add_enabled(false, egui::Button::new(d.menu_label()));
                            continue;
                        }
                        if ui.selectable_label(current == d.name, &d.name).clicked() {
                            let dev = kestrel_core::DeviceRef {
                                persistent_id: d.persistent_id,
                                display_name: d.name.clone(),
                            };
                            self.shared.edit(|show| {
                                if let Some(o) = show.output_mut(id) {
                                    o.device = Some(dev.clone());
                                }
                            });
                        }
                    }
                });
        });
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        let previews = self.runtime.previews();
        self.upload_previews(&ctx, &previews);
        let state = self.shared.snapshot();

        self.top_bar(ui, &state);

        egui::containers::Panel::bottom("status")
            .frame(egui::Frame::new().fill(theme::PANEL).inner_margin(6.0))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(
                        egui::RichText::new(&state.decklink)
                            .color(theme::TEXT_DIM)
                            .size(11.0),
                    );
                    ui.separator();
                    ui.label(
                        egui::RichText::new(format!("control: {}", self.control_addr))
                            .color(theme::TEXT_DIM)
                            .size(11.0),
                    );
                    ui.separator();
                    ui.label(
                        egui::RichText::new(format!("{} frames", state.frames))
                            .color(theme::TEXT_DIM)
                            .size(11.0),
                    );
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.small_button("Save").clicked() {
                            self.save();
                        }
                        if !self.status.is_empty() {
                            ui.label(
                                egui::RichText::new(&self.status)
                                    .color(theme::TEXT_DIM)
                                    .size(11.0),
                            );
                        }
                    });
                });
            });

        match self.tab {
            Tab::Routing => {
                egui::CentralPanel::default().show(ui, |ui| {
                    matrix::show(ui, &state, &self.shared);
                });
            }
            Tab::Live => {
                egui::containers::Panel::bottom("strip")
                    .frame(egui::Frame::new().fill(theme::BG).inner_margin(8.0))
                    .resizable(false)
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.label(
                                egui::RichText::new("Outputs")
                                    .color(theme::TEXT_DIM)
                                    .size(11.0),
                            );
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    ui.add(
                                        egui::Slider::new(&mut self.thumb_width, 120.0..=320.0)
                                            .show_value(false),
                                    )
                                    .on_hover_text("Thumbnail size");
                                },
                            );
                        });
                        let clicked = strip::show(
                            ui,
                            strip::StripInput {
                                textures: &self.output_tex,
                                state: &state,
                                thumb_width: self.thumb_width,
                            },
                        );
                        if let Some(id) = clicked {
                            self.preview.selected = state
                                .outputs
                                .iter()
                                .find(|o| o.id == id)
                                .and_then(|o| o.assigned);
                        }
                        ui.add_space(4.0);
                        // Settings for whichever output is selected, or the
                        // first — one row rather than a panel per output, which
                        // at eight outputs is unreadable.
                        let target = self
                            .preview
                            .selected
                            .and_then(|roi| state.outputs.iter().find(|o| o.assigned == Some(roi)))
                            .or(state.outputs.first())
                            .map(|o| o.id);
                        if let Some(id) = target {
                            self.output_settings(ui, &state, id);
                        }
                    });

                egui::containers::Panel::right("regions")
                    .frame(egui::Frame::new().fill(theme::BG).inner_margin(10.0))
                    .default_size(240.0)
                    .show(ui, |ui| self.regions_panel(ui, &state));

                egui::CentralPanel::default()
                    .frame(egui::Frame::new().fill(theme::BG).inner_margin(10.0))
                    .show(ui, |ui| {
                        self.preview.show(
                            ui,
                            PreviewInput {
                                texture: self.input_tex.as_ref(),
                                input_size: Size::new(state.input.width, state.input.height),
                                state: &state,
                                shared: &self.shared,
                            },
                        );
                    });
            }
        }

        // Repaint at roughly the preview rate. The frame path is on its own
        // thread and does not care what this window does, so there is no reason
        // to spin here.
        ctx.request_repaint_after(std::time::Duration::from_millis(60));
    }
}

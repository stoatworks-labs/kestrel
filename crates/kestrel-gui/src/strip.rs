//! The row of output previews along the bottom.
//!
//! One tile per output, always — an output carrying nothing gets a tile showing
//! the black it is really putting out, because the whole point of the app's
//! "always output" rule is that there is no such thing as an output that is not
//! doing anything.

use crate::theme;
use egui::{Color32, Sense, Stroke, StrokeKind, Vec2};
use kestrel_control::StateView;
use kestrel_core::{scale_quality, OutputId};
use std::collections::HashMap;

pub struct StripInput<'a> {
    pub textures: &'a HashMap<OutputId, egui::TextureHandle>,
    pub state: &'a StateView,
    pub thumb_width: f32,
}

/// Draw the strip. Returns an output the user clicked, if any.
pub fn show(ui: &mut egui::Ui, input: StripInput<'_>) -> Option<OutputId> {
    let mut clicked = None;
    let thumb = Vec2::new(input.thumb_width, input.thumb_width * 9.0 / 16.0);

    egui::ScrollArea::horizontal()
        .auto_shrink([false, true])
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                for o in &input.state.outputs {
                    ui.vertical(|ui| {
                        ui.set_width(thumb.x);
                        let (rect, response) = ui.allocate_exact_size(thumb, Sense::click());
                        let painter = ui.painter_at(rect);
                        painter.rect_filled(rect, 3.0, Color32::from_rgb(8, 9, 11));

                        if let Some(tex) = input.textures.get(&o.id) {
                            painter.image(
                                tex.id(),
                                rect,
                                egui::Rect::from_min_max(egui::Pos2::ZERO, egui::pos2(1.0, 1.0)),
                                Color32::WHITE,
                            );
                        }

                        // A live tile is ringed in the region's own colour, so
                        // the strip and the preview agree without reading a
                        // word.
                        let on_air = o.on_air == "region" || o.on_air == "full input";
                        let ring = if !input.state.outputs_enabled {
                            theme::LIVE
                        } else if on_air {
                            o.assigned
                                .and_then(|id| input.state.rois.iter().find(|r| r.id == id))
                                .map(|r| theme::roi_colour(r.colour))
                                .unwrap_or(theme::OK)
                        } else {
                            theme::LINE
                        };
                        painter.rect_stroke(
                            rect,
                            3.0,
                            Stroke::new(if on_air { 2.5 } else { 1.0 }, ring),
                            StrokeKind::Inside,
                        );

                        if !input.state.outputs_enabled {
                            painter.rect_filled(rect, 3.0, Color32::from_black_alpha(150));
                            painter.text(
                                rect.center(),
                                egui::Align2::CENTER_CENTER,
                                "OFF",
                                egui::FontId::proportional(20.0),
                                theme::LIVE,
                            );
                        }

                        if response.clicked() {
                            clicked = Some(o.id);
                        }

                        ui.horizontal(|ui| {
                            ui.label(egui::RichText::new(&o.label).strong().size(12.0));
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    // The scale badge. The number this whole
                                    // window exists to put in front of someone.
                                    match o.scale_percent {
                                        Some(pct) => {
                                            let q = scale_quality(pct / 100.0);
                                            ui.label(
                                                egui::RichText::new(format!("{pct:.0}%"))
                                                    .color(theme::scale_colour(q))
                                                    .strong()
                                                    .size(13.0),
                                            )
                                            .on_hover_text(scale_hint(pct));
                                        }
                                        None => {
                                            ui.label(
                                                egui::RichText::new("—")
                                                    .color(theme::TEXT_DIM)
                                                    .size(13.0),
                                            );
                                        }
                                    }
                                },
                            );
                        });

                        let (text, colour) = match o.assigned_name.as_deref() {
                            Some(n) if input.state.outputs_enabled => (n.to_string(), ring),
                            Some(n) => (format!("{n} (muted)"), theme::TEXT_DIM),
                            None => (o.on_air.clone(), theme::TEXT_DIM),
                        };
                        ui.label(egui::RichText::new(text).color(colour).size(11.0));

                        let device = o.device.as_deref().unwrap_or("no card assigned");
                        ui.label(
                            egui::RichText::new(device)
                                .color(theme::TEXT_DIM)
                                .size(10.0),
                        )
                        .on_hover_text(match o.buffered {
                            Some(b) => format!(
                                "card buffer: {b} frames.\nA number that walks steadily up \
                                 or down means Kestrel's clock and the card's disagree."
                            ),
                            None => "This output is rendered but has nowhere to go — \
                                     assign a DeckLink port to it."
                                .into(),
                        });
                    });
                    ui.add_space(4.0);
                }
            });
        });

    clicked
}

fn scale_hint(pct: f64) -> String {
    if pct <= 100.0 {
        format!(
            "{pct:.0}% — at or below 1:1. Every output pixel is backed by a real \
             source pixel."
        )
    } else {
        format!(
            "{pct:.0}% — this output is inventing {:.0}% of its detail. Past 200% \
             it is visible on a big screen; the fix is a tighter camera, not a \
             better scaler.",
            100.0 - (100.0 / pct * 100.0)
        )
    }
}

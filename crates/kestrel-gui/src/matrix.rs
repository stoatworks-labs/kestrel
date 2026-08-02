//! The crosspoint matrix: regions down the side, outputs across the top.
//!
//! Laid out like an AV router's patch grid because that is what it is, and an
//! operator who has used one should not have to learn anything. The one rule
//! worth stating: a **column** holds at most one crosspoint (an output shows
//! one thing), a **row** holds as many as you like (a region can feed a screen
//! and a record feed at once).

use crate::theme;
use egui::{Color32, Sense, Stroke, StrokeKind, Vec2};
use kestrel_control::{Shared, StateView};
use std::sync::Arc;

const CELL: f32 = 42.0;
const ROW_LABEL: f32 = 170.0;

pub fn show(ui: &mut egui::Ui, state: &StateView, shared: &Arc<Shared>) {
    ui.horizontal(|ui| {
        ui.heading("Routing");
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if !state.outputs_enabled {
                ui.label(
                    egui::RichText::new("OUTPUTS OFF — every output is black")
                        .color(theme::LIVE)
                        .strong(),
                );
            }
        });
    });
    ui.label(
        egui::RichText::new(
            "Click a crosspoint to take that region to that output; click it again to \
             clear. An output with no crosspoint keeps running and carries its idle fill.",
        )
        .color(theme::TEXT_DIM)
        .size(11.0),
    );
    ui.add_space(8.0);

    if state.rois.is_empty() {
        ui.label(
            egui::RichText::new("No regions yet — draw one on the Live tab.")
                .color(theme::TEXT_DIM),
        );
        return;
    }

    egui::ScrollArea::both().show(ui, |ui| {
        // --- column headers ---
        ui.horizontal(|ui| {
            ui.allocate_exact_size(Vec2::new(ROW_LABEL, CELL), Sense::hover());
            for o in &state.outputs {
                let (rect, response) =
                    ui.allocate_exact_size(Vec2::new(CELL, CELL), Sense::hover());
                let painter = ui.painter_at(rect.expand(2.0));
                let live = o.assigned.is_some() && state.outputs_enabled;
                painter.text(
                    rect.center(),
                    egui::Align2::CENTER_CENTER,
                    short_label(&o.label),
                    egui::FontId::proportional(12.0),
                    if live {
                        Color32::WHITE
                    } else {
                        theme::TEXT_DIM
                    },
                );
                response.on_hover_text(format!(
                    "{}\n{}\ncarrying: {}",
                    o.label,
                    o.device.as_deref().unwrap_or("no card assigned"),
                    o.on_air
                ));
            }
        });

        // --- one row per region ---
        for roi in &state.rois {
            ui.horizontal(|ui| {
                let (label_rect, _) =
                    ui.allocate_exact_size(Vec2::new(ROW_LABEL, CELL), Sense::hover());
                let painter = ui.painter_at(label_rect);
                let colour = theme::roi_colour(roi.colour);
                painter.rect_filled(
                    egui::Rect::from_min_size(
                        label_rect.left_top() + Vec2::new(0.0, 8.0),
                        Vec2::new(4.0, CELL - 16.0),
                    ),
                    2.0,
                    colour,
                );
                painter.text(
                    label_rect.left_center() + Vec2::new(12.0, 0.0),
                    egui::Align2::LEFT_CENTER,
                    &roi.name,
                    egui::FontId::proportional(13.0),
                    Color32::WHITE,
                );

                for o in &state.outputs {
                    let (rect, response) =
                        ui.allocate_exact_size(Vec2::new(CELL, CELL), Sense::click());
                    let painter = ui.painter_at(rect);
                    let inner = rect.shrink(4.0);
                    let taken = o.assigned == Some(roi.id);

                    let bg = if taken && state.outputs_enabled {
                        colour
                    } else if taken {
                        colour.gamma_multiply(0.35)
                    } else if response.hovered() {
                        Color32::from_rgb(46, 50, 58)
                    } else {
                        theme::PANEL
                    };
                    painter.rect_filled(inner, 3.0, bg);
                    painter.rect_stroke(
                        inner,
                        3.0,
                        Stroke::new(1.0, theme::LINE),
                        StrokeKind::Inside,
                    );

                    if taken {
                        painter.text(
                            inner.center(),
                            egui::Align2::CENTER_CENTER,
                            "●",
                            egui::FontId::proportional(15.0),
                            Color32::from_rgb(20, 20, 20),
                        );
                    }

                    if response.clicked() {
                        // Toggling in the core rather than here: exclusivity
                        // down a column is structural there, and duplicating
                        // the rule in the UI is how the two drift apart.
                        let _ = shared.edit(|show| show.toggle_crosspoint(o.id, roi.id));
                    }

                    response.on_hover_text(if taken {
                        format!("Clear {} from {}", roi.name, o.label)
                    } else {
                        format!("Take {} to {}", roi.name, o.label)
                    });
                }
            });
        }
    });
}

/// Column headers are one cell wide, so long labels get shortened rather than
/// overlapping their neighbours. The full label is in the tooltip.
fn short_label(label: &str) -> String {
    let digits: String = label.chars().filter(|c| c.is_ascii_digit()).collect();
    if !digits.is_empty() && digits.len() <= 2 {
        return digits;
    }
    label.chars().take(3).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn column_headers_shorten_to_something_recognisable() {
        assert_eq!(short_label("Output 3"), "3");
        assert_eq!(short_label("DeckLink Duo (2)"), "2");
        assert_eq!(short_label("SDI 12"), "12");
        // No digits to grab: fall back to a prefix rather than an empty header.
        assert_eq!(short_label("Foldback"), "Fol");
    }
}

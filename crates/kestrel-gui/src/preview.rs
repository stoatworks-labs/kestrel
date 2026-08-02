//! The large input preview, and the region editing that happens on it.

use crate::theme;
use egui::{Color32, Pos2, Rect, Sense, Stroke, StrokeKind, Vec2};
use kestrel_control::{Shared, StateView};
use kestrel_core::{NormRect, RoiId, Size};
use std::sync::Arc;

/// Screen-space radius of a corner grab handle.
const HANDLE: f32 = 7.0;

#[derive(Clone, Copy, PartialEq)]
enum Grab {
    Move,
    /// 0 = top-left, 1 = top-right, 2 = bottom-right, 3 = bottom-left.
    Corner(usize),
}

struct Drag {
    roi: RoiId,
    grab: Grab,
    /// The rect as it was when the drag started. Every frame recomputes from
    /// this plus the total delta rather than accumulating per-frame deltas —
    /// accumulation drifts, and drift in a region is a shot slowly sliding off
    /// the thing it was framing.
    start: NormRect,
    start_pointer: Pos2,
}

#[derive(Default)]
pub struct PreviewState {
    pub selected: Option<RoiId>,
    drag: Option<Drag>,
}

pub struct PreviewInput<'a> {
    pub texture: Option<&'a egui::TextureHandle>,
    pub input_size: Size,
    pub state: &'a StateView,
    pub shared: &'a Arc<Shared>,
}

impl PreviewState {
    pub fn show(&mut self, ui: &mut egui::Ui, input: PreviewInput<'_>) {
        let available = ui.available_size();
        let aspect = input.input_size.aspect().max(0.1) as f32;
        // Letterbox the preview inside whatever space is going, so the regions
        // drawn on it are geometrically honest. A stretched preview would make
        // a 16:9 crop look like something else, which is the one thing this
        // window must not do.
        let mut size = Vec2::new(available.x, available.x / aspect);
        if size.y > available.y {
            size = Vec2::new(available.y * aspect, available.y);
        }
        let (rect, response) = ui.allocate_exact_size(size, Sense::click_and_drag());
        let painter = ui.painter_at(rect);

        painter.rect_filled(rect, 4.0, Color32::from_rgb(10, 11, 13));
        if let Some(tex) = input.texture {
            painter.image(
                tex.id(),
                rect,
                Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
                Color32::WHITE,
            );
        }

        if !input.state.input.live {
            painter.rect_filled(rect, 4.0, Color32::from_black_alpha(180));
            painter.text(
                rect.center(),
                egui::Align2::CENTER_CENTER,
                "NO INPUT",
                egui::FontId::proportional(28.0),
                theme::LIVE,
            );
        }

        self.handle_input(&response, rect, &input);
        self.paint_regions(&painter, rect, &input);

        painter.rect_stroke(rect, 4.0, Stroke::new(1.0, theme::LINE), StrokeKind::Inside);
    }

    fn paint_regions(&self, painter: &egui::Painter, area: Rect, input: &PreviewInput<'_>) {
        for roi in &input.state.rois {
            let r = to_screen(&norm(roi.rect), area);
            let colour = theme::roi_colour(roi.colour);
            let on_air = !roi.outputs.is_empty() && input.state.outputs_enabled;
            let selected = self.selected == Some(roi.id);

            // A region that is on air is drawn thicker and tinted. Tally has to
            // be readable at a glance from a metre away, not by reading a list.
            let width = if on_air { 3.0 } else { 1.5 };
            if on_air {
                painter.rect_filled(r, 2.0, colour.gamma_multiply(0.12));
            }
            painter.rect_stroke(r, 2.0, Stroke::new(width, colour), StrokeKind::Middle);

            let label = if roi.outputs.is_empty() {
                roi.name.clone()
            } else {
                let names: Vec<String> = roi
                    .outputs
                    .iter()
                    .filter_map(|id| input.state.outputs.iter().find(|o| o.id == *id))
                    .map(|o| o.label.clone())
                    .collect();
                format!("{}  ▸ {}", roi.name, names.join(", "))
            };
            let anchor = r.left_top() + Vec2::new(4.0, -6.0);
            let galley =
                painter.layout_no_wrap(label, egui::FontId::proportional(12.0), Color32::WHITE);
            let bg = Rect::from_min_size(
                anchor - Vec2::new(2.0, galley.size().y),
                galley.size() + Vec2::new(6.0, 2.0),
            );
            painter.rect_filled(bg, 2.0, colour.gamma_multiply(0.85));
            painter.galley(
                anchor - Vec2::new(0.0, galley.size().y),
                galley,
                Color32::BLACK,
            );

            if selected {
                for c in corners(r) {
                    painter.rect_filled(
                        Rect::from_center_size(c, Vec2::splat(HANDLE * 2.0)),
                        1.0,
                        Color32::WHITE,
                    );
                    painter.rect_stroke(
                        Rect::from_center_size(c, Vec2::splat(HANDLE * 2.0)),
                        1.0,
                        Stroke::new(1.0, Color32::BLACK),
                        StrokeKind::Inside,
                    );
                }
            }
        }
    }

    fn handle_input(&mut self, response: &egui::Response, area: Rect, input: &PreviewInput<'_>) {
        if response.drag_started() {
            if let Some(p) = response.interact_pointer_pos() {
                self.drag = self.begin_drag(p, area, input);
                if let Some(d) = &self.drag {
                    self.selected = Some(d.roi);
                }
            }
        }

        if let (Some(drag), Some(p)) = (&self.drag, response.interact_pointer_pos()) {
            let dx = ((p.x - drag.start_pointer.x) / area.width().max(1.0)) as f64;
            let dy = ((p.y - drag.start_pointer.y) / area.height().max(1.0)) as f64;
            let next = apply_grab(drag.start, drag.grab, dx, dy);
            let (roi, aspect, in_size) = (drag.roi, None::<f64>, input.input_size);
            let _ = aspect;
            input.shared.edit(|show| {
                let target = show.output_format.size.aspect();
                if let Some(r) = show.roi_mut(roi) {
                    let locked = r.lock_aspect;
                    r.rect = if locked && drag.grab != Grab::Move {
                        // Moving never changes shape, so the lock only bites on
                        // a resize — otherwise dragging a locked region across
                        // the frame would keep nudging it a pixel at a time.
                        next.clamped().with_aspect(target, in_size)
                    } else {
                        next.clamped()
                    };
                }
            });
        }

        if response.drag_stopped() {
            self.drag = None;
        }

        // A click on empty space clears the selection, which is how you stop
        // nudging a region with the arrow keys.
        if response.clicked() {
            if let Some(p) = response.interact_pointer_pos() {
                self.selected = hit_region(p, area, input).map(|(id, _)| id);
            }
        }
    }

    fn begin_drag(&self, p: Pos2, area: Rect, input: &PreviewInput<'_>) -> Option<Drag> {
        // Corners of the *selected* region win over any region's body, so a
        // handle sitting on top of another region still resizes rather than
        // dragging the one underneath.
        if let Some(sel) = self.selected {
            if let Some(roi) = input.state.rois.iter().find(|r| r.id == sel) {
                let screen = to_screen(&norm(roi.rect), area);
                for (i, c) in corners(screen).into_iter().enumerate() {
                    if (c - p).length() <= HANDLE * 1.8 {
                        return Some(Drag {
                            roi: sel,
                            grab: Grab::Corner(i),
                            start: norm(roi.rect),
                            start_pointer: p,
                        });
                    }
                }
            }
        }
        hit_region(p, area, input).map(|(id, rect)| Drag {
            roi: id,
            grab: Grab::Move,
            start: rect,
            start_pointer: p,
        })
    }
}

/// The topmost region under a point. Later regions are drawn on top, so they
/// are searched first.
fn hit_region(p: Pos2, area: Rect, input: &PreviewInput<'_>) -> Option<(RoiId, NormRect)> {
    input
        .state
        .rois
        .iter()
        .rev()
        .find(|r| to_screen(&norm(r.rect), area).contains(p))
        .map(|r| (r.id, norm(r.rect)))
}

fn apply_grab(start: NormRect, grab: Grab, dx: f64, dy: f64) -> NormRect {
    match grab {
        Grab::Move => NormRect::new(start.x + dx, start.y + dy, start.w, start.h),
        Grab::Corner(i) => {
            // Drag the grabbed corner, hold the opposite one. Anything else
            // makes a resize feel like the region is running away.
            let (mut x0, mut y0) = (start.x, start.y);
            let (mut x1, mut y1) = (start.right(), start.bottom());
            match i {
                0 => {
                    x0 += dx;
                    y0 += dy;
                }
                1 => {
                    x1 += dx;
                    y0 += dy;
                }
                2 => {
                    x1 += dx;
                    y1 += dy;
                }
                _ => {
                    x0 += dx;
                    y1 += dy;
                }
            }
            // Dragging a corner past its opposite flips the rect rather than
            // collapsing it to nothing.
            let (lx, rx) = if x0 <= x1 { (x0, x1) } else { (x1, x0) };
            let (ty, by) = if y0 <= y1 { (y0, y1) } else { (y1, y0) };
            NormRect::new(lx, ty, rx - lx, by - ty)
        }
    }
}

fn norm(r: [f64; 4]) -> NormRect {
    NormRect::new(r[0], r[1], r[2], r[3])
}

fn to_screen(r: &NormRect, area: Rect) -> Rect {
    Rect::from_min_size(
        area.min + Vec2::new(r.x as f32 * area.width(), r.y as f32 * area.height()),
        Vec2::new(r.w as f32 * area.width(), r.h as f32 * area.height()),
    )
}

fn corners(r: Rect) -> [Pos2; 4] {
    [
        r.left_top(),
        r.right_top(),
        r.right_bottom(),
        r.left_bottom(),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    const AREA: Rect = Rect {
        min: Pos2 { x: 100.0, y: 50.0 },
        max: Pos2 { x: 900.0, y: 500.0 },
    };

    #[test]
    fn a_region_maps_to_the_right_part_of_the_preview() {
        let r = to_screen(&NormRect::new(0.5, 0.0, 0.5, 1.0), AREA);
        assert!((r.min.x - 500.0).abs() < 1e-3, "{r:?}");
        assert!((r.max.x - 900.0).abs() < 1e-3);
        assert!((r.height() - 450.0).abs() < 1e-3);
    }

    #[test]
    fn moving_changes_position_and_never_size() {
        let start = NormRect::new(0.2, 0.2, 0.3, 0.3);
        let moved = apply_grab(start, Grab::Move, 0.1, -0.05);
        assert!((moved.x - 0.3).abs() < 1e-12);
        assert!((moved.y - 0.15).abs() < 1e-12);
        assert!((moved.w - start.w).abs() < 1e-12, "size must not drift");
        assert!((moved.h - start.h).abs() < 1e-12);
    }

    #[test]
    fn dragging_a_corner_holds_the_opposite_one_still() {
        let start = NormRect::new(0.2, 0.2, 0.4, 0.4);
        // Top-left corner in by 0.1 on both axes.
        let r = apply_grab(start, Grab::Corner(0), 0.1, 0.1);
        assert!(
            (r.right() - start.right()).abs() < 1e-12,
            "right edge moved"
        );
        assert!((r.bottom() - start.bottom()).abs() < 1e-12, "bottom moved");
        assert!((r.x - 0.3).abs() < 1e-12);
        assert!((r.w - 0.3).abs() < 1e-12);
    }

    #[test]
    fn each_corner_holds_its_own_opposite() {
        let start = NormRect::new(0.3, 0.3, 0.2, 0.2);
        let d = 0.05;
        let br = apply_grab(start, Grab::Corner(2), d, d);
        assert!((br.x - start.x).abs() < 1e-12 && (br.y - start.y).abs() < 1e-12);
        let tr = apply_grab(start, Grab::Corner(1), d, -d);
        assert!((tr.x - start.x).abs() < 1e-12 && (tr.bottom() - start.bottom()).abs() < 1e-12);
        let bl = apply_grab(start, Grab::Corner(3), -d, d);
        assert!((bl.right() - start.right()).abs() < 1e-12 && (bl.y - start.y).abs() < 1e-12);
    }

    #[test]
    fn dragging_a_corner_past_the_opposite_flips_rather_than_collapsing() {
        let start = NormRect::new(0.3, 0.3, 0.2, 0.2);
        let r = apply_grab(start, Grab::Corner(0), 0.35, 0.35);
        assert!(
            r.w > 0.0 && r.h > 0.0,
            "a flipped drag must stay a rectangle: {r:?}"
        );
        assert!((r.x - 0.5).abs() < 1e-12, "{r:?}");
    }

    #[test]
    fn a_drag_is_computed_from_the_start_rect_so_it_cannot_drift() {
        // Twenty small steps must land exactly where one big step does.
        let start = NormRect::new(0.1, 0.1, 0.3, 0.3);
        let total = apply_grab(start, Grab::Move, 0.2, 0.2);
        let mut stepped = start;
        for i in 1..=20 {
            stepped = apply_grab(start, Grab::Move, 0.01 * i as f64, 0.01 * i as f64);
        }
        assert!((stepped.x - total.x).abs() < 1e-12);
        assert!((stepped.y - total.y).abs() < 1e-12);
    }
}

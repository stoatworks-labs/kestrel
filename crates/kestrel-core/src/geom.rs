//! Rectangles, and the scaling arithmetic the operator actually reads off the
//! screen.

use serde::{Deserialize, Serialize};

/// A rectangle in **normalised input coordinates**: `0.0..1.0` across the input
/// raster, origin top-left.
///
/// Normalised rather than pixels on purpose. A region of interest is a decision
/// about the *stage* — "the lectern", "stage left" — and that decision does not
/// change when the camera operator swaps a 1080p feed for a 2160p one, or when
/// the input format redetects mid-show. Storing pixels would silently move
/// every region the first time the source changed raster.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct NormRect {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

impl NormRect {
    /// The whole frame.
    pub const FULL: NormRect = NormRect {
        x: 0.0,
        y: 0.0,
        w: 1.0,
        h: 1.0,
    };

    pub fn new(x: f64, y: f64, w: f64, h: f64) -> Self {
        Self { x, y, w, h }
    }

    pub fn right(&self) -> f64 {
        self.x + self.w
    }

    pub fn bottom(&self) -> f64 {
        self.y + self.h
    }

    pub fn centre(&self) -> (f64, f64) {
        (self.x + self.w / 2.0, self.y + self.h / 2.0)
    }

    /// Size in input pixels, given the input raster.
    pub fn size_px(&self, input: Size) -> (f64, f64) {
        (self.w * input.w as f64, self.h * input.h as f64)
    }

    /// Aspect ratio of this rect *as displayed*, which depends on the input
    /// raster: a square in normalised space is 16:9 on a 16:9 source.
    pub fn aspect(&self, input: Size) -> f64 {
        let (w, h) = self.size_px(input);
        if h <= 0.0 {
            0.0
        } else {
            w / h
        }
    }

    /// Push the rect back inside the frame without changing its size, shrinking
    /// only if it is larger than the frame in that axis.
    ///
    /// Translate-then-shrink (rather than clipping) is what a drag off the edge
    /// should feel like: the region stops at the edge rather than silently
    /// getting smaller, which would change the scale factor under the operator.
    pub fn clamped(mut self) -> Self {
        self.w = self.w.clamp(MIN_SIZE, 1.0);
        self.h = self.h.clamp(MIN_SIZE, 1.0);
        self.x = self.x.clamp(0.0, 1.0 - self.w);
        self.y = self.y.clamp(0.0, 1.0 - self.h);
        self
    }

    pub fn is_valid(&self) -> bool {
        self.w >= MIN_SIZE
            && self.h >= MIN_SIZE
            && self.x >= -EPS
            && self.y >= -EPS
            && self.right() <= 1.0 + EPS
            && self.bottom() <= 1.0 + EPS
    }

    /// Resize about the centre so the rect matches `target_aspect` (a display
    /// aspect, e.g. 16/9) on the given input raster, then clamp back in frame.
    ///
    /// Shrinks the long axis rather than growing the short one, so locking the
    /// aspect of a region that already fills the frame cannot push it out of
    /// bounds and get silently clamped into a different shape.
    pub fn with_aspect(self, target_aspect: f64, input: Size) -> Self {
        if target_aspect <= 0.0 || input.w == 0 || input.h == 0 {
            return self;
        }
        let current = self.aspect(input);
        if current <= 0.0 {
            return self;
        }
        let (cx, cy) = self.centre();
        let (mut w, mut h) = (self.w, self.h);
        if current > target_aspect {
            // Too wide — narrow it.
            w = self.w * (target_aspect / current);
        } else {
            // Too tall — shorten it.
            h = self.h * (current / target_aspect);
        }
        NormRect::new(cx - w / 2.0, cy - h / 2.0, w, h).clamped()
    }
}

/// A raster size in pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Size {
    pub w: u32,
    pub h: u32,
}

impl Size {
    pub const fn new(w: u32, h: u32) -> Self {
        Self { w, h }
    }

    pub fn aspect(&self) -> f64 {
        if self.h == 0 {
            0.0
        } else {
            self.w as f64 / self.h as f64
        }
    }
}

/// How a region that does not match the output aspect is placed on the output.
///
/// Same vocabulary as WebLinked's `--scaling fit|fill|stretch`, deliberately —
/// an operator who knows one should not have to learn the other.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FitMode {
    /// Whole region visible, bars where the aspect does not match.
    #[default]
    Fit,
    /// Output filled, region cropped where the aspect does not match.
    Fill,
    /// Region distorted to fill the output.
    Stretch,
}

/// The magnification an output is applying to its region, as a ratio (`2.0` is
/// a 200% blow-up).
///
/// This is the number in the badge under each output thumbnail, and it is the
/// single most useful thing on the screen: it says how much of the source's
/// real detail is being invented. Under [`FitMode::Fit`] the binding axis is
/// the one with the *smaller* ratio (that axis fills the output, the other gets
/// bars); under [`FitMode::Fill`] it is the larger.
pub fn scale_factor(rect: &NormRect, input: Size, output: Size, fit: FitMode) -> f64 {
    let (rw, rh) = rect.size_px(input);
    if rw <= 0.0 || rh <= 0.0 {
        return 0.0;
    }
    let sx = output.w as f64 / rw;
    let sy = output.h as f64 / rh;
    match fit {
        FitMode::Fit => sx.min(sy),
        FitMode::Fill => sx.max(sy),
        // Stretch has a different factor per axis. Report the geometric mean:
        // it is the one number whose square is the true area magnification.
        FitMode::Stretch => (sx * sy).sqrt(),
    }
}

/// [`scale_factor`] as the percentage shown in the UI.
pub fn scale_percent(rect: &NormRect, input: Size, output: Size, fit: FitMode) -> f64 {
    scale_factor(rect, input, output, fit) * 100.0
}

/// How hard a given magnification is pushing the source.
///
/// Thresholds are a judgement about *pictures*, not a hardware limit: at 1:1 or
/// below every output pixel is backed by a real source pixel; past 2x a 1080p
/// source is carrying a shot that wanted a camera, and it shows on a big screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScaleQuality {
    /// At or below 1:1 — no invented detail.
    Native,
    /// Up to 2x. Softening, still perfectly usable.
    Soft,
    /// Beyond 2x. Visibly upscaled.
    Heavy,
}

pub fn scale_quality(factor: f64) -> ScaleQuality {
    if factor <= 1.0 + EPS {
        ScaleQuality::Native
    } else if factor <= 2.0 + EPS {
        ScaleQuality::Soft
    } else {
        ScaleQuality::Heavy
    }
}

/// Where a region actually lands on an output.
///
/// Two rectangles, both normalised: `src` is the part of the input that gets
/// sampled, `dst` is the part of the output it covers. Everything outside `dst`
/// is bars. Keeping both explicit means the shader is the same three lines for
/// all three fit modes, and the mode's whole meaning lives here, in arithmetic
/// that can be tested without a GPU.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Placement {
    pub src: NormRect,
    pub dst: NormRect,
}

pub fn place(rect: &NormRect, input: Size, output: Size, fit: FitMode) -> Placement {
    let region_aspect = rect.aspect(input);
    let out_aspect = output.aspect();
    if region_aspect <= 0.0 || out_aspect <= 0.0 {
        return Placement {
            src: *rect,
            dst: NormRect::FULL,
        };
    }
    match fit {
        // Distort to fill: sample everything, cover everything.
        FitMode::Stretch => Placement {
            src: *rect,
            dst: NormRect::FULL,
        },
        // Letterbox: sample everything, cover a centred sub-rect.
        FitMode::Fit => {
            let (w, h) = if region_aspect > out_aspect {
                (1.0, out_aspect / region_aspect)
            } else {
                (region_aspect / out_aspect, 1.0)
            };
            Placement {
                src: *rect,
                dst: NormRect::new((1.0 - w) / 2.0, (1.0 - h) / 2.0, w, h),
            }
        }
        // Crop to fill: sample a centred sub-rect, cover everything.
        FitMode::Fill => Placement {
            src: rect.with_aspect(out_aspect, input),
            dst: NormRect::FULL,
        },
    }
}

/// The smallest region we allow, as a fraction of the frame. Small enough to be
/// useless in practice, large enough that a division by the region size cannot
/// blow up and that a fumbled drag cannot create a zero-area region.
pub const MIN_SIZE: f64 = 0.01;

const EPS: f64 = 1e-9;

#[cfg(test)]
mod tests {
    use super::*;

    const HD: Size = Size::new(1920, 1080);

    #[test]
    fn full_frame_on_matching_output_is_unity() {
        let s = scale_factor(&NormRect::FULL, HD, HD, FitMode::Fit);
        assert!((s - 1.0).abs() < 1e-12, "{s}");
        assert_eq!(scale_quality(s), ScaleQuality::Native);
    }

    #[test]
    fn half_width_region_doubles() {
        let r = NormRect::new(0.0, 0.0, 0.5, 0.5);
        let pct = scale_percent(&r, HD, HD, FitMode::Fit);
        assert!((pct - 200.0).abs() < 1e-9, "{pct}");
        assert_eq!(scale_quality(pct / 100.0), ScaleQuality::Soft);
    }

    #[test]
    fn fit_takes_the_smaller_axis_and_fill_the_larger() {
        // A region twice as wide (relative to the output) as it is tall.
        let r = NormRect::new(0.0, 0.0, 0.5, 0.25); // 960 x 270 px
        let fit = scale_factor(&r, HD, HD, FitMode::Fit);
        let fill = scale_factor(&r, HD, HD, FitMode::Fill);
        assert!((fit - 2.0).abs() < 1e-9, "fit {fit}");
        assert!((fill - 4.0).abs() < 1e-9, "fill {fill}");
        assert!(fit < fill);
    }

    #[test]
    fn a_downscale_is_reported_as_native() {
        // A 4K region shown on an HD output is a reduction, not a blow-up.
        let uhd = Size::new(3840, 2160);
        let s = scale_factor(&NormRect::FULL, uhd, HD, FitMode::Fit);
        assert!((s - 0.5).abs() < 1e-12, "{s}");
        assert_eq!(scale_quality(s), ScaleQuality::Native);
    }

    #[test]
    fn clamping_translates_before_it_shrinks() {
        let r = NormRect::new(0.8, 0.8, 0.5, 0.5).clamped();
        assert!((r.w - 0.5).abs() < 1e-12, "width must survive: {r:?}");
        assert!((r.h - 0.5).abs() < 1e-12);
        assert!((r.x - 0.5).abs() < 1e-12, "must slide back in: {r:?}");
        assert!(r.is_valid());
    }

    #[test]
    fn clamping_an_oversized_rect_shrinks_it_to_the_frame() {
        let r = NormRect::new(-0.5, -0.5, 2.0, 2.0).clamped();
        assert_eq!(r, NormRect::FULL);
    }

    #[test]
    fn aspect_lock_shrinks_the_long_axis_and_stays_in_frame() {
        // A tall region on a 16:9 source, locked to 16:9.
        let r = NormRect::new(0.4, 0.1, 0.2, 0.8).with_aspect(16.0 / 9.0, HD);
        let got = r.aspect(HD);
        assert!((got - 16.0 / 9.0).abs() < 1e-9, "aspect {got}: {r:?}");
        assert!(r.is_valid(), "{r:?}");
        // Locking must not have grown the region past the frame.
        assert!(r.h <= 0.8 + 1e-12);
    }

    #[test]
    fn aspect_lock_on_the_full_frame_is_a_no_op_at_matching_aspect() {
        let r = NormRect::FULL.with_aspect(16.0 / 9.0, HD);
        assert!(
            (r.w - 1.0).abs() < 1e-9 && (r.h - 1.0).abs() < 1e-9,
            "{r:?}"
        );
    }

    #[test]
    fn aspect_lock_is_idempotent() {
        let once = NormRect::new(0.1, 0.1, 0.6, 0.2).with_aspect(4.0 / 3.0, HD);
        let twice = once.with_aspect(4.0 / 3.0, HD);
        assert!((once.w - twice.w).abs() < 1e-9, "{once:?} {twice:?}");
        assert!((once.h - twice.h).abs() < 1e-9);
    }

    #[test]
    fn a_degenerate_region_scales_to_zero_rather_than_infinity() {
        let r = NormRect::new(0.0, 0.0, 0.0, 0.0);
        assert_eq!(scale_factor(&r, HD, HD, FitMode::Fit), 0.0);
    }

    // --- placement ------------------------------------------------------

    #[test]
    fn a_matching_aspect_region_fills_the_output_in_every_mode() {
        // Half of a 16:9 frame in each axis is still 16:9.
        let r = NormRect::new(0.25, 0.25, 0.5, 0.5);
        for fit in [FitMode::Fit, FitMode::Fill, FitMode::Stretch] {
            let p = place(&r, HD, HD, fit);
            assert_eq!(p.src, r, "{fit:?} must sample the region as drawn");
            assert!(
                (p.dst.w - 1.0).abs() < 1e-9 && (p.dst.h - 1.0).abs() < 1e-9,
                "{fit:?} left bars on a matching aspect: {:?}",
                p.dst
            );
        }
    }

    #[test]
    fn fit_pillarboxes_a_tall_region_and_keeps_all_of_it() {
        // 4:3-shaped region on a 16:9 output.
        let r = NormRect::new(0.3, 0.1, 0.3, 0.4); // 576 x 432 px = 4:3
        let p = place(&r, HD, HD, FitMode::Fit);
        assert_eq!(p.src, r, "fit never crops");
        assert!((p.dst.h - 1.0).abs() < 1e-9, "full height: {:?}", p.dst);
        let want_w = (4.0 / 3.0) / (16.0 / 9.0);
        assert!((p.dst.w - want_w).abs() < 1e-9, "{:?}", p.dst);
        // Centred, so the two bars are equal.
        assert!((p.dst.x - (1.0 - want_w) / 2.0).abs() < 1e-9);
    }

    #[test]
    fn fill_crops_the_region_and_leaves_no_bars() {
        let r = NormRect::new(0.3, 0.1, 0.3, 0.4); // 4:3
        let p = place(&r, HD, HD, FitMode::Fill);
        assert_eq!(p.dst, NormRect::FULL, "fill never leaves bars");
        let got = p.src.aspect(HD);
        assert!((got - 16.0 / 9.0).abs() < 1e-9, "src aspect {got}");
        // It crops the *long* axis of the region, so the crop is inside it.
        assert!(
            p.src.h < r.h + 1e-12 && p.src.w <= r.w + 1e-12,
            "{:?}",
            p.src
        );
    }

    #[test]
    fn stretch_uses_everything_on_both_sides() {
        let r = NormRect::new(0.3, 0.1, 0.3, 0.4);
        let p = place(&r, HD, HD, FitMode::Stretch);
        assert_eq!(p.src, r);
        assert_eq!(p.dst, NormRect::FULL);
    }

    #[test]
    fn placement_survives_a_degenerate_output() {
        let p = place(&NormRect::FULL, HD, Size::new(0, 0), FitMode::Fit);
        assert_eq!(p.dst, NormRect::FULL, "must not divide by a zero aspect");
    }
}

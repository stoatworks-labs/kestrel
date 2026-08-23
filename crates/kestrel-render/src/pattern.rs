//! Synthetic UYVY frames.
//!
//! Two jobs. They are the fixtures the GPU tests assert against, and they are
//! the input source when there is no card in the machine — which is most
//! development, and which is what makes the whole app usable and demonstrable
//! without SDI hardware plugged in.
//!
//! The colour maths here is a deliberate second implementation of what
//! `common.wgsl` does. That is the point: a test that generated its expectation
//! with the same code under test would agree with a wrong shader.

use kestrel_core::Size;

/// BT.709 limited range, RGB (0..255) to Y'CbCr (0..255).
pub fn rgb_to_ycbcr(rgb: [u8; 3]) -> [u8; 3] {
    let r = rgb[0] as f64 / 255.0;
    let g = rgb[1] as f64 / 255.0;
    let b = rgb[2] as f64 / 255.0;
    let y = 0.2126 * r + 0.7152 * g + 0.0722 * b;
    let cb = (b - y) / 1.8556;
    let cr = (r - y) / 1.5748;
    [
        clamp_u8(219.0 * y + 16.0),
        clamp_u8(224.0 * cb + 128.0),
        clamp_u8(224.0 * cr + 128.0),
    ]
}

/// The inverse, for checking a round trip.
pub fn ycbcr_to_rgb(ycc: [u8; 3]) -> [u8; 3] {
    let y = (ycc[0] as f64 - 16.0) / 219.0;
    let cb = (ycc[1] as f64 - 128.0) / 224.0;
    let cr = (ycc[2] as f64 - 128.0) / 224.0;
    [
        clamp_u8(255.0 * (y + 1.5748 * cr)),
        clamp_u8(255.0 * (y - 0.187324 * cb - 0.468124 * cr)),
        clamp_u8(255.0 * (y + 1.8556 * cb)),
    ]
}

fn clamp_u8(v: f64) -> u8 {
    v.round().clamp(0.0, 255.0) as u8
}

/// Row stride of a UYVY frame: two bytes a pixel, macropixels indivisible.
pub fn row_bytes(size: Size) -> u32 {
    size.w.div_ceil(2) * 4
}

/// Build a UYVY frame by asking `px` for the RGB at each pixel.
///
/// Chroma is taken from the *even* pixel of each pair, matching where 4:2:2
/// sites it, rather than averaged — a generated fixture should be exactly what
/// the format can represent, so that a round-trip test failure means the code
/// is wrong and not the fixture.
pub fn build_uyvy(size: Size, mut px: impl FnMut(u32, u32) -> [u8; 3]) -> Vec<u8> {
    let stride = row_bytes(size) as usize;
    let mut out = vec![0u8; stride * size.h as usize];
    for y in 0..size.h {
        for mx in 0..size.w.div_ceil(2) {
            let x0 = mx * 2;
            let x1 = (x0 + 1).min(size.w.saturating_sub(1));
            let a = rgb_to_ycbcr(px(x0, y));
            let b = rgb_to_ycbcr(px(x1, y));
            let i = y as usize * stride + mx as usize * 4;
            out[i] = a[1]; // U
            out[i + 1] = a[0]; // Y0
            out[i + 2] = a[2]; // V
            out[i + 3] = b[0]; // Y1
        }
    }
    out
}

pub fn solid_uyvy(size: Size, rgb: [u8; 3]) -> Vec<u8> {
    build_uyvy(size, |_, _| rgb)
}

/// EBU 75% bars, the same eight in the same order as `fill.wgsl`.
pub fn bars_uyvy(size: Size) -> Vec<u8> {
    build_uyvy(size, |x, _| {
        let bar = (x as u64 * 8 / size.w.max(1) as u64).min(7) as usize;
        BARS_75[bar]
    })
}

pub const BARS_75: [[u8; 3]; 8] = [
    [191, 191, 191],
    [191, 191, 0],
    [0, 191, 191],
    [0, 191, 0],
    [191, 0, 191],
    [191, 0, 0],
    [0, 0, 191],
    [0, 0, 0],
];

/// A horizontal luma ramp. Useful for looking at a scaler, useless for exact
/// assertions.
pub fn gradient_uyvy(size: Size) -> Vec<u8> {
    build_uyvy(size, |x, _| {
        let v = (x * 255 / size.w.max(1)) as u8;
        [v, v, v]
    })
}

/// Four large flat blocks: red, green, blue, white, clockwise from top-left.
///
/// The fixture GPU tests should reach for by default. A small or busy fixture
/// is all texel boundary, so any filtered sample lands between two colours and
/// a readback assertion comes out a few percent off *every* value — which looks
/// exactly like a broken colour conversion and is not one. Big flat regions put
/// the sample point well inside one colour.
pub fn quadrants_uyvy(size: Size) -> Vec<u8> {
    build_uyvy(size, |x, y| quadrant_colour(x, y, size))
}

pub fn quadrant_colour(x: u32, y: u32, size: Size) -> [u8; 3] {
    let left = x < size.w / 2;
    let top = y < size.h / 2;
    match (top, left) {
        (true, true) => [255, 0, 0],
        (true, false) => [0, 255, 0],
        (false, false) => [0, 0, 255],
        (false, true) => [255, 255, 255],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SD: Size = Size::new(64, 64);

    #[test]
    fn primaries_survive_a_cpu_round_trip() {
        for rgb in [
            [255, 0, 0],
            [0, 255, 0],
            [0, 0, 255],
            [255, 255, 255],
            [0, 0, 0],
            [128, 128, 128],
        ] {
            let back = ycbcr_to_rgb(rgb_to_ycbcr(rgb));
            for i in 0..3 {
                let d = back[i] as i32 - rgb[i] as i32;
                assert!(d.abs() <= 2, "{rgb:?} -> {back:?} (channel {i} off by {d})");
            }
        }
    }

    #[test]
    fn black_and_white_land_on_the_legal_range() {
        assert_eq!(rgb_to_ycbcr([0, 0, 0])[0], 16, "black must be Y=16");
        assert_eq!(rgb_to_ycbcr([255, 255, 255])[0], 235, "white must be Y=235");
        // Neutral colours must be exactly neutral chroma, or a grey ramp tints.
        let g = rgb_to_ycbcr([128, 128, 128]);
        assert_eq!((g[1], g[2]), (128, 128));
    }

    #[test]
    fn a_frame_is_exactly_two_bytes_a_pixel() {
        let f = solid_uyvy(SD, [10, 20, 30]);
        assert_eq!(f.len(), 64 * 64 * 2);
        assert_eq!(row_bytes(SD), 128);
    }

    #[test]
    fn an_odd_width_rounds_the_macropixel_up() {
        let odd = Size::new(65, 4);
        assert_eq!(row_bytes(odd), 33 * 4);
        assert_eq!(solid_uyvy(odd, [0, 0, 0]).len(), 33 * 4 * 4);
    }

    #[test]
    fn a_solid_frame_repeats_one_macropixel() {
        let f = solid_uyvy(SD, [191, 0, 0]);
        let first: [u8; 4] = f[0..4].try_into().unwrap();
        assert!(
            f.as_chunks::<4>().0.iter().all(|c| *c == first),
            "a solid frame must be one macropixel repeated"
        );
    }

    #[test]
    fn bars_start_white_and_end_black() {
        let f = bars_uyvy(Size::new(256, 8));
        // Y of the first macropixel is white-ish, of the last is black.
        assert_eq!(f[1], rgb_to_ycbcr(BARS_75[0])[0]);
        let last = f.len() - 4;
        assert_eq!(f[last + 1], rgb_to_ycbcr(BARS_75[7])[0]);
    }
}

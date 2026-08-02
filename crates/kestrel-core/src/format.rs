//! Video formats, expressed the way SDI expresses them.

use crate::geom::Size;
use serde::{Deserialize, Serialize};
use std::fmt;

/// A frame rate as an exact rational.
///
/// Never a float. 59.94 is 60000/1001 and nothing else; storing it as `59.94`
/// and multiplying up is how a scheduled-playback clock accumulates a drift
/// that only shows up as a dropped frame twenty minutes into a show.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct FrameRate {
    pub num: u32,
    pub den: u32,
}

impl FrameRate {
    pub const fn new(num: u32, den: u32) -> Self {
        Self { num, den }
    }

    pub fn as_f64(&self) -> f64 {
        if self.den == 0 {
            0.0
        } else {
            self.num as f64 / self.den as f64
        }
    }

    /// Frame duration in the units of `scale`, for DeckLink's scheduling API
    /// (`ScheduleVideoFrame` takes a time value and a time scale).
    pub fn duration_in(&self, scale: i64) -> i64 {
        if self.num == 0 {
            return 0;
        }
        scale * self.den as i64 / self.num as i64
    }
}

impl fmt::Display for FrameRate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let v = self.as_f64();
        // 29.97 and 59.94 want two decimals; 25 and 50 want none.
        if (v - v.round()).abs() < 1e-6 {
            write!(f, "{}", v.round() as u32)
        } else {
            write!(f, "{v:.2}")
        }
    }
}

/// Progressive, or one of the two interlaced-ish things SDI does.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Scan {
    #[default]
    Progressive,
    Interlaced,
    /// Progressive segmented frame — carried as interlaced, but the two fields
    /// are from the same instant, so it must be scaled as a whole frame.
    Psf,
}

impl Scan {
    pub fn suffix(&self) -> &'static str {
        match self {
            Scan::Progressive => "p",
            Scan::Interlaced => "i",
            Scan::Psf => "psf",
        }
    }
}

/// A complete video format: raster, rate and scan.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct VideoFormat {
    pub size: Size,
    pub rate: FrameRate,
    #[serde(default)]
    pub scan: Scan,
}

impl VideoFormat {
    pub const fn new(w: u32, h: u32, num: u32, den: u32, scan: Scan) -> Self {
        Self {
            size: Size::new(w, h),
            rate: FrameRate::new(num, den),
            scan,
        }
    }

    pub fn width(&self) -> u32 {
        self.size.w
    }

    pub fn height(&self) -> u32 {
        self.size.h
    }

    /// Bytes in one row of 8-bit YUV (UYVY), which is what Kestrel moves over
    /// SDI in both directions: two pixels share a chroma pair, four bytes.
    ///
    /// Odd widths are rounded up — a UYVY macropixel is indivisible, and SDI
    /// rasters are even anyway.
    pub fn uyvy_row_bytes(&self) -> u32 {
        self.size.w.div_ceil(2) * 4
    }
}

impl Default for VideoFormat {
    fn default() -> Self {
        // 1080p50: the European live-events default, and what the rest of the
        // fleet's test material is cut at.
        Self::new(1920, 1080, 50, 1, Scan::Progressive)
    }
}

impl fmt::Display for VideoFormat {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}{}{}",
            self.size.h,
            self.scan.suffix(),
            self.rate
        )
    }
}

/// The formats offered in the UI's output-format menu.
///
/// A deliberately short list of what a DeckLink actually plays out to a live
/// switcher, rather than every mode the SDK enumerates. The device's own
/// supported-mode list is authoritative at open time; this is the menu.
pub fn common_formats() -> Vec<VideoFormat> {
    use Scan::{Interlaced, Progressive};
    vec![
        VideoFormat::new(1280, 720, 50, 1, Progressive),
        VideoFormat::new(1280, 720, 60000, 1001, Progressive),
        VideoFormat::new(1280, 720, 60, 1, Progressive),
        VideoFormat::new(1920, 1080, 25, 1, Progressive),
        VideoFormat::new(1920, 1080, 30000, 1001, Progressive),
        VideoFormat::new(1920, 1080, 25, 1, Interlaced),
        VideoFormat::new(1920, 1080, 30000, 1001, Interlaced),
        VideoFormat::new(1920, 1080, 50, 1, Progressive),
        VideoFormat::new(1920, 1080, 60000, 1001, Progressive),
        VideoFormat::new(1920, 1080, 60, 1, Progressive),
        VideoFormat::new(3840, 2160, 25, 1, Progressive),
        VideoFormat::new(3840, 2160, 30000, 1001, Progressive),
        VideoFormat::new(3840, 2160, 50, 1, Progressive),
        VideoFormat::new(3840, 2160, 60000, 1001, Progressive),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fractional_rates_are_exact() {
        let r = FrameRate::new(60000, 1001);
        assert_eq!(r.to_string(), "59.94");
        // The whole point of the rational: a frame is exactly 1001/60000 s, and
        // in DeckLink's usual 1000-per-second scale that is 16 (not 16.68).
        assert_eq!(r.duration_in(60000), 1001);
    }

    #[test]
    fn integer_rates_print_without_decimals() {
        assert_eq!(FrameRate::new(50, 1).to_string(), "50");
        assert_eq!(FrameRate::new(25, 1).to_string(), "25");
    }

    #[test]
    fn format_names_read_like_sdi() {
        assert_eq!(VideoFormat::default().to_string(), "1080p50");
        assert_eq!(
            VideoFormat::new(1920, 1080, 30000, 1001, Scan::Interlaced).to_string(),
            "1080i29.97"
        );
        assert_eq!(
            VideoFormat::new(1280, 720, 60000, 1001, Scan::Progressive).to_string(),
            "720p59.94"
        );
    }

    #[test]
    fn uyvy_rows_are_two_bytes_per_pixel() {
        assert_eq!(VideoFormat::default().uyvy_row_bytes(), 1920 * 2);
        assert_eq!(
            VideoFormat::new(3840, 2160, 50, 1, Scan::Progressive).uyvy_row_bytes(),
            3840 * 2
        );
    }

    #[test]
    fn a_zero_rate_cannot_divide_by_zero() {
        assert_eq!(FrameRate::new(0, 1).duration_in(1000), 0);
        assert_eq!(FrameRate::new(50, 0).as_f64(), 0.0);
    }

    #[test]
    fn the_menu_has_no_duplicates() {
        let all = common_formats();
        for (i, a) in all.iter().enumerate() {
            for b in &all[i + 1..] {
                assert_ne!(a, b, "duplicate format in the menu: {a}");
            }
        }
    }
}

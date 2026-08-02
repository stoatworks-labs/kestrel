//! Where frames come from.

use kestrel_core::Size;
use kestrel_render::pattern;
use parking_lot::Mutex;
use std::sync::Arc;

/// One captured frame, copied out of the driver's buffer.
///
/// The copy is not avoidable and not worth mourning: the SDK's pointer is only
/// valid for the duration of its callback, so *something* has to copy before
/// the render thread can look at it. 4 MB at 50 Hz is 200 MB/s of memcpy, which
/// is noise next to the readback the outputs already do.
#[derive(Clone)]
pub struct CapturedFrame {
    pub bytes: Vec<u8>,
    pub row_bytes: u32,
    pub size: Size,
    pub no_signal: bool,
    pub seq: u64,
}

/// The handoff between the capture callback and the render thread.
///
/// Latest-wins, one slot deep. A queue would be wrong here: this is live video,
/// and a frame that arrived while the renderer was busy is *stale* — showing it
/// late is worse than skipping it, and letting a queue grow turns a momentary
/// hiccup into permanent latency.
#[derive(Default)]
pub struct FrameSlot {
    slot: Mutex<Option<CapturedFrame>>,
    seq: std::sync::atomic::AtomicU64,
}

impl FrameSlot {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    pub fn put(&self, bytes: &[u8], row_bytes: u32, size: Size, no_signal: bool) {
        let seq = self
            .seq
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            .wrapping_add(1);
        let mut g = self.slot.lock();
        // Reuse the existing allocation when the shape has not changed, which
        // is every frame in the steady state.
        match g.as_mut() {
            Some(f) if f.bytes.len() == bytes.len() => {
                f.bytes.copy_from_slice(bytes);
                f.row_bytes = row_bytes;
                f.size = size;
                f.no_signal = no_signal;
                f.seq = seq;
            }
            _ => {
                *g = Some(CapturedFrame {
                    bytes: bytes.to_vec(),
                    row_bytes,
                    size,
                    no_signal,
                    seq,
                })
            }
        }
    }

    /// The newest frame, if it is newer than `since`.
    pub fn take_newer_than(&self, since: u64) -> Option<CapturedFrame> {
        let g = self.slot.lock();
        match g.as_ref() {
            Some(f) if f.seq > since => Some(f.clone()),
            _ => None,
        }
    }
}

/// Generated frames, for a machine with no card — which is most development,
/// and every demo.
///
/// Deliberately *moving*: a still test card cannot tell you whether the frame
/// path is running or whether you are looking at one frame from ten seconds
/// ago. The marker sweeping across the picture answers that from across a room.
pub struct SyntheticSource {
    size: Size,
    base: Vec<u8>,
    frame: Vec<u8>,
    tick: u64,
}

impl SyntheticSource {
    pub fn new(size: Size) -> Self {
        let base = pattern::bars_uyvy(size);
        Self {
            size,
            frame: base.clone(),
            base,
            tick: 0,
        }
    }

    pub fn size(&self) -> Size {
        self.size
    }

    pub fn row_bytes(&self) -> u32 {
        pattern::row_bytes(self.size)
    }

    /// Next frame: the bars, with a bright column sweeping left to right once
    /// every four seconds at 50p.
    pub fn next_frame(&mut self) -> &[u8] {
        self.frame.copy_from_slice(&self.base);
        let stride = self.row_bytes() as usize;
        let macro_w = (self.size.w.div_ceil(2)) as usize;
        let pos = (self.tick as usize / 2) % macro_w;
        let white = pattern::rgb_to_ycbcr([235, 235, 235]);
        for y in 0..self.size.h as usize {
            let i = y * stride + pos * 4;
            self.frame[i] = white[1];
            self.frame[i + 1] = white[0];
            self.frame[i + 2] = white[2];
            self.frame[i + 3] = white[0];
        }
        self.tick = self.tick.wrapping_add(1);
        &self.frame
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_slot_keeps_only_the_newest_frame() {
        let slot = FrameSlot::new();
        let size = Size::new(16, 4);
        let a = vec![1u8; 16 * 4 * 2];
        let b = vec![2u8; 16 * 4 * 2];
        slot.put(&a, 32, size, false);
        slot.put(&b, 32, size, false);
        let got = slot.take_newer_than(0).unwrap();
        assert_eq!(got.bytes[0], 2, "a backlog would show the older frame");
        assert_eq!(got.seq, 2);
    }

    #[test]
    fn a_frame_already_seen_is_not_handed_out_again() {
        let slot = FrameSlot::new();
        let size = Size::new(16, 4);
        slot.put(&[1u8; 128], 32, size, false);
        let first = slot.take_newer_than(0).unwrap();
        assert!(
            slot.take_newer_than(first.seq).is_none(),
            "re-uploading an unchanged frame is wasted bandwidth every tick"
        );
    }

    #[test]
    fn a_resize_replaces_the_buffer_rather_than_writing_past_it() {
        let slot = FrameSlot::new();
        slot.put(&[1u8; 128], 32, Size::new(16, 4), false);
        slot.put(&vec![9u8; 512], 64, Size::new(32, 8), false);
        let got = slot.take_newer_than(0).unwrap();
        assert_eq!(got.bytes.len(), 512);
        assert_eq!(got.size, Size::new(32, 8));
    }

    #[test]
    fn the_synthetic_source_actually_changes_between_frames() {
        let mut s = SyntheticSource::new(Size::new(64, 8));
        let a = s.next_frame().to_vec();
        // The marker moves every other tick.
        s.next_frame();
        let c = s.next_frame().to_vec();
        assert_ne!(a, c, "a still pattern cannot show that the path is running");
        assert_eq!(a.len(), pattern::row_bytes(Size::new(64, 8)) as usize * 8);
    }
}

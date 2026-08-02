//! The FFI side. Compiled only when `build.rs` found an SDK.

use crate::{CapturedFrame, Device, Error, OutputStats, Result};
use kestrel_core::{FrameRate, Size, VideoFormat};
use std::ffi::{c_char, c_void, CStr};
use std::panic::{catch_unwind, AssertUnwindSafe};

const NAME_MAX: usize = 128;

#[repr(C)]
#[derive(Clone, Copy)]
struct KdDevice {
    persistent_id: i64,
    name: [c_char; NAME_MAX],
    sub_device: i32,
    has_input: i32,
    has_output: i32,
    active: i32,
    full_duplex: i32,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct KdOutputStats {
    scheduled: i64,
    completed: i64,
    late: i64,
    dropped: i64,
    buffered: i32,
}

type KdFrameFn = extern "C" fn(
    ctx: *mut c_void,
    bytes: *const u8,
    row_bytes: i32,
    width: i32,
    height: i32,
    rate_num: i64,
    rate_den: i64,
    no_signal: i32,
);

extern "C" {
    fn kd_available() -> i32;
    fn kd_list_devices(out: *mut KdDevice, max: i32) -> i32;
    fn kd_last_error() -> *const c_char;
    fn kd_capture_open(persistent_id: i64, cb: KdFrameFn, ctx: *mut c_void) -> *mut c_void;
    fn kd_capture_close(handle: *mut c_void);
    fn kd_output_open(
        persistent_id: i64,
        width: i32,
        height: i32,
        rate_num: i64,
        rate_den: i64,
        interlaced: i32,
    ) -> *mut c_void;
    fn kd_output_schedule(handle: *mut c_void, bytes: *const u8, row_bytes: i32) -> i32;
    fn kd_output_stats_get(handle: *mut c_void, out: *mut KdOutputStats) -> i32;
    fn kd_output_close(handle: *mut c_void);
}

fn last_error() -> String {
    unsafe {
        let p = kd_last_error();
        if p.is_null() {
            "unknown DeckLink error".into()
        } else {
            CStr::from_ptr(p).to_string_lossy().into_owned()
        }
    }
}

fn c_name(bytes: &[c_char; NAME_MAX]) -> String {
    unsafe {
        CStr::from_ptr(bytes.as_ptr())
            .to_string_lossy()
            .into_owned()
    }
}

pub fn available() -> bool {
    unsafe { kd_available() == 1 }
}

pub fn list_devices() -> Result<Vec<Device>> {
    // 32 is well past any card Blackmagic ships (a Quad 2 is eight) and past
    // any plausible number of cards in one chassis.
    const MAX: usize = 32;
    let mut raw = [KdDevice {
        persistent_id: 0,
        name: [0; NAME_MAX],
        sub_device: 0,
        has_input: 0,
        has_output: 0,
        active: 0,
        full_duplex: 0,
    }; MAX];

    let n = unsafe { kd_list_devices(raw.as_mut_ptr(), MAX as i32) };
    if n < 0 {
        return Err(Error::Sdk(last_error()));
    }
    Ok(raw[..n as usize]
        .iter()
        .map(|d| Device {
            persistent_id: d.persistent_id,
            name: c_name(&d.name),
            sub_device: d.sub_device.max(0) as u32,
            has_input: d.has_input != 0,
            has_output: d.has_output != 0,
            active: d.active != 0,
            full_duplex: d.full_duplex != 0,
        })
        .collect())
}

// --- capture --------------------------------------------------------------

type FrameHandler = Box<dyn Fn(CapturedFrame<'_>) + Send + Sync + 'static>;

/// A running capture. Dropping it stops the streams.
pub struct Capture {
    handle: *mut c_void,
    /// Kept alive for exactly as long as the card can call into it. Freed only
    /// after `kd_capture_close` has returned, which is the point at which the
    /// SDK guarantees no callback is in flight.
    handler: *mut FrameHandler,
}

// The handle is only ever touched from `Drop`, and the handler behind it is
// `Send + Sync` by its own bound. The raw pointers are what make the compiler
// ask.
unsafe impl Send for Capture {}
unsafe impl Sync for Capture {}

extern "C" fn on_frame(
    ctx: *mut c_void,
    bytes: *const u8,
    row_bytes: i32,
    width: i32,
    height: i32,
    rate_num: i64,
    rate_den: i64,
    no_signal: i32,
) {
    if ctx.is_null() || bytes.is_null() || width <= 0 || height <= 0 || row_bytes <= 0 {
        return;
    }
    // Unwinding out of an `extern "C"` frame is undefined behaviour, and this
    // one is called from a driver thread. A panic in a user callback must kill
    // the frame, not the process.
    let _ = catch_unwind(AssertUnwindSafe(|| {
        let handler = unsafe { &*(ctx as *const FrameHandler) };
        let len = row_bytes as usize * height as usize;
        let slice = unsafe { std::slice::from_raw_parts(bytes, len) };
        let rate = if rate_num > 0 && rate_den > 0 {
            Some(FrameRate::new(rate_num as u32, rate_den as u32))
        } else {
            None
        };
        handler(CapturedFrame {
            bytes: slice,
            row_bytes: row_bytes as u32,
            size: Size::new(width as u32, height as u32),
            rate,
            no_signal: no_signal != 0,
        });
    }));
}

impl Capture {
    /// Open an input with format detection on, so the source format does not
    /// have to be known up front — the callback reports whatever arrives.
    pub fn open<F>(persistent_id: i64, handler: F) -> Result<Self>
    where
        F: Fn(CapturedFrame<'_>) + Send + Sync + 'static,
    {
        let boxed: *mut FrameHandler = Box::into_raw(Box::new(Box::new(handler)));
        let handle = unsafe { kd_capture_open(persistent_id, on_frame, boxed as *mut c_void) };
        if handle.is_null() {
            drop(unsafe { Box::from_raw(boxed) });
            return Err(Error::Sdk(last_error()));
        }
        Ok(Self {
            handle,
            handler: boxed,
        })
    }
}

impl Drop for Capture {
    fn drop(&mut self) {
        unsafe {
            kd_capture_close(self.handle);
            // Only now: the shim stops the streams and clears the callback
            // before returning, so no driver thread can still be inside the
            // handler.
            drop(Box::from_raw(self.handler));
        }
    }
}

// --- playback -------------------------------------------------------------

/// A running output. Dropping it stops scheduled playback.
pub struct Playback {
    handle: *mut c_void,
    format: VideoFormat,
}

unsafe impl Send for Playback {}
unsafe impl Sync for Playback {}

impl Playback {
    pub fn open(persistent_id: i64, format: VideoFormat) -> Result<Self> {
        let interlaced = matches!(
            format.scan,
            kestrel_core::Scan::Interlaced | kestrel_core::Scan::Psf
        );
        let handle = unsafe {
            kd_output_open(
                persistent_id,
                format.width() as i32,
                format.height() as i32,
                format.rate.num as i64,
                format.rate.den as i64,
                i32::from(interlaced),
            )
        };
        if handle.is_null() {
            return Err(Error::Sdk(last_error()));
        }
        Ok(Self { handle, format })
    }

    pub fn format(&self) -> VideoFormat {
        self.format
    }

    /// Queue one UYVY frame.
    pub fn schedule(&self, uyvy: &[u8], row_bytes: u32) -> Result<()> {
        let need = row_bytes as usize * self.format.height() as usize;
        if uyvy.len() < need {
            return Err(Error::Sdk(format!(
                "short frame for playback: {} bytes, need {need}",
                uyvy.len()
            )));
        }
        let rc = unsafe { kd_output_schedule(self.handle, uyvy.as_ptr(), row_bytes as i32) };
        if rc != 0 {
            return Err(Error::Sdk(last_error()));
        }
        Ok(())
    }

    pub fn stats(&self) -> OutputStats {
        let mut s = KdOutputStats::default();
        unsafe { kd_output_stats_get(self.handle, &mut s) };
        OutputStats {
            scheduled: s.scheduled,
            completed: s.completed,
            late: s.late,
            dropped: s.dropped,
            buffered: s.buffered,
        }
    }
}

impl Drop for Playback {
    fn drop(&mut self) {
        unsafe { kd_output_close(self.handle) };
    }
}

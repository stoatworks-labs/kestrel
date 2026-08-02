//! What the crate is when it was built without the Blackmagic SDK.
//!
//! Same API, every call refusing with [`Error::NotCompiledIn`]. Not an empty
//! device list: an empty list is the *other* failure — "the SDK is here, the
//! driver found nothing" — and the two have different fixes. Everything above
//! this crate is written against this shape, so the app runs, the GUI opens,
//! the routing works and the synthetic input source plays; only SDI is missing.

use crate::{CapturedFrame, Device, Error, OutputStats, Result};
use kestrel_core::VideoFormat;

pub fn available() -> bool {
    false
}

pub fn list_devices() -> Result<Vec<Device>> {
    Err(Error::NotCompiledIn)
}

pub struct Capture;

impl Capture {
    pub fn open<F>(_persistent_id: i64, _handler: F) -> Result<Self>
    where
        F: Fn(CapturedFrame<'_>) + Send + Sync + 'static,
    {
        Err(Error::NotCompiledIn)
    }
}

pub struct Playback;

impl Playback {
    pub fn open(_persistent_id: i64, _format: VideoFormat) -> Result<Self> {
        Err(Error::NotCompiledIn)
    }

    pub fn format(&self) -> VideoFormat {
        unreachable!("a stub Playback cannot be constructed")
    }

    pub fn schedule(&self, _uyvy: &[u8], _row_bytes: u32) -> Result<()> {
        Err(Error::NotCompiledIn)
    }

    pub fn stats(&self) -> OutputStats {
        OutputStats::default()
    }
}

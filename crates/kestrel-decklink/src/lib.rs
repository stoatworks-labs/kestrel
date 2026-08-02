//! DeckLink capture and playback.
//!
//! ## Two independent things, reported separately
//!
//! * **Was SDI compiled in?** [`available`]. The shim needs the Blackmagic SDK
//!   headers *at build time*, and they are licence-gated, so a build without
//!   them has no SDI at all.
//! * **Is there a card?** [`list_devices`]. Finding devices needs Desktop Video
//!   installed *at run time*.
//!
//! A machine can easily have one and not the other, and the two failures look
//! nothing alike from the user's chair. They are never collapsed into one
//! "DeckLink not working" message.
//!
//! ## Profiles, which is the thing that wastes an afternoon
//!
//! A multi-sub-device card presents *all* its sub-devices whatever profile it
//! is in, and the ones the profile has switched off support **no display modes
//! at all**. A DeckLink Duo 2 in its two-sub-device profile shows four
//! sub-devices of which two are dead. Kestrel wants one input and several
//! outputs at once, which on a Duo 2 means the four-sub-device half-duplex
//! profile — so [`Device::active`] is surfaced, and the UI says "inactive in
//! this card's profile" rather than leaving an operator to conclude the card
//! is broken.

use kestrel_core::{FrameRate, Size};

#[cfg(decklink)]
mod real;
#[cfg(decklink)]
use real as backend;

#[cfg(not(decklink))]
mod stub;
#[cfg(not(decklink))]
use stub as backend;

pub use backend::{Capture, Playback};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(
        "this build has no DeckLink support: it was compiled without the \
         Blackmagic SDK. Rebuild with DECKLINK_SDK_DIR pointing at a copy."
    )]
    NotCompiledIn,
    #[error("DeckLink: {0}")]
    Sdk(String),
}

pub type Result<T> = std::result::Result<T, Error>;

/// One DeckLink sub-device.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Device {
    /// `BMDDeckLinkPersistentID` — stable across reboots and reordering, so it
    /// is what a show file remembers rather than an index.
    pub persistent_id: i64,
    pub name: String,
    /// Index within the physical card.
    pub sub_device: u32,
    pub has_input: bool,
    pub has_output: bool,
    /// False when the card's current profile has this sub-device switched off.
    /// Such a sub-device still appears in the list and offers no display modes.
    pub active: bool,
    /// Whether this sub-device does input and output simultaneously.
    pub full_duplex: bool,
}

impl Device {
    /// What to show in a device menu, including why a port is unusable.
    pub fn menu_label(&self) -> String {
        if !self.active {
            format!("{} — inactive in this card's profile", self.name)
        } else {
            self.name.clone()
        }
    }
}

/// One frame off the wire. Valid only for the duration of the callback.
pub struct CapturedFrame<'a> {
    pub bytes: &'a [u8],
    pub row_bytes: u32,
    pub size: Size,
    /// What the card says the source rate is, when it knows.
    pub rate: Option<FrameRate>,
    /// The card synthesised this frame because nothing is arriving.
    ///
    /// These still tick at the nominal rate, which is why "frames are
    /// arriving" is not the same question as "a source is connected". A
    /// free-running input with nothing plugged in reports a mix of these and
    /// good frames, and a display mode of `ntsc` — that pattern is the
    /// signature of no lock, not of a signal.
    pub no_signal: bool,
}

/// Counters from a running output. `buffered` is the one to watch: a value that
/// walks steadily up or down means the app's clock and the card's disagree.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct OutputStats {
    pub scheduled: i64,
    pub completed: i64,
    pub late: i64,
    pub dropped: i64,
    pub buffered: i32,
}

/// Whether this build has SDI support compiled in at all.
pub fn available() -> bool {
    backend::available()
}

/// Every DeckLink sub-device the driver can see, active or not.
pub fn list_devices() -> Result<Vec<Device>> {
    backend::list_devices()
}

/// A one-line summary for the UI and the logs, covering both failure modes.
pub fn status_line() -> String {
    if !available() {
        return "DeckLink: not compiled in (built without the Blackmagic SDK)".into();
    }
    match list_devices() {
        Err(e) => format!("DeckLink: {e}"),
        Ok(d) if d.is_empty() => {
            "DeckLink: compiled in, no devices found (is Desktop Video installed?)".into()
        }
        Ok(d) => {
            let active = d.iter().filter(|x| x.active).count();
            format!(
                "DeckLink: {} sub-device{}, {active} active in the current profile",
                d.len(),
                if d.len() == 1 { "" } else { "s" }
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_status_line_distinguishes_the_two_failures() {
        let s = status_line();
        assert!(s.starts_with("DeckLink: "), "{s}");
        if !available() {
            assert!(
                s.contains("not compiled in"),
                "a build without the SDK must say so, not blame the hardware: {s}"
            );
        }
    }

    #[test]
    fn an_inactive_port_explains_itself_in_the_menu() {
        let d = Device {
            persistent_id: 1,
            name: "DeckLink Duo (3)".into(),
            sub_device: 2,
            has_input: true,
            has_output: true,
            active: false,
            full_duplex: false,
        };
        assert!(d.menu_label().contains("profile"), "{}", d.menu_label());
    }
}

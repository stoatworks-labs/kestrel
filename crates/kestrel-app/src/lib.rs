//! Kestrel's runtime, shared by the CLI and the GUI.

pub mod runtime;
pub mod show_file;
pub mod source;

pub use runtime::{Config, InputSource, Runtime};
pub use show_file::{default_show, load_or_default, save};

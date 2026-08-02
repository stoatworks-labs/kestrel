//! Kestrel's domain model.
//!
//! A wide shot goes in; a handful of tighter shots come out, each one a crop of
//! that same frame scaled back up to the output raster. This crate holds the
//! decisions — which region, on which output, at what magnification — and none
//! of the machinery. No GPU, no SDI, no window: the whole thing is arithmetic
//! and a struct, so the rules that matter on air are cheap to test.
//!
//! The rule that matters most is in [`Show::plan`]: **every output is planned
//! every frame**, whatever the routing says, whatever the global kill says, and
//! whether or not there is an input. An SDI output that stops is an SDI output
//! the switcher downstream has to re-lock.

pub mod format;
pub mod geom;
pub mod show;

pub use format::{common_formats, FrameRate, Scan, VideoFormat};
pub use geom::{
    place, scale_factor, scale_percent, scale_quality, FitMode, NormRect, Placement, ScaleQuality,
    Size, MIN_SIZE,
};
pub use show::{
    DeviceRef, IdleFill, Output, OutputId, OutputPlan, Pattern, PlanSource, Roi, RoiId,
    ScalingFilter, Show, ShowError, ROI_PALETTE,
};

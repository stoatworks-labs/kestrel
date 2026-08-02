//! The show: regions, outputs, the crosspoint between them, and the one
//! function that decides what every output is carrying right now.

use crate::format::VideoFormat;
use crate::geom::{scale_factor, FitMode, NormRect, ScaleQuality, Size};
use serde::{Deserialize, Serialize};
use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct RoiId(pub u32);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct OutputId(pub u32);

impl fmt::Display for RoiId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "roi{}", self.0)
    }
}

impl fmt::Display for OutputId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "out{}", self.0)
    }
}

/// A region of interest: a named crop of the input.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Roi {
    pub id: RoiId,
    pub name: String,
    pub rect: NormRect,
    /// Overlay colour on the preview, and the tally colour on a control surface.
    pub colour: [u8; 3],
    /// Keep this region at the output aspect while it is dragged.
    ///
    /// On by default: a region drawn free-hand almost never matches 16:9, and
    /// the difference shows up on air as bars nobody asked for.
    #[serde(default = "yes")]
    pub lock_aspect: bool,
}

fn yes() -> bool {
    true
}

/// What an output shows when nothing is routed to it.
///
/// Note that none of these stop the output. See [`Show::plan`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum IdleFill {
    /// Legal black. The safe default into a switcher.
    #[default]
    Black,
    /// The whole input, fitted. Useful for a spare output being used as a
    /// confidence feed.
    FullInput,
    /// Colour bars — unmistakably "nothing is routed here", from across a room.
    Bars,
}

/// A physical DeckLink sub-device, remembered across restarts.
///
/// `persistent_id` is the device's own `BMDDeckLinkPersistentID`, which is
/// stable across reboots and reordering; `display_name` exists only so that a
/// show opened on a machine without that card can say *which* card is missing
/// rather than showing an empty slot.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DeviceRef {
    pub persistent_id: i64,
    pub display_name: String,
}

/// One physical output port.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Output {
    pub id: OutputId,
    pub label: String,
    /// `None` while the show is being built on a machine with no card.
    #[serde(default)]
    pub device: Option<DeviceRef>,
    /// The crosspoint. `None` is a perfectly normal steady state.
    #[serde(default)]
    pub assigned: Option<RoiId>,
    #[serde(default)]
    pub idle: IdleFill,
    #[serde(default)]
    pub fit: FitMode,
}

/// Which resampling filter the crop is scaled with.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ScalingFilter {
    /// Hardware bilinear. Cheapest, softest.
    Bilinear,
    /// Catmull-Rom bicubic. Sixteen taps, visibly sharper on the 2x-and-beyond
    /// blow-ups this app exists to do, which is why it is the default.
    #[default]
    Bicubic,
}

/// Everything that defines a running Kestrel, and everything that is saved.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Show {
    /// Bumped when the on-disk shape changes incompatibly.
    #[serde(default = "current_version")]
    pub version: u32,
    /// The format every output runs at.
    ///
    /// One global format rather than per-output: these ports feed one switcher,
    /// which wants one format, and DeckLink sub-devices on a shared card share a
    /// clock domain anyway. Per-output override is a later problem.
    #[serde(default)]
    pub output_format: VideoFormat,
    /// The last seen (or manually set) input raster, used for the scale
    /// arithmetic before a card is open. Autodetection overwrites it.
    #[serde(default = "default_input_size")]
    pub input_size: Size,
    #[serde(default)]
    pub scaling: ScalingFilter,
    /// The global output kill. `false` blacks every output; it does **not**
    /// stop them.
    #[serde(default = "yes")]
    pub outputs_enabled: bool,
    pub rois: Vec<Roi>,
    pub outputs: Vec<Output>,
    #[serde(default)]
    next_roi_id: u32,
    #[serde(default)]
    next_output_id: u32,
}

fn current_version() -> u32 {
    1
}

fn default_input_size() -> Size {
    Size::new(1920, 1080)
}

/// A picture that needs no input, so it is always available.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pattern {
    /// Legal black (Y=16 once encoded), not zero.
    Black,
    Bars,
}

/// What one output is carrying this frame.
///
/// Deliberately self-contained: everything the renderer needs is in the
/// variant, so the GPU code never reaches back into the show to look something
/// up. That is what keeps "what is on air" a decision made in one tested
/// function rather than spread across a render loop.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum PlanSource {
    /// Crop and scale a rectangle of the live input. `roi` is `None` when this
    /// is the whole frame as a confidence feed rather than a routed region.
    Crop {
        roi: Option<RoiId>,
        rect: NormRect,
        fit: FitMode,
    },
    /// A generated picture — nothing is routed here.
    Pattern(Pattern),
    /// The global output kill is engaged. Renders black, on every output,
    /// regardless of routing or idle fill.
    Muted,
    /// Something is routed, but there is no input to crop. Renders black —
    /// never the last good frame, which would freeze a shot on air and look
    /// live to anyone watching the output.
    NoInput,
}

impl PlanSource {
    /// True when this output is carrying a routed region — i.e. when a scale
    /// percentage is meaningful.
    pub fn is_live_region(&self) -> bool {
        matches!(self, PlanSource::Crop { roi: Some(_), .. })
    }
}

/// One output's decision for this frame.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct OutputPlan {
    pub output: OutputId,
    pub source: PlanSource,
}

#[derive(Debug, thiserror::Error, PartialEq)]
pub enum ShowError {
    #[error("no region with id {0}")]
    NoSuchRoi(RoiId),
    #[error("no output with id {0}")]
    NoSuchOutput(OutputId),
}

impl Default for Show {
    fn default() -> Self {
        Self::new()
    }
}

impl Show {
    /// An empty show at 1080p50 with no outputs.
    pub fn new() -> Self {
        Self {
            version: current_version(),
            output_format: VideoFormat::default(),
            input_size: default_input_size(),
            scaling: ScalingFilter::default(),
            outputs_enabled: true,
            rois: Vec::new(),
            outputs: Vec::new(),
            next_roi_id: 1,
            next_output_id: 1,
        }
    }

    /// A show with `n` unassigned outputs, for first run before any card is
    /// found. Every one of them will still be putting out black.
    pub fn with_outputs(n: usize) -> Self {
        let mut s = Self::new();
        for i in 0..n {
            s.add_output(format!("Output {}", i + 1));
        }
        s
    }

    // ---- regions -------------------------------------------------------

    pub fn add_roi(&mut self, name: impl Into<String>, rect: NormRect) -> RoiId {
        let id = RoiId(self.next_roi_id);
        self.next_roi_id += 1;
        let colour = ROI_PALETTE[(id.0 as usize).saturating_sub(1) % ROI_PALETTE.len()];
        self.rois.push(Roi {
            id,
            name: name.into(),
            rect: rect.clamped(),
            colour,
            lock_aspect: true,
        });
        id
    }

    pub fn roi(&self, id: RoiId) -> Option<&Roi> {
        self.rois.iter().find(|r| r.id == id)
    }

    pub fn roi_mut(&mut self, id: RoiId) -> Option<&mut Roi> {
        self.rois.iter_mut().find(|r| r.id == id)
    }

    /// Remove a region **and every crosspoint that pointed at it**.
    ///
    /// The two halves are one operation on purpose. Leaving a dangling
    /// assignment would make an output fall through to a lookup miss, and the
    /// obvious "just show black then" behaviour hides the fact that a live
    /// output silently lost its source.
    pub fn remove_roi(&mut self, id: RoiId) -> Result<(), ShowError> {
        if !self.rois.iter().any(|r| r.id == id) {
            return Err(ShowError::NoSuchRoi(id));
        }
        self.rois.retain(|r| r.id != id);
        for o in &mut self.outputs {
            if o.assigned == Some(id) {
                o.assigned = None;
            }
        }
        Ok(())
    }

    // ---- outputs -------------------------------------------------------

    pub fn add_output(&mut self, label: impl Into<String>) -> OutputId {
        let id = OutputId(self.next_output_id);
        self.next_output_id += 1;
        self.outputs.push(Output {
            id,
            label: label.into(),
            device: None,
            assigned: None,
            idle: IdleFill::default(),
            fit: FitMode::default(),
        });
        id
    }

    pub fn output(&self, id: OutputId) -> Option<&Output> {
        self.outputs.iter().find(|o| o.id == id)
    }

    pub fn output_mut(&mut self, id: OutputId) -> Option<&mut Output> {
        self.outputs.iter_mut().find(|o| o.id == id)
    }

    pub fn remove_output(&mut self, id: OutputId) -> Result<(), ShowError> {
        if !self.outputs.iter().any(|o| o.id == id) {
            return Err(ShowError::NoSuchOutput(id));
        }
        self.outputs.retain(|o| o.id != id);
        Ok(())
    }

    // ---- the crosspoint ------------------------------------------------

    /// Route a region to an output, or pass `None` to clear it.
    ///
    /// Exclusivity down a column is structural: an output holds one
    /// `Option<RoiId>`, so taking a new region necessarily drops the old one.
    /// Across a row it is free — one region can feed as many outputs as you
    /// like, which is the normal way to send the same crop to a screen and a
    /// record feed.
    pub fn route(&mut self, output: OutputId, roi: Option<RoiId>) -> Result<(), ShowError> {
        if let Some(r) = roi {
            if !self.rois.iter().any(|x| x.id == r) {
                return Err(ShowError::NoSuchRoi(r));
            }
        }
        let o = self
            .outputs
            .iter_mut()
            .find(|o| o.id == output)
            .ok_or(ShowError::NoSuchOutput(output))?;
        o.assigned = roi;
        Ok(())
    }

    /// Toggle a single cell of the matrix: routes if the cell is off, clears if
    /// it is already on. This is what clicking a crosspoint does.
    pub fn toggle_crosspoint(&mut self, output: OutputId, roi: RoiId) -> Result<(), ShowError> {
        let current = self
            .output(output)
            .ok_or(ShowError::NoSuchOutput(output))?
            .assigned;
        let next = if current == Some(roi) { None } else { Some(roi) };
        self.route(output, next)
    }

    /// Which outputs are carrying this region right now.
    pub fn outputs_for(&self, roi: RoiId) -> Vec<OutputId> {
        self.outputs
            .iter()
            .filter(|o| o.assigned == Some(roi))
            .map(|o| o.id)
            .collect()
    }

    // ---- what is on air ------------------------------------------------

    /// Decide what every output is carrying this frame.
    ///
    /// **The invariant this whole app rests on: there is exactly one entry per
    /// output, always.** Not "one per routed output". A live switcher that
    /// loses an input goes to a black or a glitch on that bus, and re-locking
    /// costs frames; so an unrouted Kestrel output is a valid, running,
    /// black-filled signal rather than a stopped one. Nothing here — no global
    /// kill, no missing region, no missing input — can shorten this list. The
    /// tests below pin that down.
    pub fn plan(&self, input_live: bool) -> Vec<OutputPlan> {
        self.outputs
            .iter()
            .map(|o| OutputPlan {
                output: o.id,
                source: self.source_for(o, input_live),
            })
            .collect()
    }

    fn source_for(&self, o: &Output, input_live: bool) -> PlanSource {
        if !self.outputs_enabled {
            return PlanSource::Muted;
        }
        match o.assigned.and_then(|id| self.roi(id)) {
            Some(roi) if input_live => PlanSource::Crop {
                roi: Some(roi.id),
                rect: roi.rect,
                fit: o.fit,
            },
            Some(_) => PlanSource::NoInput,
            None => match o.idle {
                IdleFill::Black => PlanSource::Pattern(Pattern::Black),
                IdleFill::Bars => PlanSource::Pattern(Pattern::Bars),
                // The one idle fill that needs an input to exist.
                IdleFill::FullInput if input_live => PlanSource::Crop {
                    roi: None,
                    rect: NormRect::FULL,
                    fit: o.fit,
                },
                IdleFill::FullInput => PlanSource::NoInput,
            },
        }
    }

    /// The magnification an output is applying, or `None` when it is not
    /// carrying a region.
    pub fn scale_of(&self, output: OutputId) -> Option<f64> {
        let o = self.output(output)?;
        let roi = self.roi(o.assigned?)?;
        Some(scale_factor(
            &roi.rect,
            self.input_size,
            self.output_format.size,
            o.fit,
        ))
    }

    pub fn scale_quality_of(&self, output: OutputId) -> Option<ScaleQuality> {
        self.scale_of(output).map(crate::geom::scale_quality)
    }

    /// Re-apply every aspect-locked region against the current output format.
    /// Called after the output format changes, so the regions follow it.
    pub fn reapply_aspect_locks(&mut self) {
        let target = self.output_format.size.aspect();
        let input = self.input_size;
        for r in &mut self.rois {
            if r.lock_aspect {
                r.rect = r.rect.with_aspect(target, input);
            }
        }
    }

    // ---- persistence ---------------------------------------------------

    pub fn to_json(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string_pretty(self)
    }

    pub fn from_json(s: &str) -> Result<Self, serde_json::Error> {
        let mut show: Show = serde_json::from_str(s)?;
        show.repair();
        Ok(show)
    }

    /// Make a loaded show internally consistent.
    ///
    /// A show file is editable by hand, and a hand-edited one can reference a
    /// region that was deleted, or reuse an id. Both would otherwise surface as
    /// an output that mysteriously shows black.
    pub fn repair(&mut self) {
        let known: Vec<RoiId> = self.rois.iter().map(|r| r.id).collect();
        for o in &mut self.outputs {
            if let Some(a) = o.assigned {
                if !known.contains(&a) {
                    o.assigned = None;
                }
            }
        }
        for r in &mut self.rois {
            r.rect = r.rect.clamped();
        }
        let max_roi = self.rois.iter().map(|r| r.id.0).max().unwrap_or(0);
        let max_out = self.outputs.iter().map(|o| o.id.0).max().unwrap_or(0);
        self.next_roi_id = self.next_roi_id.max(max_roi + 1);
        self.next_output_id = self.next_output_id.max(max_out + 1);
    }
}

/// Overlay colours, in the order regions get created. Chosen to stay
/// distinguishable on a busy stage picture and against each other.
pub const ROI_PALETTE: [[u8; 3]; 8] = [
    [255, 92, 92],
    [92, 200, 255],
    [255, 200, 64],
    [130, 230, 130],
    [220, 130, 255],
    [255, 150, 80],
    [110, 160, 255],
    [80, 230, 210],
];

#[cfg(test)]
mod tests {
    use super::*;

    fn show_with(n_outputs: usize, n_rois: usize) -> Show {
        let mut s = Show::with_outputs(n_outputs);
        for i in 0..n_rois {
            s.add_roi(
                format!("R{i}"),
                NormRect::new(0.1 * i as f64, 0.1, 0.3, 0.3),
            );
        }
        s
    }

    // --- the load-bearing invariant ------------------------------------

    #[test]
    fn every_output_is_planned_in_every_state() {
        let mut s = show_with(4, 2);
        let roi = s.rois[0].id;
        s.route(s.outputs[0].id, Some(roi)).unwrap();

        for enabled in [true, false] {
            for live in [true, false] {
                s.outputs_enabled = enabled;
                let plan = s.plan(live);
                assert_eq!(
                    plan.len(),
                    4,
                    "outputs must never drop out of the plan \
                     (enabled={enabled}, live={live})"
                );
                for o in &s.outputs {
                    assert!(
                        plan.iter().any(|p| p.output == o.id),
                        "{} missing (enabled={enabled}, live={live})",
                        o.id
                    );
                }
            }
        }
    }

    #[test]
    fn an_output_with_no_region_is_still_planned() {
        let s = show_with(3, 0);
        let plan = s.plan(true);
        assert_eq!(plan.len(), 3);
        assert!(plan
            .iter()
            .all(|p| p.source == PlanSource::Pattern(Pattern::Black)));
    }

    #[test]
    fn the_global_kill_mutes_everything_including_idle_fills() {
        let mut s = show_with(3, 1);
        let roi = s.rois[0].id;
        s.route(s.outputs[0].id, Some(roi)).unwrap();
        s.outputs[1].idle = IdleFill::Bars;
        s.outputs_enabled = false;

        let plan = s.plan(true);
        assert_eq!(plan.len(), 3);
        assert!(
            plan.iter().all(|p| p.source == PlanSource::Muted),
            "the kill must beat both a routed region and a bars idle: {plan:?}"
        );
    }

    #[test]
    fn losing_the_input_blacks_a_routed_output_rather_than_freezing_it() {
        let mut s = show_with(1, 1);
        let roi = s.rois[0].id;
        s.route(s.outputs[0].id, Some(roi)).unwrap();
        assert_eq!(s.plan(false)[0].source, PlanSource::NoInput);
    }

    #[test]
    fn losing_the_input_does_not_disturb_a_generated_idle_fill() {
        let mut s = show_with(2, 0);
        s.outputs[0].idle = IdleFill::Bars;
        s.outputs[1].idle = IdleFill::FullInput;
        let plan = s.plan(false);
        assert_eq!(plan[0].source, PlanSource::Pattern(Pattern::Bars));
        assert_eq!(
            plan[1].source,
            PlanSource::NoInput,
            "a full-input idle has nothing to show without an input"
        );
    }

    // --- the crosspoint -------------------------------------------------

    #[test]
    fn one_region_can_feed_many_outputs() {
        let mut s = show_with(3, 1);
        let roi = s.rois[0].id;
        for o in s.outputs.iter().map(|o| o.id).collect::<Vec<_>>() {
            s.route(o, Some(roi)).unwrap();
        }
        assert_eq!(s.outputs_for(roi).len(), 3);
    }

    #[test]
    fn an_output_takes_only_the_last_region_routed_to_it() {
        let mut s = show_with(1, 2);
        let (a, b) = (s.rois[0].id, s.rois[1].id);
        let out = s.outputs[0].id;
        s.route(out, Some(a)).unwrap();
        s.route(out, Some(b)).unwrap();
        assert_eq!(s.output(out).unwrap().assigned, Some(b));
        assert!(s.outputs_for(a).is_empty());
    }

    #[test]
    fn clicking_a_live_crosspoint_clears_it() {
        let mut s = show_with(1, 1);
        let (out, roi) = (s.outputs[0].id, s.rois[0].id);
        s.toggle_crosspoint(out, roi).unwrap();
        assert_eq!(s.output(out).unwrap().assigned, Some(roi));
        s.toggle_crosspoint(out, roi).unwrap();
        assert_eq!(s.output(out).unwrap().assigned, None);
    }

    #[test]
    fn deleting_a_region_clears_the_outputs_carrying_it() {
        let mut s = show_with(2, 1);
        let roi = s.rois[0].id;
        s.route(s.outputs[0].id, Some(roi)).unwrap();
        s.route(s.outputs[1].id, Some(roi)).unwrap();
        s.remove_roi(roi).unwrap();
        assert!(s.outputs.iter().all(|o| o.assigned.is_none()));
        // And they keep outputting.
        assert_eq!(s.plan(true).len(), 2);
    }

    #[test]
    fn routing_an_unknown_region_is_refused_rather_than_silently_ignored() {
        let mut s = show_with(1, 0);
        let out = s.outputs[0].id;
        assert_eq!(
            s.route(out, Some(RoiId(99))),
            Err(ShowError::NoSuchRoi(RoiId(99)))
        );
        assert_eq!(
            s.route(OutputId(99), None),
            Err(ShowError::NoSuchOutput(OutputId(99)))
        );
    }

    #[test]
    fn ids_are_never_reused_after_a_delete() {
        let mut s = show_with(0, 2);
        let first = s.rois[0].id;
        s.remove_roi(first).unwrap();
        let new = s.add_roi("later", NormRect::FULL);
        assert_ne!(new, first, "a stale control-surface id must not resolve");
    }

    // --- scale reporting ------------------------------------------------

    #[test]
    fn scale_is_reported_only_for_a_routed_output() {
        let mut s = show_with(2, 1);
        s.rois[0].rect = NormRect::new(0.0, 0.0, 0.5, 0.5);
        let (a, b) = (s.outputs[0].id, s.outputs[1].id);
        s.route(a, Some(s.rois[0].id)).unwrap();
        assert!((s.scale_of(a).unwrap() - 2.0).abs() < 1e-9);
        assert_eq!(s.scale_of(b), None);
    }

    // --- persistence ----------------------------------------------------

    #[test]
    fn a_show_round_trips_through_json() {
        let mut s = show_with(4, 3);
        s.route(s.outputs[1].id, Some(s.rois[2].id)).unwrap();
        s.outputs[0].idle = IdleFill::Bars;
        s.output_format = crate::format::VideoFormat::new(
            1280,
            720,
            60000,
            1001,
            crate::format::Scan::Progressive,
        );
        let json = s.to_json().unwrap();
        let back = Show::from_json(&json).unwrap();
        assert_eq!(s, back);
    }

    #[test]
    fn loading_repairs_a_hand_edited_dangling_crosspoint() {
        let mut s = show_with(1, 1);
        s.route(s.outputs[0].id, Some(s.rois[0].id)).unwrap();
        let json = s.to_json().unwrap().replace("\"rois\": [", "\"rois\": [ ");
        // Simulate a hand edit that removed the region but left the route.
        let mut broken: Show = serde_json::from_str(&json).unwrap();
        broken.rois.clear();
        broken.repair();
        assert_eq!(broken.outputs[0].assigned, None);
        assert_eq!(broken.plan(true).len(), 1);
    }

    #[test]
    fn loading_a_file_written_before_a_field_existed_still_works() {
        // Every optional field left out: this is the minimum a hand-written
        // show can be.
        let json = r#"{
            "rois": [{"id": 1, "name": "Lectern",
                      "rect": {"x":0.1,"y":0.1,"w":0.2,"h":0.2},
                      "colour": [255,0,0]}],
            "outputs": [{"id": 1, "label": "SDI 1"}]
        }"#;
        let s = Show::from_json(json).unwrap();
        assert_eq!(s.rois.len(), 1);
        assert!(s.rois[0].lock_aspect, "aspect lock defaults on");
        assert!(s.outputs_enabled, "outputs default to live");
        assert_eq!(s.output_format.to_string(), "1080p50");
        // And ids continue past what the file used.
        let mut s = s;
        assert_eq!(s.add_roi("next", NormRect::FULL), RoiId(2));
    }

    #[test]
    fn changing_the_output_format_pulls_locked_regions_to_the_new_aspect() {
        let mut s = show_with(0, 1);
        s.rois[0].rect = NormRect::new(0.2, 0.2, 0.4, 0.4);
        s.output_format =
            crate::format::VideoFormat::new(1440, 1080, 50, 1, crate::format::Scan::Progressive);
        s.reapply_aspect_locks();
        let got = s.rois[0].rect.aspect(s.input_size);
        assert!((got - 4.0 / 3.0).abs() < 1e-9, "aspect {got}");
    }

    #[test]
    fn an_unlocked_region_is_left_alone_by_a_format_change() {
        let mut s = show_with(0, 1);
        s.rois[0].lock_aspect = false;
        let before = s.rois[0].rect;
        s.output_format =
            crate::format::VideoFormat::new(1440, 1080, 50, 1, crate::format::Scan::Progressive);
        s.reapply_aspect_locks();
        assert_eq!(s.rois[0].rect, before);
    }
}

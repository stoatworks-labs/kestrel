//! The state everything shares, and the view of it that goes on the wire.

use kestrel_core::{
    scale_percent, IdleFill, OutputId, PlanSource, RoiId, ScaleQuality, Show, ShowError, Size,
};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

/// Live facts about the machine that are not part of the saved show.
#[derive(Debug, Clone, Default, Serialize)]
pub struct Runtime {
    /// A source is locked and delivering real frames.
    pub input_live: bool,
    pub input_size: Option<Size>,
    pub input_device: Option<String>,
    /// Human-readable, and honest about the difference between "no SDI in this
    /// build" and "no card found".
    pub decklink: String,
    /// Frames the engine has rendered since start.
    pub frames: u64,
    /// Per-output card counters, keyed by output id as a string (JSON object
    /// keys must be strings).
    pub buffered: std::collections::BTreeMap<String, i32>,
}

/// The show plus the runtime, behind one lock, with a revision counter.
///
/// The revision is what makes the WebSocket feed cheap: the server re-snapshots
/// at a fixed rate and only sends when the number moved. Every mutating path
/// goes through [`Shared::edit`], so there is exactly one place that can forget
/// to bump it.
pub struct Shared {
    show: Mutex<Show>,
    runtime: Mutex<Runtime>,
    revision: AtomicU64,
}

impl Shared {
    pub fn new(show: Show) -> Arc<Self> {
        Arc::new(Self {
            show: Mutex::new(show),
            runtime: Mutex::new(Runtime::default()),
            revision: AtomicU64::new(1),
        })
    }

    pub fn revision(&self) -> u64 {
        self.revision.load(Ordering::Relaxed)
    }

    /// Read the show. Keep the guard short — the render loop wants it too.
    pub fn show(&self) -> parking_lot::MutexGuard<'_, Show> {
        self.show.lock()
    }

    /// Mutate the show and bump the revision.
    ///
    /// The only way to change the show. Anything that took `show()` and wrote
    /// through it would leave every control surface showing stale tally until
    /// something else happened to bump the counter — which is the kind of bug
    /// that only appears when an operator is watching.
    pub fn edit<T>(&self, f: impl FnOnce(&mut Show) -> T) -> T {
        let out = {
            let mut g = self.show.lock();
            f(&mut g)
        };
        self.revision.fetch_add(1, Ordering::Relaxed);
        out
    }

    pub fn runtime(&self) -> Runtime {
        self.runtime.lock().clone()
    }

    /// Update the runtime facts. Bumps the revision only when something a
    /// control surface would care about actually changed — the frame counter
    /// moves every frame and must not push a WebSocket message every frame.
    pub fn set_runtime(&self, f: impl FnOnce(&mut Runtime)) {
        let mut g = self.runtime.lock();
        let before = (g.input_live, g.input_size, g.decklink.clone());
        f(&mut g);
        let after = (g.input_live, g.input_size, g.decklink.clone());
        drop(g);
        if before != after {
            self.revision.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// The full state, as sent over HTTP and pushed over the WebSocket.
    pub fn snapshot(&self) -> StateView {
        let show = self.show.lock();
        let runtime = self.runtime.lock().clone();
        let plan = show.plan(runtime.input_live);

        let rois = show
            .rois
            .iter()
            .map(|r| RoiView {
                id: r.id,
                name: r.name.clone(),
                rect: [r.rect.x, r.rect.y, r.rect.w, r.rect.h],
                colour: r.colour,
                lock_aspect: r.lock_aspect,
                outputs: show.outputs_for(r.id),
            })
            .collect();

        let outputs = show
            .outputs
            .iter()
            .map(|o| {
                let source = plan
                    .iter()
                    .find(|p| p.output == o.id)
                    .map(|p| p.source)
                    .unwrap_or(PlanSource::NoInput);
                let assigned_roi = o.assigned.and_then(|id| show.roi(id));
                let scale = assigned_roi.map(|r| {
                    scale_percent(&r.rect, show.input_size, show.output_format.size, o.fit)
                });
                OutputView {
                    id: o.id,
                    label: o.label.clone(),
                    assigned: o.assigned,
                    assigned_name: assigned_roi.map(|r| r.name.clone()),
                    idle: o.idle,
                    fit: format!("{:?}", o.fit).to_lowercase(),
                    scale_percent: scale,
                    quality: scale
                        .map(|s| kestrel_core::scale_quality(s / 100.0))
                        .map(quality_name)
                        .map(str::to_string),
                    device: o.device.as_ref().map(|d| d.display_name.clone()),
                    // What this output is *actually* carrying right now, which
                    // is not always what it is routed to — a muted or
                    // input-less output is on air with black.
                    on_air: source_name(&source).to_string(),
                    buffered: runtime.buffered.get(&o.id.0.to_string()).copied(),
                }
            })
            .collect();

        StateView {
            revision: self.revision(),
            outputs_enabled: show.outputs_enabled,
            output_format: FormatView {
                name: show.output_format.to_string(),
                width: show.output_format.width(),
                height: show.output_format.height(),
                rate_num: show.output_format.rate.num,
                rate_den: show.output_format.rate.den,
            },
            scaling: format!("{:?}", show.scaling).to_lowercase(),
            input: InputView {
                live: runtime.input_live,
                width: runtime.input_size.map(|s| s.w).unwrap_or(show.input_size.w),
                height: runtime.input_size.map(|s| s.h).unwrap_or(show.input_size.h),
                device: runtime.input_device.clone(),
            },
            decklink: runtime.decklink.clone(),
            frames: runtime.frames,
            rois,
            outputs,
        }
    }
}

fn quality_name(q: ScaleQuality) -> &'static str {
    match q {
        ScaleQuality::Native => "native",
        ScaleQuality::Soft => "soft",
        ScaleQuality::Heavy => "heavy",
    }
}

fn source_name(s: &PlanSource) -> &'static str {
    match s {
        PlanSource::Crop { roi: Some(_), .. } => "region",
        PlanSource::Crop { roi: None, .. } => "full input",
        PlanSource::Pattern(kestrel_core::Pattern::Black) => "black",
        PlanSource::Pattern(kestrel_core::Pattern::Bars) => "bars",
        PlanSource::Muted => "muted",
        PlanSource::NoInput => "no input",
    }
}

// --- the wire format ------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
pub struct StateView {
    pub revision: u64,
    pub outputs_enabled: bool,
    pub output_format: FormatView,
    pub scaling: String,
    pub input: InputView,
    pub decklink: String,
    pub frames: u64,
    pub rois: Vec<RoiView>,
    pub outputs: Vec<OutputView>,
}

#[derive(Debug, Clone, Serialize)]
pub struct FormatView {
    pub name: String,
    pub width: u32,
    pub height: u32,
    pub rate_num: u32,
    pub rate_den: u32,
}

#[derive(Debug, Clone, Serialize)]
pub struct InputView {
    pub live: bool,
    pub width: u32,
    pub height: u32,
    pub device: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct RoiView {
    pub id: RoiId,
    pub name: String,
    /// `[x, y, w, h]`, normalised. An array rather than an object because a
    /// control surface mostly passes it straight back.
    pub rect: [f64; 4],
    pub colour: [u8; 3],
    pub lock_aspect: bool,
    /// Every output carrying this region — the tally a control surface lights.
    pub outputs: Vec<OutputId>,
}

#[derive(Debug, Clone, Serialize)]
pub struct OutputView {
    pub id: OutputId,
    pub label: String,
    pub assigned: Option<RoiId>,
    pub assigned_name: Option<String>,
    pub idle: IdleFill,
    pub fit: String,
    pub scale_percent: Option<f64>,
    pub quality: Option<String>,
    pub device: Option<String>,
    pub on_air: String,
    pub buffered: Option<i32>,
}

// --- command bodies -------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct RouteBody {
    pub output: OutputId,
    /// `null` clears the crosspoint, which is a normal steady state, not an
    /// error.
    #[serde(default)]
    pub roi: Option<RoiId>,
}

#[derive(Debug, Deserialize)]
pub struct EnableBody {
    #[serde(default)]
    pub enabled: Option<bool>,
    /// Toggle instead of set. A control surface with one button wants this.
    #[serde(default)]
    pub toggle: bool,
}

#[derive(Debug, Deserialize)]
pub struct RoiBody {
    #[serde(default)]
    pub name: Option<String>,
    /// `[x, y, w, h]` normalised.
    #[serde(default)]
    pub rect: Option<[f64; 4]>,
    #[serde(default)]
    pub lock_aspect: Option<bool>,
}

#[derive(Debug, Deserialize)]
pub struct OutputBody {
    #[serde(default)]
    pub label: Option<String>,
    #[serde(default)]
    pub idle: Option<IdleFill>,
    #[serde(default)]
    pub fit: Option<kestrel_core::FitMode>,
}

/// Every command answers with this, including refusals.
///
/// Refusals come back as HTTP 200 with `ok: false` and an `error` string rather
/// than a 4xx, matching the rest of the fleet's control APIs — and unlike
/// srt-router, `error` is **never** empty on a refusal, so a client does not
/// have to invent the message.
#[derive(Debug, Serialize)]
pub struct Reply {
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<u32>,
}

impl Reply {
    pub fn ok() -> Self {
        Self {
            ok: true,
            error: None,
            id: None,
        }
    }

    pub fn created(id: u32) -> Self {
        Self {
            ok: true,
            error: None,
            id: Some(id),
        }
    }

    pub fn refused(msg: impl std::fmt::Display) -> Self {
        Self {
            ok: false,
            error: Some(msg.to_string()),
            id: None,
        }
    }
}

impl From<Result<(), ShowError>> for Reply {
    fn from(r: Result<(), ShowError>) -> Self {
        match r {
            Ok(()) => Reply::ok(),
            Err(e) => Reply::refused(e),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kestrel_core::NormRect;

    fn shared() -> Arc<Shared> {
        let mut show = Show::with_outputs(3);
        show.add_roi("Lectern", NormRect::new(0.1, 0.1, 0.25, 0.25));
        show.add_roi("Wide", NormRect::FULL);
        Shared::new(show)
    }

    #[test]
    fn editing_bumps_the_revision_and_reading_does_not() {
        let s = shared();
        let r0 = s.revision();
        let _ = s.show().outputs.len();
        assert_eq!(s.revision(), r0, "a read must not look like a change");
        s.edit(|show| show.outputs_enabled = false);
        assert!(s.revision() > r0);
    }

    #[test]
    fn the_frame_counter_does_not_push_a_message_every_frame() {
        // The WebSocket feed sends on revision change. If ticking the frame
        // counter bumped it, the feed would be a 50 Hz firehose.
        let s = shared();
        let r0 = s.revision();
        for _ in 0..100 {
            s.set_runtime(|r| r.frames += 1);
        }
        assert_eq!(s.revision(), r0);
    }

    #[test]
    fn losing_the_input_does_push_a_message() {
        let s = shared();
        s.set_runtime(|r| r.input_live = true);
        let r0 = s.revision();
        s.set_runtime(|r| r.input_live = false);
        assert!(
            s.revision() > r0,
            "input lock is exactly what tally cares about"
        );
    }

    #[test]
    fn the_snapshot_reports_what_is_on_air_not_just_what_is_routed() {
        let s = shared();
        let (out, roi) = {
            let show = s.show();
            (show.outputs[0].id, show.rois[0].id)
        };
        s.edit(|show| show.route(out, Some(roi)).unwrap());
        s.set_runtime(|r| r.input_live = true);

        let v = s.snapshot();
        assert_eq!(v.outputs[0].on_air, "region");
        assert_eq!(v.outputs[0].assigned_name.as_deref(), Some("Lectern"));

        // Kill the outputs: still routed, no longer on air.
        s.edit(|show| show.outputs_enabled = false);
        let v = s.snapshot();
        assert_eq!(
            v.outputs[0].assigned,
            Some(roi),
            "the route survives a mute"
        );
        assert!(
            v.outputs.iter().all(|o| o.on_air == "muted"),
            "every output must report muted, not just the routed one"
        );
    }

    #[test]
    fn an_unrouted_output_still_appears_with_its_idle_fill() {
        let s = shared();
        let v = s.snapshot();
        assert_eq!(v.outputs.len(), 3);
        assert!(v.outputs.iter().all(|o| o.assigned.is_none()));
        assert!(v.outputs.iter().all(|o| o.on_air == "black"));
        assert!(v.outputs.iter().all(|o| o.scale_percent.is_none()));
    }

    #[test]
    fn scale_and_quality_travel_together() {
        let s = shared();
        let (out, roi) = {
            let show = s.show();
            (show.outputs[0].id, show.rois[0].id)
        };
        s.edit(|show| show.route(out, Some(roi)).unwrap());
        let v = s.snapshot();
        // A quarter-width region on a same-size output is a 4x blow-up.
        let pct = v.outputs[0].scale_percent.unwrap();
        assert!((pct - 400.0).abs() < 1e-6, "{pct}");
        assert_eq!(v.outputs[0].quality.as_deref(), Some("heavy"));
    }

    #[test]
    fn a_region_carries_the_outputs_showing_it() {
        let s = shared();
        let (a, b, roi) = {
            let show = s.show();
            (show.outputs[0].id, show.outputs[1].id, show.rois[1].id)
        };
        s.edit(|show| {
            show.route(a, Some(roi)).unwrap();
            show.route(b, Some(roi)).unwrap();
        });
        let v = s.snapshot();
        let r = v.rois.iter().find(|r| r.id == roi).unwrap();
        assert_eq!(r.outputs, vec![a, b], "tally must list every output");
    }

    #[test]
    fn a_refusal_always_carries_a_message() {
        let r: Reply = Err(ShowError::NoSuchRoi(RoiId(99))).into();
        assert!(!r.ok);
        assert!(
            r.error.as_deref().is_some_and(|e| !e.is_empty()),
            "a client must never have to invent the error text"
        );
    }
}

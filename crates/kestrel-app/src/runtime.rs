//! The frame path, on its own thread.
//!
//! Deliberately not driven by the UI's frame loop. An SDI output runs on a
//! clock, and the things a GUI does — opening a menu, being dragged between
//! displays, waiting on a file dialog — are exactly the things that would
//! otherwise put a hole in it. The UI gets thumbnails posted to it and never
//! blocks anything here.

use crate::source::{FrameSlot, SyntheticSource};
use anyhow::{Context, Result};
use kestrel_control::Shared;
use kestrel_core::{OutputId, Size, VideoFormat};
use kestrel_decklink as dl;
use kestrel_render::{Engine, Gpu, Previews};
use parking_lot::Mutex;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Where the input comes from.
#[derive(Debug, Clone, PartialEq)]
pub enum InputSource {
    /// A DeckLink sub-device, by persistent id.
    DeckLink { persistent_id: i64, name: String },
    /// Generated bars with a moving marker. What runs when there is no card.
    Synthetic { size: Size },
}

impl InputSource {
    pub fn label(&self) -> String {
        match self {
            InputSource::DeckLink { name, .. } => name.clone(),
            InputSource::Synthetic { size } => {
                format!("synthetic bars {}x{}", size.w, size.h)
            }
        }
    }
}

#[derive(Debug, Clone)]
pub struct Config {
    pub input: InputSource,
    /// How often thumbnails are posted to the UI. Much slower than the frame
    /// rate on purpose — nobody needs a 50 Hz thumbnail, and the readback is
    /// the expensive part.
    pub preview_hz: f64,
    /// Height of an output thumbnail in pixels.
    pub thumb_height: u32,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            input: InputSource::Synthetic {
                size: Size::new(1920, 1080),
            },
            preview_hz: 12.0,
            thumb_height: 96,
        }
    }
}

/// A running frame path. Dropping it stops the thread and closes the card.
pub struct Runtime {
    previews: Arc<Mutex<Previews>>,
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Runtime {
    pub fn start(shared: Arc<Shared>, config: Config) -> Result<Self> {
        let previews = Arc::new(Mutex::new(Previews::default()));
        let stop = Arc::new(AtomicBool::new(false));

        let thread = {
            let previews = previews.clone();
            let stop = stop.clone();
            std::thread::Builder::new()
                .name("kestrel-frames".into())
                .spawn(move || {
                    if let Err(e) = run(shared.clone(), config, previews, stop) {
                        tracing::error!(error = %e, "frame path stopped");
                        shared.set_runtime(|r| r.decklink = format!("frame path stopped: {e}"));
                    }
                })
                .context("could not start the frame-path thread")?
        };

        Ok(Self {
            previews,
            stop,
            thread: Some(thread),
        })
    }

    /// The latest thumbnails. Cheap to call; returns a clone so the UI never
    /// holds the lock while it draws.
    pub fn previews(&self) -> Previews {
        self.previews.lock().clone()
    }
}

impl Drop for Runtime {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

fn run(
    shared: Arc<Shared>,
    config: Config,
    previews: Arc<Mutex<Previews>>,
    stop: Arc<AtomicBool>,
) -> Result<()> {
    let (mut format, initial_input) = {
        let show = shared.show();
        (show.output_format, show.input_size)
    };

    let gpu = pollster::block_on(Gpu::new())?;
    let adapter = gpu.adapter_name.clone();
    let mut engine = Engine::new(gpu, initial_input, format)?;
    tracing::info!(%adapter, %format, "frame path starting");

    shared.set_runtime(|r| r.decklink = dl::status_line());

    // --- the input ------------------------------------------------------
    let slot = FrameSlot::new();
    let mut synthetic = None;
    let _capture = match &config.input {
        InputSource::DeckLink { persistent_id, .. } => {
            let s = slot.clone();
            Some(
                dl::Capture::open(*persistent_id, move |f| {
                    s.put(f.bytes, f.row_bytes, f.size, f.no_signal);
                })
                .context("could not open the DeckLink input")?,
            )
        }
        InputSource::Synthetic { size } => {
            synthetic = Some(SyntheticSource::new(*size));
            None
        }
    };
    shared.set_runtime(|r| r.input_device = Some(config.input.label()));

    let mut playbacks: HashMap<OutputId, PlaybackSlot> = HashMap::new();
    let mut last_seq = 0u64;
    let mut last_good = Instant::now();
    let mut frames = 0u64;
    let mut preview_due = Instant::now();

    // Deadlines from tick zero, never `now + period`. Accumulating a period
    // onto the current time folds in every scheduling delay and the output rate
    // drifts slowly low — which shows up hours later as the card's buffer
    // draining, not as anything you would notice in a ten-minute test.
    let start = Instant::now();
    let mut tick = 0u64;

    while !stop.load(Ordering::Relaxed) {
        tick += 1;
        let period = frame_period(format);
        let deadline = start + period.mul_f64(tick as f64);

        // --- reconfigure to match the show --------------------------------
        let (plan, want_format, scaling, devices) = {
            let show = shared.show();
            (
                show.plan(engine.input_live()),
                show.output_format,
                show.scaling,
                show.outputs
                    .iter()
                    .map(|o| (o.id, o.device.clone()))
                    .collect::<Vec<_>>(),
            )
        };
        if want_format != format {
            tracing::info!(from = %format, to = %want_format, "output format changed");
            format = want_format;
            engine.set_output_format(format);
            // Every open card is running the old mode. Closing them all and
            // letting the reconcile below reopen is the only safe move: a
            // DeckLink cannot change display mode under a running schedule.
            playbacks.clear();
        }
        engine.set_scaling(scaling);
        reconcile_playbacks(&mut playbacks, &devices, format);

        // --- the input ----------------------------------------------------
        if let Some(syn) = synthetic.as_mut() {
            let size = syn.size();
            let row = syn.row_bytes();
            engine.set_input_size(size);
            engine.upload_input(syn.next(), row)?;
            last_good = Instant::now();
        } else if let Some(frame) = slot.take_newer_than(last_seq) {
            last_seq = frame.seq;
            if frame.no_signal {
                // The card is still ticking; there is just nothing on the wire.
                // Not an upload, and not an error.
            } else {
                engine.set_input_size(frame.size);
                engine.upload_input(&frame.bytes, frame.row_bytes)?;
                last_good = Instant::now();
            }
        }

        // A watchdog rather than "no frame this tick": at 25p into a 50p output
        // half the ticks legitimately have no new frame, and treating that as a
        // lost source would strobe every output. Three frame periods, floored
        // at 200 ms, is long enough to ride out a mode change and short enough
        // that an unplugged cable is obvious.
        let grace = period.mul_f32(3.0).max(Duration::from_millis(200));
        if engine.input_live() && last_good.elapsed() > grace {
            tracing::warn!("input lost");
            engine.set_input_live(false);
        }

        // --- render and send ----------------------------------------------
        let mut errors: Vec<String> = Vec::new();
        engine.render(&plan, &mut |id, bytes, row| {
            if let Some(p) = playbacks.get(&id) {
                if let Err(e) = p.playback.schedule(bytes, row) {
                    errors.push(format!("{id}: {e}"));
                }
            }
        })?;
        for e in &errors {
            tracing::warn!(error = %e, "output refused a frame");
        }
        frames += 1;

        // --- tell the UI ---------------------------------------------------
        let live = engine.input_live();
        let in_size = engine.input_size();
        let buffered: std::collections::BTreeMap<String, i32> = playbacks
            .iter()
            .map(|(id, p)| (id.0.to_string(), p.playback.stats().buffered))
            .collect();
        shared.set_runtime(|r| {
            r.input_live = live;
            r.input_size = Some(in_size);
            r.frames = frames;
            r.buffered = buffered;
        });

        if Instant::now() >= preview_due {
            let p = engine.capture_previews(config.thumb_height);
            *previews.lock() = p;
            preview_due = Instant::now()
                + Duration::from_secs_f64(1.0 / config.preview_hz.clamp(1.0, 60.0));
        }

        // --- pace -----------------------------------------------------------
        let now = Instant::now();
        if deadline > now {
            std::thread::sleep(deadline - now);
        } else if now - deadline > period * 10 {
            // Far enough behind that catching up would mean a burst of frames
            // nobody will see. Re-base rather than sprint.
            tracing::warn!(
                behind_ms = (now - deadline).as_millis(),
                "frame path fell behind; re-basing the clock"
            );
            tick = (now.duration_since(start).as_secs_f64() / period.as_secs_f64()) as u64;
        }
    }

    tracing::info!(frames, "frame path stopping");
    Ok(())
}

struct PlaybackSlot {
    persistent_id: i64,
    format: VideoFormat,
    playback: dl::Playback,
}

/// Open, close and reopen cards so the running set matches the show.
///
/// An output with no card assigned is *not* an error and not skipped upstream:
/// it is still planned, still rendered, and its frame simply has nowhere to go.
/// That keeps "every output is planned every frame" true whether or not there
/// is hardware, which is what lets the whole app be built and demonstrated on a
/// machine with no card in it.
fn reconcile_playbacks(
    playbacks: &mut HashMap<OutputId, PlaybackSlot>,
    devices: &[(OutputId, Option<kestrel_core::DeviceRef>)],
    format: VideoFormat,
) {
    let wanted: HashMap<OutputId, i64> = devices
        .iter()
        .filter_map(|(id, d)| d.as_ref().map(|d| (*id, d.persistent_id)))
        .collect();

    playbacks.retain(|id, slot| {
        wanted.get(id) == Some(&slot.persistent_id) && slot.format == format
    });

    for (id, persistent_id) in wanted {
        if playbacks.contains_key(&id) {
            continue;
        }
        match dl::Playback::open(persistent_id, format) {
            Ok(playback) => {
                tracing::info!(%id, persistent_id, %format, "output open");
                playbacks.insert(
                    id,
                    PlaybackSlot {
                        persistent_id,
                        format,
                        playback,
                    },
                );
            }
            Err(e) => {
                // Logged once per attempt rather than swallowed, but never
                // fatal: one dead port must not take the other three off air.
                tracing::warn!(%id, persistent_id, error = %e, "could not open output");
            }
        }
    }
}

fn frame_period(f: VideoFormat) -> Duration {
    let fps = f.rate.as_f64();
    if fps <= 0.0 {
        Duration::from_millis(20)
    } else {
        Duration::from_secs_f64(1.0 / fps)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kestrel_core::Scan;

    #[test]
    fn frame_periods_are_exact_for_the_awkward_rates() {
        let p = frame_period(VideoFormat::new(1920, 1080, 60000, 1001, Scan::Progressive));
        assert!((p.as_secs_f64() - 1001.0 / 60000.0).abs() < 1e-12);
        let p = frame_period(VideoFormat::new(1920, 1080, 50, 1, Scan::Progressive));
        assert!((p.as_secs_f64() - 0.02).abs() < 1e-12);
    }

    #[test]
    fn a_zero_rate_does_not_produce_a_zero_period_busy_loop() {
        let p = frame_period(VideoFormat::new(1920, 1080, 0, 1, Scan::Progressive));
        assert!(p > Duration::ZERO);
    }

    #[test]
    fn reconciling_drops_nothing_when_nothing_changed() {
        // With no SDK compiled in, opening always fails — which is exactly the
        // case worth pinning down: reconcile must not panic, must not retain a
        // stale entry, and must leave the app running.
        let mut p: HashMap<OutputId, PlaybackSlot> = HashMap::new();
        let devices = vec![(
            OutputId(1),
            Some(kestrel_core::DeviceRef {
                persistent_id: 42,
                display_name: "nope".into(),
            }),
        )];
        let fmt = VideoFormat::default();
        reconcile_playbacks(&mut p, &devices, fmt);
        reconcile_playbacks(&mut p, &devices, fmt);
        if !dl::available() {
            assert!(p.is_empty(), "a card that cannot open must not be retained");
        }
    }

    #[test]
    fn an_output_with_no_device_is_simply_not_opened() {
        let mut p: HashMap<OutputId, PlaybackSlot> = HashMap::new();
        reconcile_playbacks(&mut p, &[(OutputId(1), None)], VideoFormat::default());
        assert!(p.is_empty());
    }
}

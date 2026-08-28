# Notes

Working notes for this repo: status, decisions, and the traps that have actually bitten.
Migrated out of Claude Code's memory on 2026-08-24, so they are written in the first
person and dated by when each thing was learned — that date is usually the useful part.

Cross-cutting notes that are not specific to this repo live in
[fleet-notes](https://github.com/stoatworks-labs/fleet-notes).

*Kestrel — DeckLink ROI router: crops of one wide shot scaled back out to SDI outputs. PUBLIC MIT, Rust/wgpu/egui. SDI hardware-verified on a Duo 2 (2026-08-16) at a fixed 1080p50; format autodetect, the GUI's widgets, Windows and Linux still unverified*

**Kestrel** (`~/Projects/kestrel`, PUBLIC MIT, started 2026-08-02) takes one
video input — a stage wide shot — lets you draw any number of **regions of
interest** on it, and sends each region back out of a DeckLink cropped and
scaled to the output raster. Named for the bird that hovers over a wide field
and picks out one thing at a time.

Rust workspace: `kestrel-core` (decisions, no GPU/IO), `kestrel-render` (wgpu),
`kestrel-decklink` (C ABI over the Blackmagic SDK), `kestrel-control`
(HTTP + WebSocket), `kestrel-app` (frame thread + CLI), `kestrel-gui` (egui),
plus the vendored `diag`. Companion module is a separate repo,
**companion-module-kestrel**, also PUBLIC MIT.

## The invariant everything rests on

**Every output is planned and rendered every frame**, whatever the routing, the
global kill, the input or the card say. `Show::plan()` is the only place that
decides, and it returns exactly one entry per output — always. An SDI output
that stops is one the switcher has to re-lock. So: the global kill *mutes*
(routes survive); a lost input gives black, never the last good frame; an output
with no card is still rendered and its frame goes nowhere.

## Verified vs assumed — the line to hold

**Measured here:** the whole GPU path by pixel readback (14 tests: 4:2:2 decode,
luma column alignment, crop/magnify, fit-vs-fill bars, both filters agreeing on
flat colour, legal black Y=16, a padded-row raster, full pack→decode round
trip); the control API (14 HTTP tests); the Companion module against a really
running Kestrel; **49.99 fps sustained at 1080p50 with the GUI open**, 50.20
headless. 111 Rust tests, clippy clean.

**SDI is now hardware-verified (2026-08-16)** on a real DeckLink Duo 2 in a
Mercury Helios 3S Thunderbolt chassis, ports 1↔4 cabled as a loopback:

- **Output path PASSES.** Kestrel rendered bars to port 1; `weblinked`'s
  `tools/sdi_probe.mm` — an independently-written BT.709 reference, deliberately
  not shared with the app — read all 8 bars back correctly off the wire at
  1920x1080, black at Y=16, verdict PASS. That is scheduled playback, pre-roll,
  the frame pool, stride and 8-bit YUV packing proven on real SDI.
- **Enumeration and capture work.** `kestrel devices` reports 4 sub-devices all
  active, half-duplex, cross-validating dl_scan exactly. `kestrel init` wrote
  real persistent ids. Capture reports `live`, 1920x1080, from Duo (4).
- **Input loss verified**, by feeding it an *unconnected* sub-device (the only
  genuinely dead source on a one-cable rig — see
  [decklink output persists after exit](https://github.com/stoatworks-labs/fleet-notes/blob/main/notes/reference_decklink_output_persists_after_exit.md), which is why killing the
  transmitter proves nothing). Kestrel reported `live: false`, and the output
  kept transmitting 1080p50 **legal black** — 26 frames received, never the last
  good frame, never stopped.
- **Global kill verified on the wire.** Engaged: 25 frames still flowing, all
  black, and the crosspoint survived. Released: bars back, probe PASS.

**No bugs were found.** An apparent input-loss bug was chased and disproved; the
watchdog in `crates/kestrel-app/src/runtime.rs` (~line 223) is correct, and both
`bmdFrameHasNoInputSource` and `bmdDeckLinkStatusVideoInputSignalLocked` are
reliable.

Still unproven: **format autodetect and mode changes** were never exercised (a
second SDI cable on ports 2↔3 would allow it). **The GUI's widgets have never
been clicked** — the
window opens, runs and serves, but the layout was never visually confirmed (see
**screenshot capture** (working-practice note, kept in Claude memory); two attempts, both reported success and
captured the wrong window). Windows and Linux: never built.

**Not built:** NDI/network protocols, Spout, Syphon, fullscreen GPU output,
audio, per-output format override. All roadmap.

## Things that cost real time here

- **macOS App Nap nearly killed it** — see [macos app nap](https://github.com/stoatworks-labs/fleet-notes/blob/main/notes/reference_macos_app_nap.md). A
  dedicated frame thread is *not* enough.
- **Two DeckLink SDKs on this Mac** and first-hit order picks the wrong one: a
  10.11 header set inside the NDI SDK's examples, a 12.2 one inside Unreal's
  BlackmagicMedia. `build.rs` now reads `DeckLinkAPIVersion.h` from every
  candidate and takes the newest, rejecting anything below 11.0 (which is when
  `IDeckLinkProfileAttributes` and `BMDDeckLinkDuplex` arrived).
- **Card profiles**: a multi-sub-device card lists *all* sub-devices whatever
  profile it is in, and switched-off ones offer no display modes at all — which
  reads as broken hardware. Kestrel needs the 4-sub-device half-duplex profile
  on a Duo 2. `kestrel devices` explains it.
- **egui 0.35 broke the panel API**: `TopBottomPanel`/`SidePanel` are gone,
  replaced by one `egui::containers::Panel`; panels show into `&mut Ui`, not
  `&Context`; `eframe::App` has `ui()` not `update()`; style is per-theme via
  `all_styles_mut`.
- **Deadlines from the rational at each tick**, never `period * n` — the latter
  drifts a millisecond an hour at 59.94, invisible in a short test.

Reuses the fleet heavily: [weblinked](https://github.com/stoatworks-labs/weblinked/blob/main/docs/NOTES.md) (`weblinked`) for the DeckLink sequence and
`tools/dl_scan.mm`'s capture loop, [unmapper](https://github.com/stoatworks-labs/UnMapper/blob/main/docs/NOTES.md) (`UnMapper`) for the render-once
architecture and the wgpu/egui traps, `companion-module-srt-router` for the
module pattern. See [agents md convention](https://github.com/stoatworks-labs/fleet-notes/blob/main/notes/reference_agents_md_convention.md).

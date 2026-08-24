# Kestrel — orientation for whoever picks this up next

Read this before changing anything. `README.md` is what the app does;
`CLAUDE.md` is the command reference. This file is the *why*, and the list of
things that are true but not obvious.

## The one invariant

**Every output is planned and rendered every frame, whatever else is true.**

`Show::plan()` in `crates/kestrel-core/src/show.rs` is the only place that
decides what is on air, and it returns exactly one entry per output — always.
No global kill, no missing region, no absent input and no unassigned card can
shorten that list.

This is not tidiness. An SDI output that stops is one the switcher downstream
has to re-lock, and that costs frames on air. If you find yourself adding an
early return or a `filter` to the render loop, you are about to break the thing
the app exists to guarantee. The tests in that file check the invariant from
every combination of states; keep them passing.

Corollaries worth stating because they look like bugs otherwise:

- The **global kill mutes, it does not stop**. Crosspoints survive it.
- **Losing the input produces black, not the last good frame.** A frozen frame
  looks live to anyone watching an output.
- **An output with no card is still rendered.** Its frame goes nowhere. That is
  what lets the whole app be built and demonstrated on a machine with no card.

## Layout

| crate | what it is |
|---|---|
| `kestrel-core` | the decisions: regions, outputs, the crosspoint, scale arithmetic, the show file. No GPU, no IO. |
| `kestrel-render` | the four GPU stages and the readback. |
| `kestrel-decklink` | a C ABI over the Blackmagic SDK, plus the safe Rust over it. Optional at build time. |
| `kestrel-control` | shared state, the HTTP command API, the WebSocket state feed. |
| `kestrel-app` | the frame-path thread, the show file, the `kestrel` CLI. |
| `kestrel-gui` | the operator window. |
| `diag` | vendored from the rest of the fleet; do not edit it here. |

The GUI, the render loop and the control API all mutate through one
`Shared::edit`, which is the only thing that bumps the revision the WebSocket
feed watches. Writing through `Shared::show()` instead compiles fine and leaves
every control surface showing stale tally.

## Traps, all of them found the hard way here

**macOS App Nap will destroy the frame rate.** The frame path is on its own
thread precisely so the UI cannot stall it — and that is not enough. macOS
demotes the *whole process* the moment the window is not frontmost, niced, with
timers coalesced. Measured on an M4 Max: **50.2 fps → 6.7 fps** with the window
merely covered by another app, logs full of "fell behind", every SDI output
dropping frames while looking fine from the operator's chair. Fixed by
`NSProcessInfo beginActivityWithOptions:` with `NSActivityLatencyCritical`
(`crates/kestrel-app/native/activity.m`) plus a QoS bump on the frame thread.
`kestrel_app::keep_awake()` must be called at startup by **every** binary.
Verified back at 49.99 fps, zero fell-behind warnings.

**Deadlines come from the rational, at each tick.** `period * n` truncates once
and then multiplies the truncation — 0.33 ns per tick at 59.94, over a
millisecond an hour, which surfaces as the card's buffer draining hours in and
nothing at all in a ten-minute test. `elapsed_at_tick` does the division last.

**Never a `vec3` in a uniform block.** WGSL aligns `vec3<T>` to 16 bytes, so it
does not sit where the `[T; 3]` beside it in Rust does — it pushes everything
after it and changes the block size. Pad with scalars. Uniform blocks also round
*up* to 16, so an "obviously" 56-byte struct is 64 and a 56-byte binding is
rejected with an error naming no field. `crates/kestrel-render/src/uniforms.rs`
asserts the sizes and offsets so a future edit fails `cargo test` rather than a
shader compile on a show day.

**Row padding is not an edge case.** wgpu wants `bytes_per_row` a multiple of
256. An HD UYVY row is 3840 = 15 × 256, so the naive memcpy works perfectly at
1080p and shears every frame the first time somebody picks a raster that is not.
There is one code path, always padded; the aligned case is a memcpy inside it.
`a_raster_whose_rows_need_padding_reads_back_without_shear` uses 1366×768,
which is the raster that catches it.

**Two DeckLink SDKs on this Mac, and the older one is found first.** A 10.11
header set lives inside the NDI SDK's examples and a 12.2 one inside Unreal's
BlackmagicMedia. First-hit order picked 10.11 and the build died on symbols that
have existed since 11.0, which reads like a broken shim. `build.rs` now checks
every candidate's `DeckLinkAPIVersion.h` and takes the newest, rejecting
anything below 11.0 with a reason.

**A DeckLink output keeps transmitting after its process exits, so killing the
transmitter does not test input loss.** On a loopback rig the far end stays
genuinely *locked* to valid 1080p50 black long after the process that opened the
output is gone — `bmdDeckLinkStatusVideoInputSignalLocked` reads YES and the
frames are not flagged `bmdFrameHasNoInputSource`, because none of that is a
lie. This reads exactly like a broken input-loss watchdog and invites you to
"fix" correct code; a whole session went into disproving it here. Both flags are
reliable. To test signal loss, point the input at a sub-device with **nothing
plugged into it** — the only genuinely dead source on a one-cable rig, and it
reports `signal-locked=no` with zero good frames.

**Card profiles.** A multi-sub-device card lists *all* its sub-devices whatever
profile it is in, and the switched-off ones offer no display modes at all — an
open attempt on one looks exactly like broken hardware. `Device::active` and
`menu_label()` exist for this; do not let a UI drop the explanation.

**egui 0.35 is not egui 0.34.** `TopBottomPanel`/`SidePanel` are gone, replaced
by one `egui::containers::Panel` with `top`/`bottom`/`left`/`right`
constructors, every panel shows into a `&mut Ui` rather than a `&Context`, and
`eframe::App` has `ui(&mut self, ui, frame)` instead of `update(ctx, frame)`.
Style is per-theme now: set both via `all_styles_mut` and pin the theme, or a
machine in light mode gets default chrome and the tally colours stop reading.

**`clamped()` can never produce an invalid rect,** because it forces the size up
to `MIN_SIZE`. `rect.clamped().is_valid()` is therefore always true and was, for
a while, a validation guard that validated nothing. Position is forgiving
(clamped back into frame, matching what dragging does); size is not (a
zero-area region is refused).

**`@companion-module/base` 2.x has no `runEntrypoint`.** Export the default
class and `UpgradeScripts`; Companion imports them. Calling it fails at import
with a bare `SyntaxError` naming the export rather than the version. And
`InstanceBase`'s constructor refuses to run outside Companion's host, so the
smoke test builds its instance with `Object.create(ModuleInstance.prototype)` —
subclassing or hand-rolling a look-alike would test a copy of the logic instead
of the logic.

## Verified vs assumed

Keep this section honest. "Compiles" is never "works".

**Verified on this machine, by measurement:**

- The whole GPU path, by pixel readback against an independently-written CPU
  reference: 4:2:2 decode of the primaries, luma column alignment, crop and
  magnification, fit/fill bars, both scaling filters agreeing on a flat colour,
  legal black (Y=16, not 0) on every idle path, bars against the reference,
  a padded-row raster, and a full pack→decode round trip. 14 GPU tests.
- The control API end to end, 14 HTTP tests through the real router.
- The Companion module against a **really running Kestrel** (`test/live.mjs` in
  that repo): the WebSocket feed, every field it reads, a real take, the scale
  variable, the global kill muting everything while keeping routes, a clear, and
  a refusal carrying Kestrel's own message.
- **49.99 fps sustained at 1080p50** with the GUI open and occluded, zero
  fell-behind warnings. 50.20 fps headless.
- 111 Rust tests, clippy clean.
- **SDI, on a real DeckLink Duo 2** (2026-08-16, Mercury Helios 3S Thunderbolt
  chassis, Desktop Video 16.0.1, ports 1↔4 cabled as a loopback):
  - **Output.** Kestrel rendered bars to port 1 and `weblinked`'s
    `tools/sdi_probe.mm` — an independent BT.709 reference, deliberately not
    sharing code with this app — read all eight bars back correctly off the
    wire at 1920x1080, black at Y=16. PASS. That is scheduled playback,
    pre-roll, the frame pool, stride and the 8-bit YUV packing, measured.
  - **Enumeration.** `kestrel devices` reports four sub-devices, all active,
    half-duplex, agreeing exactly with `tools/dl_scan.mm`. `kestrel init` wrote
    real persistent ids.
  - **Capture.** Reports live at 1920x1080 off a real input.
  - **Input loss.** Fed an *unconnected* sub-device, Kestrel reports
    `live: false` and the output goes on transmitting legal black — 26 frames
    received, never the last good frame, never stopped.
  - **The global kill, on the wire.** Engaged: 25 frames still flowing, all
    black, crosspoint intact. Released: bars back, PASS.

**Written, compiled, never run against hardware — do not describe as working:**

- **Format autodetection and mode changes.** Everything above ran at a fixed
  1080p50. A second cable across ports 2↔3 is what this needs.
- **The GUI's widgets have never been clicked.** The window opens, runs, renders
  and serves; its layout has not been visually confirmed, because
  `screencapture` and window-raising are unreliable on this Mac (two attempts,
  both returned success and captured the wrong window). The logic underneath —
  drag arithmetic, corner grabs, aspect locking, the matrix's short labels — is
  unit-tested; the widgets are not.
- **Windows and Linux.** Never built, never run.

**Not built at all:** NDI and the other network protocols, Spout, Syphon,
fullscreen GPU output, audio of any kind, per-output format override.

## Sibling projects worth reading first

- `weblinked` — DeckLink **output** verified on a real Duo 2, and
  `docs/04-verification.md` §19 is where the scheduled-playback numbers and the
  profile findings come from. `tools/dl_scan.mm` is a working capture loop.
- `UnMapper` — the same architectural shape (render once, every output is a
  crop) and the wgpu/egui traps.
- `companion-module-srt-router` — the house pattern for a Companion module.

## Notes

`docs/NOTES.md` carries this repo's working notes — current status, decisions
already made, and the traps that have actually bitten. Read it before changing
anything non-obvious. Cross-cutting fleet knowledge lives in
[fleet-notes](https://github.com/stoatworks-labs/fleet-notes).

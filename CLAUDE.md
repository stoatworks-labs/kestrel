# Kestrel — commands

Read [`AGENTS.md`](AGENTS.md) first. It has the invariant this app rests on, the
traps, and the verified-vs-assumed list. This file is only the commands.

## Build and test

```bash
cargo test --workspace          # 111 tests; the GPU ones need a real adapter
cargo clippy --workspace --all-targets
cargo fmt --all
```

The DeckLink shim compiles only when an SDK is found. It is autodetected from
Unreal's BlackmagicMedia copy on this Mac; override with:

```bash
DECKLINK_SDK_DIR="/path/to/Blackmagic DeckLink SDK 12.9" cargo build --release
```

SDK 11.0 or newer. Watch the `cargo::warning` lines — they say which SDK was
picked and which were rejected.

## Run

```bash
cargo run --release -p kestrel-gui                    # the operator window
cargo run --release -p kestrel-gui -- --http 127.0.0.1:9720 --show my.show.json
cargo run --release -p kestrel-app --bin kestrel -- devices
cargo run --release -p kestrel-app --bin kestrel -- run --http 0.0.0.0:9720
cargo run --release -p kestrel-app --bin kestrel -- init my.show.json
```

With no card, the input is generated bars with a sweeping marker and everything
but the SDI works.

## Drive it

```bash
curl -s localhost:9720/api/state | python3 -m json.tool
curl -s -X POST localhost:9720/api/route -H 'content-type: application/json' -d '{"output":1,"roi":2}'
curl -s -X POST localhost:9720/api/output/enable -H 'content-type: application/json' -d '{"toggle":true}'
```

Refusals are HTTP 200 with `ok:false` — check the body, not the status.

## Companion module

Separate repo, `~/Projects/companion-module-kestrel`.

```bash
npm test                          # against a fake Kestrel
node test/live.mjs 127.0.0.1:9720 # against a really running one; skips if absent
```

## Logs

`~/Library/Logs/kestrel-gui/` and `~/Library/Logs/kestrel/`, via the vendored
`diag` crate.

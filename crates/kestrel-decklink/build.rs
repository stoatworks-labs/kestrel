//! Compiles the DeckLink shim — when, and only when, an SDK is available.
//!
//! The SDK is a free but licence-gated Blackmagic download and is not ours to
//! redistribute, so nothing from it is vendored. Point the build at a copy:
//!
//! ```text
//! DECKLINK_SDK_DIR="/path/to/Blackmagic DeckLink SDK 12.9" cargo build
//! ```
//!
//! Without one the crate still builds and reports itself unavailable. That is
//! deliberate: the whole app must be developable, testable and demonstrable on
//! a machine with no card and no SDK, and "DeckLink was not compiled in" must
//! be distinguishable from "no card found", because the fixes are different.

use std::path::{Path, PathBuf};

fn main() {
    println!("cargo::rustc-check-cfg=cfg(decklink)");
    println!("cargo::rerun-if-env-changed=DECKLINK_SDK_DIR");
    println!("cargo::rerun-if-changed=shim/kestrel_decklink.cpp");
    println!("cargo::rerun-if-changed=shim/kestrel_decklink.h");

    let Some(include) = find_sdk() else {
        println!(
            "cargo::warning=DeckLink SDK not found — building without SDI \
             support. Set DECKLINK_SDK_DIR to enable it."
        );
        return;
    };

    // On macOS and Linux the API is reached through DeckLinkAPIDispatch.cpp,
    // which ships in the SDK and dlopens the installed driver. That is why
    // there is no link library on those platforms: a binary built with the SDK
    // still runs on a machine with no Desktop Video installed, and simply finds
    // no devices.
    let dispatch = include.join("DeckLinkAPIDispatch.cpp");
    if !cfg!(windows) && !dispatch.exists() {
        println!(
            "cargo::warning=DeckLinkAPIDispatch.cpp missing from {} — \
             building without SDI support.",
            include.display()
        );
        return;
    }

    let mut build = cc::Build::new();
    build
        .cpp(true)
        .std("c++17")
        .include(&include)
        .include("shim")
        .file("shim/kestrel_decklink.cpp")
        .warnings(false);

    if !cfg!(windows) {
        build.file(&dispatch);
    }

    build.compile("kestrel_decklink");

    if cfg!(target_os = "macos") {
        println!("cargo::rustc-link-lib=framework=CoreFoundation");
    } else if cfg!(target_os = "linux") {
        println!("cargo::rustc-link-lib=dl");
    }

    println!("cargo::rustc-cfg=decklink");
    println!(
        "cargo::warning=DeckLink SDK found at {} — SDI support compiled in.",
        include.display()
    );
}

/// The oldest SDK whose headers this shim compiles against.
///
/// `IDeckLinkProfileAttributes` and `BMDDeckLinkDuplex` arrived in 11.0, and
/// both are load-bearing: the first is how a persistent id is read, the second
/// is how a sub-device that the card's profile has switched off is told apart
/// from one that is merely busy. There is no useful subset of this shim that
/// works without them, so an older SDK is refused with a reason rather than
/// producing a page of "undeclared identifier".
const MIN_SDK: (u32, u32) = (11, 0);

/// Reads `BLACKMAGIC_DECKLINK_API_VERSION_STRING` out of an SDK include dir.
fn sdk_version(include: &Path) -> Option<(u32, u32)> {
    let text = std::fs::read_to_string(include.join("DeckLinkAPIVersion.h")).ok()?;
    let line = text
        .lines()
        .find(|l| l.contains("BLACKMAGIC_DECKLINK_API_VERSION_STRING"))?;
    let quoted = line.split('"').nth(1)?;
    let mut parts = quoted.split('.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next().unwrap_or("0").parse().unwrap_or(0);
    Some((major, minor))
}

/// The directory containing `DeckLinkAPI.h`.
///
/// Every candidate is checked and the **newest** wins, rather than the first
/// hit. This machine has two: an SDK 10.11 header set inside the NDI SDK's
/// examples and a 12.2 one inside Unreal's BlackmagicMedia. First-hit order
/// picked the 10.11 copy and the build failed on symbols that have existed
/// since 11.0, which reads like a broken shim rather than an old header set.
fn find_sdk() -> Option<PathBuf> {
    let mut roots: Vec<PathBuf> = Vec::new();
    if let Ok(dir) = std::env::var("DECKLINK_SDK_DIR") {
        roots.push(PathBuf::from(dir));
    }
    // Two copies that exist on a typical macOS edit-suite machine without the
    // SDK archive ever having been downloaded. Both are real SDK header sets
    // shipped inside another vendor's product; using one is a convenience for
    // a local build, never a substitute for pointing at your own copy.
    roots.push(PathBuf::from(
        "/Library/NDI SDK for Apple/examples/C++/NDIlib_Send_BMD/BMDSDK",
    ));
    for ue in ["UE_5.7", "UE_5.6", "UE_5.5"] {
        roots.push(PathBuf::from(format!(
            "/Users/Shared/Epic Games/{ue}/Engine/Plugins/Media/BlackmagicMedia/\
             Source/ThirdParty/BlackmagicLib/Include"
        )));
    }

    // Accepted layouts, in the order the official archive and the
    // redistributions use them.
    let suffixes = [
        "Mac/include",
        "Linux/include",
        "Win/include",
        "include",
        "Blackmagic DeckLink SDK/Mac/include",
        // Some redistributions flatten to <dir>/<Platform>/ with the headers
        // directly inside — Unreal's copy, for one.
        "Mac",
        "Linux",
        "Win",
        "",
    ];

    let mut best: Option<((u32, u32), PathBuf)> = None;
    let mut rejected: Vec<String> = Vec::new();

    for root in roots {
        for suffix in suffixes {
            let dir = if suffix.is_empty() {
                root.clone()
            } else {
                root.join(suffix)
            };
            if !dir.join("DeckLinkAPI.h").exists() {
                continue;
            }
            // An SDK with no version header is assumed ancient rather than
            // assumed fine: guessing high here trades a clear message for a
            // wall of compiler errors.
            let version = sdk_version(&dir).unwrap_or((0, 0));
            if version < MIN_SDK {
                rejected.push(format!("{} (SDK {}.{})", dir.display(), version.0, version.1));
                continue;
            }
            if best.as_ref().is_none_or(|(v, _)| version > *v) {
                best = Some((version, canonical(&dir)));
            }
        }
    }

    for r in &rejected {
        println!(
            "cargo::warning=ignoring DeckLink headers at {r}: Kestrel needs \
             SDK {}.{} or newer.",
            MIN_SDK.0, MIN_SDK.1
        );
    }

    best.map(|(v, dir)| {
        println!("cargo::warning=using DeckLink SDK {}.{}", v.0, v.1);
        dir
    })
}

fn canonical(p: &Path) -> PathBuf {
    p.canonicalize().unwrap_or_else(|_| p.to_path_buf())
}

//! `kestrel-gui` — the operator window.

mod app;
mod matrix;
mod preview;
mod strip;
mod theme;

use anyhow::Result;
use kestrel_app::{load_or_default, Config, InputSource, Runtime};
use kestrel_control::Shared;
use kestrel_core::Size;
use kestrel_decklink as dl;
use std::net::SocketAddr;
use std::path::PathBuf;

fn main() -> Result<()> {
    let _guard = diag::init(
        diag::Options::new("kestrel-gui", "KESTREL", env!("CARGO_PKG_VERSION"))
            .with_default_filter("info,wgpu_core=warn,wgpu_hal=warn,naga=warn"),
    )
    .ok();

    // Before anything else. macOS App Nap demotes this whole process the
    // moment the window stops being frontmost — which is most of a show — and
    // that costs the frame path most of its frame rate. See `platform.rs`.
    kestrel_app::keep_awake();

    let args: Vec<String> = std::env::args().collect();
    let show_path = flag(&args, "--show").map(PathBuf::from);
    let http: SocketAddr = flag(&args, "--http")
        .as_deref()
        .unwrap_or("127.0.0.1:9720")
        .parse()?;
    let input_id: Option<i64> = flag(&args, "--input").and_then(|s| s.parse().ok());

    let show = load_or_default(show_path.as_deref(), input_id)?;
    let shared = Shared::new(show);

    // Pick the input: the named card, else the first active DeckLink that can
    // capture, else generated bars. Falling back rather than refusing is what
    // makes the window openable — and the whole routing model demonstrable — on
    // a machine with nothing plugged in.
    let input = choose_input(input_id);
    tracing::info!(input = %input.label(), "input source");

    let runtime = Runtime::start(
        shared.clone(),
        Config {
            input,
            ..Config::default()
        },
    )?;

    // The control server runs on its own tokio runtime in the background, so a
    // Companion module can drive the same show the window is showing.
    let control = shared.clone();
    std::thread::Builder::new()
        .name("kestrel-control".into())
        .spawn(move || {
            let rt = match tokio::runtime::Runtime::new() {
                Ok(rt) => rt,
                Err(e) => {
                    tracing::error!(error = %e, "no tokio runtime; control API disabled");
                    return;
                }
            };
            rt.block_on(async move {
                match kestrel_control::serve(control, http).await {
                    Ok((_, handle)) => {
                        handle.await.ok();
                    }
                    Err(e) => tracing::error!(error = %e, "control API not listening"),
                }
            });
        })?;

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Kestrel")
            .with_inner_size([1440.0, 900.0])
            .with_min_inner_size([980.0, 640.0]),
        ..Default::default()
    };

    eframe::run_native(
        "Kestrel",
        options,
        Box::new(move |cc| {
            Ok(Box::new(app::App::new(
                cc,
                shared,
                runtime,
                show_path,
                http.to_string(),
            )))
        }),
    )
    .map_err(|e| anyhow::anyhow!("could not open the window: {e}"))
}

fn choose_input(requested: Option<i64>) -> InputSource {
    let devices = dl::list_devices().unwrap_or_default();
    let chosen = match requested {
        Some(id) => devices.iter().find(|d| d.persistent_id == id),
        None => devices.iter().find(|d| d.active && d.has_input),
    };
    match chosen {
        Some(d) => InputSource::DeckLink {
            persistent_id: d.persistent_id,
            name: d.name.clone(),
        },
        None => InputSource::Synthetic {
            size: Size::new(1920, 1080),
        },
    }
}

fn flag(args: &[String], name: &str) -> Option<String> {
    let i = args.iter().position(|a| a == name)?;
    args.get(i + 1).cloned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flags_are_read_as_name_then_value() {
        let args: Vec<String> = ["kestrel-gui", "--show", "a.json", "--http", "0.0.0.0:1"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(flag(&args, "--show").as_deref(), Some("a.json"));
        assert_eq!(flag(&args, "--http").as_deref(), Some("0.0.0.0:1"));
        assert_eq!(flag(&args, "--input"), None);
    }

    #[test]
    fn a_trailing_flag_with_no_value_is_none_rather_than_a_panic() {
        let args: Vec<String> = ["kestrel-gui", "--show"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(flag(&args, "--show"), None);
    }
}

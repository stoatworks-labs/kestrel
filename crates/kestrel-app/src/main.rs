//! `kestrel` — the headless runner and the diagnostics.
//!
//! The GUI is a separate binary (`kestrel-gui`). This one exists so the frame
//! path can be run, timed and pointed at a control surface with no window at
//! all — which is how it belongs on a rack machine, and how it gets tested.

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use kestrel_app::{load_or_default, runtime, save, Config, InputSource, Runtime};
use kestrel_control::Shared;
use kestrel_core::Size;
use kestrel_decklink as dl;
use std::net::SocketAddr;
use std::path::PathBuf;

#[derive(Parser)]
#[command(
    name = "kestrel",
    version,
    about = "Regions of interest from one wide shot, out of a DeckLink."
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// List DeckLink sub-devices, and say why any are unusable.
    Devices,
    /// Run the frame path with no window.
    Run(RunArgs),
    /// Write a starting show file for this machine and exit.
    Init {
        #[arg(default_value = "kestrel.show.json")]
        path: PathBuf,
    },
}

#[derive(Parser)]
struct RunArgs {
    /// Show file. Created from the machine's own devices if absent.
    #[arg(long)]
    show: Option<PathBuf>,
    /// Where the control API listens.
    #[arg(long, default_value = "127.0.0.1:9720")]
    http: SocketAddr,
    /// Capture from this DeckLink persistent id. Without it, generated bars.
    #[arg(long)]
    input: Option<i64>,
    /// Raster for the generated input, when there is no card.
    #[arg(long, default_value = "1920x1080")]
    synthetic_size: String,
}

fn main() -> Result<()> {
    // The guard must outlive `run`, or the log file is never written. Failing
    // to start logging is not a reason to refuse to run: the real output goes
    // to stdout regardless.
    let _guard = diag::init(
        diag::Options::new("kestrel", "KESTREL", env!("CARGO_PKG_VERSION"))
            .with_default_filter("warn,kestrel=info,kestrel_app=info"),
    )
    .ok();
    kestrel_app::keep_awake();
    let cli = Cli::parse();
    match cli.command.unwrap_or(Command::Run(RunArgs {
        show: None,
        http: "127.0.0.1:9720".parse().unwrap(),
        input: None,
        synthetic_size: "1920x1080".into(),
    })) {
        Command::Devices => devices(),
        Command::Init { path } => {
            let show = kestrel_app::default_show(None);
            save(&show, &path)?;
            println!("wrote {}", path.display());
            println!("{}", dl::status_line());
            Ok(())
        }
        Command::Run(args) => run(args),
    }
}

fn devices() -> Result<()> {
    println!("{}", dl::status_line());
    let devices = match dl::list_devices() {
        Ok(d) => d,
        Err(e) => {
            println!("{e}");
            return Ok(());
        }
    };
    if devices.is_empty() {
        return Ok(());
    }
    println!();
    println!(
        "{:<20}  {:>18}  {:>3}  {:<6}  {:<7}  state",
        "name", "persistent id", "sub", "in/out", "duplex"
    );
    for d in &devices {
        println!(
            "{:<20}  {:>18}  {:>3}  {:<6}  {:<7}  {}",
            d.name,
            d.persistent_id,
            d.sub_device,
            match (d.has_input, d.has_output) {
                (true, true) => "both",
                (true, false) => "in",
                (false, true) => "out",
                _ => "-",
            },
            if d.full_duplex { "full" } else { "half" },
            if d.active {
                "active"
            } else {
                "INACTIVE in this card's profile"
            }
        );
    }
    if devices.iter().any(|d| !d.active) {
        println!();
        println!(
            "An INACTIVE sub-device is not a broken one: the card's profile has \
             switched it off, and it offers no display modes at all until the \
             profile changes. Kestrel wants one input and several outputs at \
             once, which on a Duo 2 means the four-sub-device half-duplex \
             profile. Change it in Blackmagic Desktop Video Setup."
        );
    }
    Ok(())
}

fn run(args: RunArgs) -> Result<()> {
    let show = load_or_default(args.show.as_deref(), args.input)?;
    let shared = Shared::new(show);

    let input = match args.input {
        Some(id) => {
            let name = dl::list_devices()
                .unwrap_or_default()
                .into_iter()
                .find(|d| d.persistent_id == id)
                .map(|d| d.name)
                .unwrap_or_else(|| format!("DeckLink {id}"));
            InputSource::DeckLink {
                persistent_id: id,
                name,
            }
        }
        None => InputSource::Synthetic {
            size: parse_size(&args.synthetic_size)?,
        },
    };

    println!("{}", dl::status_line());
    println!("input: {}", input.label());

    let _runtime = Runtime::start(
        shared.clone(),
        Config {
            input,
            ..runtime::Config::default()
        },
    )?;

    // The control server owns the main thread; the frame path is already on its
    // own. Ctrl-C is the only way out, which is correct for a rack process.
    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(async move {
        let (bound, handle) = kestrel_control::serve(shared, args.http).await?;
        println!("control API on http://{bound}  (WebSocket at /ws)");
        handle.await.ok();
        Ok::<_, anyhow::Error>(())
    })
}

fn parse_size(s: &str) -> Result<Size> {
    let (w, h) = s
        .split_once(['x', 'X'])
        .with_context(|| format!("expected WIDTHxHEIGHT, got {s:?}"))?;
    Ok(Size::new(
        w.trim().parse().context("width")?,
        h.trim().parse().context("height")?,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes_parse_the_way_people_type_them() {
        assert_eq!(parse_size("1920x1080").unwrap(), Size::new(1920, 1080));
        assert_eq!(parse_size("1280X720").unwrap(), Size::new(1280, 720));
        assert!(parse_size("1920").is_err());
        assert!(parse_size("axb").is_err());
    }
}

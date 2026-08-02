//! Loading and saving a show, and what a first run looks like.

use anyhow::{Context, Result};
use kestrel_core::{DeviceRef, NormRect, Show};
use kestrel_decklink as dl;
use std::path::Path;

/// A show for a machine we have just looked at.
///
/// Outputs are created for every *active* DeckLink sub-device that can play
/// out, minus the one chosen as the input; and if there is no card at all, four
/// unassigned outputs, because the routing, the matrix and the whole UI must be
/// usable and demonstrable with nothing plugged in.
pub fn default_show(input_device: Option<i64>) -> Show {
    let mut show = Show::new();

    let devices = dl::list_devices().unwrap_or_default();
    let usable: Vec<&dl::Device> = devices
        .iter()
        .filter(|d| d.active && d.has_output && Some(d.persistent_id) != input_device)
        .collect();

    if usable.is_empty() {
        for i in 0..4 {
            show.add_output(format!("Output {}", i + 1));
        }
    } else {
        for d in usable {
            let id = show.add_output(d.name.clone());
            if let Some(o) = show.output_mut(id) {
                o.device = Some(DeviceRef {
                    persistent_id: d.persistent_id,
                    display_name: d.name.clone(),
                });
            }
        }
    }

    // One region to start from, so a first run has something to drag rather
    // than an empty picture and no clue what to do.
    show.add_roi("Region 1", NormRect::new(0.3, 0.3, 0.4, 0.4));
    show.reapply_aspect_locks();
    show
}

pub fn load(path: &Path) -> Result<Show> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("could not read {}", path.display()))?;
    Show::from_json(&text).with_context(|| format!("{} is not a valid show file", path.display()))
}

/// Load a show, or build a default one if the path is absent or missing.
///
/// A missing file is *not* an error — it is a first run. A malformed one is,
/// and says so rather than silently starting empty and losing the operator's
/// regions the next time it saves.
pub fn load_or_default(path: Option<&Path>, input_device: Option<i64>) -> Result<Show> {
    match path {
        Some(p) if p.exists() => load(p),
        _ => Ok(default_show(input_device)),
    }
}

pub fn save(show: &Show, path: &Path) -> Result<()> {
    if let Some(dir) = path.parent() {
        if !dir.as_os_str().is_empty() {
            std::fs::create_dir_all(dir).ok();
        }
    }
    let json = show.to_json()?;
    // Write-then-rename: a crash mid-save must not leave a truncated show file
    // where a working one used to be.
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, json).with_context(|| format!("could not write {}", tmp.display()))?;
    std::fs::rename(&tmp, path)
        .with_context(|| format!("could not replace {}", path.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_machine_with_no_card_still_gets_a_usable_show() {
        let show = default_show(None);
        assert!(!show.outputs.is_empty(), "the UI needs outputs to show");
        assert_eq!(show.rois.len(), 1, "and something to drag");
        assert_eq!(show.plan(false).len(), show.outputs.len());
    }

    #[test]
    fn a_missing_file_is_a_first_run_not_a_failure() {
        let p = std::path::PathBuf::from("/nonexistent/kestrel/show.json");
        let show = load_or_default(Some(&p), None).expect("a missing show file must not fail");
        assert!(!show.outputs.is_empty());
    }

    #[test]
    fn a_malformed_file_is_reported_rather_than_silently_replaced() {
        let dir = std::env::temp_dir().join("kestrel-test-showfile");
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("broken.json");
        std::fs::write(&p, "{ not json").unwrap();
        let err = load_or_default(Some(&p), None).expect_err("must not silently start empty");
        assert!(err.to_string().contains("not a valid show file"), "{err}");
        std::fs::remove_file(&p).ok();
    }

    #[test]
    fn a_show_survives_a_save_and_load() {
        let dir = std::env::temp_dir().join("kestrel-test-showfile");
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("round.json");

        let mut show = default_show(None);
        let roi = show.rois[0].id;
        let out = show.outputs[0].id;
        show.route(out, Some(roi)).unwrap();
        save(&show, &p).unwrap();

        let back = load(&p).unwrap();
        assert_eq!(back, show);
        assert_eq!(back.output(out).unwrap().assigned, Some(roi));
        std::fs::remove_file(&p).ok();
    }
}

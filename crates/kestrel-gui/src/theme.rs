//! Colours and small shared bits of style.
//!
//! Dark, low-contrast chrome with saturated accents only where something is
//! *on air* — this window sits next to a lit stage and a wall of screens, and
//! anything bright that is not carrying information is glare.

use egui::Color32;
use kestrel_core::ScaleQuality;

pub const BG: Color32 = Color32::from_rgb(18, 19, 22);
pub const PANEL: Color32 = Color32::from_rgb(26, 28, 33);
pub const LINE: Color32 = Color32::from_rgb(48, 52, 60);
pub const TEXT_DIM: Color32 = Color32::from_rgb(140, 148, 160);

/// On air. The one saturated colour in the window.
pub const LIVE: Color32 = Color32::from_rgb(224, 58, 58);
pub const OK: Color32 = Color32::from_rgb(90, 200, 130);
pub const WARN: Color32 = Color32::from_rgb(240, 176, 64);

/// The scale badge's colour.
///
/// This is the most useful number on the screen, so it is coloured by what it
/// *means* rather than left as text: green while every output pixel is backed
/// by a real source pixel, amber once detail is being invented, red past 2x
/// where it shows on a big screen.
pub fn scale_colour(q: ScaleQuality) -> Color32 {
    match q {
        ScaleQuality::Native => OK,
        ScaleQuality::Soft => WARN,
        ScaleQuality::Heavy => LIVE,
    }
}

pub fn roi_colour(rgb: [u8; 3]) -> Color32 {
    Color32::from_rgb(rgb[0], rgb[1], rgb[2])
}

/// Applied to **both** themes, and the viewport is pinned dark.
///
/// egui 0.35 keeps a style per theme and follows the OS. Setting only the dark
/// one leaves a machine in light mode showing default egui chrome — which is
/// not a cosmetic problem here: the tally colours are chosen against a dark
/// background, and on a light one an "on air" ring stops reading as one.
pub fn apply(ctx: &egui::Context) {
    ctx.set_theme(egui::Theme::Dark);
    ctx.all_styles_mut(|style| {
        style.visuals = egui::Visuals::dark();
        style.visuals.panel_fill = BG;
        style.visuals.window_fill = PANEL;
        style.visuals.extreme_bg_color = Color32::from_rgb(12, 13, 15);
        style.visuals.widgets.noninteractive.bg_stroke = egui::Stroke::new(1.0, LINE);
        style.visuals.widgets.inactive.bg_fill = PANEL;
        style.visuals.widgets.hovered.bg_fill = Color32::from_rgb(38, 42, 50);
        style.visuals.widgets.active.bg_fill = Color32::from_rgb(52, 58, 68);
        style.spacing.item_spacing = egui::vec2(8.0, 6.0);
        style.spacing.button_padding = egui::vec2(10.0, 6.0);
    });
}

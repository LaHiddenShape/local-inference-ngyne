//! Colors of the app, named by what they mean, and the egui visuals built
//! from them. The background scene (pyramids) keeps its own palette.

use eframe::egui::{self, Color32, Stroke};

/// Gold accent: the bar along replies, selection.
pub(crate) const ACCENT: Color32 = Color32::from_rgb(205, 165, 70);
/// Brighter gold for small marks on dark backgrounds: pinned star, links,
/// loading light, text cursor.
pub(crate) const ACCENT_BRIGHT: Color32 = Color32::from_rgb(225, 185, 85);
/// Gold readable on the light bubbles and bars.
pub(crate) const ACCENT_ON_LIGHT: Color32 = Color32::from_rgb(150, 110, 20);
pub(crate) const SUCCESS: Color32 = Color32::from_rgb(90, 200, 120);
pub(crate) const WARNING: Color32 = Color32::from_rgb(220, 180, 60);
pub(crate) const DANGER: Color32 = Color32::from_rgb(220, 90, 90);

/// `ACCENT_BRIGHT` with an opacity in 0..=1.
pub(crate) fn accent_bright(alpha: f32) -> Color32 {
    let [r, g, b, _] = ACCENT_BRIGHT.to_array();
    Color32::from_rgba_unmultiplied(r, g, b, (alpha.clamp(0.0, 1.0) * 255.0) as u8)
}

/// Dark theme with square corners everywhere and gold instead of egui's blue
/// for selection, links and the text cursor.
pub(crate) fn visuals() -> egui::Visuals {
    let mut v = egui::Visuals::dark();
    v.window_corner_radius = egui::CornerRadius::ZERO;
    v.menu_corner_radius = egui::CornerRadius::ZERO;
    for w in [
        &mut v.widgets.noninteractive,
        &mut v.widgets.inactive,
        &mut v.widgets.hovered,
        &mut v.widgets.active,
        &mut v.widgets.open,
    ] {
        w.corner_radius = egui::CornerRadius::ZERO;
    }
    // selected items and text: dark gold behind warm white
    v.selection.bg_fill = Color32::from_rgb(112, 86, 34);
    v.selection.stroke = Stroke::new(1.0, Color32::from_rgb(250, 236, 204));
    v.hyperlink_color = ACCENT_BRIGHT;
    v.text_cursor.stroke = Stroke::new(2.0, ACCENT_BRIGHT);
    v.warn_fg_color = WARNING;
    v.error_fg_color = DANGER;
    v
}

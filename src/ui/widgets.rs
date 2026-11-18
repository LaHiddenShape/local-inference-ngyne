
use eframe::egui::{self, Color32, Pos2, Rect, Vec2};

use crate::ui::theme;

pub(crate) const OLLAMA_DOWNLOAD_URL: &str = "https://ollama.com/download";

/// Small rectangle with a band of light sweeping across it (loading indicator).
pub(crate) fn shimmer_bar(ui: &mut egui::Ui, t: f32) {
    let (r, _) = ui.allocate_exact_size(Vec2::new(150.0, 6.0), egui::Sense::hover());
    let p = ui.painter();
    p.rect_filled(r, 0.0, Color32::from_gray(34));
    let band = 50.0;
    let x = r.left() - band + ((t * 0.9).fract()) * (r.width() + band * 2.0);
    let slices = 25;
    for k in 0..slices {
        let sx = x + k as f32 * band / slices as f32;
        let a = 1.0 - ((k as f32 / slices as f32) * 2.0 - 1.0).abs(); // bright in the middle
        let slice = Rect::from_min_max(Pos2::new(sx, r.top()), Pos2::new(sx + band / slices as f32 + 0.5, r.bottom()))
            .intersect(r);
        if slice.width() > 0.0 {
            p.rect_filled(slice, 0.0, theme::accent_bright(a * a));
        }
    }
}

/// Font family for large, thin headings (Inter Light).
pub(crate) fn light_family() -> egui::FontFamily {
    egui::FontFamily::Name("light".into())
}

/// Inter (SIL OFL, see assets/fonts/OFL.txt) for UI text, with egui's default
/// fonts kept as fallback for symbols and non-Latin scripts.
pub(crate) fn install_fonts(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();
    fonts.font_data.insert(
        "inter".into(),
        std::sync::Arc::new(egui::FontData::from_static(include_bytes!("../../assets/fonts/Inter-400.ttf"))),
    );
    fonts.font_data.insert(
        "inter-light".into(),
        std::sync::Arc::new(egui::FontData::from_static(include_bytes!("../../assets/fonts/Inter-300.ttf"))),
    );
    let proportional = fonts.families.entry(egui::FontFamily::Proportional).or_default();
    proportional.insert(0, "inter".into());
    let mut light = vec!["inter-light".to_string()];
    light.extend(proportional.iter().cloned());
    fonts.families.insert(light_family(), light);
    ctx.set_fonts(fonts);
}

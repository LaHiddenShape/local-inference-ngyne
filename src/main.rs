mod app;
mod attach;
mod background;
mod backend;
mod discover;
mod store;
mod system;
mod ui;

use std::sync::Arc;

use eframe::egui;

/// Application id: names the data directory and the HTTP user agent.
pub const APP_ID: &str = "local-inference-ngyne";

fn main() -> eframe::Result {
    store::migrate_legacy_data();
    let mut viewport = egui::ViewportBuilder::default()
        .with_inner_size([1100.0, 760.0])
        // below this the top bar, the chat column and the history panel overlap
        .with_min_inner_size([900.0, 560.0])
        .with_title("lin")
        // Wayland app id / X11 class: lets the desktop match the .desktop entry
        .with_app_id(APP_ID);
    if let Ok(icon) = eframe::icon_data::from_png_bytes(include_bytes!("../assets/icon.png")) {
        viewport = viewport.with_icon(Arc::new(icon));
    }
    let options = eframe::NativeOptions { viewport, ..Default::default() };
    eframe::run_native(
        APP_ID,
        options,
        Box::new(|cc| Ok(Box::new(app::App::new(cc.egui_ctx.clone())))),
    )
}

#[cfg(test)]
mod tests {
    #[test]
    fn bundled_icon_decodes() {
        let icon = eframe::icon_data::from_png_bytes(include_bytes!("../assets/icon.png")).unwrap();
        assert_eq!((icon.width, icon.height), (256, 256));
    }
}

mod app;
mod attach;
mod background;
mod backend;
mod discover;
mod store;
mod system;
mod ui;

use eframe::egui;

/// Application id: names the data directory and the HTTP user agent.
pub const APP_ID: &str = "local-inference-ngyne";

fn main() -> eframe::Result {
    store::migrate_legacy_data();
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1100.0, 760.0])
            .with_title("Local Inference"),
        ..Default::default()
    };
    eframe::run_native(
        APP_ID,
        options,
        Box::new(|cc| Ok(Box::new(app::App::new(cc.egui_ctx.clone())))),
    )
}

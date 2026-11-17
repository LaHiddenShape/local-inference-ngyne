mod app;
mod background;
mod backend;
mod catalog;
mod store;
mod system;
mod ui;

use eframe::egui;

fn main() -> eframe::Result {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1100.0, 760.0])
            .with_title("Local Inference"),
        ..Default::default()
    };
    eframe::run_native(
        "local-inference-ui",
        options,
        Box::new(|cc| Ok(Box::new(app::App::new(cc.egui_ctx.clone())))),
    )
}

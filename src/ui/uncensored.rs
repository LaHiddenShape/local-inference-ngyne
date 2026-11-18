use eframe::egui::{self, Color32, RichText};

use crate::app::App;
use crate::discover::ListKind;

impl App {
    pub(crate) fn uncensored_window(&mut self, ctx: &egui::Context) {
        self.browser_opened(ListKind::Uncensored, self.show_uncensored);
        let mut open = self.show_uncensored;
        egui::Window::new("Uncensored models")
            .open(&mut open)
            .default_size([720.0, 600.0])
            .collapsible(false)
            .show(ctx, |ui| {
                self.runtime_notice(ui);
                self.system_summary(ui);
                ui.separator();
                let shown = self.browser_header(ui, ListKind::Uncensored);
                ui.label(
                    RichText::new(
                        "Weights only (no executable code), downloaded by Ollama and checked by SHA-256. \
                         Community models are published by anyone and not reviewed by Ollama. \
                         Uncensored output can be wrong, offensive or harmful.",
                    )
                    .small()
                    .color(Color32::from_rgb(220, 180, 60)),
                );
                ui.separator();
                egui::ScrollArea::vertical()
                    .auto_shrink(false)
                    .show(ui, |ui| self.browser_list(ui, ListKind::Uncensored, &shown));
            });
        self.show_uncensored = open;
    }
}

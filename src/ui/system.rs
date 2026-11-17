
use eframe::egui::{self, Color32, FontId, Pos2, Rect, RichText, Vec2};

use crate::app::App;
use crate::system::{fmt_bytes, SystemInfo};

impl App {
    // ----------------------------------------------------------------- system

    pub(crate) fn system_window(&mut self, ctx: &egui::Context) {
        let mut open = self.show_system;
        egui::Window::new("System")
            .open(&mut open)
            .default_size([460.0, 320.0])
            .collapsible(false)
            .show(ctx, |ui| {
                let s = self.sys.clone();
                egui::Grid::new("sys_grid").num_columns(2).spacing([16.0, 6.0]).show(ui, |ui| {
                    let row = |ui: &mut egui::Ui, k: &str, v: String| {
                        ui.label(RichText::new(k).color(Color32::GRAY));
                        ui.label(v);
                        ui.end_row();
                    };
                    row(ui, "OS", s.os.clone());
                    row(ui, "Kernel", s.kernel.clone());
                    row(ui, "CPU", format!("{} ({} threads)", s.cpu, s.threads));
                    row(ui, "RAM", format!("{} total · {} available", fmt_bytes(s.ram_total), fmt_bytes(s.ram_available)));
                    if s.gpus.is_empty() {
                        row(ui, "GPU", "none detected".into());
                    }
                    for (i, g) in s.gpus.iter().enumerate() {
                        let vram = g.vram.map(fmt_bytes).unwrap_or_else(|| "shared memory".into());
                        row(ui, &format!("GPU {i} ({})", g.vendor), format!("{} · {}", g.name, vram));
                    }
                    row(ui, "Runtime", self.backend.endpoint());
                    row(ui, "Models dir", s.models_dir.display().to_string());
                    row(ui, "Disk", format!("{} free / {}", fmt_bytes(s.disk_free), fmt_bytes(s.disk_total)));
                });
                ui.add_space(8.0);
                ui.label(
                    RichText::new(
                        "A model is recommended when it fits in GPU memory, \
                         usable (slower) when it fits in RAM, and not recommended otherwise.",
                    )
                    .small()
                    .color(Color32::GRAY),
                );
                if ui.button("Refresh").clicked() {
                    self.sys = SystemInfo::probe();
                }
            });
        self.show_system = open;
    }

    pub(crate) fn toast(&mut self, ui: &mut egui::Ui, rect: Rect, now: f64) {
        let Some((msg, start)) = &mut self.toast else { return };
        if *start == 0.0 {
            *start = now;
        }
        if now - *start > 1.8 {
            self.toast = None;
            return;
        }
        let pos = Pos2::new(rect.center().x, rect.bottom() - 20.0);
        let galley = ui.painter().layout_no_wrap(msg.clone(), FontId::proportional(13.0), Color32::from_gray(15));
        let bg = Rect::from_center_size(pos, galley.size() + Vec2::new(20.0, 10.0));
        ui.painter().rect_filled(bg, 0.0, Color32::from_gray(225));
        ui.painter().galley(bg.min + Vec2::new(10.0, 5.0), galley, Color32::from_gray(15));
    }
}

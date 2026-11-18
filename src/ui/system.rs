
use eframe::egui::{self, Color32, FontId, Pos2, Rect, RichText, Vec2};

use crate::app::App;
use crate::store;
use crate::system::fmt_bytes;

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
                    self.refresh_system();
                }
                ui.separator();
                ui.label(RichText::new("Preferences").strong());
                if ui
                    .checkbox(&mut self.settings.animations, "Animated background")
                    .on_hover_text("Pyramid glyphs and the falcon. Off: a still background, less CPU and GPU when idle.")
                    .changed()
                {
                    let _ = store::save_settings(&self.settings);
                }
                ui.horizontal(|ui| {
                    ui.label("Context window");
                    let label = |c: Option<u64>| c.map(|n| format!("up to {}K tokens", n / 1024)).unwrap_or_else(|| "model maximum".into());
                    let before = self.settings.max_context;
                    egui::ComboBox::from_id_salt("max_context")
                        .selected_text(label(before))
                        .show_ui(ui, |ui| {
                            for c in [Some(4096), Some(8192), Some(16384), Some(32768), Some(65536), Some(131072), None] {
                                ui.selectable_value(&mut self.settings.max_context, c, label(c));
                            }
                        })
                        .response
                        .on_hover_text(
                            "How much of the conversation the model can see. A larger window remembers more \
                             but needs more memory (KV cache) and can make replies slower. \
                             The model is reloaded with the new size on the next message.",
                        );
                    if self.settings.max_context != before {
                        let _ = store::save_settings(&self.settings);
                    }
                });
            });
        self.show_system = open;
    }

    /// The current notice, stacked from `anchor` (see `notice_anchor`).
    pub(crate) fn toast(&mut self, ui: &mut egui::Ui, (anchor, dir): (Pos2, f32), now: f64) {
        let Some((msg, start)) = &mut self.toast else { return };
        if *start == 0.0 {
            *start = now;
        }
        let life = toast_seconds(msg);
        if now - *start > life {
            self.toast = None;
            return;
        }
        ui.ctx().request_repaint_after(std::time::Duration::from_secs_f64(life - (now - *start))); // to hide it
        let galley = ui.painter().layout(msg.clone(), FontId::proportional(13.0), Color32::from_gray(15), 560.0);
        let bg = notice_rect(anchor, dir, galley.size() + Vec2::new(20.0, 10.0));
        ui.painter().rect_filled(bg, 0.0, Color32::from_gray(225));
        ui.painter().galley(bg.min + Vec2::new(10.0, 5.0), galley, Color32::from_gray(15));
    }
}

/// How long a notice stays: long ones (errors, warnings) get time to be read.
fn toast_seconds(msg: &str) -> f64 {
    (2.5 + 0.06 * msg.chars().count() as f64).min(8.0)
}

/// A notice of `size`, centered on `anchor` and stacked upwards (`dir` < 0)
/// or downwards from it.
pub(crate) fn notice_rect(anchor: Pos2, dir: f32, size: Vec2) -> Rect {
    let top = if dir < 0.0 { anchor.y - size.y } else { anchor.y };
    Rect::from_min_size(Pos2::new(anchor.x - size.x / 2.0, top), size)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn notices_stack_away_from_the_anchor_and_last_by_length() {
        let up = notice_rect(Pos2::new(100.0, 500.0), -1.0, Vec2::new(40.0, 20.0));
        assert_eq!((up.left(), up.top(), up.bottom()), (80.0, 480.0, 500.0));
        let down = notice_rect(Pos2::new(100.0, 500.0), 1.0, Vec2::new(40.0, 20.0));
        assert_eq!((down.top(), down.bottom()), (500.0, 520.0));
        assert!(toast_seconds("Reply copied") < 4.0);
        assert_eq!(toast_seconds(&"x".repeat(300)), 8.0);
    }
}

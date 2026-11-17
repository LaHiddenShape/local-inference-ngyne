use std::sync::atomic::Ordering;

use eframe::egui::{self, Color32, RichText};

use crate::app::App;
use crate::backend::{InstalledModel, RuntimeStatus};
use crate::discover::{model_url, ListKind};
use crate::system::{fmt_bytes, Fit, SystemInfo};
use crate::ui::widgets::*;

impl App {
    // ---------------------------------------------------------- model manager

    pub(crate) fn models_window(&mut self, ctx: &egui::Context) {
        // refresh the installed list each time the window is opened
        if self.show_models && !self.models_was_open && matches!(self.status, RuntimeStatus::Online { .. }) {
            self.backend.refresh_models();
        }
        self.models_was_open = self.show_models;
        self.browser_opened(ListKind::Library, self.show_models);
        let mut open = self.show_models;
        egui::Window::new("Models")
            .open(&mut open)
            .default_size([680.0, 560.0])
            .collapsible(false)
            .show(ctx, |ui| {
                self.runtime_notice(ui);
                self.system_summary(ui);
                ui.separator();

                ui.horizontal(|ui| {
                    ui.label("Pull any model:");
                    ui.add(egui::TextEdit::singleline(&mut self.custom_pull).hint_text("e.g. qwen2.5-coder:7b").desired_width(200.0));
                    let tag = self.custom_pull.trim().to_string();
                    if ui.add_enabled(!tag.is_empty(), egui::Button::new("Download")).clicked() {
                        self.start_pull(&tag);
                        self.custom_pull.clear();
                    }
                    if !tag.is_empty() {
                        let url = model_url(&tag);
                        ui.hyperlink_to(RichText::new(&url).small(), &url);
                        self.copy_button(ui, &url);
                    }
                });
                ui.separator();

                egui::ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.heading("Installed");
                        if ui.small_button("⟳ Refresh").on_hover_text("Reload the list from Ollama").clicked() {
                            self.backend.refresh_models();
                            self.refresh_system();
                        }
                    });
                    if self.installed.is_empty() {
                        ui.label(RichText::new("No model installed yet.").color(Color32::GRAY));
                    }
                    for m in self.installed.clone() {
                        self.installed_row(ui, &m);
                    }
                    // custom pulls not shown in a list
                    let extra: Vec<String> = self
                        .pulls
                        .keys()
                        .chain(self.errors.keys())
                        .filter(|k| !self.listed(k))
                        .filter(|k| !self.installed.iter().any(|m| &m.name == *k))
                        .cloned()
                        .collect();
                    for tag in extra {
                        ui.horizontal(|ui| {
                            ui.label(RichText::new(&tag).strong());
                            self.pull_status(ui, &tag);
                        });
                    }

                    ui.add_space(12.0);
                    ui.heading("Library");
                    ui.label(
                        RichText::new("Every official model on ollama.com/library. Fit badges are based on your hardware (see System).")
                            .small()
                            .color(Color32::GRAY),
                    );
                    ui.add_space(4.0);
                    self.browser_header(ui, ListKind::Library);
                    ui.add_space(4.0);
                    self.browser_list(ui, ListKind::Library);
                });
            });
        self.show_models = open;
    }

    pub(crate) fn runtime_notice(&mut self, ui: &mut egui::Ui) {
        match self.status.clone() {
            RuntimeStatus::NotInstalled => {
                ui.colored_label(Color32::from_rgb(220, 80, 80), "Ollama is not installed (the `ollama` command was not found).");
                ui.horizontal(|ui| {
                    ui.hyperlink_to(OLLAMA_DOWNLOAD_URL, OLLAMA_DOWNLOAD_URL);
                    self.copy_button(ui, OLLAMA_DOWNLOAD_URL);
                    if ui.button("Retry").clicked() {
                        self.backend.ensure_running();
                    }
                });
                ui.separator();
            }
            RuntimeStatus::Offline { error } => {
                ui.colored_label(Color32::from_rgb(220, 80, 80), format!("Ollama is not reachable: {error}"));
                if ui.button("Retry").clicked() {
                    self.backend.ensure_running();
                }
                ui.separator();
            }
            _ => {}
        }
    }

    pub(crate) fn system_summary(&mut self, ui: &mut egui::Ui) {
        ui.horizontal_wrapped(|ui| {
            if self.probing {
                ui.label(RichText::new("Probing hardware…").small().color(Color32::GRAY));
                return;
            }
            let s = &self.sys;
            let gpu = match s.gpus.iter().find(|g| g.vram.is_some()) {
                Some(g) => format!("{} ({})", g.name, fmt_bytes(g.vram.unwrap_or(0))),
                None => "no dedicated GPU".into(),
            };
            ui.label(
                RichText::new(format!(
                    "RAM {} · GPU {} · Disk free {}",
                    fmt_bytes(s.ram_total),
                    gpu,
                    fmt_bytes(s.disk_free)
                ))
                .small()
                .color(Color32::from_gray(170)),
            );
            if ui.small_button("Details").clicked() {
                self.show_system = true;
            }
        });
    }

    pub(crate) fn fit_badge(ui: &mut egui::Ui, fit: Fit) {
        let color = match fit {
            Fit::Gpu => Color32::from_rgb(90, 200, 120),
            Fit::Cpu => Color32::from_rgb(220, 180, 60),
            Fit::TooBig | Fit::NoDisk => Color32::from_rgb(220, 90, 90),
        };
        ui.label(RichText::new(fit.label()).small().color(color));
    }

    pub(crate) fn pull_status(&mut self, ui: &mut egui::Ui, tag: &str) {
        if let Some(p) = self.pulls.get(tag) {
            let frac = if p.total > 0 { p.completed as f32 / p.total as f32 } else { 0.0 };
            let text = if p.total > 0 {
                format!("{} · {} / {}", p.status, fmt_bytes(p.completed), fmt_bytes(p.total))
            } else {
                p.status.clone()
            };
            ui.add(egui::ProgressBar::new(frac).text(text).desired_width(260.0).corner_radius(0.0));
            if ui.small_button("Cancel").clicked() {
                p.cancel.store(true, Ordering::Relaxed);
            }
        } else if let Some(e) = self.errors.get(tag).cloned() {
            ui.colored_label(Color32::from_rgb(220, 90, 90), format!("⚠ {e}"));
            if ui.small_button("Retry").clicked() {
                self.start_pull(tag);
            }
        }
    }

    pub(crate) fn installed_row(&mut self, ui: &mut egui::Ui, m: &InstalledModel) {
        ui.horizontal(|ui| {
            let active = self.selected.as_deref() == Some(m.name.as_str());
            ui.label(RichText::new(&m.name).strong());
            ui.label(RichText::new(fmt_bytes(m.size)).small().color(Color32::GRAY));
            // context window as reported by the runtime (fetched once per model)
            match self.ctx_limits.get(&m.name) {
                Some(Some(n)) => {
                    ui.label(RichText::new(format!("{}K context", n / 1024)).small().color(Color32::GRAY));
                }
                Some(None) => {}
                None => {
                    if matches!(self.status, RuntimeStatus::Online { .. }) {
                        self.ctx_limits.insert(m.name.clone(), None);
                        self.backend.model_info(&m.name);
                    }
                }
            }
            // already on disk: only memory matters
            if !self.probing {
                Self::fit_badge(ui, Fit::assess(m.size, &SystemInfo { disk_free: 0, ..self.sys.clone() }));
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if self.confirm_delete.as_deref() == Some(m.name.as_str()) {
                    if ui.button("Cancel").clicked() {
                        self.confirm_delete = None;
                    }
                    if ui.button(RichText::new("Confirm delete").color(Color32::from_rgb(230, 100, 100))).clicked() {
                        self.backend.delete(&m.name);
                        self.confirm_delete = None;
                    }
                } else if ui.small_button("Delete").clicked() {
                    self.confirm_delete = Some(m.name.clone());
                }
                if active {
                    ui.label(RichText::new("in use").small().color(Color32::from_rgb(90, 200, 120)));
                } else if ui.small_button("Use").clicked() {
                    self.selected = Some(m.name.clone());
                }
                let base = m.name.trim_end_matches(":latest");
                self.copy_button(ui, &model_url(base));
            });
        });
        if let Some(e) = self.errors.get(&m.name) {
            ui.colored_label(Color32::from_rgb(220, 90, 90), format!("⚠ {e}"));
        }
    }
}

mod background;
mod backend;
mod catalog;
mod system;

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::sync::Arc;

use eframe::egui::{self, Align2, Color32, FontId, Pos2, Rect, RichText, Stroke, Vec2};

use background::{draw_background, CHAT_WIDTH};
use backend::ollama::{Ollama, DEFAULT_URL};
use backend::{Backend, ChatMessage, Event, InstalledModel, RuntimeStatus, Sink};
use catalog::{model_url, CATALOG};
use system::{fmt_bytes, Fit, SystemInfo};

const OLLAMA_DOWNLOAD_URL: &str = "https://ollama.com/download";

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
        Box::new(|cc| Ok(Box::new(App::new(cc.egui_ctx.clone())))),
    )
}

struct PullState {
    status: String,
    completed: u64,
    total: u64,
    cancel: Arc<AtomicBool>,
}

struct App {
    backend: Box<dyn Backend>,
    rx: Receiver<Event>,
    status: RuntimeStatus,
    installed: Vec<InstalledModel>,
    sys: SystemInfo,
    selected: Option<String>,

    pulls: HashMap<String, PullState>,
    errors: HashMap<String, String>,
    confirm_delete: Option<String>,
    custom_pull: String,

    input: String,
    messages: Vec<ChatMessage>,
    chat_cancel: Option<Arc<AtomicBool>>,

    show_models: bool,
    show_system: bool,
    toast: Option<(String, f64)>,
}

impl App {
    fn new(ctx: egui::Context) -> Self {
        let (tx, rx) = mpsc::channel();
        let backend = Ollama::new(DEFAULT_URL, Sink::new(tx, ctx));
        backend.ensure_running();
        Self {
            backend: Box::new(backend),
            rx,
            status: RuntimeStatus::Checking,
            installed: Vec::new(),
            sys: SystemInfo::probe(),
            selected: None,
            pulls: HashMap::new(),
            errors: HashMap::new(),
            confirm_delete: None,
            custom_pull: String::new(),
            input: String::new(),
            messages: Vec::new(),
            chat_cancel: None,
            show_models: false,
            show_system: false,
            toast: None,
        }
    }

    fn is_installed(&self, tag: &str) -> bool {
        self.installed.iter().any(|m| m.name == tag || m.name == format!("{tag}:latest"))
    }

    fn streaming(&self) -> bool {
        self.chat_cancel.is_some()
    }

    fn handle_events(&mut self) {
        while let Ok(ev) = self.rx.try_recv() {
            match ev {
                Event::Status(s) => self.status = s,
                Event::Installed(list) => {
                    self.installed = list;
                    let still_there = self
                        .selected
                        .as_ref()
                        .is_some_and(|s| self.installed.iter().any(|m| &m.name == s));
                    if !still_there {
                        self.selected = self.installed.first().map(|m| m.name.clone());
                    }
                    // first launch without any model: open the model manager
                    if self.installed.is_empty() && self.pulls.is_empty() {
                        self.show_models = true;
                    }
                }
                Event::PullProgress { model, status, completed, total } => {
                    if let Some(p) = self.pulls.get_mut(&model) {
                        p.status = status;
                        if total > 0 {
                            p.completed = completed;
                            p.total = total;
                        }
                    }
                }
                Event::PullDone { model, error } => {
                    self.pulls.remove(&model);
                    match error {
                        Some(e) => {
                            self.errors.insert(model, e);
                        }
                        None => {
                            self.errors.remove(&model);
                            if self.selected.is_none() {
                                self.selected = Some(model);
                            }
                            self.sys = SystemInfo::probe(); // disk space changed
                        }
                    }
                }
                Event::Deleted { model, error } => {
                    if let Some(e) = error {
                        self.errors.insert(model, e);
                    } else {
                        self.sys = SystemInfo::probe();
                    }
                }
                Event::ChatToken(tok) => {
                    if let Some(last) = self.messages.last_mut() {
                        last.content.push_str(&tok);
                    }
                }
                Event::ChatDone { error } => {
                    self.chat_cancel = None;
                    if let (Some(e), Some(last)) = (error, self.messages.last_mut()) {
                        last.content.push_str(&format!("\n\n[error: {e}]"));
                    }
                }
            }
        }
    }

    fn start_pull(&mut self, tag: &str) {
        let tag = tag.trim().to_string();
        if tag.is_empty() || self.pulls.contains_key(&tag) {
            return;
        }
        let cancel = Arc::new(AtomicBool::new(false));
        self.errors.remove(&tag);
        self.pulls.insert(
            tag.clone(),
            PullState { status: "starting".into(), completed: 0, total: 0, cancel: cancel.clone() },
        );
        self.backend.pull(&tag, cancel);
    }

    fn send(&mut self) {
        let text = self.input.trim().to_string();
        if text.is_empty() || self.streaming() {
            return;
        }
        let Some(model) = self.selected.clone() else {
            self.notify("Download or select a model first");
            self.show_models = true;
            return;
        };
        self.input.clear();
        self.messages.push(ChatMessage { role: "user".into(), content: text });
        let history = self.messages.clone();
        self.messages.push(ChatMessage { role: "assistant".into(), content: String::new() });
        let cancel = Arc::new(AtomicBool::new(false));
        self.chat_cancel = Some(cancel.clone());
        self.backend.chat(&model, history, cancel);
    }

    fn stop(&mut self) {
        if let Some(c) = &self.chat_cancel {
            c.store(true, Ordering::Relaxed);
        }
    }

    fn notify(&mut self, msg: &str) {
        self.toast = Some((msg.to_string(), 0.0));
    }

    fn copy_button(&mut self, ui: &mut egui::Ui, url: &str) {
        if ui.small_button("⧉ Copy").on_hover_text("Copy link").clicked() {
            ui.ctx().copy_text(url.to_string());
            self.notify("Link copied");
        }
    }

    // ---------------------------------------------------------------- top bar

    fn top_bar(&mut self, ui: &mut egui::Ui, rect: Rect) {
        let bar = Rect::from_center_size(Pos2::new(rect.center().x, rect.top() + 28.0), Vec2::new(620.0, 34.0));
        ui.scope_builder(egui::UiBuilder::new().max_rect(bar), |ui| {
            ui.horizontal_centered(|ui| {
                let (color, text) = match &self.status {
                    RuntimeStatus::Checking => (Color32::GRAY, "checking…".to_string()),
                    RuntimeStatus::Starting => (Color32::from_rgb(220, 180, 60), "starting ollama…".into()),
                    RuntimeStatus::Online { version } => (Color32::from_rgb(90, 200, 120), format!("ollama {version}")),
                    RuntimeStatus::NotInstalled => (Color32::from_rgb(220, 80, 80), "ollama not installed".into()),
                    RuntimeStatus::Offline { .. } => (Color32::from_rgb(220, 80, 80), "ollama offline".into()),
                };
                let (r, _) = ui.allocate_exact_size(Vec2::splat(10.0), egui::Sense::hover());
                ui.painter().circle_filled(r.center(), 4.0, color);
                let resp = ui.label(RichText::new(text).size(12.0).color(Color32::from_gray(170)));
                if let RuntimeStatus::Offline { error } = &self.status {
                    resp.on_hover_text(error);
                    if ui.small_button("Retry").clicked() {
                        self.backend.ensure_running();
                    }
                }

                ui.add_space(12.0);
                let label = self.selected.clone().unwrap_or_else(|| "no model".into());
                egui::ComboBox::from_id_salt("model_select")
                    .selected_text(label)
                    .width(170.0)
                    .show_ui(ui, |ui| {
                        for m in &self.installed {
                            ui.selectable_value(&mut self.selected, Some(m.name.clone()), &m.name);
                        }
                    });

                if ui.button("Models").clicked() {
                    self.show_models = !self.show_models;
                }
                if ui.button("System").clicked() {
                    self.show_system = !self.show_system;
                }
                if !self.messages.is_empty() && !self.streaming() && ui.button("New chat").clicked() {
                    self.messages.clear();
                }
            });
        });
    }

    // ------------------------------------------------------------------- chat

    fn chat(&mut self, ui: &mut egui::Ui, rect: Rect) {
        let panel_w = CHAT_WIDTH.min(rect.width() - 40.0);

        if self.messages.is_empty() {
            ui.painter().text(
                rect.center() - Vec2::new(0.0, 70.0),
                Align2::CENTER_CENTER,
                "what's on your mind?",
                FontId::proportional(40.0),
                Color32::from_gray(235),
            );
        } else {
            let hist = Rect::from_min_max(
                Pos2::new(rect.center().x - panel_w / 2.0, rect.top() + 60.0),
                Pos2::new(rect.center().x + panel_w / 2.0, rect.bottom() - 120.0),
            );
            let streaming = self.streaming();
            let n = self.messages.len();
            ui.scope_builder(egui::UiBuilder::new().max_rect(hist), |ui| {
                egui::ScrollArea::vertical().stick_to_bottom(true).show(ui, |ui| {
                    for (i, m) in self.messages.iter().enumerate() {
                        let user = m.role == "user";
                        let layout = if user {
                            egui::Layout::right_to_left(egui::Align::TOP)
                        } else {
                            egui::Layout::left_to_right(egui::Align::TOP)
                        };
                        ui.with_layout(layout, |ui| {
                            egui::Frame::new()
                                .fill(if user {
                                    Color32::from_rgba_unmultiplied(232, 232, 230, 235)
                                } else {
                                    Color32::from_rgba_unmultiplied(28, 28, 32, 225)
                                })
                                .corner_radius(10.0)
                                .inner_margin(10.0)
                                .show(ui, |ui| {
                                    ui.set_max_width(panel_w * 0.85);
                                    let c = if user { Color32::from_gray(15) } else { Color32::from_gray(225) };
                                    let text = if m.content.is_empty() && streaming && i == n - 1 {
                                        "…".to_string()
                                    } else {
                                        m.content.clone()
                                    };
                                    ui.add(egui::Label::new(RichText::new(text).color(c).size(15.0)).selectable(true));
                                });
                        });
                        ui.add_space(8.0);
                    }
                });
            });
        }

        let input_center = if self.messages.is_empty() {
            rect.center() + Vec2::new(0.0, 10.0)
        } else {
            Pos2::new(rect.center().x, rect.bottom() - 70.0)
        };
        let input_rect = Rect::from_center_size(input_center, Vec2::new(panel_w, 76.0));
        let mut send = false;
        let mut stop = false;
        ui.scope_builder(egui::UiBuilder::new().max_rect(input_rect), |ui| {
            egui::Frame::new()
                .fill(Color32::from_rgba_unmultiplied(22, 22, 26, 235))
                .stroke(Stroke::new(1.0, Color32::from_gray(50)))
                .corner_radius(12.0)
                .inner_margin(10.0)
                .show(ui, |ui| {
                    ui.set_width(panel_w - 20.0);
                    let hint = match &self.selected {
                        Some(m) => format!("Ask {m} anything…"),
                        None => "Download a model to start…".into(),
                    };
                    let resp = ui.add(
                        egui::TextEdit::singleline(&mut self.input)
                            .hint_text(hint)
                            .frame(egui::Frame::NONE)
                            .desired_width(f32::INFINITY)
                            .text_color(Color32::from_gray(230))
                            .font(FontId::proportional(15.0)),
                    );
                    if resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                        send = true;
                        resp.request_focus();
                    }
                    ui.horizontal(|ui| {
                        ui.label(RichText::new("+").color(Color32::from_gray(150)).size(16.0));
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if self.streaming() {
                                if ui.button("■ Stop").clicked() {
                                    stop = true;
                                }
                            } else {
                                if ui.button("↗").clicked() {
                                    send = true;
                                }
                                ui.label(RichText::new("↵ send").color(Color32::from_gray(110)).size(11.0));
                            }
                        });
                    });
                });
        });
        if send {
            self.send();
        }
        if stop {
            self.stop();
        }
    }

    // ---------------------------------------------------------- model manager

    fn models_window(&mut self, ctx: &egui::Context) {
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
                    ui.heading("Installed");
                    if self.installed.is_empty() {
                        ui.label(RichText::new("No model installed yet.").color(Color32::GRAY));
                    }
                    for m in self.installed.clone() {
                        self.installed_row(ui, &m);
                    }
                    // custom pulls not in the catalog
                    let extra: Vec<String> = self
                        .pulls
                        .keys()
                        .chain(self.errors.keys())
                        .filter(|k| !CATALOG.iter().any(|c| c.tag == k.as_str()))
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
                    ui.heading("Catalog");
                    ui.label(
                        RichText::new("Recommendations are based on your hardware (see System).")
                            .small()
                            .color(Color32::GRAY),
                    );
                    ui.add_space(4.0);
                    for c in CATALOG {
                        self.catalog_row(ui, c);
                    }
                });
            });
        self.show_models = open;
    }

    fn runtime_notice(&mut self, ui: &mut egui::Ui) {
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

    fn system_summary(&mut self, ui: &mut egui::Ui) {
        ui.horizontal_wrapped(|ui| {
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

    fn fit_badge(ui: &mut egui::Ui, fit: Fit) {
        let color = match fit {
            Fit::Gpu => Color32::from_rgb(90, 200, 120),
            Fit::Cpu => Color32::from_rgb(220, 180, 60),
            Fit::TooBig | Fit::NoDisk => Color32::from_rgb(220, 90, 90),
        };
        ui.label(RichText::new(fit.label()).small().color(color));
    }

    fn pull_status(&mut self, ui: &mut egui::Ui, tag: &str) {
        if let Some(p) = self.pulls.get(tag) {
            let frac = if p.total > 0 { p.completed as f32 / p.total as f32 } else { 0.0 };
            let text = if p.total > 0 {
                format!("{} · {} / {}", p.status, fmt_bytes(p.completed), fmt_bytes(p.total))
            } else {
                p.status.clone()
            };
            ui.add(egui::ProgressBar::new(frac).text(text).desired_width(260.0));
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

    fn catalog_row(&mut self, ui: &mut egui::Ui, c: &catalog::CatalogModel) {
        let fit = Fit::assess(c.size_bytes(), &self.sys);
        let url = c.url();
        egui::Frame::new()
            .fill(Color32::from_rgb(20, 20, 24))
            .corner_radius(8.0)
            .inner_margin(8.0)
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    ui.label(RichText::new(c.tag).strong().size(15.0));
                    ui.label(RichText::new(format!("{} · {:.1} GB", c.params, c.size_gb)).small().color(Color32::GRAY));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        Self::fit_badge(ui, fit);
                    });
                });
                ui.label(RichText::new(c.description).color(Color32::from_gray(190)));
                ui.horizontal(|ui| {
                    ui.hyperlink_to(RichText::new(&url).small().monospace(), &url);
                    self.copy_button(ui, &url);
                });
                ui.horizontal(|ui| {
                    if self.is_installed(c.tag) {
                        ui.label(RichText::new("✓ Installed").color(Color32::from_rgb(90, 200, 120)));
                    } else if self.pulls.contains_key(c.tag) || self.errors.contains_key(c.tag) {
                        self.pull_status(ui, c.tag);
                    } else {
                        let label = match fit {
                            Fit::Gpu | Fit::Cpu => "Download",
                            Fit::TooBig | Fit::NoDisk => "Download anyway",
                        };
                        let online = matches!(self.status, RuntimeStatus::Online { .. });
                        if ui.add_enabled(online, egui::Button::new(label)).clicked() {
                            self.start_pull(c.tag);
                        }
                    }
                });
            });
        ui.add_space(6.0);
    }

    fn installed_row(&mut self, ui: &mut egui::Ui, m: &InstalledModel) {
        ui.horizontal(|ui| {
            let active = self.selected.as_deref() == Some(m.name.as_str());
            ui.label(RichText::new(&m.name).strong());
            ui.label(RichText::new(fmt_bytes(m.size)).small().color(Color32::GRAY));
            // already on disk: only memory matters
            Self::fit_badge(ui, Fit::assess(m.size, &SystemInfo { disk_free: 0, ..self.sys.clone() }));
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

    // ----------------------------------------------------------------- system

    fn system_window(&mut self, ctx: &egui::Context) {
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

    fn toast(&mut self, ui: &mut egui::Ui, rect: Rect, now: f64) {
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
        ui.painter().rect_filled(bg, 6.0, Color32::from_gray(225));
        ui.painter().galley(bg.min + Vec2::new(10.0, 5.0), galley, Color32::from_gray(15));
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        ctx.set_visuals(egui::Visuals::dark());
        self.handle_events();
        let now = ctx.input(|i| i.time);
        ctx.request_repaint();

        egui::CentralPanel::default().frame(egui::Frame::NONE).show(ui, |ui| {
            let rect = ui.max_rect();
            draw_background(ui.painter(), rect, now as f32);
            self.top_bar(ui, rect);
            self.chat(ui, rect);
            self.toast(ui, rect, now);
        });

        self.models_window(&ctx);
        self.system_window(&ctx);
    }
}

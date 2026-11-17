
use eframe::egui::{self, Align2, Color32, FontId, Pos2, Rect, RichText, Stroke, Vec2};

use crate::app::App;
use crate::background::CHAT_WIDTH;
use crate::backend::RuntimeStatus;
use crate::ui::widgets::*;

impl App {
    // ---------------------------------------------------------------- top bar

    pub(crate) fn top_bar(&mut self, ui: &mut egui::Ui, rect: Rect) {
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
                ui.painter().rect_filled(Rect::from_center_size(r.center(), Vec2::splat(8.0)), 0.0, color);
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
                    self.new_chat();
                }
            });
        });
    }

    // ------------------------------------------------------------------- chat

    pub(crate) fn chat(&mut self, ui: &mut egui::Ui, rect: Rect) {
        let panel_w = CHAT_WIDTH.min(rect.width() - 40.0);
        let input_h = 118.0;
        let left = rect.center().x - panel_w / 2.0;

        if self.messages.is_empty() {
            ui.painter().text(
                rect.center() - Vec2::new(0.0, 100.0),
                Align2::CENTER_CENTER,
                "what's on your mind?",
                FontId::proportional(42.0),
                Color32::from_gray(235),
            );
        } else {
            let hist = Rect::from_min_max(
                Pos2::new(left, rect.top() + 64.0),
                Pos2::new(left + panel_w, rect.bottom() - input_h - 40.0),
            );
            let streaming = self.streaming();
            let n = self.messages.len();
            let model = self.selected.clone().unwrap_or_default();
            let now = ui.input(|i| i.time) as f32;
            ui.scope_builder(egui::UiBuilder::new().max_rect(hist), |ui| {
                egui::ScrollArea::vertical().stick_to_bottom(true).auto_shrink(false).show(ui, |ui| {
                    ui.set_width(panel_w - 12.0); // room for the scrollbar
                    for (i, m) in self.messages.iter().enumerate() {
                        let waiting = m.content.is_empty() && streaming && i == n - 1;
                        let text = m.content.clone();
                        if m.role == "user" {
                            // user: compact block aligned right
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::TOP), |ui| {
                                egui::Frame::new()
                                    .fill(Color32::from_gray(225))
                                    .inner_margin(egui::Margin::symmetric(14, 10))
                                    .show(ui, |ui| {
                                        ui.set_max_width(panel_w * 0.72);
                                        ui.add(
                                            egui::Label::new(RichText::new(text).color(Color32::from_gray(15)).size(15.0))
                                                .selectable(true),
                                        );
                                    });
                            });
                        } else {
                            // assistant: full-width block with a thin accent bar and model name
                            let block = egui::Frame::new()
                                .fill(Color32::from_rgba_unmultiplied(18, 18, 22, 235))
                                .stroke(Stroke::new(1.0, Color32::from_gray(38)))
                                .inner_margin(egui::Margin { left: 16, right: 14, top: 10, bottom: 12 })
                                .show(ui, |ui| {
                                    ui.set_width(ui.available_width());
                                    ui.label(RichText::new(&model).small().monospace().color(Color32::from_gray(120)));
                                    ui.add_space(2.0);
                                    if waiting {
                                        shimmer_bar(ui, now);
                                    } else {
                                        ui.add(
                                            egui::Label::new(RichText::new(text).color(Color32::from_gray(225)).size(15.0))
                                                .selectable(true),
                                        );
                                    }
                                });
                            // gold accent bar along the left edge, exactly as tall as the block
                            let r = block.response.rect;
                            ui.painter().rect_filled(
                                Rect::from_min_max(r.left_top(), Pos2::new(r.left() + 3.0, r.bottom())),
                                0.0,
                                Color32::from_rgb(205, 165, 70),
                            );
                        }
                        ui.add_space(14.0);
                    }
                });
            });
        }

        let input_rect = if self.messages.is_empty() {
            Rect::from_center_size(rect.center() + Vec2::new(0.0, 10.0), Vec2::new(panel_w, input_h))
        } else {
            Rect::from_min_size(Pos2::new(left, rect.bottom() - input_h - 24.0), Vec2::new(panel_w, input_h))
        };
        let input_id = egui::Id::unique("chat_input");
        let mut send = false;
        let mut stop = false;
        // Enter sends, Shift+Enter inserts a newline
        if ui.memory(|m| m.has_focus(input_id))
            && ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Enter))
        {
            send = true;
        }
        ui.scope_builder(egui::UiBuilder::new().max_rect(input_rect), |ui| {
            egui::Frame::new()
                .fill(Color32::from_rgba_unmultiplied(20, 20, 24, 240))
                .stroke(Stroke::new(1.0, Color32::from_gray(58)))
                .inner_margin(egui::Margin::symmetric(14, 12))
                .show(ui, |ui| {
                    ui.set_width(panel_w - 28.0);
                    let hint = match &self.selected {
                        Some(m) => format!("Ask {m} anything…"),
                        None => "Download a model to start…".into(),
                    };
                    egui::ScrollArea::vertical().max_height(input_h - 54.0).id_salt("input_scroll").show(ui, |ui| {
                        ui.add(
                            egui::TextEdit::multiline(&mut self.input)
                                .id(input_id)
                                .hint_text(hint)
                                .frame(egui::Frame::NONE)
                                .desired_rows(3)
                                .desired_width(f32::INFINITY)
                                .text_color(Color32::from_gray(230))
                                .font(FontId::proportional(16.0)),
                        );
                    });
                    ui.add_space(4.0);
                    ui.horizontal(|ui| {
                        ui.label(RichText::new("+").color(Color32::from_gray(150)).size(16.0));
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if self.streaming() {
                                if ui.add(egui::Button::new("■ Stop").min_size(Vec2::new(64.0, 28.0))).clicked() {
                                    stop = true;
                                }
                            } else {
                                if ui.add(egui::Button::new(RichText::new("↗").size(16.0)).min_size(Vec2::new(34.0, 28.0))).clicked() {
                                    send = true;
                                }
                                ui.label(RichText::new("↵ send   ⇧↵ newline").color(Color32::from_gray(110)).size(11.0));
                            }
                        });
                    });
                });
        });
        if send {
            self.send();
            ui.memory_mut(|m| m.request_focus(input_id));
        }
        if stop {
            self.stop();
        }
    }
}

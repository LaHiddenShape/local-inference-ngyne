
use eframe::egui::{self, Align2, Color32, FontId, Pos2, Rect, RichText, Stroke, Vec2};

use std::collections::HashMap;

use crate::app::{App, Thumb};
use crate::attach::{Attachment, AttachmentData};
use crate::ui::theme;
use crate::background::CHAT_WIDTH;
use crate::backend::{ChatMessage, Event, Role, RuntimeStatus, Sink};
use crate::ui::widgets::*;

impl App {
    // ---------------------------------------------------------------- top bar

    pub(crate) fn top_bar(&mut self, ui: &mut egui::Ui, rect: Rect) {
        // narrow (history open on a small window): the status shrinks to its dot
        let compact = rect.width() < 760.0;
        let width = if compact { 500.0 } else { 620.0 };
        let bar = Rect::from_center_size(Pos2::new(rect.center().x, rect.top() + 28.0), Vec2::new(width, 34.0));
        ui.scope_builder(egui::UiBuilder::new().max_rect(bar), |ui| {
            ui.horizontal_centered(|ui| {
                let (color, text) = match &self.runtime.status {
                    RuntimeStatus::Checking => (Color32::GRAY, "checking…".to_string()),
                    RuntimeStatus::Starting => (theme::WARNING, "starting ollama…".into()),
                    RuntimeStatus::Online { version } => (theme::SUCCESS, format!("ollama {version}")),
                    RuntimeStatus::NotInstalled => (theme::DANGER, "ollama not installed".into()),
                    RuntimeStatus::Offline { .. } => (theme::DANGER, "ollama offline".into()),
                };
                let (r, dot) = ui.allocate_exact_size(Vec2::splat(10.0), egui::Sense::hover());
                ui.painter().rect_filled(Rect::from_center_size(r.center(), Vec2::splat(8.0)), 0.0, color);
                let status = if compact {
                    dot.on_hover_text(&text)
                } else {
                    ui.label(RichText::new(&text).size(12.0).color(Color32::from_gray(170)))
                };
                if let RuntimeStatus::Offline { error } = &self.runtime.status {
                    status.on_hover_text(error);
                    if ui.small_button("Retry").clicked() {
                        self.backend.ensure_running();
                    }
                }

                ui.add_space(12.0);
                let label = self.runtime.selected.clone().unwrap_or_else(|| "no model".into());
                // locked while a reply is streaming so it is attributed to the right model
                let streaming = self.chat.streaming();
                ui.add_enabled_ui(!streaming, |ui| {
                    let combo = egui::ComboBox::from_id_salt("model_select")
                        .selected_text(label)
                        .width(if compact { 130.0 } else { 170.0 })
                        .show_ui(ui, |ui| {
                            for m in &self.runtime.installed {
                                ui.selectable_value(&mut self.runtime.selected, Some(m.name.clone()), &m.name);
                            }
                        });
                    // the selector shows no label: name it for screen readers
                    combo.response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::ComboBox, !streaming, "Model"));
                })
                .response
                .on_disabled_hover_text("Wait for the reply to finish");

                if ui.button("Models").clicked() {
                    self.view.show_models = !self.view.show_models;
                }
                if ui.button("Uncensored").clicked() {
                    self.view.show_uncensored = !self.view.show_uncensored;
                }
                if ui.button("System").clicked() {
                    self.view.show_system = !self.view.show_system;
                }
                // downloads keep going with the Models window closed: show them here
                if !self.runtime.pulls.is_empty() {
                    let (done, total) = self.runtime.pulls.values().fold((0, 0), |(d, t), p| (d + p.completed, t + p.total));
                    let progress = if total > 0 { format!("{:.0}%", done as f64 * 100.0 / total as f64) } else { "…".into() };
                    let tip: Vec<String> = self.runtime.pulls.iter().map(|(tag, p)| format!("{tag}: {}", p.status)).collect();
                    if ui.button(format!("↓ {progress}")).on_hover_text(tip.join("\n")).clicked() {
                        self.view.show_models = true;
                    }
                }
                if !self.chat.messages.is_empty() && !self.chat.streaming() && ui.button("New chat").on_hover_text("Ctrl+N").clicked() {
                    self.new_chat();
                }
            });
        });
    }

    // ------------------------------------------------------------------- chat

    pub(crate) fn chat(&mut self, ui: &mut egui::Ui, rect: Rect) {
        let panel_w = CHAT_WIDTH.min(rect.width() - 40.0);
        self.handle_dropped_files(ui, rect);
        let overflow = self.context_overflow();
        let no_vision = self.chat.pending.iter().any(|a| a.is_image()) && !self.runtime.selected_has_vision();
        let warnings = overflow.is_some() as u8 + no_vision as u8;
        let pending_h = match () {
            _ if self.chat.pending.is_empty() => 0.0,
            _ if self.chat.pending.iter().any(|a| a.is_image()) => 66.0, // 48 px previews
            _ => 30.0,
        };
        let input_h = 118.0 + pending_h + 18.0 * warnings as f32;
        let left = rect.center().x - panel_w / 2.0;

        if self.chat.messages.is_empty() {
            greeting(ui, rect.center() - Vec2::new(0.0, 100.0));
        } else {
            ui.ctx().data_mut(|d| d.remove::<f64>(egui::Id::unique(GREETING_ID)));
            let hist = Rect::from_min_max(
                Pos2::new(left, rect.top() + 64.0),
                Pos2::new(left + panel_w, rect.bottom() - input_h - 40.0),
            );
            let streaming = self.chat.streaming();
            let n = self.chat.messages.len();
            let fallback_model = self.runtime.selected.clone().unwrap_or_default();
            let now = ui.input(|i| i.time) as f32;
            let mut copy: Option<String> = None;
            let mut regenerate = false;
            let width_bits = panel_w.to_bits();
            let layout = &mut self.chat.msg_layout;
            layout.resize(n, (0, 0.0));
            ui.scope_builder(egui::UiBuilder::new().max_rect(hist), |ui| {
                egui::ScrollArea::vertical().stick_to_bottom(true).auto_shrink(false).show_viewport(ui, |ui, viewport| {
                    ui.set_width(panel_w - 12.0); // room for the scrollbar
                    let origin = ui.max_rect().top(); // where content y = 0 is
                    // blocks are stacked edge to edge; the spacing lives inside them
                    let spacing = ui.spacing().item_spacing.y;
                    ui.spacing_mut().item_spacing.y = 0.0;
                    let mut prev_model: Option<&str> = None;
                    for (i, m) in self.chat.messages.iter().enumerate() {
                        let model = m.model.as_deref().unwrap_or(&fallback_model);
                        // mark a model switch inside the conversation
                        let switched = m.role == Role::Assistant && prev_model.is_some_and(|p| p != model);
                        if m.role == Role::Assistant {
                            prev_model = Some(model);
                        }
                        // reasoning of a thinking model: its own field, or inline <think> tags
                        let (inline_thinking, answer) =
                            if m.role == Role::Assistant { split_thinking(&m.content) } else { ("", m.content.as_str()) };
                        let thinking = if m.thinking.is_empty() { inline_thinking } else { m.thinking.as_str() };
                        let waiting = answer.is_empty() && streaming && i == n - 1;
                        // messages far from the visible area are not laid out (parsing
                        // the Markdown is the costly part): their last height is reserved
                        let key = layout_key(m, width_bits, switched, waiting);
                        let top = ui.cursor().top() - origin;
                        let (known, height) = layout[i];
                        if known == key && (top + height < viewport.top() - 300.0 || top > viewport.bottom() + 300.0) {
                            ui.allocate_space(Vec2::new(ui.available_width(), height));
                            continue;
                        }
                        let text = m.content.as_str();
                        let block = ui.scope(|ui| {
                            ui.spacing_mut().item_spacing.y = spacing;
                            if switched {
                                ui.vertical_centered(|ui| {
                                    ui.label(
                                        RichText::new(format!("— switched to {model} —"))
                                            .small()
                                            .monospace()
                                            .color(Color32::from_gray(110)),
                                    );
                                });
                                ui.add_space(10.0);
                            }
                            if m.role == Role::User {
                                // user: compact block aligned right
                                ui.with_layout(egui::Layout::right_to_left(egui::Align::TOP), |ui| {
                                    egui::Frame::new()
                                        .fill(Color32::from_gray(225))
                                        .inner_margin(egui::Margin::symmetric(14, 10))
                                        .show(ui, |ui| {
                                            ui.set_max_width(panel_w * 0.72);
                                            ui.with_layout(egui::Layout::top_down(egui::Align::Max), |ui| {
                                                if !m.attachments.is_empty() {
                                                    ui.horizontal_wrapped(|ui| {
                                                        for a in &m.attachments {
                                                            if !image_preview(ui, &mut self.chat.thumbs, &self.sink, a, 96.0) {
                                                                attachment_chip(ui, a, true);
                                                            }
                                                        }
                                                    });
                                                }
                                                if !text.is_empty() {
                                                    ui.add(
                                                        egui::Label::new(RichText::new(text).color(Color32::from_gray(15)).size(15.0))
                                                            .selectable(true),
                                                    );
                                                }
                                            });
                                        });
                                });
                                ui.with_layout(egui::Layout::right_to_left(egui::Align::TOP), |ui| {
                                    if m.time > 0 {
                                        ui.label(RichText::new(fmt_time(m.time)).small().monospace().color(Color32::from_gray(110)));
                                    }
                                    if !text.is_empty() && copy_icon(ui).on_hover_text("Copy the prompt").clicked() {
                                        copy = Some(text.to_string());
                                    }
                                });
                            } else {
                                // assistant: full-width block with a thin accent bar and model name
                                let block = egui::Frame::new()
                                    .fill(Color32::from_rgba_unmultiplied(18, 18, 22, 235))
                                    .stroke(Stroke::new(1.0, Color32::from_gray(38)))
                                    .inner_margin(egui::Margin { left: 16, right: 14, top: 10, bottom: 12 })
                                    .show(ui, |ui| {
                                        ui.set_width(ui.available_width());
                                        ui.label(RichText::new(model).small().monospace().color(Color32::from_gray(120)));
                                        ui.add_space(2.0);
                                        if !thinking.is_empty() {
                                            let title = if waiting { "Thinking…" } else { "Reasoning" };
                                            egui::CollapsingHeader::new(RichText::new(title).small().color(Color32::from_gray(140)))
                                                .id_salt(("thinking", m.time, i))
                                                .show(ui, |ui| {
                                                    ui.label(RichText::new(thinking.trim()).small().color(Color32::from_gray(150)));
                                                });
                                        }
                                        if waiting {
                                            shimmer_bar(ui, now);
                                        } else if !answer.is_empty() {
                                            // markdown: headings, lists, tables, code blocks with highlighting
                                            ui.scope(|ui| {
                                                ui.visuals_mut().override_text_color = Some(Color32::from_gray(225));
                                                if let Some(body) = ui.style_mut().text_styles.get_mut(&egui::TextStyle::Body) {
                                                    body.size = 15.0;
                                                }
                                                let markdown = keep_line_breaks(answer);
                                                egui_commonmark::CommonMarkViewer::new().show(ui, &mut self.chat.md_cache, &markdown);
                                            });
                                        }
                                        if let Some(e) = &m.error {
                                            ui.label(RichText::new(format!("⚠ {e}")).small().color(theme::DANGER));
                                        }
                                    });
                                // gold accent bar along the left edge, exactly as tall as the block
                                let r = block.response.rect;
                                ui.painter().rect_filled(
                                    Rect::from_min_max(r.left_top(), Pos2::new(r.left() + 3.0, r.bottom())),
                                    0.0,
                                    theme::ACCENT,
                                );
                                // actions under the reply
                                if !waiting {
                                    ui.horizontal(|ui| {
                                        let small = |t: &str| {
                                            egui::Button::new(RichText::new(t).small().monospace().color(Color32::from_gray(130)))
                                                .frame(false)
                                        };
                                        if !answer.is_empty() && copy_icon(ui).on_hover_text("Copy the reply as Markdown").clicked() {
                                            copy = Some(answer.to_string());
                                        }
                                        if i == n - 1 && !streaming && ui.add(small("regenerate")).on_hover_text("Ask again with the selected model").clicked() {
                                            regenerate = true;
                                        }
                                        if m.time > 0 {
                                            ui.label(RichText::new(fmt_time(m.time)).small().monospace().color(Color32::from_gray(100)));
                                        }
                                        if m.stopped {
                                            ui.label(RichText::new("· stopped").small().monospace().color(Color32::from_gray(100)));
                                        }
                                    });
                                }
                            }
                            ui.add_space(14.0);
                        });
                        layout[i] = (key, block.response.rect.height());
                    }
                });
            });
            if let Some(text) = copy {
                ui.ctx().copy_text(text);
                self.view.notify("Reply copied");
            }
            if regenerate {
                self.regenerate();
            }
        }

        let input_rect = if self.chat.messages.is_empty() {
            Rect::from_center_size(rect.center() + Vec2::new(0.0, 10.0), Vec2::new(panel_w, input_h))
        } else {
            Rect::from_min_size(Pos2::new(left, rect.bottom() - input_h - 24.0), Vec2::new(panel_w, input_h))
        };
        // notices go above the message box, or below it while it sits centered
        self.view.notice_anchor = if self.chat.messages.is_empty() {
            (input_rect.center_bottom() + Vec2::new(0.0, 14.0), 1.0)
        } else {
            (input_rect.center_top() - Vec2::new(0.0, 10.0), -1.0)
        };
        let input_id = egui::Id::unique("chat_input");
        if std::mem::take(&mut self.view.focus_input) {
            ui.memory_mut(|m| m.request_focus(input_id));
        }
        let mut send = false;
        let mut stop = false;
        // Enter sends, Shift+Enter inserts a newline; while a reply streams,
        // Enter in an empty box stops it
        if ui.memory(|m| m.has_focus(input_id))
            && ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Enter))
        {
            if self.chat.streaming() && self.chat.input.trim().is_empty() && self.chat.pending.is_empty() {
                stop = true;
            } else {
                send = true;
            }
        }
        ui.scope_builder(egui::UiBuilder::new().max_rect(input_rect), |ui| {
            egui::Frame::new()
                .fill(Color32::from_rgba_unmultiplied(20, 20, 24, 240))
                .stroke(Stroke::new(1.0, Color32::from_gray(58)))
                .inner_margin(egui::Margin::symmetric(14, 12))
                .show(ui, |ui| {
                    ui.set_width(panel_w - 28.0);
                    let hint = match &self.runtime.selected {
                        Some(m) => format!("Ask {m} anything…"),
                        None => "Download a model to start…".into(),
                    };
                    if !self.chat.pending.is_empty() {
                        let mut remove = None;
                        ui.horizontal_wrapped(|ui| {
                            for (i, a) in self.chat.pending.iter().enumerate() {
                                if !image_preview(ui, &mut self.chat.thumbs, &self.sink, a, 48.0) {
                                    attachment_chip(ui, a, false);
                                }
                                if ui.small_button("×").on_hover_text("Remove").clicked() {
                                    remove = Some(i);
                                }
                                ui.add_space(6.0);
                            }
                        });
                        if let Some(i) = remove {
                            self.chat.pending.remove(i);
                        }
                    }
                    let warn = |ui: &mut egui::Ui, text: String| {
                        ui.label(RichText::new(text).small().color(theme::DANGER));
                    };
                    if no_vision {
                        warn(ui, "This model does not accept images (try gemma3 or llava)".into());
                    }
                    if let Some((need, window)) = overflow {
                        warn(
                            ui,
                            format!(
                                "About {} tokens to send, the context window holds {}: the start will be cut. \
                                 Raise it in System, or start a new chat.",
                                short_tokens(need),
                                short_tokens(window)
                            ),
                        );
                    }
                    egui::ScrollArea::vertical().max_height(118.0 - 54.0).id_salt("input_scroll").show(ui, |ui| {
                        ui.add(
                            egui::TextEdit::multiline(&mut self.chat.input)
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
                        self.context_gauge(ui);
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if self.chat.streaming() {
                                if ui.add(egui::Button::new("■ Stop").min_size(Vec2::new(64.0, 28.0))).on_hover_text("Esc, or Enter in an empty box").clicked() {
                                    stop = true;
                                }
                            } else {
                                let something = !self.chat.input.trim().is_empty() || !self.chat.pending.is_empty();
                                let button = egui::Button::new(RichText::new("↗").size(16.0)).min_size(Vec2::new(34.0, 28.0));
                                if ui.add_enabled(something, button).on_hover_text("Send (Enter)").clicked() {
                                    send = true;
                                }
                                if ui
                                    .add(egui::Button::new(RichText::new("📎").size(15.0)).min_size(Vec2::new(34.0, 28.0)))
                                    .on_hover_text("Attach text files or images (or drop them on the window)")
                                    .clicked()
                                {
                                    self.pick_files();
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
            self.chat.stop();
        }
    }

    /// Files dropped on the window become attachments; shows a hint while hovering.
    fn handle_dropped_files(&mut self, ui: &mut egui::Ui, rect: Rect) {
        let (hovering, dropped) = ui.input(|i| (!i.raw.hovered_files.is_empty(), i.raw.dropped_files.clone()));
        if hovering {
            ui.painter().rect_filled(rect, 0.0, Color32::from_rgba_unmultiplied(0, 0, 0, 160));
            ui.painter().text(
                rect.center(),
                Align2::CENTER_CENTER,
                "Drop files to attach",
                FontId::proportional(24.0),
                Color32::from_gray(235),
            );
        }
        let paths: Vec<_> =
            dropped.iter().map(|f| f.path().to_path_buf()).filter(|p| !p.as_os_str().is_empty()).collect();
        if !paths.is_empty() {
            self.load_attachments(paths);
        }
    }

    /// Tokens used by the conversation vs. the context window of the model;
    /// a click opens the System window, where the window size is set.
    fn context_gauge(&mut self, ui: &mut egui::Ui) {
        let (Some(usage), Some(model)) = (self.chat.usage, self.runtime.selected.as_deref()) else { return };
        let limit = self.num_ctx(model);
        let used = usage.prompt_tokens + usage.completion_tokens;
        let frac = (used as f32 / limit as f32).clamp(0.0, 1.0);
        let color = if frac > 0.9 {
            theme::DANGER
        } else if frac > 0.75 {
            theme::WARNING
        } else {
            Color32::from_gray(110)
        };
        let (r, bar) = ui.allocate_exact_size(Vec2::new(60.0, 4.0), egui::Sense::click());
        ui.painter().rect_filled(r, 0.0, Color32::from_gray(40));
        ui.painter().rect_filled(Rect::from_min_size(r.min, Vec2::new(r.width() * frac, r.height())), 0.0, color);
        let text = format!("{} / {}", short_tokens(used), short_tokens(limit));
        let label = ui.add(egui::Label::new(RichText::new(text).small().monospace().color(color)).sense(egui::Sense::click()));
        let tip = if frac > 0.9 {
            "Context almost full: the oldest messages will be dropped by the model. \
             Start a new chat, or click to raise the context window."
        } else {
            "Tokens used by this conversation / context window of the model. Click to change it."
        };
        if bar.union(label).on_hover_text(tip).clicked() {
            self.view.show_system = true;
        }
    }
}

/// Splits a reply that starts with `<think>…</think>` (models or runtimes that
/// put the reasoning in the text) into the reasoning and the answer. While the
/// closing tag has not arrived yet, everything is reasoning.
fn split_thinking(content: &str) -> (&str, &str) {
    let Some(rest) = content.trim_start().strip_prefix("<think>") else { return ("", content) };
    match rest.split_once("</think>") {
        Some((thinking, answer)) => (thinking, answer.trim_start()),
        None => (rest, ""),
    }
}

/// Markdown joins lines separated by a single newline into one paragraph, but
/// replies often mean them as line breaks (poems, addresses, short lists
/// without markers). Turns those into hard breaks, outside fenced code.
fn keep_line_breaks(text: &str) -> std::borrow::Cow<'_, str> {
    if !text.contains('\n') {
        return text.into();
    }
    let lines: Vec<&str> = text.split('\n').collect();
    let mut out = String::with_capacity(text.len() + lines.len() * 2);
    let mut fence: Option<&str> = None;
    for (i, line) in lines.iter().enumerate() {
        let trimmed = line.trim_start();
        let is_fence = trimmed.starts_with("```") || trimmed.starts_with("~~~");
        if is_fence {
            match fence {
                None => fence = Some(&trimmed[..3]),
                Some(f) if trimmed.starts_with(f) => fence = None,
                Some(_) => {}
            }
        }
        out.push_str(line);
        if let Some(next) = lines.get(i + 1) {
            let in_code = fence.is_some() || is_fence;
            if !in_code && !line.trim().is_empty() && !next.trim().is_empty() && !line.ends_with("  ") {
                out.push_str("  ");
            }
            out.push('\n');
        }
    }
    out.into()
}

/// "950", "17.4k", or "8K" for window sizes (multiples of 1024).
fn short_tokens(n: u64) -> String {
    if n >= 1024 && n.is_multiple_of(1024) {
        format!("{}K", n / 1024)
    } else if n >= 1000 {
        format!("{:.1}k", n as f32 / 1000.0)
    } else {
        n.to_string()
    }
}

/// Changes whenever the laid-out height of a message may change.
fn layout_key(m: &ChatMessage, width_bits: u32, switched: bool, waiting: bool) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    (width_bits, m.content.len(), m.thinking.len(), m.attachments.len(), m.time, m.stopped, m.error.is_some()).hash(&mut h);
    (switched, waiting, m.role).hash(&mut h);
    h.finish()
}

/// Preview of an image attachment, `height` tall, decoded in the background
/// the first time (a gray box of the same size meanwhile, so nothing moves).
/// Returns false for text files and undecodable images: show a chip instead.
fn image_preview(ui: &mut egui::Ui, thumbs: &mut HashMap<u64, Thumb>, sink: &Sink, a: &Attachment, height: f32) -> bool {
    let AttachmentData::Image(b64) = &a.data else { return false };
    let key = crate::attach::fingerprint(a);
    match thumbs.get(&key) {
        Some(Thumb::Failed) => false,
        Some(Thumb::Ready(tex)) => {
            let size = tex.size_vec2();
            let width = (size.x * height / size.y.max(1.0)).clamp(height / 2.0, height * 2.0);
            ui.add(egui::Image::new(tex).fit_to_exact_size(Vec2::new(width, height))).on_hover_text(&a.name);
            true
        }
        loading => {
            if loading.is_none() {
                thumbs.insert(key, Thumb::Loading);
                let (sink, b64) = (sink.clone(), b64.clone());
                // 2x the shown size, sharp on HiDPI screens
                let max = (height * 2.0) as u32 * 2;
                std::thread::spawn(move || sink.send(Event::Thumbnail { key, image: crate::attach::thumbnail(&b64, max) }));
            }
            let (r, resp) = ui.allocate_exact_size(Vec2::splat(height), egui::Sense::hover());
            ui.painter().rect_filled(r, 0.0, Color32::from_gray(60));
            resp.on_hover_text(&a.name);
            true
        }
    }
}

/// Small label naming an attached file; `light` for use on the light user bubble.
/// A single widget (a frameless, unclickable button) so that a row made taller
/// by an image preview does not stretch it.
fn attachment_chip(ui: &mut egui::Ui, a: &Attachment, light: bool) {
    let icon = if a.is_image() { "🖼" } else { "📄" };
    let (fill, text) = if light {
        (Color32::from_gray(200), Color32::from_gray(25))
    } else {
        (Color32::from_gray(45), Color32::from_gray(210))
    };
    // long file names are shortened, the full name is in the tooltip
    let short: String = a.name.chars().take(28).collect();
    let shown = if a.name.chars().count() > 28 { format!("{short}…") } else { short };
    let chip = egui::Button::new(RichText::new(format!("{icon} {shown}")).small().color(text))
        .fill(fill)
        .stroke(Stroke::NONE)
        .sense(egui::Sense::hover());
    ui.add(chip).on_hover_text(&a.name);
}

/// "14:32" for today, "17/11 14:32" for another day of this year, else "17/11/2025 14:32"
/// (local time).
fn fmt_time(secs: u64) -> String {
    fn local(secs: u64) -> libc::tm {
        let t = secs as libc::time_t;
        // SAFETY: localtime_r writes only into the provided struct.
        let mut tm: libc::tm = unsafe { std::mem::zeroed() };
        unsafe { libc::localtime_r(&t, &mut tm) };
        tm
    }
    let tm = local(secs);
    let now = local(std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0));
    let hm = format!("{:02}:{:02}", tm.tm_hour, tm.tm_min);
    if tm.tm_year == now.tm_year && tm.tm_yday == now.tm_yday {
        hm
    } else if tm.tm_year == now.tm_year {
        format!("{:02}/{:02} {hm}", tm.tm_mday, tm.tm_mon + 1)
    } else {
        format!("{:02}/{:02}/{} {hm}", tm.tm_mday, tm.tm_mon + 1, tm.tm_year + 1900)
    }
}

const GREETING_ID: &str = "greeting_start";

/// "what's on your mind?" with a glitch look: the letters decode from random
/// glyphs left to right, then the line keeps twitching now and then with a
/// red/cyan channel split and jittered letters. Replayed each time the chat
/// becomes empty.
fn greeting(ui: &egui::Ui, center: Pos2) {
    const TEXT: &str = "what's on your mind?";
    const GLYPHS: &[u8] = b"!<>-_\\/[]{}=+*^?#$%&@01";
    const DECODE: f64 = 0.06; // delay between two letters settling
    const SCRAMBLE: f64 = 0.35; // how long a letter scrambles before settling
    const TICK: f64 = 0.05; // how often a scrambled glyph changes
    const BURST_EVERY: f64 = 3.2; // average time between glitch bursts
    const BURST_LEN: f64 = 0.18;

    let id = egui::Id::unique(GREETING_ID);
    let now = ui.input(|i| i.time);
    let start = ui.ctx().data_mut(|d| *d.get_temp_mut_or(id, now));
    let t = now - start;
    let tick = (t / TICK) as u64;

    // a burst window, at a pseudo-random offset inside each period
    let period = (t / BURST_EVERY) as u64;
    let burst_at = BURST_EVERY * (period as f64 + 0.3 + 0.5 * unit(hash(period)));
    let decoding = t < SCRAMBLE + TEXT.len() as f64 * DECODE;
    let burst = !decoding && (burst_at..burst_at + BURST_LEN).contains(&t);

    let font = FontId::new(44.0, light_family());
    let color = Color32::from_gray(235);
    let painter = ui.painter();
    let full = painter.layout_no_wrap(TEXT.to_owned(), font.clone(), color);
    let origin = center - full.size() / 2.0;

    for (n, (i, ch)) in TEXT.char_indices().enumerate() {
        if ch == ' ' {
            continue;
        }
        let local = t - n as f64 * DECODE;
        if local < 0.0 {
            continue;
        }
        let seed = hash(tick ^ ((n as u64) << 32));
        let mut glyph = ch;
        let mut offset = Vec2::ZERO;
        let mut split = 0.0;
        if local < SCRAMBLE {
            glyph = GLYPHS[(seed % GLYPHS.len() as u64) as usize] as char;
        } else if burst && seed.is_multiple_of(3) {
            offset = Vec2::new((unit(seed >> 8) as f32 - 0.5) * 14.0, (unit(seed >> 16) as f32 - 0.5) * 6.0);
            split = 3.0;
            if seed.is_multiple_of(7) {
                glyph = GLYPHS[((seed >> 24) % GLYPHS.len() as u64) as usize] as char;
            }
        }
        let x = painter.layout_no_wrap(TEXT[..i].to_owned(), font.clone(), color).size().x;
        let pos = origin + Vec2::new(x, 0.0) + offset;
        let decode_split = if local < SCRAMBLE { 2.0 } else { split };
        if decode_split > 0.0 {
            let s = Vec2::new(decode_split, 0.0);
            painter.text(pos - s, Align2::LEFT_TOP, glyph, font.clone(), Color32::from_rgba_unmultiplied(255, 40, 80, 150));
            painter.text(pos + s, Align2::LEFT_TOP, glyph, font.clone(), Color32::from_rgba_unmultiplied(40, 220, 255, 150));
        }
        painter.text(pos, Align2::LEFT_TOP, glyph, font.clone(), color);
    }

    if decoding || burst {
        ui.ctx().request_repaint();
    } else {
        // sleep until the next burst
        let next = if t < burst_at { burst_at } else { BURST_EVERY * (period + 1) as f64 + 0.3 * BURST_EVERY };
        ui.ctx().request_repaint_after(std::time::Duration::from_secs_f64((next - t).max(0.01)));
    }
}

fn hash(mut x: u64) -> u64 {
    // splitmix64
    x = x.wrapping_add(0x9E37_79B9_7F4A_7C15);
    x = (x ^ (x >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    x ^ (x >> 31)
}

fn unit(x: u64) -> f64 {
    (x >> 11) as f64 / (1u64 << 53) as f64
}

/// A small "copy" icon (two stacked sheets), brighter on hover.
fn copy_icon(ui: &mut egui::Ui) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(Vec2::splat(16.0), egui::Sense::click());
    let color = if resp.hovered() { Color32::from_gray(220) } else { Color32::from_gray(130) };
    let stroke = Stroke::new(1.2, color);
    let c = rect.center();
    let back = Rect::from_min_size(c + Vec2::new(-5.0, -5.0), Vec2::new(7.0, 8.0));
    let front = back.translate(Vec2::new(3.0, 2.5));
    ui.painter().rect_stroke(back, 0.0, stroke, egui::StrokeKind::Middle);
    ui.painter().rect_filled(front, 0.0, Color32::from_rgb(11, 11, 13));
    ui.painter().rect_stroke(front, 0.0, stroke, egui::StrokeKind::Middle);
    resp.on_hover_cursor(egui::CursorIcon::PointingHand)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inline_reasoning_is_split_from_the_answer() {
        assert_eq!(split_thinking("hello"), ("", "hello"));
        assert_eq!(split_thinking("<think>hmm</think>\n\nAnswer"), ("hmm", "Answer"));
        assert_eq!(split_thinking("<think>still thinking"), ("still thinking", ""));
    }

    #[test]
    fn single_newlines_become_hard_breaks_outside_code() {
        assert_eq!(keep_line_breaks("one line"), "one line");
        assert_eq!(keep_line_breaks("Sand stairs\nkings asleep"), "Sand stairs  \nkings asleep");
        // paragraphs and fenced code are left alone
        assert_eq!(keep_line_breaks("a\n\nb"), "a\n\nb");
        let code = "```rust\nlet a = 1;\nlet b = 2;\n```\ntext";
        assert_eq!(keep_line_breaks(code), code);
        assert_eq!(keep_line_breaks("x\n```\ny\n```"), "x  \n```\ny\n```");
    }
}

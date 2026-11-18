
use eframe::egui::{self, Align2, Color32, FontId, Pos2, Rect, RichText, Stroke, Vec2};

use crate::app::App;
use crate::background::CHAT_WIDTH;
use crate::backend::{ChatMessage, Role, RuntimeStatus};
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
                // locked while a reply is streaming so it is attributed to the right model
                let streaming = self.streaming();
                ui.add_enabled_ui(!streaming, |ui| {
                    egui::ComboBox::from_id_salt("model_select")
                        .selected_text(label)
                        .width(170.0)
                        .show_ui(ui, |ui| {
                            for m in &self.installed {
                                ui.selectable_value(&mut self.selected, Some(m.name.clone()), &m.name);
                            }
                        });
                })
                .response
                .on_disabled_hover_text("Wait for the reply to finish");

                if ui.button("Models").clicked() {
                    self.show_models = !self.show_models;
                }
                if ui.button("Uncensored").clicked() {
                    self.show_uncensored = !self.show_uncensored;
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
        self.handle_dropped_files(ui, rect);
        let overflow = self.context_overflow();
        let no_vision = self.pending.iter().any(|a| a.is_image()) && !self.selected_has_vision();
        let warnings = overflow.is_some() as u8 + no_vision as u8;
        let input_h = if self.pending.is_empty() { 118.0 } else { 148.0 } + 18.0 * warnings as f32;
        let left = rect.center().x - panel_w / 2.0;

        if self.messages.is_empty() {
            ui.painter().text(
                rect.center() - Vec2::new(0.0, 100.0),
                Align2::CENTER_CENTER,
                "what's on your mind?",
                FontId::new(44.0, light_family()),
                Color32::from_gray(235),
            );
        } else {
            let hist = Rect::from_min_max(
                Pos2::new(left, rect.top() + 64.0),
                Pos2::new(left + panel_w, rect.bottom() - input_h - 40.0),
            );
            let streaming = self.streaming();
            let n = self.messages.len();
            let fallback_model = self.selected.clone().unwrap_or_default();
            let now = ui.input(|i| i.time) as f32;
            let mut copy: Option<String> = None;
            let mut regenerate = false;
            let width_bits = panel_w.to_bits();
            let layout = &mut self.msg_layout;
            layout.resize(n, (0, 0.0));
            ui.scope_builder(egui::UiBuilder::new().max_rect(hist), |ui| {
                egui::ScrollArea::vertical().stick_to_bottom(true).auto_shrink(false).show_viewport(ui, |ui, viewport| {
                    ui.set_width(panel_w - 12.0); // room for the scrollbar
                    let origin = ui.max_rect().top(); // where content y = 0 is
                    // blocks are stacked edge to edge; the spacing lives inside them
                    let spacing = ui.spacing().item_spacing.y;
                    ui.spacing_mut().item_spacing.y = 0.0;
                    let mut prev_model: Option<&str> = None;
                    for (i, m) in self.messages.iter().enumerate() {
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
                                                            attachment_chip(ui, a, true);
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
                                if m.time > 0 {
                                    ui.with_layout(egui::Layout::right_to_left(egui::Align::TOP), |ui| {
                                        ui.label(RichText::new(fmt_time(m.time)).small().monospace().color(Color32::from_gray(110)));
                                    });
                                }
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
                                                egui_commonmark::CommonMarkViewer::new().show(ui, &mut self.md_cache, &markdown);
                                            });
                                        }
                                        if let Some(e) = &m.error {
                                            ui.label(RichText::new(format!("⚠ {e}")).small().color(Color32::from_rgb(220, 90, 90)));
                                        }
                                    });
                                // gold accent bar along the left edge, exactly as tall as the block
                                let r = block.response.rect;
                                ui.painter().rect_filled(
                                    Rect::from_min_max(r.left_top(), Pos2::new(r.left() + 3.0, r.bottom())),
                                    0.0,
                                    Color32::from_rgb(205, 165, 70),
                                );
                                // actions under the reply
                                if !waiting {
                                    ui.horizontal(|ui| {
                                        let small = |t: &str| {
                                            egui::Button::new(RichText::new(t).small().monospace().color(Color32::from_gray(130)))
                                                .frame(false)
                                        };
                                        if !answer.is_empty() && ui.add(small("copy")).on_hover_text("Copy the reply as Markdown").clicked() {
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
                self.notify("Reply copied");
            }
            if regenerate {
                self.regenerate();
            }
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
                    if !self.pending.is_empty() {
                        let mut remove = None;
                        ui.horizontal_wrapped(|ui| {
                            for (i, a) in self.pending.iter().enumerate() {
                                attachment_chip(ui, a, false);
                                if ui.small_button("×").on_hover_text("Remove").clicked() {
                                    remove = Some(i);
                                }
                                ui.add_space(6.0);
                            }
                        });
                        if let Some(i) = remove {
                            self.pending.remove(i);
                        }
                    }
                    let warn = |ui: &mut egui::Ui, text: String| {
                        ui.label(RichText::new(text).small().color(Color32::from_rgb(220, 90, 90)));
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
                        self.context_gauge(ui);
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if self.streaming() {
                                if ui.add(egui::Button::new("■ Stop").min_size(Vec2::new(64.0, 28.0))).clicked() {
                                    stop = true;
                                }
                            } else {
                                if ui.add(egui::Button::new(RichText::new("↗").size(16.0)).min_size(Vec2::new(34.0, 28.0))).clicked() {
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
            self.stop();
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
        let (Some(usage), Some(model)) = (self.usage, self.selected.as_deref()) else { return };
        let limit = self.num_ctx(model);
        let used = usage.prompt_tokens + usage.completion_tokens;
        let frac = (used as f32 / limit as f32).clamp(0.0, 1.0);
        let color = if frac > 0.9 {
            Color32::from_rgb(220, 90, 90)
        } else if frac > 0.75 {
            Color32::from_rgb(220, 180, 60)
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
            self.show_system = true;
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

/// Small label naming an attached file; `light` for use on the light user bubble.
fn attachment_chip(ui: &mut egui::Ui, a: &crate::attach::Attachment, light: bool) {
    let icon = if a.is_image() { "🖼" } else { "📄" };
    let (fill, text) = if light {
        (Color32::from_gray(200), Color32::from_gray(25))
    } else {
        (Color32::from_gray(45), Color32::from_gray(210))
    };
    egui::Frame::new().fill(fill).inner_margin(egui::Margin::symmetric(6, 2)).show(ui, |ui| {
        // long file names are shortened, the full name is in the tooltip
        let short: String = a.name.chars().take(28).collect();
        let shown = if a.name.chars().count() > 28 { format!("{short}…") } else { short };
        ui.label(RichText::new(format!("{icon} {shown}")).small().color(text)).on_hover_text(&a.name);
    });
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

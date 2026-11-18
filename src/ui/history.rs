
use eframe::egui::{self, Color32, Pos2, Rect, RichText, Vec2};

use crate::app::App;
use crate::ui::theme;
use crate::backend::Role;
use crate::store::{self, Conversation};

const UNDO_SECONDS: f64 = 5.0;

impl App {
    // ---------------------------------------------------------------- history

    /// Writes the current chat into the history (creating an entry if needed).
    pub(crate) fn persist(&mut self) {
        if self.messages.is_empty() {
            return;
        }
        let now = store::now_ms();
        let id = *self.current.get_or_insert(now);
        let model = self.selected.clone().unwrap_or_default();
        let pinned = self.conversations.iter().any(|c| c.id == id && c.pinned);
        let conv = Conversation {
            pinned,
            id,
            title: store::title_from(&self.messages),
            model,
            updated: now,
            messages: self.messages.clone(),
        };
        self.conversations.retain(|c| c.id != id);
        self.writer.save(conv.clone());
        self.conversations.insert(0, conv);
    }

    pub(crate) fn new_chat(&mut self) {
        self.stop();
        self.messages.clear();
        self.current = None;
        self.usage = None;
    }

    pub(crate) fn open_conversation(&mut self, id: u64) {
        let Some(c) = self.conversations.iter().find(|c| c.id == id).cloned() else { return };
        self.messages = c.messages;
        // older files have no per-message model: fall back to the conversation's
        for m in self.messages.iter_mut().filter(|m| m.role == Role::Assistant && m.model.is_none()) {
            m.model = Some(c.model.clone());
        }
        self.usage = None;
        self.current = Some(id);
        if self.installed.iter().any(|m| m.name == c.model) {
            self.selected = Some(c.model);
        }
    }

    /// Deletes right away, but keeps the conversation in memory for a few
    /// seconds so it can be restored with Undo.
    pub(crate) fn delete_conversation(&mut self, id: u64) {
        let Some(pos) = self.conversations.iter().position(|c| c.id == id) else { return };
        let conv = self.conversations.remove(pos);
        if self.current == Some(id) {
            self.new_chat();
        }
        self.undo_delete = Some((conv, pos, 0.0));
        self.writer.delete(id);
    }

    pub(crate) fn undo_delete(&mut self) {
        if let Some((conv, pos, _)) = self.undo_delete.take() {
            let pos = pos.min(self.conversations.len());
            self.writer.save(conv.clone());
            self.conversations.insert(pos, conv);
        }
    }

    /// "Deleted · Undo" bar shown for a few seconds after a deletion.
    pub(crate) fn undo_bar(&mut self, ui: &mut egui::Ui, rect: Rect, now: f64) {
        let Some((conv, _, start)) = &mut self.undo_delete else { return };
        if *start == 0.0 {
            *start = now;
        }
        if now - *start > UNDO_SECONDS {
            self.undo_delete = None;
            return;
        }
        ui.ctx().request_repaint_after(std::time::Duration::from_secs_f64(UNDO_SECONDS - (now - *start))); // to hide it
        let title = conv.title.clone();
        let bar = Rect::from_center_size(Pos2::new(rect.center().x, rect.bottom() - 22.0), Vec2::new(340.0, 30.0));
        let mut undo = false;
        ui.scope_builder(egui::UiBuilder::new().max_rect(bar), |ui| {
            egui::Frame::new().fill(Color32::from_gray(225)).inner_margin(egui::Margin::symmetric(10, 4)).show(ui, |ui| {
                ui.horizontal(|ui| {
                    let short: String = title.chars().take(28).collect();
                    ui.label(RichText::new(format!("Deleted “{short}”")).size(13.0).color(Color32::from_gray(15)));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let b = egui::Button::new(RichText::new("Undo").strong().color(theme::ACCENT_ON_LIGHT)).frame(false);
                        if ui.add(b).clicked() {
                            undo = true;
                        }
                    });
                });
            });
        });
        if undo {
            self.undo_delete();
        }
    }

    pub(crate) fn toggle_pin(&mut self, id: u64) {
        if let Some(c) = self.conversations.iter_mut().find(|c| c.id == id) {
            c.pinned = !c.pinned;
            self.writer.save(c.clone());
        }
    }

    pub(crate) fn history_panel(&mut self, ui: &mut egui::Ui) {
        let streaming = self.streaming();
        ui.add_space(10.0);
        ui.horizontal(|ui| {
            ui.label(RichText::new("History").strong().size(15.0));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.small_button("«").on_hover_text("Close").clicked() {
                    self.show_history = false;
                }
            });
        });
        ui.add_space(6.0);
        if ui.add_enabled(!streaming, egui::Button::new("+ New chat").min_size(Vec2::new(ui.available_width(), 28.0))).clicked() {
            self.new_chat();
        }
        ui.add_space(8.0);

        // search over titles and message contents
        ui.add(
            egui::TextEdit::singleline(&mut self.history_query)
                .hint_text("Search…")
                .desired_width(f32::INFINITY),
        );
        ui.add_space(8.0);

        if self.conversations.is_empty() {
            ui.label(RichText::new("No conversations yet.").small().color(Color32::GRAY));
            return;
        }

        let query = self.history_query.trim().to_lowercase();
        if !query.is_empty() {
            // (re)build the lowercase text only for conversations that changed
            for c in &self.conversations {
                let fresh = self.search_cache.get(&c.id).is_some_and(|(u, _)| *u == c.updated);
                if !fresh {
                    let mut text = c.title.to_lowercase();
                    for m in &c.messages {
                        text.push('\n');
                        text.push_str(&m.content.to_lowercase());
                    }
                    self.search_cache.insert(c.id, (c.updated, text));
                }
            }
        }
        let cache = &self.search_cache;
        let matches = |c: &Conversation| {
            query.is_empty() || cache.get(&c.id).is_some_and(|(_, t)| t.contains(&query))
        };
        let pinned: Vec<&Conversation> = self.conversations.iter().filter(|c| c.pinned && matches(c)).collect();
        let others: Vec<&Conversation> = self.conversations.iter().filter(|c| !c.pinned && matches(c)).collect();

        let mut open = None;
        let mut delete = None;
        let mut pin = None;
        egui::ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
            if pinned.is_empty() && others.is_empty() {
                ui.label(RichText::new("No match.").small().color(Color32::GRAY));
            }
            for (title, list) in [("PINNED", &pinned), ("RECENT", &others)] {
                if list.is_empty() {
                    continue;
                }
                if !pinned.is_empty() {
                    ui.label(RichText::new(title).small().monospace().color(Color32::from_gray(110)));
                    ui.add_space(2.0);
                }
                for c in list.iter() {
                    let active = self.current == Some(c.id);
                    ui.horizontal(|ui| {
                        // fixed width so long titles are cut with "…" instead of
                        // pushing the pin / delete buttons out of the panel
                        let w = (ui.available_width() - 48.0).max(40.0);
                        let resp = ui
                            .add_enabled_ui(!streaming, |ui| {
                                ui.add_sized(
                                    Vec2::new(w, 26.0),
                                    egui::Button::selectable(active, RichText::new(&c.title).size(13.0)).truncate(),
                                )
                            })
                            .inner;
                        if resp.clicked() {
                            open = Some(c.id);
                        }
                        resp.on_hover_text(format!("{} · {} messages", c.model, c.messages.len()));
                        let star = if c.pinned { RichText::new("★").color(theme::ACCENT_BRIGHT) } else { RichText::new("☆") };
                        if ui.add(egui::Button::new(star).small().frame(false)).on_hover_text(if c.pinned { "Unpin" } else { "Pin" }).clicked() {
                            pin = Some(c.id);
                        }
                        if ui.add_enabled(!streaming, egui::Button::new("×").small().frame(false)).on_hover_text("Delete").clicked() {
                            delete = Some(c.id);
                        }
                    });
                }
                ui.add_space(10.0);
            }
        });
        if let Some(id) = open {
            self.open_conversation(id);
        }
        if let Some(id) = pin {
            self.toggle_pin(id);
        }
        if let Some(id) = delete {
            self.delete_conversation(id);
        }
    }
}

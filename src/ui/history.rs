
use eframe::egui::{self, Color32, Pos2, RichText, Vec2};

use crate::app::App;
use crate::ui::theme;
use crate::backend::{Event, Role};
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
        let existing = self.conversations.iter().find(|c| c.id == id);
        let pinned = existing.is_some_and(|c| c.pinned);
        let renamed = existing.is_some_and(|c| c.renamed);
        let title = match existing {
            Some(c) if c.renamed => c.title.clone(),
            _ => store::title_from(&self.messages),
        };
        let conv = Conversation { pinned, renamed, id, title, model, updated: now, messages: self.messages.clone() };
        self.conversations.retain(|c| c.id != id);
        self.writer.save(conv.clone());
        self.conversations.insert(0, conv);
    }

    pub(crate) fn new_chat(&mut self) {
        self.stop();
        self.messages.clear();
        self.current = None;
        self.usage = None;
        self.focus_input = true;
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
        self.focus_input = true;
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
    /// Returns where the next notice goes.
    pub(crate) fn undo_bar(&mut self, ui: &mut egui::Ui, (anchor, dir): (Pos2, f32), now: f64) -> (Pos2, f32) {
        let Some((conv, _, start)) = &mut self.undo_delete else { return (anchor, dir) };
        if *start == 0.0 {
            *start = now;
        }
        if now - *start > UNDO_SECONDS {
            self.undo_delete = None;
            return (anchor, dir);
        }
        ui.ctx().request_repaint_after(std::time::Duration::from_secs_f64(UNDO_SECONDS - (now - *start))); // to hide it
        let title = conv.title.clone();
        let bar = crate::ui::system::notice_rect(anchor, dir, Vec2::new(340.0, 30.0));
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
        (Pos2::new(anchor.x, anchor.y + dir * (bar.height() + 6.0)), dir)
    }

    /// Sets a title by hand (an empty one goes back to the first message).
    pub(crate) fn rename_conversation(&mut self, id: u64, title: &str) {
        let Some(c) = self.conversations.iter_mut().find(|c| c.id == id) else { return };
        let title = title.trim();
        c.renamed = !title.is_empty();
        c.title = if c.renamed { title.chars().take(80).collect() } else { store::title_from(&c.messages) };
        self.writer.save(c.clone());
    }

    /// Saves the conversation as a Markdown file chosen in the system dialog.
    pub(crate) fn export_conversation(&self, id: u64) {
        let Some(c) = self.conversations.iter().find(|c| c.id == id) else { return };
        let markdown = store::to_markdown(c);
        let name = format!("{}.md", store::file_name_for(&c.title));
        let sink = self.sink.clone();
        std::thread::spawn(move || {
            let dialog = rfd::FileDialog::new().set_title("Export conversation").set_file_name(&name).add_filter("Markdown", &["md"]);
            let Some(path) = dialog.save_file() else { return };
            let msg = match std::fs::write(&path, markdown) {
                Ok(()) => format!("Exported to {}", path.display()),
                Err(e) => format!("Could not export: {e}"),
            };
            sink.send(Event::Notice(msg));
        });
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
        let search = ui.add(
            egui::TextEdit::singleline(&mut self.history_query)
                .hint_text("Search…  (Ctrl+K)")
                .desired_width(f32::INFINITY),
        );
        if std::mem::take(&mut self.focus_search) {
            search.request_focus();
        }
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
        // pinned first, then the others by age (the list is sorted, newest first)
        let midnight = local_midnight();
        let mut groups: Vec<(&str, Vec<&Conversation>)> = Vec::new();
        for c in self.conversations.iter().filter(|c| matches(c)) {
            let group = if c.pinned { "PINNED" } else { age_group(c.updated / 1000, midnight) };
            match groups.iter_mut().find(|(name, _)| *name == group) {
                Some((_, list)) => list.push(c),
                None => groups.push((group, vec![c])),
            }
        }
        groups.sort_by_key(|(name, _)| GROUPS.iter().position(|g| g == name));

        let mut open = None;
        let mut delete = None;
        let mut pin = None;
        let mut rename = None;
        let mut renamed = None;
        let mut export = None;
        egui::ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
            if groups.is_empty() {
                ui.label(RichText::new("No match.").small().color(Color32::GRAY));
            }
            for (group, list) in &groups {
                ui.label(RichText::new(*group).small().monospace().color(Color32::from_gray(110)));
                ui.add_space(2.0);
                for c in list {
                    let active = self.current == Some(c.id);
                    ui.horizontal(|ui| {
                        // fixed width so long titles are cut with "…" instead of
                        // pushing the pin / delete buttons out of the panel
                        let w = (ui.available_width() - 48.0).max(40.0);
                        if let Some((id, text)) = self.renaming.as_mut().filter(|(id, _)| *id == c.id) {
                            // Enter or a click elsewhere saves, Esc cancels
                            let edit = ui.add_sized(Vec2::new(w, 26.0), egui::TextEdit::singleline(text));
                            if std::mem::take(&mut self.focus_rename) {
                                edit.request_focus();
                            }
                            if edit.lost_focus() {
                                let cancelled = ui.input(|i| i.key_pressed(egui::Key::Escape));
                                renamed = Some((*id, (!cancelled).then(|| text.clone())));
                            }
                            return;
                        }
                        let resp = ui
                            .add_enabled_ui(!streaming, |ui| {
                                ui.add_sized(
                                    Vec2::new(w, 26.0),
                                    // the empty right text pushes the title to the left
                                    egui::Button::selectable(active, RichText::new(&c.title).size(13.0)).truncate().right_text(""),
                                )
                            })
                            .inner;
                        if resp.clicked() {
                            open = Some(c.id);
                        }
                        if resp.double_clicked() {
                            rename = Some(c.id);
                        }
                        resp.context_menu(|ui| {
                            if ui.button("Rename").clicked() {
                                rename = Some(c.id);
                                ui.close();
                            }
                            if ui.button(if c.pinned { "Unpin" } else { "Pin" }).clicked() {
                                pin = Some(c.id);
                                ui.close();
                            }
                            if ui.button("Export as Markdown…").clicked() {
                                export = Some(c.id);
                                ui.close();
                            }
                            ui.separator();
                            if ui.add_enabled(!streaming, egui::Button::new("Delete")).clicked() {
                                delete = Some(c.id);
                                ui.close();
                            }
                        });
                        resp.on_hover_text(format!(
                            "{} · {} messages\nDouble-click to rename, right-click for more",
                            c.model,
                            c.messages.len()
                        ));
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
        if let Some(id) = export {
            self.export_conversation(id);
        }
        if let Some(id) = rename {
            let title = self.conversations.iter().find(|c| c.id == id).map(|c| c.title.clone()).unwrap_or_default();
            self.renaming = Some((id, title));
            self.focus_rename = true;
        }
        if let Some((id, title)) = renamed {
            self.renaming = None;
            if let Some(title) = title {
                self.rename_conversation(id, &title);
            }
        }
    }
}

/// History groups, in display order.
const GROUPS: &[&str] = &["PINNED", "TODAY", "YESTERDAY", "PREVIOUS 7 DAYS", "PREVIOUS 30 DAYS", "OLDER"];

/// Group of a conversation last updated at `secs`, given the last local midnight.
fn age_group(secs: u64, midnight: u64) -> &'static str {
    const DAY: u64 = 86_400;
    if secs >= midnight {
        "TODAY"
    } else if secs >= midnight.saturating_sub(DAY) {
        "YESTERDAY"
    } else if secs >= midnight.saturating_sub(7 * DAY) {
        "PREVIOUS 7 DAYS"
    } else if secs >= midnight.saturating_sub(30 * DAY) {
        "PREVIOUS 30 DAYS"
    } else {
        "OLDER"
    }
}

/// Unix time of the last local midnight.
fn local_midnight() -> u64 {
    let now = (store::now_ms() / 1000) as libc::time_t;
    // SAFETY: localtime_r and mktime only read and write the given struct
    unsafe {
        let mut tm: libc::tm = std::mem::zeroed();
        libc::localtime_r(&now, &mut tm);
        tm.tm_hour = 0;
        tm.tm_min = 0;
        tm.tm_sec = 0;
        tm.tm_isdst = -1; // let mktime work out daylight saving time
        libc::mktime(&mut tm).max(0) as u64
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn conversations_are_grouped_by_age() {
        let midnight = 1_000_000_000;
        assert_eq!(age_group(midnight + 10, midnight), "TODAY");
        assert_eq!(age_group(midnight - 10, midnight), "YESTERDAY");
        assert_eq!(age_group(midnight - 3 * 86_400, midnight), "PREVIOUS 7 DAYS");
        assert_eq!(age_group(midnight - 20 * 86_400, midnight), "PREVIOUS 30 DAYS");
        assert_eq!(age_group(midnight - 400 * 86_400, midnight), "OLDER");
        let now = store::now_ms() / 1000;
        assert!(local_midnight() <= now && now - local_midnight() < 86_400 + 3600);
    }
}

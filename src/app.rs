use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::sync::Arc;

use eframe::egui::{self, Color32, Rect, RichText, Vec2};

use crate::background::{draw_background, next_change};
use crate::backend::ollama::{url_from_env, Ollama};
use crate::backend::{Usage, Backend, ChatMessage, Event, InstalledModel, RuntimeStatus, Sink};
use crate::attach::{estimate_tokens, prompt_len, Attachment};
use crate::store::{self, Conversation};
use crate::system::SystemInfo;
use crate::ui::widgets::*;

pub(crate) struct PullState {
    pub(crate) status: String,
    pub(crate) completed: u64,
    pub(crate) total: u64,
    pub(crate) cancel: Arc<AtomicBool>,
}

/// A live model list from ollama.com and its filters (Models and Uncensored windows).
#[derive(Default)]
pub(crate) struct BrowserState {
    pub(crate) models: Vec<crate::discover::RemoteModel>,
    /// Unix seconds of the list shown (0 = never fetched).
    pub(crate) fetched_at: u64,
    pub(crate) discovering: bool,
    pub(crate) error: Option<String>,
    pub(crate) filter: String,
    /// Size tag picked per model name.
    pub(crate) size: HashMap<String, String>,
    /// Number of rows shown (grows with "Show more").
    pub(crate) limit: usize,
    pub(crate) was_open: bool,
    pub(crate) filters: Filters,
}

#[derive(Clone, Copy, Default, PartialEq)]
pub(crate) enum Source {
    #[default]
    All,
    Official,
    Community,
}

#[derive(Clone, Copy, Default, PartialEq)]
pub(crate) enum SortBy {
    #[default]
    Downloads,
    Name,
    Smallest,
}

#[derive(Clone, Default, PartialEq)]
pub(crate) struct Filters {
    pub(crate) source: Source,
    pub(crate) sort: SortBy,
    /// Maximum parameters in billions (`None` = any).
    pub(crate) max_params: Option<f64>,
    /// Only models that fit in this machine's memory.
    pub(crate) fits: bool,
    pub(crate) vision: bool,
    pub(crate) tools: bool,
    pub(crate) thinking: bool,
    /// Hide models whose size is unknown.
    pub(crate) known_size: bool,
}

pub(crate) struct App {
    pub(crate) backend: Box<dyn Backend>,
    pub(crate) rx: Receiver<Event>,
    pub(crate) status: RuntimeStatus,
    pub(crate) installed: Vec<InstalledModel>,
    pub(crate) sys: SystemInfo,
    pub(crate) selected: Option<String>,

    pub(crate) pulls: HashMap<String, PullState>,
    pub(crate) errors: HashMap<String, String>,
    pub(crate) confirm_delete: Option<String>,
    pub(crate) custom_pull: String,

    pub(crate) input: String,
    /// Files attached to the message being typed.
    pub(crate) pending: Vec<Attachment>,
    /// Whether each model accepts images (from the runtime's capabilities).
    pub(crate) vision: HashMap<String, bool>,
    /// Whether each model can stream its reasoning (the runtime's "thinking" capability).
    pub(crate) thinks: HashMap<String, bool>,
    pub(crate) messages: Vec<ChatMessage>,
    pub(crate) chat_cancel: Option<Arc<AtomicBool>>,

    pub(crate) show_models: bool,
    pub(crate) show_system: bool,
    pub(crate) show_uncensored: bool,
    pub(crate) unc: BrowserState,
    pub(crate) library: BrowserState,
    /// Registry details per model tag, shared by both lists; `None` while loading.
    pub(crate) remote_details: HashMap<String, Option<Result<crate::discover::Details, String>>>,
    /// Tags waiting for a registry lookup, and lookups running.
    pub(crate) details_queue: VecDeque<String>,
    pub(crate) details_in_flight: usize,
    /// Successful lookups with their time, saved to disk.
    pub(crate) details_cache: HashMap<String, (u64, crate::discover::Details)>,
    pub(crate) toast: Option<(String, f64)>,

    pub(crate) conversations: Vec<Conversation>,
    /// Id of the conversation shown in the chat, `None` for a new unsaved one.
    pub(crate) current: Option<u64>,
    pub(crate) show_history: bool,
    pub(crate) history_query: String,
    /// Used to detect when the Models window gets opened.
    pub(crate) models_was_open: bool,
    /// The Models window was opened by itself because no model is installed.
    pub(crate) models_prompted: bool,
    /// Context length per model (`None` while unknown / being fetched).
    pub(crate) ctx_limits: HashMap<String, Option<u64>>,
    /// Token usage of the last reply in the current chat.
    pub(crate) usage: Option<Usage>,
    pub(crate) settings: store::Settings,
    /// Hardware probe running in the background.
    pub(crate) probing: bool,
    pub(crate) sink: Sink,
    /// Lowercased searchable text per conversation id, tagged with its `updated` stamp.
    pub(crate) search_cache: HashMap<u64, (u64, String)>,
    pub(crate) md_cache: egui_commonmark::CommonMarkCache,
    /// Per message: a fingerprint of what it looked like and its height, so
    /// messages out of view can be skipped (see `chat`).
    pub(crate) msg_layout: Vec<(u64, f32)>,
    /// Recently deleted conversation, its former position and when the undo bar appeared.
    pub(crate) undo_delete: Option<(Conversation, usize, f64)>,
    /// Saves conversations on a background thread.
    pub(crate) writer: store::Writer,
    /// When the runtime was last checked (app time, seconds).
    pub(crate) last_health: f64,
}

/// How often to check that the runtime still answers, in seconds.
const HEALTH_INTERVAL: f64 = 10.0;

/// Used until the model's real limit is known.
const FALLBACK_NUM_CTX: u64 = 4096;


impl App {
    pub(crate) fn new(ctx: egui::Context) -> Self {
        ctx.set_visuals(square_visuals());
        install_fonts(&ctx);
        let (tx, rx) = mpsc::channel();
        let sink = Sink::new(tx, ctx);
        sink.probe_system();
        let backend = Ollama::new(&url_from_env(), sink.clone());
        backend.ensure_running();
        let loaded = store::load();
        let details_cache = crate::discover::load_details_cache();
        let writer = {
            let sink = sink.clone();
            store::Writer::spawn(move |e| sink.send(Event::Notice(e)))
        };
        Self {
            backend: Box::new(backend),
            rx,
            status: RuntimeStatus::Checking,
            installed: Vec::new(),
            sys: SystemInfo::default(),
            probing: true,
            sink,
            search_cache: HashMap::new(),
            md_cache: Default::default(),
            msg_layout: Vec::new(),
            undo_delete: None,
            selected: None,
            pulls: HashMap::new(),
            errors: HashMap::new(),
            confirm_delete: None,
            custom_pull: String::new(),
            input: String::new(),
            pending: Vec::new(),
            vision: HashMap::new(),
            thinks: HashMap::new(),
            messages: Vec::new(),
            chat_cancel: None,
            show_models: false,
            show_system: false,
            show_uncensored: false,
            unc: BrowserState::default(),
            library: BrowserState::default(),
            remote_details: details_cache.iter().map(|(tag, (_, d))| (tag.clone(), Some(Ok(d.clone())))).collect(),
            details_queue: VecDeque::new(),
            details_in_flight: 0,
            details_cache,
            toast: loaded.warning.map(|w| (w, 0.0)),
            conversations: loaded.conversations,
            writer,
            last_health: 0.0,
            current: None,
            show_history: false,
            history_query: String::new(),
            models_was_open: false,
            models_prompted: false,
            ctx_limits: HashMap::new(),
            usage: None,
            settings: store::load_settings(),
        }
    }

    pub(crate) fn is_installed(&self, tag: &str) -> bool {
        self.installed.iter().any(|m| m.name == tag || m.name == format!("{tag}:latest"))
    }

    pub(crate) fn streaming(&self) -> bool {
        self.chat_cancel.is_some()
    }

    pub(crate) fn handle_events(&mut self) {
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
                        // restore the last used model if it is still installed
                        let last = self.settings.last_model.clone().filter(|l| self.installed.iter().any(|m| &m.name == l));
                        self.selected = last.or_else(|| self.installed.first().map(|m| m.name.clone()));
                    }
                    // no model yet: open the model manager, once (not after every refresh)
                    if self.installed.is_empty() && self.pulls.is_empty() && !self.models_prompted {
                        self.models_prompted = true;
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
                            self.refresh_system(); // disk space changed
                        }
                    }
                }
                Event::Deleted { model, error } => {
                    if let Some(e) = error {
                        self.errors.insert(model, e);
                    } else {
                        self.refresh_system();
                    }
                }
                Event::ChatToken(tok) => {
                    if let Some(last) = self.messages.last_mut() {
                        last.content.push_str(&tok);
                    }
                }
                Event::ChatThinking(t) => {
                    if let Some(last) = self.messages.last_mut() {
                        last.thinking.push_str(&t);
                    }
                }
                Event::ChatDone { error, usage } => {
                    if error.is_some() {
                        self.backend.check(); // the runtime may have gone away
                    }
                    let stopped = self.chat_cancel.take().is_some_and(|c| c.load(Ordering::Relaxed));
                    if let Some(last) = self.messages.last_mut() {
                        last.stopped = stopped;
                        last.error = error; // shown apart, never sent back to the model
                    }
                    if usage.is_some() {
                        self.usage = usage;
                    }
                    self.persist();
                }
                Event::System(info) => {
                    self.sys = *info;
                    self.probing = false;
                }
                Event::ModelInfo { model, context_length, vision, thinking } => {
                    self.vision.insert(model.clone(), vision);
                    self.thinks.insert(model.clone(), thinking);
                    self.ctx_limits.insert(model, context_length);
                }
                Event::Attached(results) => self.add_attachments(results),
                Event::Discovered { kind, result } => {
                    let b = self.browser_mut(kind);
                    b.discovering = false;
                    match result {
                        Ok(models) => {
                            b.models = models;
                            b.fetched_at = crate::discover::load_cache(kind).map(|c| c.1).unwrap_or(0);
                            b.error = None;
                        }
                        Err(e) => b.error = Some(e),
                    }
                }
                Event::RemoteDetails { tag, result } => {
                    self.details_in_flight = self.details_in_flight.saturating_sub(1);
                    if let Ok(d) = &result {
                        self.details_cache.insert(tag.clone(), (crate::discover::now_secs(), d.clone()));
                    }
                    self.remote_details.insert(tag, Some(result));
                    self.pump_details();
                    if self.details_in_flight == 0 && self.details_queue.is_empty() {
                        crate::discover::save_details_cache(&self.details_cache); // batch done
                    }
                }
                Event::Notice(msg) => self.notify(&msg),
                Event::Health(result) => match (result, &self.status) {
                    (Ok(version), RuntimeStatus::Online { .. } | RuntimeStatus::Offline { .. }) => {
                        // back after an outage (e.g. the service was restarted)
                        if matches!(self.status, RuntimeStatus::Offline { .. }) {
                            self.backend.refresh_models();
                        }
                        self.status = RuntimeStatus::Online { version };
                    }
                    (Err(e), RuntimeStatus::Online { .. }) => {
                        self.status = RuntimeStatus::Offline { error: format!("Ollama stopped responding: {e}") };
                    }
                    _ => {} // still down, or a start is in progress
                },
            }
        }
    }

    pub(crate) fn start_pull(&mut self, tag: &str) {
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

    pub(crate) fn send(&mut self) {
        let text = self.input.trim().to_string();
        if (text.is_empty() && self.pending.is_empty()) || self.streaming() {
            return;
        }
        if self.selected.is_none() {
            self.notify("Download or select a model first");
            self.show_models = true;
            return;
        }
        if self.pending.iter().any(Attachment::is_image) && !self.selected_has_vision() {
            self.notify("This model does not accept images");
            return;
        }
        self.input.clear();
        let attachments = std::mem::take(&mut self.pending);
        self.messages.push(ChatMessage { attachments, ..ChatMessage::new("user", text) });
        self.ask();
    }

    /// Drops the last reply and asks again (with the currently selected model).
    pub(crate) fn regenerate(&mut self) {
        if self.streaming() || self.selected.is_none() {
            return;
        }
        if self.messages.last().is_some_and(|m| m.role == "assistant") {
            self.messages.pop();
            self.ask();
        }
    }

    /// Sends the current history and streams the reply into a new message.
    fn ask(&mut self) {
        let Some(model) = self.selected.clone() else { return };
        let history = self.messages.clone();
        self.messages.push(ChatMessage { model: Some(model.clone()), ..ChatMessage::new("assistant", "") });
        let cancel = Arc::new(AtomicBool::new(false));
        self.chat_cancel = Some(cancel.clone());
        self.persist();
        let num_ctx = self.num_ctx(&model);
        let think = self.thinks.get(&model).copied().unwrap_or(false);
        self.backend.chat(&model, history, num_ctx, think, cancel);
    }

    /// Context window to request: the model's maximum, capped by the setting
    /// (a larger window needs more memory for the KV cache).
    pub(crate) fn num_ctx(&self, model: &str) -> u64 {
        let cap = self.settings.max_context.unwrap_or(u64::MAX);
        match self.ctx_limits.get(model) {
            Some(Some(max)) => (*max).min(cap),
            _ => FALLBACK_NUM_CTX.min(cap),
        }
    }

    /// When the message being written would not fit in the context window:
    /// a rough token count of the whole request, and the window size.
    pub(crate) fn context_overflow(&self) -> Option<(u64, u64)> {
        let model = self.selected.as_deref()?;
        let composing = estimate_tokens(prompt_len(&self.input, &self.pending));
        if self.input.trim().is_empty() && self.pending.is_empty() {
            return None;
        }
        // tokens counted by the runtime for the conversation so far, else an estimate
        let history = match self.usage {
            Some(u) => u.prompt_tokens + u.completion_tokens,
            None => self.messages.iter().map(|m| estimate_tokens(prompt_len(&m.content, &m.attachments))).sum(),
        };
        let window = self.num_ctx(model);
        (history + composing > window).then_some((history + composing, window))
    }

    /// Asks the runtime for the selected model's context length (once per model).
    pub(crate) fn ensure_model_info(&mut self) {
        if let Some(m) = &self.selected {
            if !self.ctx_limits.contains_key(m) && matches!(self.status, RuntimeStatus::Online { .. }) {
                self.ctx_limits.insert(m.clone(), None);
                self.backend.model_info(m);
            }
        }
    }

    pub(crate) fn refresh_system(&mut self) {
        if !self.probing {
            self.probing = true;
            self.sink.probe_system();
        }
    }

    pub(crate) fn stop(&mut self) {
        if let Some(c) = &self.chat_cancel {
            c.store(true, Ordering::Relaxed);
        }
    }

    /// `true` while unknown, so a slow model probe never blocks sending.
    pub(crate) fn selected_has_vision(&self) -> bool {
        self.selected.as_ref().is_none_or(|m| self.vision.get(m).copied().unwrap_or(true))
    }

    /// Opens the system file dialog on a background thread.
    pub(crate) fn pick_files(&self) {
        let sink = self.sink.clone();
        std::thread::spawn(move || {
            let Some(paths) = rfd::FileDialog::new().set_title("Attach files").pick_files() else { return };
            sink.send(Event::Attached(paths.iter().map(|p| Attachment::load(p)).collect()));
        });
    }

    /// Reads files (images are base64-encoded, up to 20 MB) off the UI thread.
    pub(crate) fn load_attachments(&self, paths: Vec<std::path::PathBuf>) {
        let sink = self.sink.clone();
        std::thread::spawn(move || sink.send(Event::Attached(paths.iter().map(|p| Attachment::load(p)).collect())));
    }

    pub(crate) fn add_attachments(&mut self, results: Vec<Result<Attachment, String>>) {
        let mut errors = Vec::new();
        for r in results {
            match r {
                Ok(a) => {
                    if !self.pending.contains(&a) {
                        self.pending.push(a);
                    }
                }
                Err(e) => errors.push(e),
            }
        }
        if !errors.is_empty() {
            self.notify(&errors.join(" · "));
        }
    }

    pub(crate) fn notify(&mut self, msg: &str) {
        self.toast = Some((msg.to_string(), 0.0));
    }

    pub(crate) fn copy_button(&mut self, ui: &mut egui::Ui, url: &str) {
        if ui.small_button("⧉ Copy").on_hover_text("Copy link").clicked() {
            ui.ctx().copy_text(url.to_string());
            self.notify("Link copied");
        }
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.handle_events();
        self.ensure_model_info();
        // remember the selected model across restarts
        if self.selected.is_some() && self.selected != self.settings.last_model {
            self.settings.last_model = self.selected.clone();
            let _ = store::save_settings(&self.settings);
        }
        let now = ctx.input(|i| i.time);
        // notice when the runtime stops or comes back
        let settled = matches!(self.status, RuntimeStatus::Online { .. } | RuntimeStatus::Offline { .. });
        if settled && now - self.last_health > HEALTH_INTERVAL {
            self.last_health = now;
            self.backend.check();
        }
        // Repaint only when something changes: tokens, events and input wake the
        // UI on their own; the background asks for its next change, and stays
        // still while the window is not focused.
        let waiting = self.streaming() && self.messages.last().is_some_and(|m| m.content.is_empty());
        let focused = ctx.input(|i| i.viewport().focused.unwrap_or(true));
        if waiting {
            ctx.request_repaint(); // smooth loading shimmer until the first token
        } else if self.settings.animations && focused {
            ctx.request_repaint_after(next_change(now as f32));
        }
        if settled {
            ctx.request_repaint_after(std::time::Duration::from_secs_f64(HEALTH_INTERVAL));
        }

        let mut history_open = self.show_history;
        egui::Panel::left("history")
            .resizable(false)
            .exact_size(250.0)
            .frame(egui::Frame::new().fill(Color32::from_rgb(16, 16, 19)).inner_margin(10.0))
            .show_collapsible(ui, &mut history_open, |ui| self.history_panel(ui));
        // the close button inside the panel may have changed `show_history`
        self.show_history = self.show_history && history_open;

        egui::CentralPanel::default().frame(egui::Frame::NONE).show(ui, |ui| {
            let rect = ui.max_rect();
            draw_background(ui.painter(), rect, now as f32, self.settings.animations, !self.messages.is_empty());
            self.top_bar(ui, rect);
            if !self.show_history {
                let btn = Rect::from_min_size(rect.min + Vec2::new(12.0, 12.0), Vec2::new(36.0, 30.0));
                if ui.put(btn, egui::Button::new(RichText::new("☰").size(16.0))).on_hover_text("History").clicked() {
                    self.show_history = true;
                }
            }
            self.chat(ui, rect);
            self.toast(ui, rect, now);
            self.undo_bar(ui, rect, now);
        });

        self.models_window(&ctx);
        self.system_window(&ctx);
        self.uncensored_window(&ctx);
    }

    fn on_exit(&mut self) {
        self.writer.close(); // finish pending history writes
        self.backend.shutdown(); // clean stop of an `ollama serve` we started
    }
}

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::sync::Arc;

use eframe::egui::{self, Color32, Pos2, Rect, RichText, Vec2};

use crate::background::{draw_background, next_change};
use crate::backend::ollama::{url_from_env, Ollama};
use crate::backend::{Backend, ChatMessage, Event, InstalledModel, Role, RuntimeStatus, Sink, Usage};
use crate::attach::{estimate_tokens, prompt_len, Attachment};
use crate::store::{self, Conversation};
use crate::discover::{Details, ListKind};
use crate::system::SystemInfo;
use crate::ui::widgets::*;

pub(crate) struct PullState {
    pub(crate) status: String,
    pub(crate) completed: u64,
    pub(crate) total: u64,
    pub(crate) cancel: Arc<AtomicBool>,
    /// Smoothed download speed in bytes per second (0 until measured).
    pub(crate) rate: f64,
    /// Last speed sample: when, and how many bytes were done then.
    pub(crate) sample: Option<(std::time::Instant, u64)>,
}

impl PullState {
    /// Folds a progress report into the speed estimate (sampled every half
    /// second; Ollama downloads layer by layer, so a new layer restarts it).
    fn measure(&mut self, completed: u64) {
        let now = std::time::Instant::now();
        match self.sample {
            Some((at, done)) if completed >= done => {
                let dt = now.duration_since(at).as_secs_f64();
                if dt >= 0.5 {
                    let speed = (completed - done) as f64 / dt;
                    self.rate = if self.rate == 0.0 { speed } else { self.rate * 0.7 + speed * 0.3 };
                    self.sample = Some((now, completed));
                }
            }
            _ => self.sample = Some((now, completed)),
        }
    }
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

/// The app: the runtime backend and its event channel, the settings, and the
/// state grouped by area. The UI code lives in `ui/`, as `impl App` blocks.
pub(crate) struct App {
    pub(crate) backend: Box<dyn Backend>,
    pub(crate) rx: Receiver<Event>,
    pub(crate) sink: Sink,
    pub(crate) settings: store::Settings,
    pub(crate) runtime: Runtime,
    pub(crate) hw: Hardware,
    pub(crate) chat: Chat,
    pub(crate) history: History,
    pub(crate) catalog: Catalog,
    pub(crate) view: View,
}

/// The inference runtime and its models.
pub(crate) struct Runtime {
    pub(crate) status: RuntimeStatus,
    pub(crate) installed: Vec<InstalledModel>,
    pub(crate) selected: Option<String>,
    /// Downloads in progress, and the last error per model tag.
    pub(crate) pulls: HashMap<String, PullState>,
    pub(crate) errors: HashMap<String, String>,
    /// Whether each model accepts images (from the runtime's capabilities).
    pub(crate) vision: HashMap<String, bool>,
    /// Whether each model can stream its reasoning (the "thinking" capability).
    pub(crate) thinks: HashMap<String, bool>,
    /// Context length per model (`None` while unknown / being fetched).
    pub(crate) ctx_limits: HashMap<String, Option<u64>>,
    /// When the runtime was last checked (app time, seconds).
    pub(crate) last_health: f64,
}

impl Runtime {
    pub(crate) fn is_installed(&self, tag: &str) -> bool {
        self.installed.iter().any(|m| m.name == tag || m.name == format!("{tag}:latest"))
    }

    /// `true` while unknown, so a slow model probe never blocks sending.
    pub(crate) fn selected_has_vision(&self) -> bool {
        self.selected.as_ref().is_none_or(|m| self.vision.get(m).copied().unwrap_or(true))
    }
}

/// This machine's hardware, probed in the background.
pub(crate) struct Hardware {
    pub(crate) info: SystemInfo,
    /// A probe is running: memory and disk are not known yet.
    pub(crate) probing: bool,
}

/// The conversation shown in the chat and the message being written.
pub(crate) struct Chat {
    pub(crate) messages: Vec<ChatMessage>,
    pub(crate) input: String,
    /// Files attached to the message being written.
    pub(crate) pending: Vec<Attachment>,
    /// Set while a reply streams; storing `true` in it stops the reply.
    pub(crate) cancel: Option<Arc<AtomicBool>>,
    /// Token usage of the last reply in this conversation.
    pub(crate) usage: Option<Usage>,
    pub(crate) md_cache: egui_commonmark::CommonMarkCache,
    /// Per message: a fingerprint of what it looked like and its height, so
    /// messages out of view can be skipped (see `ui::chat`).
    pub(crate) msg_layout: Vec<(u64, f32)>,
    /// Previews of image attachments, by `attach::fingerprint`.
    pub(crate) thumbs: HashMap<u64, Thumb>,
}

impl Chat {
    pub(crate) fn streaming(&self) -> bool {
        self.cancel.is_some()
    }

    pub(crate) fn stop(&self) {
        if let Some(c) = &self.cancel {
            c.store(true, Ordering::Relaxed);
        }
    }
}

/// Saved conversations and the history panel.
pub(crate) struct History {
    pub(crate) conversations: Vec<Conversation>,
    /// Id of the conversation shown in the chat, `None` for a new unsaved one.
    pub(crate) current: Option<u64>,
    /// Text of the search box.
    pub(crate) query: String,
    /// Lowercased searchable text per conversation id, tagged with its `updated` stamp.
    pub(crate) search_cache: HashMap<u64, (u64, String)>,
    /// Recently deleted conversation, its former position and when the undo bar appeared.
    pub(crate) undo_delete: Option<(Conversation, usize, f64)>,
    /// Conversation whose title is being edited, and the text.
    pub(crate) renaming: Option<(u64, String)>,
    pub(crate) focus_rename: bool,
    /// Saves conversations on a background thread.
    pub(crate) writer: store::Writer,
}

impl History {
    pub(crate) fn toggle_pin(&mut self, id: u64) {
        if let Some(c) = self.conversations.iter_mut().find(|c| c.id == id) {
            c.pinned = !c.pinned;
            self.writer.save(c.clone());
        }
    }

    /// Sets a title by hand (an empty one goes back to the first message).
    pub(crate) fn rename(&mut self, id: u64, title: &str) {
        let Some(c) = self.conversations.iter_mut().find(|c| c.id == id) else { return };
        let title = title.trim();
        c.renamed = !title.is_empty();
        c.title = if c.renamed { title.chars().take(80).collect() } else { store::title_from(&c.messages) };
        self.writer.save(c.clone());
    }

    /// Puts back the conversation deleted last, at its former place.
    pub(crate) fn undo_delete(&mut self) {
        if let Some((conv, pos, _)) = self.undo_delete.take() {
            let pos = pos.min(self.conversations.len());
            self.writer.save(conv.clone());
            self.conversations.insert(pos, conv);
        }
    }
}

/// Model lists from ollama.com and their registry details.
pub(crate) struct Catalog {
    pub(crate) library: BrowserState,
    pub(crate) unc: BrowserState,
    /// Registry details per model tag, shared by both lists; `None` while loading.
    pub(crate) details: HashMap<String, Option<Result<Details, String>>>,
    /// Tags waiting for a registry lookup, and lookups running.
    pub(crate) queue: VecDeque<String>,
    pub(crate) in_flight: usize,
    /// Successful lookups with their time, saved to disk.
    pub(crate) cache: HashMap<String, (u64, Details)>,
}

impl Catalog {
    pub(crate) fn browser(&self, kind: ListKind) -> &BrowserState {
        match kind {
            ListKind::Library => &self.library,
            ListKind::Uncensored => &self.unc,
        }
    }

    pub(crate) fn browser_mut(&mut self, kind: ListKind) -> &mut BrowserState {
        match kind {
            ListKind::Library => &mut self.library,
            ListKind::Uncensored => &mut self.unc,
        }
    }
}

/// Open windows, notices and keyboard focus requests.
pub(crate) struct View {
    pub(crate) show_history: bool,
    pub(crate) show_models: bool,
    pub(crate) show_system: bool,
    pub(crate) show_uncensored: bool,
    /// Used to detect when the Models window gets opened.
    pub(crate) models_was_open: bool,
    /// The Models window was opened by itself because no model is installed.
    pub(crate) models_prompted: bool,
    /// Installed model waiting for its delete confirmation.
    pub(crate) confirm_delete: Option<String>,
    /// Text of the "Pull any model" box.
    pub(crate) custom_pull: String,
    /// The notice shown, and when it appeared (0 until first drawn).
    pub(crate) toast: Option<(String, f64)>,
    /// Where notices start (next to the message box) and which way they
    /// stack: -1 upwards, 1 downwards. Set by `chat` each frame.
    pub(crate) notice_anchor: (Pos2, f32),
    /// Give the keyboard focus to the message box / the history search on the next frame.
    pub(crate) focus_input: bool,
    pub(crate) focus_search: bool,
}

impl View {
    pub(crate) fn notify(&mut self, msg: &str) {
        self.toast = Some((msg.to_string(), 0.0));
    }

    /// "Copy" button for a link, with a notice once copied.
    pub(crate) fn copy_button(&mut self, ui: &mut egui::Ui, url: &str) {
        if ui.small_button("⧉ Copy").on_hover_text("Copy link").clicked() {
            ui.ctx().copy_text(url.to_string());
            self.notify("Link copied");
        }
    }
}

pub(crate) enum Thumb {
    Loading,
    Ready(egui::TextureHandle),
    /// Not a decodable image: shown by name only.
    Failed,
}

/// How often to check that the runtime still answers, in seconds.
const HEALTH_INTERVAL: f64 = 10.0;

/// Used until the model's real limit is known.
const FALLBACK_NUM_CTX: u64 = 4096;

impl App {
    pub(crate) fn new(ctx: egui::Context) -> Self {
        ctx.set_visuals(crate::ui::theme::visuals());
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
            sink,
            settings: store::load_settings(),
            runtime: Runtime {
                status: RuntimeStatus::Checking,
                installed: Vec::new(),
                selected: None,
                pulls: HashMap::new(),
                errors: HashMap::new(),
                vision: HashMap::new(),
                thinks: HashMap::new(),
                ctx_limits: HashMap::new(),
                last_health: 0.0,
            },
            hw: Hardware { info: SystemInfo::default(), probing: true },
            chat: Chat {
                messages: Vec::new(),
                input: String::new(),
                pending: Vec::new(),
                cancel: None,
                usage: None,
                md_cache: Default::default(),
                msg_layout: Vec::new(),
                thumbs: HashMap::new(),
            },
            history: History {
                conversations: loaded.conversations,
                current: None,
                query: String::new(),
                search_cache: HashMap::new(),
                undo_delete: None,
                renaming: None,
                focus_rename: false,
                writer,
            },
            catalog: Catalog {
                library: BrowserState::default(),
                unc: BrowserState::default(),
                details: details_cache.iter().map(|(tag, (_, d))| (tag.clone(), Some(Ok(d.clone())))).collect(),
                queue: VecDeque::new(),
                in_flight: 0,
                cache: details_cache,
            },
            view: View {
                show_history: false,
                show_models: false,
                show_system: false,
                show_uncensored: false,
                models_was_open: false,
                models_prompted: false,
                confirm_delete: None,
                custom_pull: String::new(),
                toast: loaded.warning.map(|w| (w, 0.0)),
                notice_anchor: (Pos2::ZERO, -1.0),
                focus_input: true,
                focus_search: false,
            },
        }
    }

    pub(crate) fn handle_events(&mut self, ctx: &egui::Context) {
        while let Ok(ev) = self.rx.try_recv() {
            match ev {
                Event::Status(s) => self.runtime.status = s,
                Event::Installed(list) => {
                    self.runtime.installed = list;
                    let still_there = self
                        .runtime
                        .selected
                        .as_ref()
                        .is_some_and(|s| self.runtime.installed.iter().any(|m| &m.name == s));
                    if !still_there {
                        // restore the last used model if it is still installed
                        let last = self.settings.last_model.clone().filter(|l| self.runtime.installed.iter().any(|m| &m.name == l));
                        self.runtime.selected = last.or_else(|| self.runtime.installed.first().map(|m| m.name.clone()));
                    }
                    // no model yet: open the model manager, once (not after every refresh)
                    if self.runtime.installed.is_empty() && self.runtime.pulls.is_empty() && !self.view.models_prompted {
                        self.view.models_prompted = true;
                        self.view.show_models = true;
                    }
                }
                Event::PullProgress { model, status, completed, total } => {
                    if let Some(p) = self.runtime.pulls.get_mut(&model) {
                        p.status = status;
                        if total > 0 {
                            p.measure(completed);
                            p.completed = completed;
                            p.total = total;
                        }
                    }
                }
                Event::PullDone { model, error } => {
                    self.runtime.pulls.remove(&model);
                    match error {
                        Some(e) => {
                            self.runtime.errors.insert(model, e);
                        }
                        None => {
                            self.runtime.errors.remove(&model);
                            if self.runtime.selected.is_none() {
                                self.runtime.selected = Some(model);
                            }
                            self.refresh_system(); // disk space changed
                        }
                    }
                }
                Event::Deleted { model, error } => {
                    if let Some(e) = error {
                        self.runtime.errors.insert(model, e);
                    } else {
                        self.refresh_system();
                    }
                }
                Event::ChatToken(tok) => {
                    if let Some(last) = self.chat.messages.last_mut() {
                        last.content.push_str(&tok);
                    }
                }
                Event::ChatThinking(t) => {
                    if let Some(last) = self.chat.messages.last_mut() {
                        last.thinking.push_str(&t);
                    }
                }
                Event::ChatDone { error, usage } => {
                    if error.is_some() {
                        self.backend.check(); // the runtime may have gone away
                    }
                    let stopped = self.chat.cancel.take().is_some_and(|c| c.load(Ordering::Relaxed));
                    if let Some(last) = self.chat.messages.last_mut() {
                        last.stopped = stopped;
                        last.error = error; // shown apart, never sent back to the model
                    }
                    if usage.is_some() {
                        self.chat.usage = usage;
                    }
                    self.persist();
                }
                Event::System(info) => {
                    self.hw.info = *info;
                    self.hw.probing = false;
                }
                Event::ModelInfo { model, context_length, vision, thinking } => {
                    self.runtime.vision.insert(model.clone(), vision);
                    self.runtime.thinks.insert(model.clone(), thinking);
                    self.runtime.ctx_limits.insert(model, context_length);
                }
                Event::Attached(results) => self.add_attachments(results),
                Event::Discovered { kind, result } => {
                    let b = self.catalog.browser_mut(kind);
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
                    self.catalog.in_flight = self.catalog.in_flight.saturating_sub(1);
                    if let Ok(d) = &result {
                        self.catalog.cache.insert(tag.clone(), (crate::discover::now_secs(), d.clone()));
                    }
                    self.catalog.details.insert(tag, Some(result));
                    self.pump_details();
                    if self.catalog.in_flight == 0 && self.catalog.queue.is_empty() {
                        crate::discover::save_details_cache(&self.catalog.cache); // batch done
                    }
                }
                Event::Notice(msg) => self.view.notify(&msg),
                Event::Thumbnail { key, image } => {
                    let thumb = match image {
                        Some(img) => Thumb::Ready(ctx.load_texture(format!("thumb-{key}"), img, egui::TextureOptions::LINEAR)),
                        None => Thumb::Failed,
                    };
                    self.chat.thumbs.insert(key, thumb);
                }
                Event::Health(result) => match (result, &self.runtime.status) {
                    (Ok(version), RuntimeStatus::Online { .. } | RuntimeStatus::Offline { .. }) => {
                        // back after an outage (e.g. the service was restarted)
                        if matches!(self.runtime.status, RuntimeStatus::Offline { .. }) {
                            self.backend.refresh_models();
                        }
                        self.runtime.status = RuntimeStatus::Online { version };
                    }
                    (Err(e), RuntimeStatus::Online { .. }) => {
                        self.runtime.status = RuntimeStatus::Offline { error: format!("Ollama stopped responding: {e}") };
                    }
                    _ => {} // still down, or a start is in progress
                },
            }
        }
    }

    pub(crate) fn start_pull(&mut self, tag: &str) {
        let tag = tag.trim().to_string();
        if tag.is_empty() || self.runtime.pulls.contains_key(&tag) {
            return;
        }
        let cancel = Arc::new(AtomicBool::new(false));
        self.runtime.errors.remove(&tag);
        self.runtime.pulls.insert(
            tag.clone(),
            PullState { status: "starting".into(), completed: 0, total: 0, cancel: cancel.clone(), rate: 0.0, sample: None },
        );
        self.backend.pull(&tag, cancel);
    }

    pub(crate) fn send(&mut self) {
        let text = self.chat.input.trim().to_string();
        if (text.is_empty() && self.chat.pending.is_empty()) || self.chat.streaming() {
            return;
        }
        if self.runtime.selected.is_none() {
            self.view.notify("Download or select a model first");
            self.view.show_models = true;
            return;
        }
        if self.chat.pending.iter().any(Attachment::is_image) && !self.runtime.selected_has_vision() {
            self.view.notify("This model does not accept images");
            return;
        }
        self.chat.input.clear();
        let attachments = std::mem::take(&mut self.chat.pending);
        self.chat.messages.push(ChatMessage { attachments, ..ChatMessage::new(Role::User, text) });
        self.ask();
    }

    /// Drops the last reply and asks again (with the currently selected model).
    pub(crate) fn regenerate(&mut self) {
        if self.chat.streaming() || self.runtime.selected.is_none() {
            return;
        }
        if self.chat.messages.last().is_some_and(|m| m.role == Role::Assistant) {
            self.chat.messages.pop();
            self.ask();
        }
    }

    /// Sends the current history and streams the reply into a new message.
    fn ask(&mut self) {
        let Some(model) = self.runtime.selected.clone() else { return };
        let history = self.chat.messages.clone();
        self.chat.messages.push(ChatMessage { model: Some(model.clone()), ..ChatMessage::new(Role::Assistant, "") });
        let cancel = Arc::new(AtomicBool::new(false));
        self.chat.cancel = Some(cancel.clone());
        self.persist();
        let num_ctx = self.num_ctx(&model);
        let think = self.runtime.thinks.get(&model).copied().unwrap_or(false);
        self.backend.chat(&model, history, num_ctx, think, cancel);
    }

    /// Context window to request: the model's maximum, capped by the setting
    /// (a larger window needs more memory for the KV cache).
    pub(crate) fn num_ctx(&self, model: &str) -> u64 {
        let cap = self.settings.max_context.unwrap_or(u64::MAX);
        match self.runtime.ctx_limits.get(model) {
            Some(Some(max)) => (*max).min(cap),
            _ => FALLBACK_NUM_CTX.min(cap),
        }
    }

    /// When the message being written would not fit in the context window:
    /// a rough token count of the whole request, and the window size.
    pub(crate) fn context_overflow(&self) -> Option<(u64, u64)> {
        let model = self.runtime.selected.as_deref()?;
        let composing = estimate_tokens(prompt_len(&self.chat.input, &self.chat.pending));
        if self.chat.input.trim().is_empty() && self.chat.pending.is_empty() {
            return None;
        }
        // tokens counted by the runtime for the conversation so far, else an estimate
        let history = match self.chat.usage {
            Some(u) => u.prompt_tokens + u.completion_tokens,
            None => self.chat.messages.iter().map(|m| estimate_tokens(prompt_len(&m.content, &m.attachments))).sum(),
        };
        let window = self.num_ctx(model);
        (history + composing > window).then_some((history + composing, window))
    }

    /// Asks the runtime for the selected model's context length (once per model).
    pub(crate) fn ensure_model_info(&mut self) {
        if let Some(m) = &self.runtime.selected
            && !self.runtime.ctx_limits.contains_key(m)
            && matches!(self.runtime.status, RuntimeStatus::Online { .. })
        {
            self.runtime.ctx_limits.insert(m.clone(), None);
            self.backend.model_info(m);
        }
    }

    pub(crate) fn refresh_system(&mut self) {
        if !self.hw.probing {
            self.hw.probing = true;
            self.sink.probe_system();
        }
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
                    if !self.chat.pending.contains(&a) {
                        self.chat.pending.push(a);
                    }
                }
                Err(e) => errors.push(e),
            }
        }
        if !errors.is_empty() {
            self.view.notify(&errors.join(" · "));
        }
    }

    /// Ctrl+N new chat, Esc stops the reply, Ctrl+K searches the history.
    fn shortcuts(&mut self, ctx: &egui::Context) {
        let ctrl = egui::Modifiers::COMMAND;
        if ctx.input_mut(|i| i.consume_key(ctrl, egui::Key::N)) && !self.chat.streaming() {
            self.new_chat();
        }
        // only while streaming, so Esc still closes menus otherwise
        if self.chat.streaming() && ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape)) {
            self.chat.stop();
        }
        if ctx.input_mut(|i| i.consume_key(ctrl, egui::Key::K)) {
            self.view.show_history = true;
            self.view.focus_search = true;
        }
    }

}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.handle_events(&ctx);
        self.shortcuts(&ctx);
        self.ensure_model_info();
        // remember the selected model across restarts
        if self.runtime.selected.is_some() && self.runtime.selected != self.settings.last_model {
            self.settings.last_model = self.runtime.selected.clone();
            let _ = store::save_settings(&self.settings);
        }
        let now = ctx.input(|i| i.time);
        // notice when the runtime stops or comes back
        let settled = matches!(self.runtime.status, RuntimeStatus::Online { .. } | RuntimeStatus::Offline { .. });
        if settled && now - self.runtime.last_health > HEALTH_INTERVAL {
            self.runtime.last_health = now;
            self.backend.check();
        }
        // Repaint only when something changes: tokens, events and input wake the
        // UI on their own; the background asks for its next change, and stays
        // still while the window is not focused.
        let waiting = self.chat.streaming() && self.chat.messages.last().is_some_and(|m| m.content.is_empty());
        let focused = ctx.input(|i| i.viewport().focused.unwrap_or(true));
        if waiting {
            ctx.request_repaint(); // smooth loading shimmer until the first token
        } else if self.settings.animations && focused {
            ctx.request_repaint_after(next_change(now as f32));
        }
        if settled {
            ctx.request_repaint_after(std::time::Duration::from_secs_f64(HEALTH_INTERVAL));
        }

        let mut history_open = self.view.show_history;
        egui::Panel::left("history")
            .resizable(false)
            .exact_size(250.0)
            .frame(egui::Frame::new().fill(Color32::from_rgb(16, 16, 19)).inner_margin(10.0))
            .show_collapsible(ui, &mut history_open, |ui| self.history_panel(ui));
        // the close button inside the panel may have changed `show_history`
        self.view.show_history = self.view.show_history && history_open;

        egui::CentralPanel::default().frame(egui::Frame::NONE).show(ui, |ui| {
            let rect = ui.max_rect();
            draw_background(ui.painter(), rect, now as f32, self.settings.animations, !self.chat.messages.is_empty());
            self.top_bar(ui, rect);
            if !self.view.show_history {
                let btn = Rect::from_min_size(rect.min + Vec2::new(12.0, 12.0), Vec2::new(36.0, 30.0));
                if ui.put(btn, egui::Button::new(RichText::new("☰").size(16.0))).on_hover_text("History (Ctrl+K)").clicked() {
                    self.view.show_history = true;
                }
            }
            self.chat(ui, rect);
            // notices stack next to the message box, the undo bar closest
            let anchor = self.undo_bar(ui, self.view.notice_anchor, now);
            self.toast(ui, anchor, now);
        });

        self.models_window(&ctx);
        self.system_window(&ctx);
        self.uncensored_window(&ctx);
    }

    fn on_exit(&mut self) {
        self.history.writer.close(); // finish pending history writes
        self.backend.shutdown(); // clean stop of an `ollama serve` we started
    }
}

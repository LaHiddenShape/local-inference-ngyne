//! Inference runtime abstraction. Every call is non-blocking: work runs on a
//! background thread and results come back to the UI as [`Event`]s.

pub mod ollama;

use std::sync::atomic::AtomicBool;
use std::sync::mpsc::Sender;
use std::sync::Arc;

use eframe::egui;
use serde::Serialize;

#[derive(Clone, Debug, PartialEq)]
pub enum RuntimeStatus {
    Checking,
    Starting,
    Online { version: String },
    NotInstalled,
    Offline { error: String },
}

#[derive(Clone, Debug)]
pub struct InstalledModel {
    pub name: String,
    pub size: u64,
}

#[derive(Clone, Debug, Serialize)]
pub struct ChatMessage {
    pub role: String,
    pub content: String,
}

pub enum Event {
    Status(RuntimeStatus),
    Installed(Vec<InstalledModel>),
    PullProgress { model: String, status: String, completed: u64, total: u64 },
    PullDone { model: String, error: Option<String> },
    Deleted { model: String, error: Option<String> },
    ChatToken(String),
    ChatDone { error: Option<String> },
}

/// Channel to the UI that also wakes it up.
#[derive(Clone)]
pub struct Sink {
    tx: Sender<Event>,
    ctx: egui::Context,
}

impl Sink {
    pub fn new(tx: Sender<Event>, ctx: egui::Context) -> Self {
        Self { tx, ctx }
    }

    pub fn send(&self, event: Event) {
        let _ = self.tx.send(event);
        self.ctx.request_repaint();
    }
}

/// A local inference runtime (Ollama today; llama.cpp / OpenAI-compatible later).
pub trait Backend: Send + Sync {
    /// Detects the runtime and starts it if needed.
    fn ensure_running(&self);
    fn refresh_models(&self);
    fn pull(&self, model: &str, cancel: Arc<AtomicBool>);
    fn delete(&self, model: &str);
    fn chat(&self, model: &str, history: Vec<ChatMessage>, cancel: Arc<AtomicBool>);
}

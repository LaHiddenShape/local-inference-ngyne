//! Inference runtime abstraction. Every call is non-blocking: work runs on a
//! background thread and results come back to the UI as [`Event`]s.

pub mod ollama;

use std::sync::atomic::AtomicBool;
use std::sync::mpsc::Sender;
use std::sync::Arc;

use eframe::egui;
use serde::{Deserialize, Serialize};

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

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: String,
    pub content: String,
    /// Model that wrote this reply (assistant messages only). Kept in the
    /// history file, never sent to the runtime.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// The reply was interrupted with Stop.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub stopped: bool,
}

impl ChatMessage {
    pub fn new(role: &str, content: impl Into<String>) -> Self {
        Self { role: role.into(), content: content.into(), ..Default::default() }
    }
}

/// Token usage reported by the runtime at the end of a reply.
#[derive(Clone, Copy, Debug, Default)]
pub struct Usage {
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
}

pub enum Event {
    Status(RuntimeStatus),
    Installed(Vec<InstalledModel>),
    PullProgress { model: String, status: String, completed: u64, total: u64 },
    PullDone { model: String, error: Option<String> },
    Deleted { model: String, error: Option<String> },
    ChatToken(String),
    ChatDone { error: Option<String>, usage: Option<Usage> },
    /// Maximum context length supported by a model.
    ModelInfo { model: String, context_length: Option<u64> },
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
    /// Address of the runtime, for display.
    fn endpoint(&self) -> String;
    /// Detects the runtime and starts it if needed.
    fn ensure_running(&self);
    fn refresh_models(&self);
    fn pull(&self, model: &str, cancel: Arc<AtomicBool>);
    fn delete(&self, model: &str);
    /// Streams a reply. `num_ctx` is the context window to allocate (tokens).
    fn chat(&self, model: &str, history: Vec<ChatMessage>, num_ctx: u64, cancel: Arc<AtomicBool>);
    /// Fetches model metadata (context length).
    fn model_info(&self, model: &str);
}

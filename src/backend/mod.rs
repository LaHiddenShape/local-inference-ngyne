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

/// Author of a message, stored as "user", "assistant" or "system" like in the
/// runtime's API (so older history files load unchanged).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    #[default]
    User,
    Assistant,
    System,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: Role,
    pub content: String,
    /// Model that wrote this reply (assistant messages only). Kept in the
    /// history file, never sent to the runtime.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// The reply was interrupted with Stop.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub stopped: bool,
    /// Files attached to a user message.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attachments: Vec<crate::attach::Attachment>,
    /// When the message was written, in Unix seconds (0 for old history).
    #[serde(default, skip_serializing_if = "is_zero")]
    pub time: u64,
    /// Reasoning streamed by a thinking model, shown folded, never sent back.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub thinking: String,
    /// Why the reply failed: shown apart from its text, never sent back.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

fn is_zero(n: &u64) -> bool {
    *n == 0
}

impl ChatMessage {
    pub fn new(role: Role, content: impl Into<String>) -> Self {
        let time = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        Self { role, content: content.into(), time, ..Default::default() }
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
    /// A piece of the reasoning of a thinking model.
    ChatThinking(String),
    ChatDone { error: Option<String>, usage: Option<Usage> },
    /// Result of a background hardware probe.
    System(Box<crate::system::SystemInfo>),
    /// Maximum context length supported by a model, whether it accepts images
    /// and whether it can reason before answering.
    ModelInfo { model: String, context_length: Option<u64>, vision: bool, thinking: bool },
    /// Files picked in the file dialog, loaded in the background.
    Attached(Vec<Result<crate::attach::Attachment, String>>),
    /// Uncensored models listed on ollama.com.
    Discovered { kind: crate::discover::ListKind, result: Result<Vec<crate::discover::RemoteModel>, String> },
    /// Registry details of one model tag.
    RemoteDetails { tag: String, result: Result<crate::discover::Details, String> },
    /// A message for the user from a background task (e.g. a failed save).
    Notice(String),
    /// Result of a periodic health check: the runtime version, or the error.
    Health(Result<String, String>),
    /// Preview of an image attachment, decoded in the background (`None` if it could not be).
    Thumbnail { key: u64, image: Option<egui::ColorImage> },
}

/// Channel to the UI that also wakes it up.
#[derive(Clone)]
pub struct Sink {
    tx: Sender<Event>,
    ctx: egui::Context,
}

impl Sink {
    /// Probes the hardware on a background thread (nvidia-smi / lspci can take
    /// a moment) and reports it as [`Event::System`].
    pub fn probe_system(&self) {
        let me = self.clone();
        std::thread::spawn(move || me.send(Event::System(Box::new(crate::system::SystemInfo::probe()))));
    }

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
    /// Quick reachability check, answered with [`Event::Health`].
    fn check(&self);
    /// Stops the runtime if this app started it (waits for a clean exit).
    fn shutdown(&self);
    fn refresh_models(&self);
    fn pull(&self, model: &str, cancel: Arc<AtomicBool>);
    fn delete(&self, model: &str);
    /// Streams a reply. `num_ctx` is the context window to allocate (tokens);
    /// `think` asks a thinking model to stream its reasoning separately.
    fn chat(&self, model: &str, history: Vec<ChatMessage>, num_ctx: u64, think: bool, cancel: Arc<AtomicBool>);
    /// Fetches model metadata (context length).
    fn model_info(&self, model: &str);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roles_keep_their_names_in_json() {
        let m: ChatMessage = serde_json::from_str(r#"{"role":"assistant","content":"hi"}"#).unwrap();
        assert_eq!(m.role, Role::Assistant);
        assert_eq!(serde_json::to_value(Role::User).unwrap(), "user");
        assert!(serde_json::from_str::<ChatMessage>(r#"{"role":"robot","content":""}"#).is_err());
    }
}

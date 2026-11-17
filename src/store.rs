//! Conversation history persisted as JSON in $XDG_DATA_HOME/local-inference-ui.

use std::fs;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::backend::ChatMessage;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Conversation {
    /// Creation time in ms since epoch, doubles as a unique id.
    pub id: u64,
    pub title: String,
    pub model: String,
    /// Last update, ms since epoch (used for sorting).
    pub updated: u64,
    pub messages: Vec<ChatMessage>,
    /// Pinned conversations are listed first. Defaults to false for older files.
    #[serde(default)]
    pub pinned: bool,
}

pub fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

fn data_dir() -> PathBuf {
    let base = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")))
        .unwrap_or_else(|| PathBuf::from("."));
    base.join("local-inference-ui")
}

fn file() -> PathBuf {
    data_dir().join("conversations.json")
}

/// Loads all conversations, most recent first. Missing or corrupt file -> empty.
pub fn load() -> Vec<Conversation> {
    let mut list: Vec<Conversation> = fs::read_to_string(file())
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default();
    list.sort_by(|a, b| b.updated.cmp(&a.updated));
    list
}

/// Atomic save: write to a temp file, then rename.
pub fn save(list: &[Conversation]) -> std::io::Result<()> {
    fs::create_dir_all(data_dir())?;
    let tmp = file().with_extension("json.tmp");
    fs::write(&tmp, serde_json::to_vec_pretty(list)?)?;
    fs::rename(tmp, file())
}

/// Short title from the first user message.
pub fn title_from(messages: &[ChatMessage]) -> String {
    let first = messages.iter().find(|m| m.role == "user").map(|m| m.content.as_str()).unwrap_or("New chat");
    let line = first.lines().next().unwrap_or("").trim();
    let mut t: String = line.chars().take(40).collect();
    if line.chars().count() > 40 {
        t.push('…');
    }
    if t.is_empty() { "New chat".into() } else { t }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn msg(role: &str, content: &str) -> ChatMessage {
        ChatMessage::new(role, content)
    }

    #[test]
    fn title_uses_first_user_line_truncated() {
        assert_eq!(title_from(&[]), "New chat");
        assert_eq!(title_from(&[msg("user", "hello\nworld")]), "hello");
        let long = "x".repeat(60);
        assert_eq!(title_from(&[msg("user", &long)]).chars().count(), 41);
    }

    #[test]
    fn save_and_load_roundtrip() {
        let dir = std::env::temp_dir().join(format!("liu-test-{}", now_ms()));
        // SAFETY: test-only, single-threaded access to this variable
        unsafe { std::env::set_var("XDG_DATA_HOME", &dir) };
        let c = Conversation { pinned: true, id: 1, title: "t".into(), model: "m".into(), updated: 5, messages: vec![msg("user", "hi")] };
        save(&[c]).unwrap();
        let back = load();
        assert_eq!(back.len(), 1);
        assert_eq!(back[0].messages[0].content, "hi");
        assert!(back[0].pinned);
        let _ = fs::remove_dir_all(dir);
    }
}

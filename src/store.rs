//! Conversation history, one JSON file per conversation in
//! $XDG_DATA_HOME/local-inference-ngyne/conversations. Saves run on a
//! background thread, so a large history never stalls the UI, and a file that
//! cannot be read is reported and left untouched instead of being overwritten.

use std::collections::HashSet;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Sender};
use std::thread::{self, JoinHandle};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::backend::{ChatMessage, Role};

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
    /// The title was set by hand: keep it instead of the first message.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub renamed: bool,
}

pub fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

/// Former names of the data directory, from before the app was renamed.
const LEGACY_DIRS: &[&str] = &["local-inference-ui"];
/// Subdirectory holding one `<id>.json` file per conversation.
const CONVERSATIONS_DIR: &str = "conversations";
/// History file of earlier versions, with every conversation in one array.
const SINGLE_FILE: &str = "conversations.json";

fn data_home() -> PathBuf {
    data_home_from(std::env::var_os("XDG_DATA_HOME"), std::env::var_os("HOME"))
}

/// The XDG spec says to ignore an empty or relative `XDG_DATA_HOME`.
fn data_home_from(xdg: Option<std::ffi::OsString>, home: Option<std::ffi::OsString>) -> PathBuf {
    xdg.map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| home.filter(|h| !h.is_empty()).map(|h| PathBuf::from(h).join(".local/share")))
        .unwrap_or_else(|| PathBuf::from("."))
}

pub(crate) fn data_dir() -> PathBuf {
    data_home().join(crate::APP_ID)
}

/// Moves the data directory of a former app name to the current one, so the
/// history survives the rename. Call once at startup, before anything is read.
pub fn migrate_legacy_data() {
    if let Err(e) = migrate_in(&data_home()) {
        eprintln!("could not move the old data directory to {}: {e}", data_dir().display());
    }
}

fn migrate_in(base: &Path) -> io::Result<()> {
    let dir = base.join(crate::APP_ID);
    if dir.exists() {
        return Ok(()); // never overwrite current data
    }
    match LEGACY_DIRS.iter().map(|name| base.join(name)).find(|old| old.is_dir()) {
        Some(old) => fs::rename(old, dir),
        None => Ok(()),
    }
}

/// Replaces `path` atomically and durably: temp file, fsync, rename.
pub(crate) fn write_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    let tmp = PathBuf::from(tmp);
    let mut file = fs::File::create(&tmp)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    fs::rename(&tmp, path)?;
    // make the rename itself durable (best effort: not every filesystem supports it)
    if let Some(dir) = path.parent() {
        let _ = fs::File::open(dir).and_then(|d| d.sync_all());
    }
    Ok(())
}

fn conversation_path(dir: &Path, id: u64) -> PathBuf {
    dir.join(format!("{id}.json"))
}

fn write_conversation(dir: &Path, c: &Conversation) -> io::Result<()> {
    fs::create_dir_all(dir)?;
    write_atomic(&conversation_path(dir, c.id), &serde_json::to_vec(c)?)
}

/// What was found on disk at startup.
pub struct Loaded {
    /// Most recent first.
    pub conversations: Vec<Conversation>,
    /// Set when some files could not be read; they are left untouched.
    pub warning: Option<String>,
}

pub fn load() -> Loaded {
    load_from(&data_dir())
}

fn load_from(base: &Path) -> Loaded {
    let dir = base.join(CONVERSATIONS_DIR);
    let mut problems = Vec::new();
    let single = base.join(SINGLE_FILE);
    if single.exists()
        && let Err(e) = split_single_file(&single, &dir)
    {
        problems.push(format!("{}: {e}", single.display()));
    }
    let entries = fs::read_dir(&dir).map(|rd| rd.flatten().collect::<Vec<_>>()).unwrap_or_default();
    let mut list: Vec<Conversation> = Vec::new();
    for path in entries.iter().map(|e| e.path()) {
        if path.extension().is_none_or(|ext| ext != "json") {
            continue; // e.g. the temp file of an interrupted save
        }
        let parsed = fs::read(&path)
            .map_err(|e| e.to_string())
            .and_then(|bytes| serde_json::from_slice(&bytes).map_err(|e| e.to_string()));
        match parsed {
            Ok(c) => list.push(c),
            Err(e) => problems.push(format!("{}: {e}", path.display())),
        }
    }
    list.sort_by_key(|c| std::cmp::Reverse(c.updated));
    let mut seen = HashSet::new();
    list.retain(|c| seen.insert(c.id)); // a copied file must not show twice
    let warning = (!problems.is_empty()).then(|| {
        for p in &problems {
            eprintln!("history: {p}");
        }
        format!("Could not read {} history file(s), left untouched in {}", problems.len(), base.display())
    });
    Loaded { conversations: list, warning }
}

/// Splits the single-file history of earlier versions into one file per
/// conversation, then removes it. On any error the old file is kept as is.
fn split_single_file(single: &Path, dir: &Path) -> Result<(), String> {
    let text = fs::read_to_string(single).map_err(|e| e.to_string())?;
    let list: Vec<Conversation> = serde_json::from_str(&text).map_err(|e| e.to_string())?;
    for c in &list {
        // a file already there was written later by this version: it wins
        if !conversation_path(dir, c.id).exists() {
            write_conversation(dir, c).map_err(|e| e.to_string())?;
        }
    }
    fs::remove_file(single).map_err(|e| e.to_string())
}

enum Op {
    Save(Box<Conversation>),
    Delete(u64),
}

impl Op {
    fn id(&self) -> u64 {
        match self {
            Op::Save(c) => c.id,
            Op::Delete(id) => *id,
        }
    }
}

/// Saves and deletes conversation files on a background thread, in order.
/// Dropping it (or calling [`Writer::close`]) waits for pending writes.
pub struct Writer {
    tx: Option<Sender<Op>>,
    worker: Option<JoinHandle<()>>,
}

impl Writer {
    /// `on_error` receives a message for the user when a write fails.
    pub fn spawn(on_error: impl Fn(String) + Send + 'static) -> Self {
        Self::spawn_in(data_dir().join(CONVERSATIONS_DIR), on_error)
    }

    fn spawn_in(dir: PathBuf, on_error: impl Fn(String) + Send + 'static) -> Self {
        let (tx, rx) = mpsc::channel::<Op>();
        let worker = thread::Builder::new()
            .name("history-writer".into())
            .spawn(move || {
                while let Ok(first) = rx.recv() {
                    // only the last queued operation per conversation matters
                    let mut batch = vec![first];
                    batch.extend(rx.try_iter());
                    let mut done = HashSet::new();
                    for op in batch.into_iter().rev() {
                        if done.insert(op.id())
                            && let Err(e) = apply(&dir, op)
                        {
                            on_error(format!("Could not save history: {e}"));
                        }
                    }
                }
            })
            .expect("history writer thread");
        Self { tx: Some(tx), worker: Some(worker) }
    }

    pub fn save(&self, c: Conversation) {
        self.send(Op::Save(Box::new(c)));
    }

    pub fn delete(&self, id: u64) {
        self.send(Op::Delete(id));
    }

    fn send(&self, op: Op) {
        if let Some(tx) = &self.tx {
            let _ = tx.send(op);
        }
    }

    /// Finishes pending writes; later saves are ignored.
    pub fn close(&mut self) {
        drop(self.tx.take()); // the worker stops once the queue is empty
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

impl Drop for Writer {
    fn drop(&mut self) {
        self.close();
    }
}

fn apply(dir: &Path, op: Op) -> io::Result<()> {
    match op {
        Op::Save(c) => write_conversation(dir, &c),
        Op::Delete(id) => match fs::remove_file(conversation_path(dir, id)) {
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
            other => other,
        },
    }
}

/// Small app preferences, stored next to the history. Missing fields take
/// their default, so older files still load.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub last_model: Option<String>,
    /// Animated pyramids in the background.
    pub animations: bool,
    /// Largest context window requested, in tokens (`None`: the model's maximum).
    pub max_context: Option<u64>,
}

impl Default for Settings {
    fn default() -> Self {
        Self { last_model: None, animations: true, max_context: Some(8192) }
    }
}

fn settings_file() -> PathBuf {
    data_dir().join("settings.json")
}

pub fn load_settings() -> Settings {
    fs::read_to_string(settings_file()).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default()
}

pub fn save_settings(s: &Settings) -> io::Result<()> {
    fs::create_dir_all(data_dir())?;
    write_atomic(&settings_file(), &serde_json::to_vec_pretty(s)?)
}

/// The conversation as Markdown, for export (reasoning left out).
pub fn to_markdown(c: &Conversation) -> String {
    let mut out = format!("# {}\n\n", c.title);
    for m in &c.messages {
        let who = match m.role {
            Role::User => "You".to_string(),
            _ => m.model.clone().unwrap_or_else(|| c.model.clone()),
        };
        out.push_str(&format!("### {who}\n\n"));
        if !m.attachments.is_empty() {
            let names: Vec<&str> = m.attachments.iter().map(|a| a.name.as_str()).collect();
            out.push_str(&format!("*Attached: {}*\n\n", names.join(", ")));
        }
        if !m.content.trim().is_empty() {
            out.push_str(m.content.trim());
            out.push_str("\n\n");
        }
        if let Some(e) = &m.error {
            out.push_str(&format!("*Error: {e}*\n\n"));
        }
    }
    out
}

/// A file name made of the title's letters, digits, spaces, `-` and `_`.
pub fn file_name_for(title: &str) -> String {
    let clean: String = title.chars().map(|c| if c.is_alphanumeric() || " -_".contains(c) { c } else { '_' }).collect();
    let clean: String = clean.trim().chars().take(60).collect();
    if clean.is_empty() { "conversation".into() } else { clean }
}

/// Short title from the first user message.
pub fn title_from(messages: &[ChatMessage]) -> String {
    let first = messages.iter().find(|m| m.role == Role::User).map(|m| m.content.as_str()).unwrap_or("New chat");
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
    use std::sync::{Arc, Mutex};

    fn msg(role: Role, content: &str) -> ChatMessage {
        ChatMessage::new(role, content)
    }

    fn conv(id: u64, title: &str) -> Conversation {
        Conversation {
            id,
            title: title.into(),
            model: "m".into(),
            updated: id,
            messages: vec![msg(Role::User, "hi")],
            pinned: false,
            renamed: false,
        }
    }

    fn temp_base(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("{}-{tag}-{}-{}", crate::APP_ID, std::process::id(), now_ms()));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn titles(l: &Loaded) -> Vec<&str> {
        l.conversations.iter().map(|c| c.title.as_str()).collect()
    }

    #[test]
    fn invalid_xdg_data_home_is_ignored() {
        let home = Some("/home/u".into());
        assert_eq!(data_home_from(Some("/data".into()), home.clone()), PathBuf::from("/data"));
        assert_eq!(data_home_from(Some("".into()), home.clone()), PathBuf::from("/home/u/.local/share"));
        assert_eq!(data_home_from(Some("rel/dir".into()), home.clone()), PathBuf::from("/home/u/.local/share"));
        assert_eq!(data_home_from(None, home), PathBuf::from("/home/u/.local/share"));
    }

    #[test]
    fn markdown_export_and_file_name() {
        let mut c = conv(1, "Ownership: a/b?");
        c.messages.push(ChatMessage { model: Some("qwen3:8b".into()), ..msg(Role::Assistant, "**Yes**") });
        c.messages.push(ChatMessage { error: Some("out of memory".into()), ..msg(Role::Assistant, "") });
        assert_eq!(
            to_markdown(&c),
            "# Ownership: a/b?\n\n### You\n\nhi\n\n### qwen3:8b\n\n**Yes**\n\n### m\n\n*Error: out of memory*\n\n"
        );
        assert_eq!(file_name_for("Ownership: a/b?"), "Ownership_ a_b_");
        assert_eq!(file_name_for("///"), "___");
        assert_eq!(file_name_for("  "), "conversation");
    }

    #[test]
    fn title_uses_first_user_line_truncated() {
        assert_eq!(title_from(&[]), "New chat");
        assert_eq!(title_from(&[msg(Role::User, "hello\nworld")]), "hello");
        let long = "x".repeat(60);
        assert_eq!(title_from(&[msg(Role::User, &long)]).chars().count(), 41);
    }

    #[test]
    fn writer_saves_deletes_and_keeps_the_last_version() {
        let base = temp_base("writer");
        let errors = Arc::new(Mutex::new(Vec::new()));
        let sink = errors.clone();
        let mut w = Writer::spawn_in(base.join(CONVERSATIONS_DIR), move |e| sink.lock().unwrap().push(e));
        w.save(Conversation { pinned: true, ..conv(1, "first") });
        for i in 0..50 {
            w.save(conv(2, &format!("draft {i}")));
        }
        w.save(conv(3, "deleted"));
        w.delete(3);
        w.delete(4); // unknown id: not an error
        w.close();
        assert!(errors.lock().unwrap().is_empty(), "{:?}", errors.lock().unwrap());
        let loaded = load_from(&base);
        assert!(loaded.warning.is_none());
        assert_eq!(titles(&loaded), ["draft 49", "first"]);
        assert!(loaded.conversations[1].pinned);
        assert_eq!(loaded.conversations[1].messages[0].content, "hi");
        let _ = fs::remove_dir_all(base);
    }

    #[test]
    fn single_file_history_is_split_then_removed() {
        let base = temp_base("split");
        let old = vec![conv(1, "a"), conv(2, "b")];
        fs::write(base.join(SINGLE_FILE), serde_json::to_vec(&old).unwrap()).unwrap();
        // a copy written by this version after an interrupted split is newer
        write_conversation(&base.join(CONVERSATIONS_DIR), &conv(2, "newer")).unwrap();
        let loaded = load_from(&base);
        assert!(loaded.warning.is_none());
        assert_eq!(titles(&loaded), ["newer", "a"]);
        assert!(!base.join(SINGLE_FILE).exists());
        let _ = fs::remove_dir_all(base);
    }

    #[test]
    fn unreadable_files_are_reported_and_left_untouched() {
        let base = temp_base("unreadable");
        fs::write(base.join(SINGLE_FILE), "{ not json").unwrap();
        let dir = base.join(CONVERSATIONS_DIR);
        write_conversation(&dir, &conv(1, "ok")).unwrap();
        fs::write(dir.join("2.json"), "garbage").unwrap();
        let loaded = load_from(&base);
        assert_eq!(titles(&loaded), ["ok"]);
        assert!(loaded.warning.unwrap().starts_with("Could not read 2 history file(s)"));
        assert_eq!(fs::read_to_string(base.join(SINGLE_FILE)).unwrap(), "{ not json");
        assert_eq!(fs::read_to_string(dir.join("2.json")).unwrap(), "garbage");
        let _ = fs::remove_dir_all(base);
    }

    #[test]
    fn legacy_data_dir_is_moved_once_and_never_overwrites() {
        let base = temp_base("migrate");
        let old = base.join(LEGACY_DIRS[0]);
        let new = base.join(crate::APP_ID);
        fs::create_dir_all(&old).unwrap();
        fs::write(old.join("conversations.json"), "[]").unwrap();

        migrate_in(&base).unwrap();
        assert_eq!(fs::read_to_string(new.join("conversations.json")).unwrap(), "[]");
        assert!(!old.exists());

        // a legacy directory that reappears later must not replace current data
        fs::create_dir_all(&old).unwrap();
        fs::write(old.join("conversations.json"), "stale").unwrap();
        migrate_in(&base).unwrap();
        assert_eq!(fs::read_to_string(new.join("conversations.json")).unwrap(), "[]");

        // nothing to migrate is not an error
        let empty = base.join("empty");
        fs::create_dir_all(&empty).unwrap();
        migrate_in(&empty).unwrap();
        assert!(!empty.join(crate::APP_ID).exists());
        let _ = fs::remove_dir_all(base);
    }
}

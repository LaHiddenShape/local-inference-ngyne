//! Ollama HTTP API client (https://github.com/ollama/ollama/blob/main/docs/api.md).

use crate::attach::{images, prompt_with_files};
use std::io::{Read, Seek, SeekFrom};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, LazyLock, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use reqwest::blocking::Client;
use serde::Deserialize;
use serde_json::json;

use super::{Backend, ChatMessage, Event, InstalledModel, RuntimeStatus, Sink, Usage};

pub const DEFAULT_URL: &str = "http://127.0.0.1:11434";

/// Resolves the server URL like the `ollama` CLI does from `OLLAMA_HOST`:
/// accepts `host`, `host:port`, `:port` or a full `scheme://host:port` URL.
pub fn resolve_url(host_var: Option<&str>) -> String {
    let Some(v) = host_var.map(str::trim).filter(|s| !s.is_empty()) else {
        return DEFAULT_URL.into();
    };
    let (scheme, rest) = v.split_once("://").unwrap_or(("http", v));
    let rest = rest.trim_end_matches('/');
    let default_port = if scheme == "https" { "443" } else { "11434" };
    let (host, port) = match rest.rsplit_once(':') {
        Some((h, p)) if !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()) => (h, p),
        _ => (rest, default_port),
    };
    let host = if host.is_empty() { "127.0.0.1" } else { host };
    format!("{scheme}://{host}:{port}")
}

/// Server URL from the environment.
pub fn url_from_env() -> String {
    resolve_url(std::env::var("OLLAMA_HOST").ok().as_deref())
}

pub struct Ollama {
    base: String,
    http: Client,
    /// Async client for the cancellable streams (see [`runtime`]).
    stream_http: reqwest::Client,
    sink: Sink,
    /// `ollama serve` process spawned by us, if any (killed on exit).
    child: Arc<Mutex<Option<Child>>>,
    /// Only the instance owned by the app (not worker-thread clones) stops the server.
    owner: bool,
}

impl Clone for Ollama {
    fn clone(&self) -> Self {
        Self {
            base: self.base.clone(),
            http: self.http.clone(),
            stream_http: self.stream_http.clone(),
            sink: self.sink.clone(),
            child: self.child.clone(),
            owner: false,
        }
    }
}

impl Ollama {
    pub fn new(base: &str, sink: Sink) -> Self {
        let http = Client::builder()
            .timeout(None::<Duration>) // a model load can take a while
            .connect_timeout(Duration::from_secs(3))
            .build()
            .expect("http client");
        // no overall timeout: streams can last minutes
        let stream_http =
            reqwest::Client::builder().connect_timeout(Duration::from_secs(3)).build().expect("http client");
        Self { base: base.trim_end_matches('/').to_string(), http, stream_http, sink, child: Arc::default(), owner: true }
    }

    pub fn base_url(&self) -> &str {
        &self.base
    }

    fn url(&self, path: &str) -> String {
        format!("{}{}", self.base, path)
    }

    fn version(&self) -> Result<String, String> {
        #[derive(Deserialize)]
        struct V {
            version: String,
        }
        self.http
            .get(self.url("/api/version"))
            .timeout(Duration::from_secs(2))
            .send()
            .and_then(|r| r.error_for_status())
            .and_then(|r| r.json::<V>())
            .map(|v| v.version)
            .map_err(|e| e.to_string())
    }

    fn spawn<F: FnOnce(Ollama) + Send + 'static>(&self, f: F) {
        let me = self.clone();
        thread::spawn(move || f(me));
    }

    fn list_models(&self) -> Result<Vec<InstalledModel>, String> {
        #[derive(Deserialize)]
        struct Tags {
            models: Vec<Tag>,
        }
        #[derive(Deserialize)]
        struct Tag {
            name: String,
            size: u64,
        }
        let tags: Tags = self
            .http
            .get(self.url("/api/tags"))
            .send()
            .and_then(|r| r.error_for_status())
            .and_then(|r| r.json())
            .map_err(|e| e.to_string())?;
        let mut out: Vec<_> =
            tags.models.into_iter().map(|t| InstalledModel { name: t.name, size: t.size }).collect();
        out.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(out)
    }

    /// POSTs `body` and calls `on_line` for each NDJSON line until it returns
    /// false, the stream ends, or `cancel` is set. Cancelling drops the request
    /// at once, even while waiting for the first token: closing the connection
    /// makes Ollama stop the generation (or the download).
    fn stream(
        &self,
        path: &str,
        body: serde_json::Value,
        cancel: &AtomicBool,
        mut on_line: impl FnMut(serde_json::Value) -> Result<bool, String>,
    ) -> Result<(), String> {
        runtime().block_on(async {
            let request = self.stream_http.post(self.url(path)).json(&body).send();
            let mut cancelled = std::pin::pin!(async {
                while !cancel.load(Ordering::Relaxed) {
                    tokio::time::sleep(Duration::from_millis(50)).await;
                }
            });
            let mut resp = tokio::select! {
                r = request => r.map_err(|e| e.to_string())?,
                _ = &mut cancelled => return Ok(()),
            };
            if !resp.status().is_success() {
                let code = resp.status();
                let text = resp.text().await.unwrap_or_default();
                return Err(api_error(&text).unwrap_or_else(|| format!("HTTP {code}")));
            }
            let mut pending = Vec::new();
            loop {
                let chunk = tokio::select! {
                    c = resp.chunk() => c.map_err(|e| e.to_string())?,
                    _ = &mut cancelled => return Ok(()),
                };
                let end = chunk.is_none();
                pending.extend_from_slice(chunk.as_deref().unwrap_or_default());
                // complete lines, plus the last one once the stream has ended
                while let Some(line) = take_line(&mut pending, end) {
                    if line.iter().all(u8::is_ascii_whitespace) {
                        continue;
                    }
                    let v: serde_json::Value = serde_json::from_slice(&line).map_err(|e| e.to_string())?;
                    if let Some(err) = v.get("error").and_then(|e| e.as_str()) {
                        return Err(err.to_string());
                    }
                    if !on_line(v)? {
                        return Ok(());
                    }
                }
                if end {
                    return Ok(());
                }
            }
        })
    }

    /// Polls until the server answers (Online) or fails (Offline), 20 s at most.
    fn wait_online(&self) {
        let start = Instant::now();
        let mut last_err = String::new();
        while start.elapsed() < Duration::from_secs(20) {
            match self.version() {
                Ok(version) => {
                    self.sink.send(Event::Status(RuntimeStatus::Online { version }));
                    self.refresh_models();
                    return;
                }
                Err(e) => last_err = e,
            }
            // our server already gave up (e.g. port in use): no point in waiting
            if let Some(error) = self.reap_child() {
                self.sink.send(Event::Status(RuntimeStatus::Offline { error }));
                return;
            }
            thread::sleep(Duration::from_millis(300));
        }
        let error = match last_log_line(&log_path()) {
            Some(line) => format!("{last_err} ({line})"),
            None => last_err,
        };
        self.sink.send(Event::Status(RuntimeStatus::Offline { error }));
    }

    /// If the server we spawned has exited, reaps it and says why.
    fn reap_child(&self) -> Option<String> {
        let mut guard = self.child.lock().unwrap();
        let status = guard.as_mut()?.try_wait().ok()??;
        *guard = None;
        drop(guard);
        let why = last_log_line(&log_path()).map(|l| format!(": {l}")).unwrap_or_default();
        Some(format!("ollama serve exited ({status}){why}"))
    }

    /// Runs on the thread that spawned the server until it stops: the kernel
    /// sends PR_SET_PDEATHSIG when the *spawning thread* exits, so this thread
    /// must outlive the server. Also reaps it and reports if it dies.
    fn watch_child(&self) {
        loop {
            thread::sleep(Duration::from_millis(500));
            if self.child.lock().unwrap().is_none() {
                return; // stopped by `shutdown`, or already reaped
            }
            if let Some(error) = self.reap_child() {
                self.sink.send(Event::Status(RuntimeStatus::Offline { error }));
                return;
            }
        }
    }
}

/// Runs the streaming requests (chat, pull) so they can be dropped the
/// moment the user cancels.
fn runtime() -> &'static tokio::runtime::Runtime {
    static RUNTIME: LazyLock<tokio::runtime::Runtime> = LazyLock::new(|| {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .thread_name("ollama-stream")
            .enable_all()
            .build()
            .expect("tokio runtime")
    });
    &RUNTIME
}

/// Removes and returns the next line of `buf`, or the remaining bytes when
/// `end` is set (the stream is over and its last line had no newline).
fn take_line(buf: &mut Vec<u8>, end: bool) -> Option<Vec<u8>> {
    match buf.iter().position(|&b| b == b'\n') {
        Some(i) => {
            let mut line: Vec<u8> = buf.drain(..=i).collect();
            line.pop();
            Some(line)
        }
        None if end && !buf.is_empty() => Some(std::mem::take(buf)),
        None => None,
    }
}

/// Output of the `ollama serve` we spawn, kept to explain failures.
fn log_path() -> PathBuf {
    crate::store::data_dir().join("ollama-serve.log")
}

/// Stdout and stderr redirected to a fresh log file (null if it cannot be created).
fn log_stdio(path: &Path) -> (Stdio, Stdio) {
    let file = path.parent().map(std::fs::create_dir_all).transpose().and_then(|_| std::fs::File::create(path));
    match file.and_then(|f| Ok((f.try_clone()?, f))) {
        Ok((out, err)) => (out.into(), err.into()),
        Err(_) => (Stdio::null(), Stdio::null()),
    }
}

/// Last non-empty line of the log (read from its tail only).
fn last_log_line(path: &Path) -> Option<String> {
    let mut f = std::fs::File::open(path).ok()?;
    let len = f.metadata().ok()?.len();
    f.seek(SeekFrom::Start(len.saturating_sub(4096))).ok()?;
    let mut buf = Vec::new();
    f.read_to_end(&mut buf).ok()?;
    let text = String::from_utf8_lossy(&buf);
    let line = text.lines().rev().map(str::trim).find(|l| !l.is_empty())?;
    Some(line.chars().take(300).collect())
}

/// Asks the server to stop (on SIGTERM Ollama unloads its models and stops
/// its runner processes), then kills it if it is still there after 5 s.
fn terminate(child: &mut Child) {
    // SAFETY: plain syscall; the child is not reaped yet, so its pid is still ours
    unsafe { libc::kill(child.id() as libc::pid_t, libc::SIGTERM) };
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        if let Ok(Some(_)) = child.try_wait() {
            return;
        }
        thread::sleep(Duration::from_millis(50));
    }
    let _ = child.kill();
    let _ = child.wait();
}

fn api_error(body: &str) -> Option<String> {
    serde_json::from_str::<serde_json::Value>(body)
        .ok()?
        .get("error")?
        .as_str()
        .map(str::to_string)
}

impl Backend for Ollama {
    fn endpoint(&self) -> String {
        self.base_url().to_string()
    }

    fn ensure_running(&self) {
        self.spawn(|me| {
            me.sink.send(Event::Status(RuntimeStatus::Checking));
            if let Ok(version) = me.version() {
                me.sink.send(Event::Status(RuntimeStatus::Online { version }));
                me.refresh_models();
                return;
            }
            // the server we spawned on a previous attempt may still be starting:
            // wait for it instead of starting a second one
            let ours_alive = me.child.lock().unwrap().as_mut().is_some_and(|c| matches!(c.try_wait(), Ok(None)));
            if ours_alive {
                me.sink.send(Event::Status(RuntimeStatus::Starting));
                me.wait_online();
                return;
            }
            // not reachable: try to start it ourselves
            let (stdout, stderr) = log_stdio(&log_path());
            let mut cmd = Command::new("ollama");
            cmd.arg("serve").stdin(Stdio::null()).stdout(stdout).stderr(stderr);
            // Linux: have the kernel stop the server if this app dies (even on a crash)
            // SAFETY: prctl is async-signal-safe and touches no shared state.
            unsafe {
                cmd.pre_exec(|| {
                    if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM) != 0 {
                        return Err(std::io::Error::last_os_error());
                    }
                    Ok(())
                });
            }
            let child = cmd.spawn();
            match child {
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    me.sink.send(Event::Status(RuntimeStatus::NotInstalled));
                    return;
                }
                Err(e) => {
                    me.sink.send(Event::Status(RuntimeStatus::Offline { error: e.to_string() }));
                    return;
                }
                Ok(c) => *me.child.lock().unwrap() = Some(c),
            }
            me.sink.send(Event::Status(RuntimeStatus::Starting));
            me.wait_online();
            me.watch_child();
        });
    }

    fn check(&self) {
        self.spawn(|me| me.sink.send(Event::Health(me.version())));
    }

    fn shutdown(&self) {
        let child = self.child.lock().unwrap().take();
        if let Some(mut c) = child {
            terminate(&mut c);
        }
    }

    fn refresh_models(&self) {
        self.spawn(|me| {
            if let Ok(list) = me.list_models() {
                me.sink.send(Event::Installed(list));
            }
        });
    }

    fn pull(&self, model: &str, cancel: Arc<AtomicBool>) {
        let model = model.to_string();
        self.spawn(move |me| {
            let body = json!({ "model": model, "stream": true });
            let res = me.stream("/api/pull", body, &cancel, |v| {
                me.sink.send(Event::PullProgress {
                    model: model.clone(),
                    status: v["status"].as_str().unwrap_or("").to_string(),
                    completed: v["completed"].as_u64().unwrap_or(0),
                    total: v["total"].as_u64().unwrap_or(0),
                });
                Ok(true)
            });
            let error = match res {
                Err(e) => Some(e),
                Ok(()) if cancel.load(Ordering::Relaxed) => Some("cancelled".into()),
                Ok(()) => None,
            };
            me.sink.send(Event::PullDone { model, error });
            me.refresh_models();
        });
    }

    fn delete(&self, model: &str) {
        let model = model.to_string();
        self.spawn(move |me| {
            let res = me.http.delete(me.url("/api/delete")).json(&json!({ "model": model })).send();
            let error = match res {
                Err(e) => Some(e.to_string()),
                Ok(r) if !r.status().is_success() => {
                    let code = r.status();
                    Some(api_error(&r.text().unwrap_or_default()).unwrap_or_else(|| format!("HTTP {code}")))
                }
                Ok(_) => None,
            };
            me.sink.send(Event::Deleted { model, error });
            me.refresh_models();
        });
    }

    fn chat(&self, model: &str, history: Vec<ChatMessage>, num_ctx: u64, think: bool, cancel: Arc<AtomicBool>) {
        let model = model.to_string();
        self.spawn(move |me| {
            // only role, content and images go to the runtime; text files are
            // inlined; replies that failed before writing anything are left out
            let messages: Vec<_> = history
                .iter()
                .filter(|m| !(m.role == "assistant" && m.content.is_empty()))
                .map(|m| {
                    let mut msg = json!({ "role": m.role, "content": prompt_with_files(&m.content, &m.attachments) });
                    let imgs = images(&m.attachments);
                    if !imgs.is_empty() {
                        msg["images"] = json!(imgs);
                    }
                    msg
                })
                .collect();
            let mut body = json!({
                "model": model,
                "messages": messages,
                "stream": true,
                "options": { "num_ctx": num_ctx },
            });
            // only for models that support it: the others reject the field
            if think {
                body["think"] = json!(true);
            }
            let mut usage = None;
            let res = me.stream("/api/chat", body, &cancel, |v| {
                if let Some(t) = v["message"]["thinking"].as_str().filter(|t| !t.is_empty()) {
                    me.sink.send(Event::ChatThinking(t.to_string()));
                }
                if let Some(tok) = v["message"]["content"].as_str().filter(|t| !t.is_empty()) {
                    me.sink.send(Event::ChatToken(tok.to_string()));
                }
                let done = v["done"].as_bool().unwrap_or(false);
                if done {
                    usage = Some(Usage {
                        prompt_tokens: v["prompt_eval_count"].as_u64().unwrap_or(0),
                        completion_tokens: v["eval_count"].as_u64().unwrap_or(0),
                    });
                }
                Ok(!done)
            });
            me.sink.send(Event::ChatDone { error: res.err(), usage });
        });
    }

    fn model_info(&self, model: &str) {
        let model = model.to_string();
        self.spawn(move |me| {
            let info: Option<serde_json::Value> = me
                .http
                .post(me.url("/api/show"))
                .json(&json!({ "model": model }))
                .send()
                .and_then(|r| r.error_for_status())
                .and_then(|r| r.json())
                .ok();
            // key is "<architecture>.context_length", e.g. "llama.context_length"
            let context_length = info.as_ref().and_then(|v| v["model_info"].as_object()).and_then(|o| {
                o.iter().find(|(k, _)| k.ends_with(".context_length")).and_then(|(_, v)| v.as_u64())
            });
            let has = |cap: &str| {
                info.as_ref().and_then(|v| v["capabilities"].as_array()).is_some_and(|c| c.iter().any(|x| x == cap))
            };
            let (vision, thinking) = (has("vision"), has("thinking"));
            me.sink.send(Event::ModelInfo { model, context_length, vision, thinking });
        });
    }
}

impl Drop for Ollama {
    fn drop(&mut self) {
        if self.owner {
            self.shutdown();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;

    /// Needs a running Ollama with at least one model: `cargo test -- --ignored`.
    #[test]
    #[ignore]
    fn list_and_chat_against_local_ollama() {
        let (tx, rx) = mpsc::channel();
        let o = Ollama::new(&url_from_env(), Sink::new(tx, eframe::egui::Context::default()));
        println!("version {:?}", o.version());
        let models = o.list_models().expect("list");
        println!("installed: {:?}", models.iter().map(|m| &m.name).collect::<Vec<_>>());
        let smallest = models.iter().min_by_key(|m| m.size).expect("no model installed");
        let msg = ChatMessage::new("user", "Reply with one word: hello");
        o.chat(&smallest.name, vec![msg], 4096, false, Arc::new(AtomicBool::new(false)));
        let mut out = String::new();
        loop {
            match rx.recv_timeout(Duration::from_secs(120)).expect("timeout") {
                Event::ChatToken(t) => out.push_str(&t),
                Event::ChatDone { error, usage } => {
                    assert!(error.is_none(), "{error:?}");
                    println!("usage: {usage:?}");
                    break;
                }
                _ => {}
            }
        }
        println!("{} -> {out:?}", smallest.name);

        // Stop: the request is dropped at once and the reply ends without error
        let cancel = Arc::new(AtomicBool::new(false));
        let long = ChatMessage::new("user", "Count from 1 to 300, one number per line.");
        o.chat(&smallest.name, vec![long], 4096, false, cancel.clone());
        loop {
            match rx.recv_timeout(Duration::from_secs(120)).expect("timeout") {
                Event::ChatToken(_) => break,
                Event::ChatDone { error, .. } => panic!("ended before the first token: {error:?}"),
                _ => {}
            }
        }
        cancel.store(true, Ordering::Relaxed);
        let stopped_at = Instant::now();
        loop {
            if let Event::ChatDone { error, .. } = rx.recv_timeout(Duration::from_secs(5)).expect("no ChatDone after Stop") {
                assert!(error.is_none(), "{error:?}");
                break;
            }
        }
        assert!(stopped_at.elapsed() < Duration::from_millis(500), "{:?}", stopped_at.elapsed());
        o.model_info(&smallest.name);
        match rx.recv_timeout(Duration::from_secs(10)).expect("timeout") {
            Event::ModelInfo { context_length, .. } => {
                println!("context_length: {context_length:?}");
                assert!(context_length.is_some());
            }
            _ => panic!("unexpected event"),
        }
        assert!(!out.is_empty());
    }
}

#[cfg(test)]
mod line_tests {
    use super::take_line;

    #[test]
    fn ndjson_lines_split_across_chunks() {
        let mut buf = b"{\"a\":1}\n{\"b\"".to_vec();
        assert_eq!(take_line(&mut buf, false), Some(b"{\"a\":1}".to_vec()));
        assert_eq!(take_line(&mut buf, false), None); // incomplete, wait for more
        buf.extend_from_slice(b":2}");
        assert_eq!(take_line(&mut buf, true), Some(b"{\"b\":2}".to_vec())); // last line, no newline
        assert_eq!(take_line(&mut buf, true), None);
    }
}

#[cfg(test)]
mod url_tests {
    use super::resolve_url;

    #[test]
    fn ollama_host_forms() {
        assert_eq!(resolve_url(None), "http://127.0.0.1:11434");
        assert_eq!(resolve_url(Some("")), "http://127.0.0.1:11434");
        assert_eq!(resolve_url(Some("0.0.0.0")), "http://0.0.0.0:11434");
        assert_eq!(resolve_url(Some("myhost:9000")), "http://myhost:9000");
        assert_eq!(resolve_url(Some(":9000")), "http://127.0.0.1:9000");
        assert_eq!(resolve_url(Some("https://example.com/")), "https://example.com:443");
        assert_eq!(resolve_url(Some("http://10.0.0.2:11434")), "http://10.0.0.2:11434");
    }
}

/// These tests spawn a fake `ollama`, found first in PATH, from FAKE_OLLAMA_DIR:
///
/// ```sh
/// #!/bin/sh
/// echo $$ > "$FAKE_OLLAMA_DIR/pid"
/// if [ "$(cat "$FAKE_OLLAMA_DIR/mode")" = fail ]; then
///   echo "Error: listen tcp 127.0.0.1:11434: bind: address already in use" >&2; exit 1
/// fi
/// trap 'echo term > "$FAKE_OLLAMA_DIR/signal"; exit 0' TERM
/// while :; do sleep 0.1; done
/// ```
///
/// `FAKE_OLLAMA_DIR=d PATH=d:$PATH XDG_DATA_HOME=$(mktemp -d) cargo test -- --ignored --test-threads=1 spawn_tests`
#[cfg(test)]
mod spawn_tests {
    use super::*;

    fn fake(mode: &str) -> String {
        let dir = std::env::var("FAKE_OLLAMA_DIR").unwrap();
        std::fs::write(format!("{dir}/mode"), mode).unwrap();
        let _ = std::fs::remove_file(format!("{dir}/signal"));
        dir
    }

    /// The spawned server survives its spawning thread and is stopped with
    /// SIGTERM (a clean shutdown) when the app goes away.
    #[test]
    #[ignore]
    fn spawned_server_lives_until_app_drop() {
        let dir = fake("serve");
        let (tx, _rx) = std::sync::mpsc::channel();
        let o = Ollama::new("http://127.0.0.1:1", Sink::new(tx, eframe::egui::Context::default()));
        o.ensure_running();
        thread::sleep(Duration::from_secs(2));
        let pid = std::fs::read_to_string(format!("{dir}/pid")).unwrap().trim().to_string();
        assert!(std::path::Path::new(&format!("/proc/{pid}")).exists(), "server died too early");
        drop(o);
        let alive = std::fs::read_to_string(format!("/proc/{pid}/stat")).map(|s| !s.contains(") Z")).unwrap_or(false);
        assert!(!alive, "server still running after drop");
        assert_eq!(std::fs::read_to_string(format!("{dir}/signal")).unwrap().trim(), "term", "not stopped with SIGTERM");
    }

    /// A server that exits right away is reported at once, with its last log line.
    #[test]
    #[ignore]
    fn early_server_exit_is_reported_with_its_log() {
        fake("fail");
        let (tx, rx) = std::sync::mpsc::channel();
        let o = Ollama::new("http://127.0.0.1:1", Sink::new(tx, eframe::egui::Context::default()));
        let start = Instant::now();
        o.ensure_running();
        let error = loop {
            if let Event::Status(RuntimeStatus::Offline { error }) =
                rx.recv_timeout(Duration::from_secs(5)).expect("no Offline status in time")
            {
                break error;
            }
        };
        assert!(start.elapsed() < Duration::from_secs(5));
        assert!(error.contains("exited") && error.contains("address already in use"), "{error}");
    }
}

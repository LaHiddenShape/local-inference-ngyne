//! Ollama HTTP API client (https://github.com/ollama/ollama/blob/main/docs/api.md).

use std::io::{BufRead, BufReader};
use std::os::unix::process::CommandExt;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
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
            sink: self.sink.clone(),
            child: self.child.clone(),
            owner: false,
        }
    }
}

impl Ollama {
    pub fn new(base: &str, sink: Sink) -> Self {
        let http = Client::builder()
            .timeout(None::<Duration>) // streams can last minutes
            .connect_timeout(Duration::from_secs(3))
            .build()
            .expect("http client");
        Self { base: base.trim_end_matches('/').to_string(), http, sink, child: Arc::default(), owner: true }
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
    /// false, the stream ends, or `cancel` is set.
    fn stream(
        &self,
        path: &str,
        body: serde_json::Value,
        cancel: &AtomicBool,
        mut on_line: impl FnMut(serde_json::Value) -> Result<bool, String>,
    ) -> Result<(), String> {
        let resp = self.http.post(self.url(path)).json(&body).send().map_err(|e| e.to_string())?;
        if !resp.status().is_success() {
            let code = resp.status();
            let text = resp.text().unwrap_or_default();
            return Err(api_error(&text).unwrap_or_else(|| format!("HTTP {code}")));
        }
        for line in BufReader::new(resp).lines() {
            if cancel.load(Ordering::Relaxed) {
                return Ok(()); // dropping the response closes the connection
            }
            let line = line.map_err(|e| e.to_string())?;
            if line.trim().is_empty() {
                continue;
            }
            let v: serde_json::Value = serde_json::from_str(&line).map_err(|e| e.to_string())?;
            if let Some(err) = v.get("error").and_then(|e| e.as_str()) {
                return Err(err.to_string());
            }
            if !on_line(v)? {
                break;
            }
        }
        Ok(())
    }
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
            // not reachable: try to start it ourselves
            let mut cmd = Command::new("ollama");
            cmd.arg("serve").stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
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
            let start = Instant::now();
            let mut last_err = String::new();
            let mut online = false;
            while start.elapsed() < Duration::from_secs(20) {
                match me.version() {
                    Ok(version) => {
                        me.sink.send(Event::Status(RuntimeStatus::Online { version }));
                        me.refresh_models();
                        online = true;
                        break;
                    }
                    Err(e) => last_err = e,
                }
                thread::sleep(Duration::from_millis(300));
            }
            if !online {
                me.sink.send(Event::Status(RuntimeStatus::Offline { error: last_err }));
            }
            // PR_SET_PDEATHSIG fires when the *spawning thread* exits, so this
            // thread must outlive the server.
            drop(me);
            loop {
                thread::park();
            }
        });
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

    fn chat(&self, model: &str, history: Vec<ChatMessage>, num_ctx: u64, cancel: Arc<AtomicBool>) {
        let model = model.to_string();
        self.spawn(move |me| {
            // only role + content go to the runtime
            let messages: Vec<_> =
                history.iter().map(|m| json!({ "role": m.role, "content": m.content })).collect();
            let body = json!({
                "model": model,
                "messages": messages,
                "stream": true,
                "options": { "num_ctx": num_ctx },
            });
            let mut usage = None;
            let res = me.stream("/api/chat", body, &cancel, |v| {
                if let Some(tok) = v["message"]["content"].as_str() {
                    if !tok.is_empty() {
                        me.sink.send(Event::ChatToken(tok.to_string()));
                    }
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
            me.sink.send(Event::ModelInfo { model, context_length });
        });
    }
}

impl Drop for Ollama {
    fn drop(&mut self) {
        if self.owner {
            if let Some(mut c) = self.child.lock().unwrap().take() {
                let _ = c.kill();
                let _ = c.wait();
            }
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
        o.chat(&smallest.name, vec![msg], 4096, Arc::new(AtomicBool::new(false)));
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

#[cfg(test)]
mod spawn_tests {
    use super::*;

    /// Uses a fake `ollama` script from FAKE_OLLAMA_DIR: checks the spawned
    /// server survives its spawning thread and dies with the app.
    #[test]
    #[ignore]
    fn spawned_server_lives_until_app_drop() {
        let dir = std::env::var("FAKE_OLLAMA_DIR").unwrap();
        let (tx, _rx) = std::sync::mpsc::channel();
        let o = Ollama::new("http://127.0.0.1:1", Sink::new(tx, eframe::egui::Context::default()));
        o.ensure_running();
        thread::sleep(Duration::from_secs(2));
        let pid = std::fs::read_to_string(format!("{dir}/pid")).unwrap().trim().to_string();
        assert!(std::path::Path::new(&format!("/proc/{pid}")).exists(), "server died too early");
        drop(o);
        thread::sleep(Duration::from_millis(300));
        let alive = std::fs::read_to_string(format!("/proc/{pid}/stat")).map(|s| !s.contains(") Z")).unwrap_or(false);
        assert!(!alive, "server still running after drop");
    }
}

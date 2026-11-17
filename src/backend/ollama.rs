//! Ollama HTTP API client (https://github.com/ollama/ollama/blob/main/docs/api.md).

use std::io::{BufRead, BufReader};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use reqwest::blocking::Client;
use serde::Deserialize;
use serde_json::json;

use super::{Backend, ChatMessage, Event, InstalledModel, RuntimeStatus, Sink};

pub const DEFAULT_URL: &str = "http://127.0.0.1:11434";

#[derive(Clone)]
pub struct Ollama {
    base: String,
    http: Client,
    sink: Sink,
    /// `ollama serve` process spawned by us, if any (killed on exit).
    child: Arc<Mutex<Option<Child>>>,
}

impl Ollama {
    pub fn new(base: &str, sink: Sink) -> Self {
        let http = Client::builder()
            .timeout(None::<Duration>) // streams can last minutes
            .connect_timeout(Duration::from_secs(3))
            .build()
            .expect("http client");
        Self { base: base.trim_end_matches('/').to_string(), http, sink, child: Arc::default() }
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
    fn ensure_running(&self) {
        self.spawn(|me| {
            me.sink.send(Event::Status(RuntimeStatus::Checking));
            if let Ok(version) = me.version() {
                me.sink.send(Event::Status(RuntimeStatus::Online { version }));
                me.refresh_models();
                return;
            }
            // not reachable: try to start it ourselves
            let child = Command::new("ollama")
                .arg("serve")
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn();
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
            while start.elapsed() < Duration::from_secs(20) {
                match me.version() {
                    Ok(version) => {
                        me.sink.send(Event::Status(RuntimeStatus::Online { version }));
                        me.refresh_models();
                        return;
                    }
                    Err(e) => last_err = e,
                }
                thread::sleep(Duration::from_millis(300));
            }
            me.sink.send(Event::Status(RuntimeStatus::Offline { error: last_err }));
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

    fn chat(&self, model: &str, history: Vec<ChatMessage>, cancel: Arc<AtomicBool>) {
        let model = model.to_string();
        self.spawn(move |me| {
            let body = json!({ "model": model, "messages": history, "stream": true });
            let res = me.stream("/api/chat", body, &cancel, |v| {
                if let Some(tok) = v["message"]["content"].as_str() {
                    if !tok.is_empty() {
                        me.sink.send(Event::ChatToken(tok.to_string()));
                    }
                }
                Ok(!v["done"].as_bool().unwrap_or(false))
            });
            me.sink.send(Event::ChatDone { error: res.err() });
        });
    }
}

impl Drop for Ollama {
    fn drop(&mut self) {
        // only the last clone (owned by the app) stops the server we started
        if Arc::strong_count(&self.child) == 1 {
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
        let o = Ollama::new(DEFAULT_URL, Sink::new(tx, eframe::egui::Context::default()));
        println!("version {:?}", o.version());
        let models = o.list_models().expect("list");
        println!("installed: {:?}", models.iter().map(|m| &m.name).collect::<Vec<_>>());
        let smallest = models.iter().min_by_key(|m| m.size).expect("no model installed");
        let msg = ChatMessage { role: "user".into(), content: "Reply with one word: hello".into() };
        o.chat(&smallest.name, vec![msg], Arc::new(AtomicBool::new(false)));
        let mut out = String::new();
        loop {
            match rx.recv_timeout(Duration::from_secs(120)).expect("timeout") {
                Event::ChatToken(t) => out.push_str(&t),
                Event::ChatDone { error } => {
                    assert!(error.is_none(), "{error:?}");
                    break;
                }
                _ => {}
            }
        }
        println!("{} -> {out:?}", smallest.name);
        assert!(!out.is_empty());
    }
}

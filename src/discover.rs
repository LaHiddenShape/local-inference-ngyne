//! Live discovery of uncensored models on ollama.com.
//!
//! Ollama has no catalog API, so discovery is done in two layers:
//!
//! 1. **Listing** scrapes the search page (`ollama.com/search?q=…`), paginated
//!    with the same htmx request the site makes for infinite scroll. The only
//!    thing that must be found is each result's link (`href="/library/name"` or
//!    `href="/user/name"`), which is how the site itself navigates and is
//!    unlikely to change. Description, downloads, sizes and capabilities are
//!    extracted independently and best-effort: if one stops matching after a
//!    redesign, that field goes blank and the rest still works.
//! 2. **Details** (download size, context length, vision) come from Ollama's
//!    model registry, an OCI registry used by every `ollama pull`, plus the GGUF
//!    header of the weights read with a small HTTP range request. Both are
//!    standard formats that cannot change without breaking Ollama itself.
//!
//! Results are cached on disk; the built-in list is the last resort.

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::LazyLock;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use regex::Regex;
use reqwest::blocking::Client;
use serde::{Deserialize, Serialize};

/// Search terms: model authors use these words in names or descriptions.
const QUERIES: &[&str] = &["uncensored", "abliterated", "dolphin", "heretic"];
/// A result is kept only if its name or description contains one of these
/// (the site's search is fuzzy and returns unrelated models too).
const KEYWORDS: &[&str] = &["uncensor", "abliterat", "dolphin", "heretic", "unfilter", "unrestrict"];
const MAX_PAGES: usize = 60;
/// Cache older than this is refreshed when the window opens.
pub const CACHE_TTL_SECS: u64 = 24 * 3600;
const REGISTRY: &str = "https://registry.ollama.ai/v2";
const SITE: &str = "https://ollama.com";

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct RemoteModel {
    /// Name to pull without a tag: "dolphin3" or "user/model".
    pub name: String,
    /// Published in the official library (ollama.com/library).
    pub official: bool,
    pub description: String,
    pub downloads: Option<u64>,
    /// Size tags shown on the site, e.g. ["8b", "70b"].
    pub sizes: Vec<String>,
    /// Capabilities shown on the site, e.g. ["vision", "tools"].
    pub capabilities: Vec<String>,
}

impl RemoteModel {
    pub fn url(&self) -> String {
        crate::catalog::model_url(&self.name)
    }
}

impl RemoteModel {
    /// Size tags with their parameter count in billions, smallest first.
    pub fn params(&self) -> Vec<(String, f64)> {
        let mut v: Vec<_> = self.sizes.iter().filter_map(|s| Some((s.clone(), params_of(s)?))).collect();
        v.sort_by(|a, b| a.1.total_cmp(&b.1));
        v
    }
}

/// "8b" -> 8, "135m" -> 0.135, "8x7b" -> 56, "e2b" -> 2.
pub fn params_of(size: &str) -> Option<f64> {
    let s = size.trim().to_lowercase();
    let s = s.strip_prefix('e').unwrap_or(&s);
    let (num, scale) = if let Some(n) = s.strip_suffix('b') {
        (n, 1.0)
    } else {
        (s.strip_suffix('m')?, 0.001)
    };
    let value = match num.split_once('x') {
        Some((experts, each)) => experts.parse::<f64>().ok()? * each.parse::<f64>().ok()?,
        None => num.parse().ok()?,
    };
    Some(value * scale)
}

/// Rough download size of a 4-bit quantized model (Ollama's default).
pub fn estimated_bytes(params_b: f64) -> u64 {
    (params_b * 0.6 * crate::system::GB) as u64
}

fn repo_of(name: &str) -> String {
    if name.contains('/') { name.to_string() } else { format!("library/{name}") }
}

/// Registry facts about one tag.
#[derive(Clone, Debug, PartialEq)]
pub struct Details {
    pub size: u64,
    pub context_length: Option<u64>,
    pub vision: bool,
}

#[derive(Serialize, Deserialize)]
struct Cache {
    fetched_at: u64,
    models: Vec<RemoteModel>,
}

fn now_secs() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

fn cache_file() -> PathBuf {
    crate::store::data_dir().join("uncensored-cache.json")
}

/// Cached list and when it was fetched (Unix seconds).
pub fn load_cache() -> Option<(Vec<RemoteModel>, u64)> {
    let c: Cache = serde_json::from_str(&std::fs::read_to_string(cache_file()).ok()?).ok()?;
    (!c.models.is_empty()).then_some((c.models, c.fetched_at))
}

fn save_cache(models: &[RemoteModel]) {
    let c = Cache { fetched_at: now_secs(), models: models.to_vec() };
    let _ = std::fs::create_dir_all(crate::store::data_dir());
    let tmp = cache_file().with_extension("json.tmp");
    if std::fs::write(&tmp, serde_json::to_vec(&c).unwrap_or_default()).is_ok() {
        let _ = std::fs::rename(tmp, cache_file());
    }
}

pub fn client() -> Client {
    Client::builder()
        .user_agent("Mozilla/5.0 (local-inference-ui)")
        .connect_timeout(Duration::from_secs(5))
        .timeout(Duration::from_secs(20))
        .build()
        .expect("http client")
}

// ------------------------------------------------------------------- listing

/// Fetches every matching model, official first, then by downloads.
pub fn fetch_all(http: &Client) -> Result<Vec<RemoteModel>, String> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    let mut last_err = None;
    for q in QUERIES {
        for page in 1..=MAX_PAGES {
            let html = match fetch_page(http, q, page) {
                Ok(h) => h,
                Err(e) => {
                    last_err = Some(e);
                    break;
                }
            };
            let found = parse_page(&html);
            let mut new = 0;
            for m in found {
                if seen.insert(m.name.clone()) {
                    new += 1;
                    if is_relevant(&m) {
                        out.push(m);
                    }
                }
            }
            // past the last page the site repeats results or returns none
            if new == 0 {
                break;
            }
        }
    }
    if out.is_empty() {
        return Err(last_err.unwrap_or_else(|| "no model found on ollama.com (page format may have changed)".into()));
    }
    out.sort_by(|a, b| b.official.cmp(&a.official).then(b.downloads.cmp(&a.downloads)).then(a.name.cmp(&b.name)));
    save_cache(&out);
    Ok(out)
}

fn fetch_page(http: &Client, q: &str, page: usize) -> Result<String, String> {
    // `HX-Request` makes the site return the results fragment for any page
    // (a plain request for page > 1 redirects to page 1)
    // queries are fixed ASCII words, no encoding needed
    http.get(format!("{SITE}/search?q={q}&page={page}"))
        .header("HX-Request", "true")
        .send()
        .and_then(|r| r.error_for_status())
        .and_then(|r| r.text())
        .map_err(|e| format!("ollama.com: {e}"))
}

fn is_relevant(m: &RemoteModel) -> bool {
    let hay = format!("{} {}", m.name, m.description).to_lowercase();
    KEYWORDS.iter().any(|k| hay.contains(k))
}

static LINK: LazyLock<Regex> = LazyLock::new(|| {
    // `/library/name` or `/user/name` (one or two segments, model-name charset)
    Regex::new(r#"href="/(library/)?([A-Za-z0-9_-][A-Za-z0-9._-]*(?:/[A-Za-z0-9._-]+)?)""#).unwrap()
});
static PARAGRAPH: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?s)<p[^>]*>(.*?)</p>").unwrap());
static DOWNLOADS: LazyLock<Regex> = LazyLock::new(|| Regex::new(r#"title="([\d,.\s]+)\s*(?:downloads|pulls)""#).unwrap());
static SPAN_TEXT: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?s)<span[^>]*>\s*([^<]{1,24}?)\s*</span>").unwrap());
/// Lowercase only: "135m" is a size, "2M" is a download counter.
static SIZE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^(?:\d+x)?\d+(?:\.\d+)?[bm]$|^e\d+b$").unwrap());
static TAG: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"<[^>]+>").unwrap());

/// Site navigation links that look like model links.
const NOT_MODELS: &[&str] = &[
    "blog", "download", "pricing", "privacy", "search", "signin", "signup", "support", "terms", "public", "models",
    "docs", "settings", "cloud", "library", "about", "careers", "contact",
];
const CAPABILITIES: &[&str] = &["vision", "tools", "thinking", "embedding", "audio", "cloud"];

/// Splits the page at each model link; each chunk is one result card.
pub fn parse_page(html: &str) -> Vec<RemoteModel> {
    let links: Vec<_> = LINK.captures_iter(html).collect();
    let mut out: Vec<RemoteModel> = Vec::new();
    for (i, cap) in links.iter().enumerate() {
        let whole = cap.get(0).unwrap();
        let official = cap.get(1).is_some();
        let path = &cap[2];
        if !official && (!path.contains('/') || NOT_MODELS.contains(&path.split('/').next().unwrap_or(""))) {
            continue;
        }
        // official models have one segment; user models exactly two
        if (official && path.contains('/')) || (!official && path.split('/').count() != 2) {
            continue;
        }
        if out.iter().any(|m| m.name == path) {
            continue;
        }
        let end = links.get(i + 1).map(|c| c.get(0).unwrap().start()).unwrap_or(html.len());
        let card = &html[whole.end()..end];
        let description = PARAGRAPH
            .captures(card)
            .map(|c| html_text(&c[1]))
            .unwrap_or_default();
        let downloads = DOWNLOADS
            .captures(card)
            .and_then(|c| c[1].chars().filter(char::is_ascii_digit).collect::<String>().parse().ok());
        let mut sizes = Vec::new();
        let mut capabilities = Vec::new();
        for s in SPAN_TEXT.captures_iter(card) {
            let raw = s[1].trim();
            let t = raw.to_lowercase();
            if SIZE.is_match(raw) && !sizes.contains(&t) {
                sizes.push(t);
            } else if CAPABILITIES.contains(&t.as_str()) && !capabilities.contains(&t) {
                capabilities.push(t);
            }
        }
        out.push(RemoteModel { name: path.to_string(), official, description, downloads, sizes, capabilities });
    }
    out
}

fn html_text(s: &str) -> String {
    let t = TAG.replace_all(s, " ");
    let t = t
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&#x27;", "'");
    t.split_whitespace().collect::<Vec<_>>().join(" ")
}

// ------------------------------------------------------------------- details

/// Size, context length and vision support of `name[:tag]`, from the registry.
pub fn details(http: &Client, tag: &str) -> Result<Details, String> {
    let (name, version) = tag.split_once(':').unwrap_or((tag, "latest"));
    let manifest: serde_json::Value = http
        .get(format!("{REGISTRY}/{}/manifests/{version}", repo_of(name)))
        .header("Accept", "application/vnd.docker.distribution.manifest.v2+json")
        .send()
        .and_then(|r| r.error_for_status())
        .and_then(|r| r.json())
        .map_err(|e| format!("registry: {e}"))?;
    let layers = manifest["layers"].as_array().ok_or("registry: unexpected manifest")?;
    let media = |l: &serde_json::Value| l["mediaType"].as_str().unwrap_or("").to_string();
    let size = layers.iter().filter_map(|l| l["size"].as_u64()).sum();
    let model = layers.iter().find(|l| media(l).ends_with(".model"));
    let mut vision = layers.iter().any(|l| media(l).ends_with(".projector"));
    let mut context_length = None;
    if let Some(digest) = model.and_then(|l| l["digest"].as_str()) {
        let url = format!("{REGISTRY}/{}/blobs/{digest}", repo_of(name));
        if let Some(meta) = gguf_meta(http, &url) {
            context_length = meta.context_length;
            vision |= meta.vision;
        }
    }
    Ok(Details { size, context_length, vision })
}

#[derive(Debug, Default, PartialEq)]
struct GgufMeta {
    context_length: Option<u64>,
    vision: bool,
}

/// Reads the start of a GGUF file, growing the range until the header's
/// key/values are parsed (the context length is near the start, before the
/// large tokenizer arrays).
fn gguf_meta(http: &Client, url: &str) -> Option<GgufMeta> {
    let mut len = 256 * 1024;
    while len <= 16 * 1024 * 1024 {
        let bytes = http
            .get(url)
            .header("Range", format!("bytes=0-{}", len - 1))
            .send()
            .ok()?
            .error_for_status()
            .ok()?
            .bytes()
            .ok()?;
        match parse_gguf(&bytes) {
            Ok(m) => return Some(m),
            Err(Truncated) if bytes.len() >= len => len *= 4,
            Err(Truncated) => return None,
        }
    }
    None
}

struct Truncated;

/// Parses GGUF key/values (spec: github.com/ggml-org/ggml/blob/master/docs/gguf.md),
/// stopping as soon as the context length is known.
fn parse_gguf(b: &[u8]) -> Result<GgufMeta, Truncated> {
    struct R<'a> {
        b: &'a [u8],
        o: usize,
    }
    impl R<'_> {
        fn take(&mut self, n: usize) -> Result<&[u8], Truncated> {
            let s = self.b.get(self.o..self.o.checked_add(n).ok_or(Truncated)?).ok_or(Truncated)?;
            self.o += n;
            Ok(s)
        }
        fn u32(&mut self) -> Result<u32, Truncated> {
            Ok(u32::from_le_bytes(self.take(4)?.try_into().unwrap()))
        }
        fn u64(&mut self) -> Result<u64, Truncated> {
            Ok(u64::from_le_bytes(self.take(8)?.try_into().unwrap()))
        }
        fn string(&mut self) -> Result<String, Truncated> {
            let n = self.u64()? as usize;
            Ok(String::from_utf8_lossy(self.take(n)?).into_owned())
        }
        /// Reads a value; integers are returned, other types skipped.
        fn value(&mut self, t: u32) -> Result<Option<u64>, Truncated> {
            let fixed = |t| match t {
                0 | 1 | 7 => Some(1),
                2 | 3 => Some(2),
                4 | 5 | 6 => Some(4),
                10 | 11 | 12 => Some(8),
                _ => None,
            };
            match t {
                4 => Ok(Some(self.u32()? as u64)),
                10 => Ok(Some(self.u64()?)),
                8 => self.string().map(|_| None),
                9 => {
                    let at = self.u32()?;
                    let n = self.u64()? as usize;
                    match fixed(at) {
                        Some(sz) => {
                            self.take(n.checked_mul(sz).ok_or(Truncated)?)?;
                        }
                        None => {
                            for _ in 0..n {
                                self.value(at)?;
                            }
                        }
                    }
                    Ok(None)
                }
                t => match fixed(t) {
                    Some(sz) => self.take(sz).map(|_| None),
                    None => Err(Truncated), // unknown type: give up
                },
            }
        }
    }
    let mut r = R { b, o: 0 };
    if r.take(4)? != b"GGUF" {
        return Ok(GgufMeta::default());
    }
    r.u32()?; // version
    r.u64()?; // tensor count
    let n = r.u64()?;
    let mut meta = GgufMeta::default();
    for _ in 0..n {
        let key = r.string()?;
        let t = r.u32()?;
        let v = r.value(t)?;
        if key.contains("vision") || key.starts_with("clip.") {
            meta.vision = true;
        }
        if key.ends_with(".context_length") && !key.contains("vision") {
            meta.context_length = v;
            return Ok(meta);
        }
    }
    Ok(meta)
}

#[cfg(test)]
mod tests {
    use super::*;

    const CARD: &str = r##"<ul><li><a href="/library/dolphin-mixtral" class="x">
      <h2 title="dolphin-mixtral"><span >dolphin-mixtral</span></h2>
      <span title="Updated December 20, 2024">1y</span>
      <p class="mt-1">Uncensored, 8x7b &amp; 8x22b models.</p>
      <span title="2,006,377 downloads"><svg></svg><span >2M</span></span>
      <span><svg></svg><span >70 tags</span></span>
      <span class="b">8x7b</span><span class="b">8x22b</span></a></li>
      <li><a href="/someone/qwen3-abliterated"><span>someone/qwen3-abliterated</span>
      <span>vision</span><span>tools</span><span title="440 downloads">440</span></a></li>
      <a href="/blog">Blog</a><a href="/library/qwen3">Qwen</a></ul>"##;

    #[test]
    fn parses_cards_and_skips_navigation() {
        let m = parse_page(CARD);
        assert_eq!(m.len(), 3);
        assert_eq!(m[0].name, "dolphin-mixtral");
        assert!(m[0].official);
        assert_eq!(m[0].description, "Uncensored, 8x7b & 8x22b models.");
        assert_eq!(m[0].downloads, Some(2_006_377));
        assert_eq!(m[0].sizes, ["8x7b", "8x22b"]);
        assert_eq!(m[1].name, "someone/qwen3-abliterated");
        assert!(!m[1].official);
        assert_eq!(m[1].capabilities, ["vision", "tools"]);
        assert_eq!(m[1].downloads, Some(440));
        assert!(is_relevant(&m[0]) && is_relevant(&m[1]) && !is_relevant(&m[2]));
    }

    #[test]
    fn parameter_counts() {
        assert_eq!(params_of("8b"), Some(8.0));
        assert_eq!(params_of("135m"), Some(0.135));
        assert_eq!(params_of("8x7b"), Some(56.0));
        assert_eq!(params_of("e2b"), Some(2.0));
        assert_eq!(params_of("2.7b"), Some(2.7));
        assert_eq!(params_of("latest"), None);
    }

    #[test]
    fn gguf_header_context_length() {
        let mut b = b"GGUF".to_vec();
        b.extend(3u32.to_le_bytes());
        b.extend(0u64.to_le_bytes());
        b.extend(2u64.to_le_bytes());
        let kv = |b: &mut Vec<u8>, k: &str| {
            b.extend((k.len() as u64).to_le_bytes());
            b.extend(k.as_bytes());
        };
        kv(&mut b, "general.architecture");
        b.extend(8u32.to_le_bytes());
        kv(&mut b, "llama");
        kv(&mut b, "llama.context_length");
        b.extend(4u32.to_le_bytes());
        b.extend(131072u32.to_le_bytes());
        assert_eq!(parse_gguf(&b).ok(), Some(GgufMeta { context_length: Some(131072), vision: false }));
        assert!(parse_gguf(&b[..b.len() - 2]).is_err());
    }

    /// Hits ollama.com: run with `cargo test -- --ignored live`.
    #[test]
    #[ignore]
    fn live_listing_and_details() {
        let http = client();
        let page = fetch_page(&http, "dolphin", 1).unwrap();
        let models = parse_page(&page);
        let d3 = models.iter().find(|m| m.name == "dolphin3").expect("dolphin3 listed");
        assert!(d3.official && d3.downloads.is_some() && !d3.description.is_empty());
        let d = details(&http, "dolphin3:8b").unwrap();
        assert_eq!(d.context_length, Some(131072));
        assert!(d.size > 4_000_000_000);
    }

    /// Full discovery (writes the app's cache): `cargo test -- --ignored live_full --nocapture`.
    #[test]
    #[ignore]
    fn live_full_discovery() {
        let http = client();
        let t = std::time::Instant::now();
        let all = fetch_all(&http).unwrap();
        let official = all.iter().filter(|m| m.official).count();
        println!("{} models ({official} official) in {:?}", all.len(), t.elapsed());
        for m in all.iter().take(12) {
            println!("{:45} {:?} {:?}", m.name, m.downloads, m.sizes);
        }
        let community = all.iter().find(|m| !m.official).unwrap();
        println!("{} -> {:?}", community.name, details(&http, &community.name));
        assert!(official >= 8);
    }
}

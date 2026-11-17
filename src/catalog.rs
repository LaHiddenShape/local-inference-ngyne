//! Curated list of models offered in the UI. Ollama has no public API for its
//! library, so this list is maintained by hand. Sizes are the default
//! (4-bit quantized) download sizes from ollama.com.

pub struct CatalogModel {
    /// Full Ollama tag, e.g. "llama3.2:3b".
    pub tag: &'static str,
    pub params: &'static str,
    pub size_gb: f64,
    pub description: &'static str,
}

impl CatalogModel {
    /// Official model page, so the user can verify the source.
    pub fn url(&self) -> String {
        model_url(self.tag)
    }

    pub fn size_bytes(&self) -> u64 {
        (self.size_gb * crate::system::GB) as u64
    }
}

/// Source page of a model tag:
/// `llama3.2:3b` -> ollama.com/library/..., `user/model` -> ollama.com/user/model,
/// `hf.co/org/repo:quant` -> huggingface.co/org/repo.
pub fn model_url(tag: &str) -> String {
    if let Some(rest) = tag.strip_prefix("hf.co/").or_else(|| tag.strip_prefix("huggingface.co/")) {
        let repo = rest.split(':').next().unwrap_or(rest);
        return format!("https://huggingface.co/{repo}");
    }
    if tag.contains('/') {
        return format!("https://ollama.com/{tag}");
    }
    format!("https://ollama.com/library/{tag}")
}

pub const CATALOG: &[CatalogModel] = &[
    CatalogModel { tag: "llama3.2:1b", params: "1B", size_gb: 1.3, description: "Meta. Tiny and fast, good for simple tasks." },
    CatalogModel { tag: "llama3.2:3b", params: "3B", size_gb: 2.0, description: "Meta. Small general-purpose assistant." },
    CatalogModel { tag: "gemma3:4b", params: "4B", size_gb: 3.3, description: "Google. Compact, multilingual, vision capable." },
    CatalogModel { tag: "mistral:7b", params: "7B", size_gb: 4.1, description: "Mistral AI. Solid all-rounder." },
    CatalogModel { tag: "qwen2.5:7b", params: "7B", size_gb: 4.7, description: "Alibaba. Strong at code, math and multilingual." },
    CatalogModel { tag: "llama3.1:8b", params: "8B", size_gb: 4.9, description: "Meta. Popular general-purpose model." },
    CatalogModel { tag: "qwen3:8b", params: "8B", size_gb: 5.2, description: "Alibaba. Hybrid thinking / non-thinking modes." },
    CatalogModel { tag: "deepseek-r1:8b", params: "8B", size_gb: 5.2, description: "DeepSeek. Reasoning model (shows its thinking)." },
    CatalogModel { tag: "gemma3:12b", params: "12B", size_gb: 8.1, description: "Google. Higher quality, needs a mid-range GPU." },
    CatalogModel { tag: "qwen2.5:14b", params: "14B", size_gb: 9.0, description: "Alibaba. Larger, better reasoning and code." },
    CatalogModel { tag: "phi4:14b", params: "14B", size_gb: 9.1, description: "Microsoft. Strong reasoning for its size." },
    CatalogModel { tag: "gemma3:27b", params: "27B", size_gb: 17.0, description: "Google. High quality, needs 24 GB VRAM." },
    CatalogModel { tag: "llama3.3:70b", params: "70B", size_gb: 43.0, description: "Meta. Near frontier quality, very demanding." },
];

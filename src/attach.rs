//! Files attached to a user message: text files are inlined into the prompt,
//! images are sent base64-encoded to vision models.

use std::path::Path;

use base64::Engine;
use serde::{Deserialize, Serialize};

/// Larger text files would not fit in any local model's context anyway.
const MAX_TEXT_BYTES: u64 = 512 * 1024;
const MAX_IMAGE_BYTES: u64 = 20 * 1024 * 1024;
const IMAGE_EXTENSIONS: &[&str] = &["png", "jpg", "jpeg", "webp", "gif", "bmp"];

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum AttachmentData {
    Text(String),
    /// Base64-encoded file content.
    Image(String),
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Attachment {
    pub name: String,
    pub data: AttachmentData,
}

impl Attachment {
    pub fn is_image(&self) -> bool {
        matches!(self.data, AttachmentData::Image(_))
    }

    /// Reads a file: images by extension, anything else must be UTF-8 text.
    pub fn load(path: &Path) -> Result<Self, String> {
        let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "file".into());
        let size = std::fs::metadata(path).map_err(|e| format!("{name}: {e}"))?.len();
        let ext = path.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default();
        if IMAGE_EXTENSIONS.contains(&ext.as_str()) {
            if size > MAX_IMAGE_BYTES {
                return Err(format!("{name}: image larger than 20 MB"));
            }
            let bytes = std::fs::read(path).map_err(|e| format!("{name}: {e}"))?;
            let b64 = base64::engine::general_purpose::STANDARD.encode(bytes);
            return Ok(Self { name, data: AttachmentData::Image(b64) });
        }
        if size > MAX_TEXT_BYTES {
            return Err(format!("{name}: text file larger than 512 KB"));
        }
        let bytes = std::fs::read(path).map_err(|e| format!("{name}: {e}"))?;
        match String::from_utf8(bytes) {
            Ok(text) if !text.contains('\0') => Ok(Self { name, data: AttachmentData::Text(text) }),
            _ => Err(format!("{name}: not a text file or a supported image")),
        }
    }
}

/// Prompt sent to the runtime: text attachments first, then the message.
pub fn prompt_with_files(content: &str, attachments: &[Attachment]) -> String {
    let mut out = String::new();
    for a in attachments {
        if let AttachmentData::Text(text) = &a.data {
            out.push_str(&format!("File `{}`:\n```\n{}\n```\n\n", a.name, text.trim_end()));
        }
    }
    out.push_str(content);
    out
}

/// Base64 images to pass in the runtime's `images` field.
pub fn images(attachments: &[Attachment]) -> Vec<&str> {
    attachments
        .iter()
        .filter_map(|a| match &a.data {
            AttachmentData::Image(b64) => Some(b64.as_str()),
            AttachmentData::Text(_) => None,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loads_text_and_images_and_rejects_binaries() {
        let dir = std::env::temp_dir().join(format!("attach-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let txt = dir.join("notes.md");
        std::fs::write(&txt, "hello").unwrap();
        let png = dir.join("pic.PNG");
        std::fs::write(&png, [0x89, b'P', b'N', b'G']).unwrap();
        let bin = dir.join("prog.bin");
        std::fs::write(&bin, [0u8, 159, 146, 150]).unwrap();

        let t = Attachment::load(&txt).unwrap();
        assert_eq!(t.data, AttachmentData::Text("hello".into()));
        let i = Attachment::load(&png).unwrap();
        assert!(i.is_image());
        assert!(Attachment::load(&bin).is_err());

        let prompt = prompt_with_files("summarize", &[t, i.clone()]);
        assert_eq!(prompt, "File `notes.md`:\n```\nhello\n```\n\nsummarize");
        assert_eq!(images(&[i]).len(), 1);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}

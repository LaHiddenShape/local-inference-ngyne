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

/// Length in bytes of [`prompt_with_files`], without building it.
pub fn prompt_len(content: &str, attachments: &[Attachment]) -> usize {
    let files: usize = attachments
        .iter()
        .map(|a| match &a.data {
            // "File `{name}`:\n```\n{text}\n```\n\n"
            AttachmentData::Text(text) => a.name.len() + text.trim_end().len() + 19,
            AttachmentData::Image(_) => 0,
        })
        .sum();
    files + content.len()
}

/// Rough token count of `bytes` of text (about 4 bytes per token).
pub fn estimate_tokens(bytes: usize) -> u64 {
    bytes.div_ceil(4) as u64
}

/// Cheap identity of an attachment, to cache its preview: its name, size and
/// both ends of its data (hashing a 20 MB image every frame would not be).
pub fn fingerprint(a: &Attachment) -> u64 {
    use std::hash::{Hash, Hasher};
    let data = match &a.data {
        AttachmentData::Text(t) | AttachmentData::Image(t) => t.as_bytes(),
    };
    let ends = (&data[..data.len().min(1024)], &data[data.len().saturating_sub(1024)..]);
    let mut h = std::collections::hash_map::DefaultHasher::new();
    (&a.name, data.len(), ends).hash(&mut h);
    h.finish()
}

/// Decodes a base64 image into a preview at most `max` pixels on each side.
pub fn thumbnail(b64: &str, max: u32) -> Option<eframe::egui::ColorImage> {
    let bytes = base64::engine::general_purpose::STANDARD.decode(b64).ok()?;
    let img = image::load_from_memory(&bytes).ok()?.thumbnail(max, max).to_rgba8();
    Some(eframe::egui::ColorImage::from_rgba_unmultiplied([img.width() as usize, img.height() as usize], img.as_raw()))
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

        let prompt = prompt_with_files("summarize", &[t.clone(), i.clone()]);
        assert_eq!(prompt, "File `notes.md`:\n```\nhello\n```\n\nsummarize");
        assert_eq!(prompt_len("summarize", &[t, i.clone()]), prompt.len());
        assert_eq!(images(&[i]).len(), 1);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn previews_are_small_and_keyed_cheaply() {
        let mut png = Vec::new();
        image::RgbaImage::from_pixel(400, 200, image::Rgba([200, 150, 60, 255]))
            .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
            .unwrap();
        let b64 = base64::engine::general_purpose::STANDARD.encode(&png);
        let thumb = thumbnail(&b64, 64).unwrap();
        assert_eq!(thumb.size, [64, 32]);
        assert!(thumbnail("bm90IGFuIGltYWdl", 64).is_none()); // "not an image"
        let a = Attachment { name: "a.png".into(), data: AttachmentData::Image(b64) };
        assert_eq!(fingerprint(&a), fingerprint(&a.clone()));
        assert_ne!(fingerprint(&a), fingerprint(&Attachment { name: "b.png".into(), ..a.clone() }));
    }
}

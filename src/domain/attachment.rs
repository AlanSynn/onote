//! Attachments bounded context (`CLAUDE.md` §3.1 Attachments).
//!
//! Owns images and binary files. Link-style policy (Markdown vs Obsidian) and
//! deterministic timestamped naming live here; the actual clipboard/decode work
//! is in `infra`.

use std::path::PathBuf;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use super::vault::RelativeNotePath;

/// Preferred image link syntax (`CLAUDE.md` §1.2). Markdown is the portable default.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum LinkStyle {
    #[default]
    Markdown,
    Obsidian,
}

/// Raw image bytes captured from the clipboard (before persistence).
#[derive(Debug, Clone)]
pub struct ImageData {
    pub bytes: Vec<u8>,
    /// e.g. `image/png`.
    pub mime: String,
    pub width: u32,
    pub height: u32,
}

impl ImageData {
    /// File extension for the mime, lowercase without dot.
    pub fn extension(&self) -> &str {
        match self.mime.as_str() {
            "image/png" => "png",
            "image/jpeg" => "jpg",
            "image/gif" => "gif",
            "image/webp" => "webp",
            "image/bmp" => "bmp",
            _ => "png",
        }
    }
}

/// A persisted attachment record.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Attachment {
    /// Path relative to the vault root.
    pub path: RelativeNotePath,
    pub mime: String,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub size_bytes: u64,
    pub created_at: i64,
}

/// A reference parsed out of a Markdown token, normalized to a vault-relative path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttachmentReference {
    /// The vault-relative path the token points at.
    pub target: RelativeNotePath,
    pub style: LinkStyle,
}

impl AttachmentReference {
    /// Build the textual token to embed in a note body for the given style.
    pub fn render_token(style: LinkStyle, rel: &RelativeNotePath) -> String {
        let s = rel.as_str();
        match style {
            LinkStyle::Markdown => format!("![]({s})"),
            LinkStyle::Obsidian => format!("![[{s}]]"),
        }
    }
}

/// Deterministic, timestamped image filename policy (`CLAUDE.md` §1.2 / §3.1).
///
/// Format: `img-YYYYMMDD-HHMMSS.<ext>`. Pure function of an injected clock so the
/// domain stays free of ambient time.
pub struct AttachmentNamer;
impl AttachmentNamer {
    pub fn name(now: DateTime<Utc>, ext: &str) -> String {
        format!("img-{}.{ext}", now.format("%Y%m%d-%H%M%S"))
    }

    /// Year/month subpath under the attachment dir: `Attachments/2026/07/<name>`.
    pub fn dir_under(attachment_dir: &str, now: DateTime<Utc>) -> PathBuf {
        PathBuf::from(attachment_dir)
            .join(now.format("%Y").to_string())
            .join(now.format("%m").to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::vault::RelativeNotePath;

    #[test]
    fn markdown_token_is_portable() {
        let rel = RelativeNotePath::new("Attachments/2026/07/x.png").unwrap();
        assert_eq!(
            AttachmentReference::render_token(LinkStyle::Markdown, &rel),
            "![](Attachments/2026/07/x.png)"
        );
        assert_eq!(
            AttachmentReference::render_token(LinkStyle::Obsidian, &rel),
            "![[Attachments/2026/07/x.png]]"
        );
    }

    #[test]
    fn namer_uses_utc_compact() {
        let t = DateTime::parse_from_rfc3339("2026-07-07T12:03:01Z")
            .unwrap()
            .with_timezone(&Utc);
        assert_eq!(AttachmentNamer::name(t, "png"), "img-20260707-120301.png");
        assert_eq!(
            AttachmentNamer::dir_under("Attachments", t),
            PathBuf::from("Attachments/2026/07")
        );
    }
}

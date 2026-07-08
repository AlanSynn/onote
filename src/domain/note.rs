//! Notes bounded context (`CLAUDE.md` §3.1 Notes).
//!
//! Owns note identity, title, body, metadata, and edit state. Pure data + rules;
//! parsing is done by the `infra::markdown` adapter.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::errors::NoteError;
use super::vault::RelativeNotePath;

/// Non-cryptographic, process-stable content hash (FNV-1a 64-bit, hex).
///
/// Used purely for change/conflict detection (`CLAUDE.md` §7), never for security.
/// FNV-1a is deterministic across processes and platforms, which is all we need.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ContentHash(pub String);

impl ContentHash {
    /// Hash raw bytes with FNV-1a 64-bit and return lowercase hex.
    pub fn of_bytes(bytes: &[u8]) -> Self {
        const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
        const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;
        let mut h = FNV_OFFSET;
        for &b in bytes {
            h ^= b as u64;
            h = h.wrapping_mul(FNV_PRIME);
        }
        Self(format!("{h:016x}"))
    }

    pub fn of_str(s: &str) -> Self {
        Self::of_bytes(s.as_bytes())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Display title of a note.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NoteTitle(pub String);

impl NoteTitle {
    pub fn new(s: impl Into<String>) -> Result<Self, NoteError> {
        let s = s.into();
        if s.trim().is_empty() {
            return Err(NoteError::EmptyTitle);
        }
        Ok(Self(s))
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Raw Markdown body bytes-as-text.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MarkdownBody(pub String);

impl MarkdownBody {
    pub fn as_str(&self) -> &str {
        &self.0
    }
    pub fn into_string(self) -> String {
        self.0
    }
}

/// Parsed YAML-ish frontmatter (simple `key: value` map).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct NoteFrontmatter {
    pub fields: BTreeMap<String, String>,
}

impl NoteFrontmatter {
    pub fn get(&self, key: &str) -> Option<&str> {
        self.fields.get(key).map(|s| s.as_str())
    }
    pub fn is_empty(&self) -> bool {
        self.fields.is_empty()
    }
}

/// Lightweight entry for listings / fuzzy search.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NoteSummary {
    pub path: RelativeNotePath,
    pub title: String,
    pub modified_at: i64,
}

/// A full note loaded from the vault.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NoteDocument {
    pub path: RelativeNotePath,
    pub title: NoteTitle,
    pub body: MarkdownBody,
    pub frontmatter: NoteFrontmatter,
    /// Hash of the body as last read from disk (may differ from current `body`).
    pub content_hash: ContentHash,
    pub modified_at: i64,
}

impl NoteDocument {
    /// Hash of the *current in-memory* body.
    pub fn current_hash(&self) -> ContentHash {
        ContentHash::of_str(self.body.as_str())
    }

    pub fn is_dirty(&self) -> bool {
        self.current_hash() != self.content_hash
    }

    /// Parse raw note text into a document: splits frontmatter, extracts a title
    /// (frontmatter → H1 → stem), hashes the raw bytes. Centralized for DRY
    /// (`CLAUDE.md` §5) — readers and tests share one path.
    pub fn from_raw(path: RelativeNotePath, raw: &str, modified_at: i64) -> Self {
        let (frontmatter, body_only) = split_frontmatter(raw);
        let title = TitleExtractor::extract(&frontmatter, &body_only, &path.stem());
        Self {
            path,
            title: NoteTitle(title),
            body: MarkdownBody(raw.to_string()),
            frontmatter,
            content_hash: ContentHash::of_str(raw),
            modified_at,
        }
    }
}

/// Title extraction policy, centralized for DRY (`CLAUDE.md` §5).
///
/// Priority: frontmatter `title` → first H1 → filename stem.
pub struct TitleExtractor;
impl TitleExtractor {
    pub fn extract(frontmatter: &NoteFrontmatter, body: &str, fallback_stem: &str) -> String {
        if let Some(t) = frontmatter.get("title") {
            let t = t.trim();
            if !t.is_empty() {
                return t.to_string();
            }
        }
        for line in body.lines() {
            let trimmed = line.trim_start();
            if let Some(rest) = trimmed.strip_prefix("# ") {
                let h1 = rest.trim().to_string();
                if !h1.is_empty() {
                    return h1;
                }
            }
            // skip leading blank lines but stop at first non-heading content
            if !trimmed.is_empty() && !trimmed.starts_with('#') {
                break;
            }
        }
        if fallback_stem.is_empty() {
            "Untitled".to_string()
        } else {
            fallback_stem.to_string()
        }
    }
}

/// Split leading YAML-ish frontmatter (`---\n...\n---`) from the body, for the
/// domain's title-extraction path (frontmatter `title:` → H1 → stem).
///
/// NOTE on DRY: frontmatter is intentionally parsed in TWO places. The comrak
/// renderer strips it independently via `front_matter_delimiter` (see
/// `infra/markdown::build_options`) so it never leaks into share HTML or
/// attachment scans; THIS function parses it into `key: value` pairs for the
/// title extractor. The two paths serve different consumers (render-strip vs.
/// field-extract) and comrak does not expose parsed frontmatter fields, so the
/// duplication is by design. Values are simple `key: value` strings — nested
/// structures are out of scope for MVP.
pub fn split_frontmatter(raw: &str) -> (NoteFrontmatter, String) {
    let mut fields = BTreeMap::new();
    let mut body = raw.to_string();

    let stripped = raw
        .strip_prefix("---\n")
        .or_else(|| raw.strip_prefix("---\r\n"));
    if let Some(rest) = stripped {
        if let Some(end) = find_fm_fence(rest) {
            let fm_block = &rest[..end.0];
            let after = &rest[end.1..];
            for line in fm_block.lines() {
                if let Some((k, v)) = line.split_once(':') {
                    let key = k.trim().to_string();
                    let val = v.trim().trim_matches('"').trim_matches('\'').to_string();
                    if !key.is_empty() {
                        fields.insert(key, val);
                    }
                }
            }
            body = after.trim_start_matches(['\n', '\r']).to_string();
        }
    }
    (NoteFrontmatter { fields }, body)
}

fn find_fm_fence(s: &str) -> Option<(usize, usize)> {
    // find a line that is exactly `---` (allow trailing CR)
    let mut pos = 0;
    for line in s.split_inclusive('\n') {
        let trimmed = line.trim_end_matches('\n').trim_end_matches('\r');
        if trimmed == "---" {
            return Some((pos, pos + line.len()));
        }
        pos += line.len();
    }
    None
}

/// A hit from full-text search.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SearchHit {
    pub path: RelativeNotePath,
    pub title: String,
    /// Short context snippet around the match (may be empty).
    pub snippet: String,
    pub score: i64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn content_hash_stable() {
        let a = ContentHash::of_str("hello");
        let b = ContentHash::of_str("hello");
        assert_eq!(a, b);
        assert_ne!(a, ContentHash::of_str("hello!"));
        assert_eq!(a.as_str().len(), 16);
    }

    #[test]
    fn title_from_h1_when_no_frontmatter() {
        let fm = NoteFrontmatter::default();
        let title = TitleExtractor::extract(&fm, "# Robot Idea\nbody", "scratch");
        assert_eq!(title, "Robot Idea");
    }

    #[test]
    fn title_frontmatter_wins() {
        let mut fm = NoteFrontmatter::default();
        fm.fields.insert("title".into(), "FM Title".into());
        let title = TitleExtractor::extract(&fm, "# Other", "scratch");
        assert_eq!(title, "FM Title");
    }

    #[test]
    fn title_fallback_stem() {
        let fm = NoteFrontmatter::default();
        let title = TitleExtractor::extract(&fm, "no heading here", "Scratch");
        assert_eq!(title, "Scratch");
    }
}

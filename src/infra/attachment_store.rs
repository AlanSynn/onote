//! `AttachmentStore` over the vault filesystem (`CLAUDE.md` §3.1 Attachments).
//!
//! Pasted images land under `<attachment_dir>/<YYYY>/<MM>/img-<ts>.<ext>` with
//! deterministic names; reference scans walk the vault reusing the link extractor
//! (DRY — no second link parser, `CLAUDE.md` §5).

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::UNIX_EPOCH;

use crate::domain::attachment::{Attachment, AttachmentNamer, AttachmentReference, ImageData};
use crate::domain::errors::AttachmentError;
use crate::domain::note::MarkdownBody;
use crate::domain::vault::RelativeNotePath;
// One source of truth (§5): reuse the vault walker's set rather than
// re-declaring it, so the attachment reference scan, `list_notes`, and the file
// watcher can never drift on which dirs hold app/git internals vs user notes.
use crate::infra::filesystem_vault::SKIP_DIRS;
use crate::ports::{AttachmentStore, Clock, MarkdownLinkExtractor};

pub struct FilesystemAttachmentStore {
    root: PathBuf,
    attachment_dir: String,
    link_extractor: Arc<dyn MarkdownLinkExtractor>,
    clock: Arc<dyn Clock>,
}

impl FilesystemAttachmentStore {
    pub fn new(
        root: PathBuf,
        attachment_dir: String,
        link_extractor: Arc<dyn MarkdownLinkExtractor>,
        clock: Arc<dyn Clock>,
    ) -> Self {
        Self {
            root,
            attachment_dir,
            link_extractor,
            clock,
        }
    }
}

impl AttachmentStore for FilesystemAttachmentStore {
    fn save_image(&self, image: ImageData) -> Result<Attachment, AttachmentError> {
        let now = self.clock.now();
        let ext = image.extension().to_string();
        let name = AttachmentNamer::name(now, &ext);
        let dir_rel = AttachmentNamer::dir_under(&self.attachment_dir, now);
        let rel = RelativeNotePath::new(dir_rel.join(&name))
            .map_err(|e| AttachmentError::NotFound(e.to_string()))?;
        let abs = rel
            .resolve_within(&self.root)
            .map_err(|e| AttachmentError::NotFound(e.to_string()))?;
        if let Some(parent) = abs.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&abs, &image.bytes)?;
        let size_bytes = image.bytes.len() as u64;
        Ok(Attachment {
            path: rel,
            mime: image.mime,
            width: Some(image.width),
            height: Some(image.height),
            size_bytes,
            created_at: now.timestamp(),
        })
    }

    fn resolve(&self, reference: &AttachmentReference) -> Result<Attachment, AttachmentError> {
        let abs = reference
            .target
            .resolve_within(&self.root)
            .map_err(|e| AttachmentError::NotFound(e.to_string()))?;
        let meta = std::fs::metadata(&abs)?;
        let created_at = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        Ok(Attachment {
            path: reference.target.clone(),
            mime: guess_mime(&abs),
            width: None,
            height: None,
            size_bytes: meta.len(),
            created_at,
        })
    }

    fn is_referenced_elsewhere(
        &self,
        attachment: &Attachment,
        excluding_note: Option<&RelativeNotePath>,
    ) -> Result<bool, AttachmentError> {
        let target = attachment.path.as_str();
        for rel in walk_notes(&self.root) {
            if excluding_note == Some(&rel) {
                continue;
            }
            let Ok(abs) = rel.resolve_within(&self.root) else {
                continue;
            };
            let Ok(body) = std::fs::read_to_string(&abs) else {
                continue;
            };
            let refs = self
                .link_extractor
                .extract_attachment_links(&MarkdownBody(body));
            if refs.iter().any(|r| r.target.as_str() == target) {
                return Ok(true);
            }
        }
        Ok(false)
    }
}

fn guess_mime(path: &Path) -> String {
    match path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
        .as_deref()
    {
        Some("png") => "image/png".into(),
        Some("jpg") | Some("jpeg") => "image/jpeg".into(),
        Some("gif") => "image/gif".into(),
        Some("webp") => "image/webp".into(),
        Some("bmp") => "image/bmp".into(),
        Some("svg") => "image/svg+xml".into(),
        _ => "application/octet-stream".into(),
    }
}

/// Recursively yield `*.md` notes relative to `root`, skipping meta directories.
fn walk_notes(root: &Path) -> Vec<RelativeNotePath> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for ent in entries.flatten() {
            let path = ent.path();
            let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            // No-follow `file_type()` (mirrors `walk_md` in `filesystem_vault`):
            // unlike `Path::is_dir()` — which follows symlinks via `metadata` —
            // `DirEntry::file_type()` reports the entry's own type, so a symlink
            // is returned as `is_symlink()` (not `is_dir()`) and a symlinked
            // directory is never descended into (CPU-DoS / traversal-attempt
            // surface; content still can't escape the vault via the later
            // `strip_prefix(root)`).
            let Ok(ft) = ent.file_type() else {
                continue;
            };
            if ft.is_dir() {
                if SKIP_DIRS.contains(&name) || name.starts_with('.') {
                    continue;
                }
                stack.push(path);
            } else if path.extension().and_then(|e| e.to_str()) == Some("md") {
                if let Ok(rel) = path.strip_prefix(root) {
                    let s = rel.to_string_lossy().replace('\\', "/");
                    if let Ok(rp) = RelativeNotePath::from_user(&s) {
                        out.push(rp);
                    }
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::attachment::LinkStyle;
    use crate::ports::Clock;
    use chrono::{DateTime, Utc};
    use std::sync::Mutex;

    struct Fixed(Mutex<DateTime<Utc>>);
    impl Clock for Fixed {
        fn now(&self) -> DateTime<Utc> {
            *self.0.lock().unwrap()
        }
    }

    struct ExtractAll;
    impl MarkdownLinkExtractor for ExtractAll {
        fn extract_attachment_links(&self, body: &MarkdownBody) -> Vec<AttachmentReference> {
            body.as_str()
                .lines()
                .filter(|l| l.contains("Attachments"))
                .filter_map(|l| {
                    let s = l.trim();
                    Some(AttachmentReference {
                        target: RelativeNotePath::from_user(s).ok()?,
                        style: LinkStyle::Markdown,
                    })
                })
                .collect()
        }
    }

    #[test]
    fn save_image_writes_under_year_month() {
        let tmp = tempfile::tempdir().unwrap();
        let clock = Arc::new(Fixed(Mutex::new(
            DateTime::parse_from_rfc3339("2026-07-07T12:03:01Z")
                .unwrap()
                .with_timezone(&Utc),
        )));
        let store = FilesystemAttachmentStore::new(
            tmp.path().to_path_buf(),
            "Attachments".into(),
            Arc::new(ExtractAll),
            clock,
        );
        let img = ImageData {
            bytes: vec![0x89, b'P', b'N', b'G'],
            mime: "image/png".into(),
            width: 10,
            height: 20,
        };
        let att = store.save_image(img).unwrap();
        assert!(att
            .path
            .as_str()
            .starts_with("Attachments/2026/07/img-20260707-120301.png"));
        assert_eq!(att.size_bytes, 4);
    }

    // ── is_referenced_elsewhere ───────────────────────────────────────────────
    //
    // "Elsewhere" = "referenced by a note OTHER than `excluding_note`" (the note
    // currently deleting its image token). This is the §3.1 data-safety gate that
    // decides whether deleting an image token may also delete the file.

    fn ref_store(root: PathBuf) -> FilesystemAttachmentStore {
        let clock = Arc::new(Fixed(Mutex::new(
            DateTime::parse_from_rfc3339("2026-07-07T12:03:01Z")
                .unwrap()
                .with_timezone(&Utc),
        )));
        FilesystemAttachmentStore::new(root, "Attachments".into(), Arc::new(ExtractAll), clock)
    }

    fn attachment_at(rel: &str) -> Attachment {
        Attachment {
            path: RelativeNotePath::from_user(rel).unwrap(),
            mime: "image/png".into(),
            width: Some(10),
            height: Some(20),
            size_bytes: 4,
            created_at: 0,
        }
    }

    #[test]
    fn is_referenced_elsewhere_single_reference_truth_table() {
        // One note references the attachment. "Elsewhere" is relative to the
        // deleter: excluding the only referencing note → no other → false;
        // excluding a different note (or nothing) → the ref counts → true.
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().to_path_buf();
        std::fs::write(root.join("note-a.md"), "Attachments/2026/07/seal.png\n").unwrap();
        std::fs::write(root.join("note-b.md"), "no image here\n").unwrap();

        let store = ref_store(root);
        let att = attachment_at("Attachments/2026/07/seal.png");
        let note_a = RelativeNotePath::from_user("note-a.md").unwrap();
        let note_b = RelativeNotePath::from_user("note-b.md").unwrap();

        // Excluding the only referencing note → no OTHER reference → false.
        assert!(!store.is_referenced_elsewhere(&att, Some(&note_a)).unwrap());
        // Excluding a non-referencing note → note-a still counts → true.
        assert!(store.is_referenced_elsewhere(&att, Some(&note_b)).unwrap());
        // No exclusion → any reference counts → true.
        assert!(store.is_referenced_elsewhere(&att, None).unwrap());
    }

    #[test]
    fn is_referenced_elsewhere_reflects_removed_reference() {
        // Two notes reference the attachment; one then drops its reference. The
        // scan walks live disk, so the result reflects the new state without any
        // re-index call.
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().to_path_buf();
        std::fs::write(root.join("note-a.md"), "Attachments/2026/07/gull.png\n").unwrap();
        std::fs::write(root.join("note-b.md"), "Attachments/2026/07/gull.png\n").unwrap();

        let store = ref_store(root.clone());
        let att = attachment_at("Attachments/2026/07/gull.png");
        let note_a = RelativeNotePath::from_user("note-a.md").unwrap();

        // Both reference; excluding note-a → note-b still holds a ref → true.
        assert!(store.is_referenced_elsewhere(&att, Some(&note_a)).unwrap());

        // note-b rewrites its body, dropping the image token.
        std::fs::write(root.join("note-b.md"), "no image anymore\n").unwrap();

        // Now only note-a references; excluding it → no OTHER → false.
        assert!(!store.is_referenced_elsewhere(&att, Some(&note_a)).unwrap());
    }

    #[test]
    fn is_referenced_elsewhere_false_when_unreferenced() {
        // Zero references anywhere → always false, regardless of exclusion.
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().to_path_buf();
        std::fs::write(root.join("note-a.md"), "just text, no images\n").unwrap();

        let store = ref_store(root);
        let att = attachment_at("Attachments/2026/07/orphan.png");
        let note_a = RelativeNotePath::from_user("note-a.md").unwrap();

        assert!(!store.is_referenced_elsewhere(&att, None).unwrap());
        assert!(!store.is_referenced_elsewhere(&att, Some(&note_a)).unwrap());
    }
}

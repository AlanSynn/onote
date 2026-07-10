//! Ports — small, segregated traits the application layer depends on (`CLAUDE.md`
//! §3.3, §4 Interface Segregation). Infrastructure provides the concrete
//! adapters; tests provide fakes. Nothing here knows about files/SQLite/git/TUI.

use std::path::PathBuf;
use std::sync::mpsc;

use chrono::{DateTime, Local, Utc};

use crate::domain::attachment::{Attachment, AttachmentReference, ImageData};
use crate::domain::backup::{BackupMessage, BackupReport, BackupState};
use crate::domain::errors::{
    AttachmentError, BackupError, ClipboardError, IndexError, ShareError, VaultError,
};
use crate::domain::note::{ContentHash, MarkdownBody, NoteDocument, NoteSummary, SearchHit};
use crate::domain::session::ExternalChange;
use crate::domain::share::{SharePolicy, ShareSession, ShareSnapshot};
use crate::domain::vault::{RelativeNotePath, VaultEntry};

/// Result of an optimistic write (`CLAUDE.md` §3.3, §7).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WriteResult {
    /// Written; carries the new on-disk hash.
    Written(ContentHash),
    /// Caller asked to write but buffer matched disk — nothing happened.
    NoChange,
    /// Disk hash differed from `expected_hash`; caller must resolve.
    Conflict { current_disk_hash: ContentHash },
}

/// `CLAUDE.md` §3.3 VaultRepository.
pub trait VaultRepository: Send + Sync {
    fn list_notes(&self) -> Result<Vec<NoteSummary>, VaultError>;
    fn read_note(&self, path: &RelativeNotePath) -> Result<NoteDocument, VaultError>;
    fn write_note(
        &self,
        note: &NoteDocument,
        expected_hash: Option<&ContentHash>,
    ) -> Result<WriteResult, VaultError>;
    fn create_note(
        &self,
        title: &str,
        folder: Option<&RelativeNotePath>,
    ) -> Result<RelativeNotePath, VaultError>;
    fn delete_note(&self, path: &RelativeNotePath) -> Result<(), VaultError>;
    /// Create a folder at `path` (Explorer file-ops, `CLAUDE.md` §3.2). Must not
    /// escape the vault root (§3.1). Idempotent: an existing folder is a no-op.
    fn create_folder(&self, path: &RelativeNotePath) -> Result<(), VaultError>;
    /// Move/rename a note OR folder from `from` to `to` (Explorer file-ops).
    /// Both paths are confined to the vault root (§3.1); a busy `to` is refused
    /// rather than silently overwritten (§7 "never overwrite").
    fn rename_entry(
        &self,
        from: &RelativeNotePath,
        to: &RelativeNotePath,
    ) -> Result<(), VaultError>;
    /// Delete a note (file) OR folder (recursive) at `path` (Explorer file-ops).
    /// Confined to the vault root (§3.1). Missing → `NoteNotFound`.
    fn delete_entry(&self, path: &RelativeNotePath) -> Result<(), VaultError>;
    /// Recursive vault tree (folders + `.md` notes) for the Explorer drawer
    /// (`CLAUDE.md` §3.2 `note_drawer`). Returns top-level entries only; folders
    /// carry nested `children`. Folders-first + alphabetical, excluding cache/
    /// config dirs and dotfiles. Read straight from the source-of-truth files
    /// (§6) — no index state.
    fn list_tree(&self) -> Result<Vec<VaultEntry>, VaultError>;
    /// Current hash of bytes on disk, or `None` if the file does not exist.
    fn disk_hash(&self, path: &RelativeNotePath) -> Result<Option<ContentHash>, VaultError>;
}

/// `CLAUDE.md` §3.3 NoteIndex.
pub trait NoteIndex: Send + Sync {
    fn refresh_note(&self, note: &NoteDocument) -> Result<(), IndexError>;
    fn remove_note(&self, path: &RelativeNotePath) -> Result<(), IndexError>;
    fn fuzzy_titles(&self, query: &str) -> Result<Vec<NoteSummary>, IndexError>;
    fn full_text_search(&self, query: &str) -> Result<Vec<SearchHit>, IndexError>;
    /// Replace the entire index with `notes` atomically (clear `notes` +
    /// `notes_fts`, then reinsert all in one transaction). Bootstraps the derived
    /// cache (§6) from the source-of-truth files so an existing vault's notes are
    /// searchable without first opening each one, and evicts rows for notes
    /// deleted externally since the last run.
    fn rebuild(&self, notes: &[NoteDocument]) -> Result<(), IndexError>;
}

/// `CLAUDE.md` §3.3 AttachmentStore.
pub trait AttachmentStore: Send + Sync {
    fn save_image(&self, image: ImageData) -> Result<Attachment, AttachmentError>;
    fn resolve(&self, reference: &AttachmentReference) -> Result<Attachment, AttachmentError>;
    fn is_referenced_elsewhere(
        &self,
        attachment: &Attachment,
        excluding_note: Option<&RelativeNotePath>,
    ) -> Result<bool, AttachmentError>;
}

/// `CLAUDE.md` §2.9 / §3.3 Clipboard.
pub trait Clipboard: Send + Sync {
    fn read_text(&self) -> Result<Option<String>, ClipboardError>;
    fn read_image(&self) -> Result<Option<ImageData>, ClipboardError>;
    fn write_text(&self, text: &str) -> Result<(), ClipboardError>;
    fn write_html(&self, html: &str, plain_text: &str) -> Result<(), ClipboardError>;
    fn write_image(&self, image: &ImageData) -> Result<(), ClipboardError>;
}

/// `CLAUDE.md` §3.3 ShareServer.
pub trait ShareServer: Send + Sync {
    fn start(
        &self,
        snapshot: ShareSnapshot,
        policy: SharePolicy,
    ) -> Result<ShareSession, ShareError>;
    fn stop(&self) -> Result<(), ShareError>;
    fn local_url(&self) -> Option<String>;
}

/// `CLAUDE.md` §3.3 BackupService.
pub trait BackupService: Send + Sync {
    fn status(&self) -> Result<BackupState, BackupError>;
    fn commit(&self, message: BackupMessage) -> Result<BackupReport, BackupError>;
    fn push(&self) -> Result<BackupReport, BackupError>;
    fn pull_ff_only(&self) -> Result<BackupReport, BackupError>;
}

/// File watching port (`CLAUDE.md` §2.5). Adapters debounce and push changes.
pub trait FileWatcher: Send + Sync {
    /// Begin watching `paths` (files and/or directories). Returns a receiver of
    /// external changes; drop the watcher to stop.
    fn watch(&self, paths: &[PathBuf]) -> Result<mpsc::Receiver<ExternalChange>, VaultError>;
}

/// Markdown → HTML renderer (`CLAUDE.md` §2.3).
pub trait MarkdownRenderer: Send + Sync {
    fn render_html(&self, body: &MarkdownBody) -> String;
}

/// Extracts attachment references from note bodies (`CLAUDE.md` §1.2, §5).
pub trait MarkdownLinkExtractor: Send + Sync {
    fn extract_attachment_links(&self, body: &MarkdownBody) -> Vec<AttachmentReference>;

    /// Extracts note-link targets for in-vault navigation (`CLAUDE.md` §1.2, §2.3).
    ///
    /// Returns link TARGET strings for both standard Markdown `[text](url)`
    /// links to local paths and `[[wikilink]]` targets (alias stripped to the
    /// part before `|`). External URLs (`http`/`https`/`mailto`) are skipped —
    /// only local/in-vault links matter for navigation. The default returns
    /// empty so existing fakes and `dyn` dispatch stay source-compatible.
    fn extract_note_links(&self, _body: &MarkdownBody) -> Vec<String> {
        Vec::new()
    }

    /// Extracts `#tag` names (WITHOUT the leading `#`) from the note body
    /// (`CLAUDE.md` §1.2). The default returns empty so existing fakes and
    /// `dyn` dispatch stay source-compatible.
    fn extract_tags(&self, _body: &MarkdownBody) -> Vec<String> {
        Vec::new()
    }
}

/// Probes/loads image bytes for terminal preview (`CLAUDE.md` §2.4). The UI
/// decides how (or whether) to render; this port keeps `ratatui-image` out of
/// the domain.
pub trait ImageRenderer: Send + Sync {
    fn load(&self, abs_path: &std::path::Path) -> Result<LoadedImage, AttachmentError>;
}

/// Image bytes + metadata ready for terminal rendering.
///
/// `bytes` are the **raw file bytes** (not re-encoded): the UI layer decodes
/// once for rendering, so there is a single full decode rather than a
/// decode→re-encode→decode round-trip across the port boundary. Dimensions come
/// from a header probe (cheap). `size_bytes` is the on-disk file size.
#[derive(Debug, Clone)]
pub struct LoadedImage {
    /// Raw file bytes.
    pub bytes: Vec<u8>,
    pub mime: String,
    pub width: u32,
    pub height: u32,
    pub size_bytes: u64,
}

/// Opens a note in the Obsidian GUI via its URI scheme (`CLAUDE.md` §2.10).
pub trait UriLauncher: Send + Sync {
    fn open(&self, note_path: &RelativeNotePath) -> Result<(), VaultError>;
}

/// Time port so timestamped naming is injectable and the domain stays free of an
/// ambient clock (`CLAUDE.md` §3.1 Attachments).
pub trait Clock: Send + Sync {
    fn now(&self) -> DateTime<Utc>;

    /// Same instant as [`now`](Self::now) projected to the LOCAL timezone, for
    /// human-facing labels (daily-note date, backup message). Epoch
    /// `.timestamp()` values stay UTC (timezone-independent); only formatted
    /// wall-clock strings use this, so a note edited at 23:30 local lands in
    /// today's daily note, not yesterday's. Default derives Local from `now()` so
    /// every Clock impl (including test fakes) gets it for free.
    fn now_local(&self) -> DateTime<Local> {
        self.now().with_timezone(&Local)
    }
}

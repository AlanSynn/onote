//! Application use cases (`CLAUDE.md` §3.2, §3.3, §7).
//!
//! All ops return `anyhow::Result`; conflict/save outcomes are typed enums so the
//! UI can react without parsing errors (`CLAUDE.md` §2.11).

use std::sync::mpsc::Receiver;

use anyhow::{anyhow, Context, Result};

use crate::application::{App, OpenNote};
use crate::domain::attachment::{Attachment, AttachmentReference, ImageData, LinkStyle};
use crate::domain::backup::{BackupMessage, BackupReport, BackupState};
use crate::domain::note::{ContentHash, MarkdownBody, NoteDocument, NoteSummary, SearchHit};
use crate::domain::session::ExternalChange;
use crate::domain::share::{SharePolicy, ShareSession};
use crate::domain::vault::{EntryKind, RelativeNotePath, VaultEntry};
use crate::ports::WriteResult;

/// Replace control bytes (except `\t`) in untrusted strings before logging, to
/// prevent log-line forgery and terminal escape injection (e.g. a note filename
/// with embedded `\r\n` or ANSI/OSC-52 sequences).
///
/// Applied only to interpolated untrusted values (note paths/titles), never to
/// tracing's own static format strings.
fn sanitize_for_log(s: impl AsRef<str>) -> String {
    s.as_ref()
        .chars()
        .map(|c| if c.is_control() && c != '\t' { '·' } else { c })
        .collect()
}

/// Human label for an [`EntryKind`] used in error context strings.
fn kind_label(kind: EntryKind) -> &'static str {
    match kind {
        EntryKind::Note => "note",
        EntryKind::Folder => "folder",
    }
}

/// Compose the rename target path: the SAME parent directory as `from`, with a
/// leaf built from `name` — slugified + `.md` for a note (reusing the infra
/// `slugify` so rename matches `create_note`'s filename rule), or the raw name
/// for a folder. An empty note slug falls back to `untitled`, mirroring
/// `create_note`. `RelativeNotePath::new` re-validates, so a `..`/absolute name
/// is rejected here (§3.1) before reaching the infra.
fn compose_rename_target(
    from: &RelativeNotePath,
    name: &str,
    kind: EntryKind,
) -> Result<RelativeNotePath> {
    let mut target = from
        .as_path()
        .parent()
        .map(std::path::PathBuf::from)
        .unwrap_or_default();
    let leaf = match kind {
        EntryKind::Note => {
            let stem = crate::domain::slug::slugify(name);
            let stem = if stem.is_empty() {
                "untitled".to_string()
            } else {
                stem
            };
            format!("{stem}.md")
        }
        EntryKind::Folder => name.to_string(),
    };
    target.push(&leaf);
    Ok(RelativeNotePath::new(target)?)
}

/// Outcome of a save, surfacing §7 conflict state explicitly.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SaveOutcome {
    /// Written to disk; new content hash.
    Written(ContentHash),
    /// Buffer matched disk — nothing written.
    NoChange,
    /// Disk changed under us; caller must reload / merge / conflict-copy.
    Conflict { current_disk_hash: ContentHash },
}

/// Clipboard copy format selector (`CLAUDE.md` §8 `onote copy`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CopyFormat {
    Markdown,
    Html,
    Rich,
}

impl CopyFormat {
    /// Lowercase human label for the "copied as …" confirmation message.
    pub fn label(self) -> &'static str {
        match self {
            Self::Markdown => "markdown",
            Self::Html => "html",
            Self::Rich => "rich text",
        }
    }
}

/// Result of pasting an image from the clipboard.
#[derive(Debug, Clone)]
pub struct PastedImage {
    pub attachment: Attachment,
    /// Ready-to-insert token (`![](...)` or `![[...]]`) per configured link style.
    pub token: String,
}

/// Outcome of an Explorer rename/move (Spike 7 P7.4): the entry's new path,
/// plus the open note's new path IF the rename relocated it (so the UI can
/// follow the editor to its new location without a manual re-open).
#[derive(Debug, Clone)]
pub struct RenameOutcome {
    /// The entry's new vault-relative path.
    pub new_path: RelativeNotePath,
    /// New path of the open note, if it sat at or under the renamed entry.
    pub relocated_current: Option<RelativeNotePath>,
}

impl App {
    // ── Listing / opening ────────────────────────────────────────────────────

    pub fn list_notes(&self) -> Result<Vec<NoteSummary>> {
        Ok(self.deps().vault.list_notes()?)
    }

    /// Recursive vault tree for the Explorer drawer (`CLAUDE.md` §3.2). Pure
    /// delegation to the port — the tree is read straight from the
    /// source-of-truth files (§6) with no index state, so there is nothing for
    /// the use case to coordinate here (unlike `open_note`, which also refreshes
    /// the index). Folders-first + alphabetical ordering is the adapter's job.
    pub fn list_vault_tree(&self) -> Result<Vec<VaultEntry>> {
        Ok(self.deps().vault.list_tree()?)
    }

    pub fn search(&self, query: &str) -> Result<Vec<SearchHit>> {
        Ok(self.deps().index.full_text_search(query)?)
    }

    pub fn fuzzy(&self, query: &str) -> Result<Vec<NoteSummary>> {
        Ok(self.deps().index.fuzzy_titles(query)?)
    }

    /// Open a note by path, refresh the index, and record its baseline hash.
    pub fn open_note(&self, path: &RelativeNotePath) -> Result<NoteDocument> {
        let doc = self.deps().vault.read_note(path)?;
        if let Err(e) = self.deps().index.refresh_note(&doc) {
            tracing::warn!(note = %sanitize_for_log(doc.path.as_str()), error = ?e, "index refresh failed; search may be stale");
        }
        self.set_current(OpenNote {
            path: path.clone(),
            opened_hash: doc.content_hash.clone(),
        });
        Ok(doc)
    }

    /// Open the configured default note, creating it if absent.
    pub fn open_default(&self) -> Result<NoteDocument> {
        let path = self.config().vault_layout().default_note_relative()?;
        match self.deps().vault.read_note(&path) {
            Ok(doc) => {
                self.set_current(OpenNote {
                    path,
                    opened_hash: doc.content_hash.clone(),
                });
                if let Err(e) = self.deps().index.refresh_note(&doc) {
                    tracing::warn!(note = %sanitize_for_log(doc.path.as_str()), error = ?e, "index refresh failed; search may be stale");
                }
                Ok(doc)
            }
            Err(crate::domain::errors::VaultError::NoteNotFound(_)) => {
                self.create_note_at(&path)?;
                self.open_note(&path)
            }
            Err(e) => Err(e.into()),
        }
    }

    pub fn current_note(&self) -> Option<OpenNote> {
        self.current()
    }

    // ── Create / delete ───────────────────────────────────────────────────────

    pub fn create_note(
        &self,
        title: &str,
        folder: Option<&RelativeNotePath>,
    ) -> Result<RelativeNotePath> {
        let path = self.deps().vault.create_note(title, folder)?;
        // A freshly-created note must be searchable immediately — index it now
        // rather than only when it's later opened (§6 index tracks the
        // source-of-truth files). `sync_index_for` re-reads from disk so the
        // hash/title are authoritative. Non-fatal: a miss only degrades search.
        self.sync_index_for(&path);
        Ok(path)
    }

    /// Create a note at an explicit path (used by default/daily bootstrap).
    pub(crate) fn create_note_at(&self, path: &RelativeNotePath) -> Result<()> {
        let body = format!("# {}\n", path.stem());
        let mtime = self.now().timestamp();
        let doc = NoteDocument::from_raw(path.clone(), &body, mtime);
        self.deps().vault.write_note(&doc, None)?;
        Ok(())
    }

    pub fn delete_note(&self, path: &RelativeNotePath) -> Result<()> {
        // Delegate to the entry-general path (Spike 7 P7.4 DRY): same file
        // removal + targeted index eviction + current-clear, just without the
        // folder branch. The bool (current-deleted) is unused at the CLI call.
        let _deleted_current = self.delete_entry(path, EntryKind::Note)?;
        Ok(())
    }

    /// Create an empty folder at `path` (Explorer file-ops, `CLAUDE.md` §3.2).
    /// Idempotent (an existing folder is a no-op). Folders aren't indexed, so no
    /// index work follows. §3.1 confinement is the infra's job.
    pub fn create_folder(&self, path: &RelativeNotePath) -> Result<()> {
        self.deps().vault.create_folder(path).with_context(|| {
            format!(
                "failed to create folder {}",
                sanitize_for_log(path.as_str())
            )
        })
    }

    /// Rename/move entry `from` to a new leaf `name` in the SAME parent
    /// directory (Explorer file-ops). `name` is the new display name; a NOTE name
    /// is slugified + `.md` (identical rule to `create_note`, via the shared
    /// `slugify`), a FOLDER name is kept verbatim. §3.1 confinement + §7
    /// never-overwrite hold (infra refuses a busy target). Returns the new path
    /// and the relocated open-note path (if any) so the UI can follow.
    pub fn rename_entry(
        &self,
        from: &RelativeNotePath,
        name: &str,
        kind: EntryKind,
    ) -> Result<RenameOutcome> {
        let to = compose_rename_target(from, name, kind)?;
        self.deps().vault.rename_entry(from, &to).with_context(|| {
            format!(
                "failed to rename {} {} → {}",
                kind_label(kind),
                sanitize_for_log(from.as_str()),
                sanitize_for_log(to.as_str()),
            )
        })?;
        // Keep the index honest (§6). A folder move shifts many rows → rebuild;
        // a note move is a targeted remove + refresh.
        match kind {
            EntryKind::Note => {
                if let Err(e) = self.deps().index.remove_note(from) {
                    tracing::warn!(
                        note = %sanitize_for_log(from.as_str()),
                        error = ?e,
                        "index remove failed; search may be stale"
                    );
                }
                self.sync_index_for(&to);
            }
            EntryKind::Folder => {
                if let Err(e) = self.reindex_all() {
                    tracing::warn!(
                        error = ?e,
                        "reindex after folder rename failed; search may be stale"
                    );
                }
            }
        }
        let relocated_current = self.relocate_current(from, &to);
        Ok(RenameOutcome {
            new_path: to,
            relocated_current,
        })
    }

    /// Delete a note OR folder at `path` (Explorer file-ops). Returns whether the
    /// OPEN note was deleted (the UI must then load another note). §3.1
    /// confinement holds; a missing entry surfaces `NoteNotFound`.
    ///
    /// For a FOLDER delete, the open note counts as deleted if it sits AT or UNDER
    /// the folder (not just on an exact path match) — `remove_dir_all` wipes the
    /// whole subtree, so a nested open note's file is gone too. Without this the
    /// editor would keep a `state.path` pointing at a file that no longer exists
    /// and the next save would silently re-create it (§7 baseline corruption).
    /// Parity with `relocate_current`'s nested-prefix handling on the rename path.
    pub fn delete_entry(&self, path: &RelativeNotePath, kind: EntryKind) -> Result<bool> {
        self.deps().vault.delete_entry(path).with_context(|| {
            format!(
                "failed to delete {} {}",
                kind_label(kind),
                sanitize_for_log(path.as_str())
            )
        })?;
        let deleted_current = match kind {
            // Exact match: only that note's file was removed.
            EntryKind::Note => self.current().is_some_and(|n| &n.path == path),
            // The whole subtree is gone — the open note is hit if it is the folder
            // itself or nested under `<path>/`.
            EntryKind::Folder => self.current().is_some_and(|n| {
                let open = n.path.as_str();
                open == path.as_str() || open.starts_with(&format!("{}/", path.as_str()))
            }),
        };
        match kind {
            EntryKind::Note => {
                if let Err(e) = self.deps().index.remove_note(path) {
                    tracing::warn!(
                        note = %sanitize_for_log(path.as_str()),
                        error = ?e,
                        "index remove failed; search may be stale"
                    );
                }
            }
            EntryKind::Folder => {
                if let Err(e) = self.reindex_all() {
                    tracing::warn!(
                        error = ?e,
                        "reindex after folder delete failed; search may be stale"
                    );
                }
            }
        }
        if deleted_current {
            self.clear_current();
        }
        Ok(deleted_current)
    }

    /// If the open note is at `from` or nested under `from/`, remap it to the
    /// corresponding path under `to` (Spike 7 P7.4). Keeps the editor's §7 save
    /// baseline valid after a rename/move of the open note OR one of its ancestor
    /// folders — otherwise `state.path` would point at a path that no longer
    /// exists and the next save would silently re-create the old file. Returns
    /// the new path if remapped, else `None`.
    fn relocate_current(
        &self,
        from: &RelativeNotePath,
        to: &RelativeNotePath,
    ) -> Option<RelativeNotePath> {
        let open = self.current()?;
        let from_s = from.as_str();
        let open_s = open.path.as_str();
        let new_path = if open_s == from_s {
            // The renamed entry WAS the open note.
            to.clone()
        } else {
            // Open note nested under a renamed ancestor folder: swap the prefix.
            let rest = open_s.strip_prefix(&format!("{from_s}/"))?;
            let joined = std::path::PathBuf::from(to.as_str()).join(rest);
            RelativeNotePath::new(joined).ok()?
        };
        self.set_current(OpenNote {
            path: new_path.clone(),
            opened_hash: open.opened_hash,
        });
        Some(new_path)
    }

    /// Keep the index in sync with disk for ONE path: refresh if the note
    /// exists, remove it if it has been deleted. The single source of truth for
    /// "make the index match what's on disk for this path" — used after
    /// `create_note` and for every external-change notification (§6: the index
    /// must track the source-of-truth files, including external edits/deletes
    /// from Obsidian, `git pull`, or another terminal).
    ///
    /// Non-fatal: an index miss only degrades search, never note data — so a
    /// refresh/remove failure is logged (`CLAUDE.md` §2.11) rather than
    /// propagated, mirroring the existing post-save index-refresh policy.
    pub fn sync_index_for(&self, path: &RelativeNotePath) {
        match self.deps().vault.read_note(path) {
            Ok(doc) => {
                if let Err(e) = self.deps().index.refresh_note(&doc) {
                    tracing::warn!(
                        note = %sanitize_for_log(path.as_str()),
                        error = ?e,
                        "index refresh failed; search may be stale"
                    );
                }
            }
            Err(crate::domain::errors::VaultError::NoteNotFound(_)) => {
                if let Err(e) = self.deps().index.remove_note(path) {
                    tracing::warn!(
                        note = %sanitize_for_log(path.as_str()),
                        error = ?e,
                        "index removal failed; search may be stale"
                    );
                }
            }
            // Unreadable for another reason (permissions, I/O): leave the index
            // as-is rather than guessing — a transient read failure must not
            // evict a still-existing note from the index.
            Err(e) => tracing::warn!(
                note = %sanitize_for_log(path.as_str()),
                error = %e,
                "index sync skipped; vault read failed"
            ),
        }
    }

    /// (Re)build the search index from every note currently on disk. The index
    /// is a derived cache (§6) that starts empty on a fresh DB, so without this
    /// an existing vault's notes aren't queryable until each is opened once —
    /// breaking `open`, `gui`, Ctrl+O, and full-text search on first run. Walks
    /// the vault (source of truth), reads each note, and rebuilds in one
    /// transaction; this also evicts rows for notes deleted externally since the
    /// last run. Unreadable notes are skipped (logged), never fatal.
    pub fn reindex_all(&self) -> Result<()> {
        let summaries = self.deps().vault.list_notes()?;
        let mut docs = Vec::with_capacity(summaries.len());
        for s in &summaries {
            match self.deps().vault.read_note(&s.path) {
                Ok(d) => docs.push(d),
                Err(e) => tracing::warn!(
                    note = %sanitize_for_log(s.path.as_str()),
                    error = %e,
                    "reindex skipped unreadable note",
                ),
            }
        }
        let count = docs.len();
        self.deps().index.rebuild(&docs).with_context(|| {
            format!("index rebuild failed ({count} notes); search may be incomplete")
        })?;
        tracing::info!(indexed = count, "search index bootstrapped from vault");
        Ok(())
    }

    /// Today's daily note path (`<daily_dir>/YYYY-MM-DD.md`) in the LOCAL
    /// timezone — a note edited at 23:30 local must land in today's daily file,
    /// not yesterday's (round-8 #1: the prior UTC date rolled over at local
    /// midnight's UTC instant, e.g. ~04:00 PT).
    pub fn daily_note_path(&self) -> Result<RelativeNotePath> {
        let dir = self.config().vault_layout().daily_dir_relative()?;
        let date = self.now_local().format("%Y-%m-%d").to_string();
        RelativeNotePath::new(format!("{}/{}.md", dir.as_str(), date)).context("invalid daily path")
    }

    /// Open today's daily note, creating it from a minimal template if absent.
    pub fn open_daily(&self) -> Result<NoteDocument> {
        let path = self.daily_note_path()?;
        match self.deps().vault.read_note(&path) {
            Ok(doc) => {
                self.set_current(OpenNote {
                    path: path.clone(),
                    opened_hash: doc.content_hash.clone(),
                });
                if let Err(e) = self.deps().index.refresh_note(&doc) {
                    tracing::warn!(note = %sanitize_for_log(doc.path.as_str()), error = ?e, "index refresh failed; search may be stale");
                }
                Ok(doc)
            }
            Err(crate::domain::errors::VaultError::NoteNotFound(_)) => {
                let body = format!("# {}\n\n", self.now_local().format("%Y-%m-%d"));
                let mtime = self.now().timestamp();
                let doc = NoteDocument::from_raw(path.clone(), &body, mtime);
                self.deps().vault.write_note(&doc, None)?;
                self.open_note(&path)
            }
            Err(e) => Err(e.into()),
        }
    }

    // ── Save / conflict (§7) ─────────────────────────────────────────────────

    /// Save the current note's body using optimistic concurrency. Never overwrites
    /// on external change — returns [`SaveOutcome::Conflict`] instead.
    pub fn save_current(&self, body: &str) -> Result<SaveOutcome> {
        let Some(open) = self.current() else {
            return Err(anyhow!("no note is open"));
        };
        self.save_as(&open.path, body, Some(&open.opened_hash))
    }

    /// Save an arbitrary path/body with an optional baseline hash.
    pub fn save_as(
        &self,
        path: &RelativeNotePath,
        body: &str,
        opened_hash: Option<&ContentHash>,
    ) -> Result<SaveOutcome> {
        let mtime = self.now().timestamp();
        let doc = NoteDocument::from_raw(path.clone(), body, mtime);
        let result = self.deps().vault.write_note(&doc, opened_hash)?;
        let outcome = match result {
            WriteResult::Written(h) => {
                self.set_current(OpenNote {
                    path: path.clone(),
                    opened_hash: h.clone(),
                });
                if let Err(e) = self.deps().index.refresh_note(&doc) {
                    tracing::warn!(note = %sanitize_for_log(doc.path.as_str()), error = ?e, "index refresh failed; search may be stale");
                }
                SaveOutcome::Written(h)
            }
            WriteResult::NoChange => SaveOutcome::NoChange,
            WriteResult::Conflict { current_disk_hash } => {
                SaveOutcome::Conflict { current_disk_hash }
            }
        };
        Ok(outcome)
    }

    /// §7 resolution: discard buffer, re-read disk.
    pub fn reload_current(&self) -> Result<NoteDocument> {
        let path = self
            .current()
            .map(|n| n.path)
            .ok_or_else(|| anyhow!("no note is open"))?;
        self.open_note(&path)
    }

    /// §7 resolution: write the buffer to a sibling conflict copy (never touches
    /// the original). Returns the new path.
    pub fn write_conflict_copy(&self, body: &str) -> Result<RelativeNotePath> {
        let path = self
            .current()
            .map(|n| n.path)
            .ok_or_else(|| anyhow!("no note is open"))?;
        let ts = self.now_local().format("%Y%m%d-%H%M%S").to_string();
        let stem = path.stem();
        let copy_name = format!("{stem}.conflict-{ts}.md");
        let copy = path.with_stem(&copy_name)?;
        let mtime = self.now().timestamp();
        let doc = NoteDocument::from_raw(copy.clone(), body, mtime);
        self.deps().vault.write_note(&doc, None)?;
        Ok(copy)
    }

    /// §7 resolution: explicit overwrite, bypassing the baseline check. Use only
    /// when the user confirms.
    pub fn force_overwrite_current(&self, body: &str) -> Result<ContentHash> {
        let path = self
            .current()
            .map(|n| n.path)
            .ok_or_else(|| anyhow!("no note is open"))?;
        let mtime = self.now().timestamp();
        let doc = NoteDocument::from_raw(path.clone(), body, mtime);
        match self.deps().vault.write_note(&doc, None)? {
            WriteResult::Written(h) => {
                self.set_current(OpenNote {
                    path: path.clone(),
                    opened_hash: h.clone(),
                });
                if let Err(e) = self.deps().index.refresh_note(&doc) {
                    tracing::warn!(note = %sanitize_for_log(doc.path.as_str()), error = ?e, "index refresh failed; search may be stale");
                }
                Ok(h)
            }
            WriteResult::NoChange => {
                let h = doc.content_hash.clone();
                self.set_current(OpenNote {
                    path: path.clone(),
                    opened_hash: h.clone(),
                });
                Ok(h)
            }
            WriteResult::Conflict { .. } => Err(anyhow!("unexpected conflict on forced write")),
        }
    }

    // ── Attachments / clipboard ───────────────────────────────────────────────

    pub fn attachment_links(&self, body: &str) -> Vec<AttachmentReference> {
        self.deps()
            .link_extractor
            .extract_attachment_links(&MarkdownBody(body.to_string()))
    }

    /// Image on the current cursor line a user wants to preview. Reads it via
    /// the [`ImageRenderer`] port and returns raw bytes + dimensions + size so
    /// the UI layer can render (ratatui-image) or show a fallback
    /// (`CLAUDE.md` §2.4).
    ///
    /// Confines the read to the vault root (§3.1 "must not escape the vault
    /// root"): `RelativeNotePath` already strips `..`, and this canonicalize-
    /// and-check defeats a symlink planted in the vault (e.g. via a tampered
    /// `git pull`) that would otherwise point the reader at a file outside.
    pub fn image_preview(&self, rel: &RelativeNotePath) -> Result<crate::ports::LoadedImage> {
        let renderer = self.deps().image_renderer.clone();
        // §3.1 confinement: delegate to `RelativeNotePath::resolve_within`, the
        // single source of the vault-escape guard (canonicalize root + target,
        // refuse dangling symlinks, reject any resolved path outside the root).
        // This closes the duplicate canonicalize-and-check that lived here — a
        // planted in-vault symlink (e.g. via a tampered `git pull`) can't route
        // the reader at a file outside the vault (DRY §5).
        let abs = rel.resolve_within(&self.config().vault)?;
        Ok(renderer.load(&abs)?)
    }

    /// Copy an attachment image to the clipboard (`CLAUDE.md` §2.4 fallback
    /// "copy" action + §2.9). Reuses the same confined read as preview.
    pub fn copy_image(&self, rel: &RelativeNotePath) -> Result<()> {
        let loaded = self.image_preview(rel)?;
        let image = ImageData {
            bytes: loaded.bytes,
            mime: loaded.mime,
            width: loaded.width,
            height: loaded.height,
        };
        Ok(self.deps().clipboard.write_image(&image)?)
    }

    /// Paste an image from the clipboard, persist it, and return an insertion token.
    pub fn paste_image(&self) -> Result<Option<PastedImage>> {
        let Some(image) = self.deps().clipboard.read_image()? else {
            return Ok(None);
        };
        let attachment = self.deps().attachments.save_image(image)?;
        let token = AttachmentReference::render_token(self.link_style(), &attachment.path);
        Ok(Some(PastedImage { attachment, token }))
    }

    pub fn link_style(&self) -> LinkStyle {
        self.config().image_link_style
    }

    /// Copy the current note in the chosen format.
    pub fn copy_note(&self, fmt: CopyFormat) -> Result<()> {
        let open = self.current().ok_or_else(|| anyhow!("no note is open"))?;
        let doc = self.deps().vault.read_note(&open.path)?;
        match fmt {
            CopyFormat::Markdown => {
                self.deps().clipboard.write_text(doc.body.as_str())?;
            }
            CopyFormat::Html | CopyFormat::Rich => {
                let html = self.deps().markdown.render_html(&doc.body);
                self.deps().clipboard.write_html(&html, doc.body.as_str())?;
            }
        }
        Ok(())
    }

    /// Copy arbitrary text to the clipboard (used by share mode to copy the
    /// local URL — `CLAUDE.md` §2.8 "copy URL").
    pub fn copy_text(&self, text: &str) -> Result<()> {
        Ok(self.deps().clipboard.write_text(text)?)
    }

    // ── Share (read-only) ─────────────────────────────────────────────────────

    pub fn share_current(&self) -> Result<ShareSession> {
        let open = self.current().ok_or_else(|| anyhow!("no note is open"))?;
        let doc = self.deps().vault.read_note(&open.path)?;
        let html = self.deps().markdown.render_html(&doc.body);
        let snapshot = crate::domain::share::ShareSnapshot {
            note_path: open.path.clone(),
            title: doc.title.as_str().to_string(),
            html,
            attachment_dir: self.config().attachment_dir.clone(),
        };
        let server = self
            .deps()
            .share_server
            .clone()
            .ok_or_else(|| anyhow!("share server unavailable"))?;
        let policy = SharePolicy::new(self.config().share_port, self.config().share_allow_lan);
        Ok(server.start(snapshot, policy)?)
    }

    pub fn stop_share(&self) -> Result<()> {
        if let Some(server) = &self.deps().share_server {
            server.stop()?;
        }
        Ok(())
    }

    // ── Backup ────────────────────────────────────────────────────────────────

    fn backup(&self) -> Result<&dyn crate::ports::BackupService> {
        self.deps()
            .backup
            .as_deref()
            .ok_or_else(|| anyhow!("backup service unavailable"))
    }

    pub fn backup_status(&self) -> Result<BackupState> {
        Ok(self.backup()?.status()?)
    }
    pub fn backup_commit(&self, message: Option<String>) -> Result<BackupReport> {
        let msg = match message {
            Some(m) => BackupMessage(m),
            None => BackupMessage::auto(
                "onote backup",
                self.now_local().format("%Y-%m-%d %H:%M").to_string(),
            ),
        };
        Ok(self.backup()?.commit(msg)?)
    }
    pub fn backup_push(&self) -> Result<BackupReport> {
        Ok(self.backup()?.push()?)
    }
    pub fn backup_pull(&self) -> Result<BackupReport> {
        Ok(self.backup()?.pull_ff_only()?)
    }

    // ── GUI / watch ───────────────────────────────────────────────────────────

    pub fn open_in_gui(&self, path: &RelativeNotePath) -> Result<()> {
        let launcher = self
            .deps()
            .launcher
            .as_ref()
            .ok_or_else(|| anyhow!("GUI launcher unavailable"))?;
        launcher.open(path)?;
        Ok(())
    }

    pub fn watch(&self, paths: &[std::path::PathBuf]) -> Result<Option<Receiver<ExternalChange>>> {
        match &self.deps().watcher {
            Some(w) => Ok(Some(w.watch(paths)?)),
            None => Ok(None),
        }
    }
}

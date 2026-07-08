//! `FileWatcher` via `notify` + `notify-debouncer-mini` (`CLAUDE.md` §2.5).
//!
//! Debounces external edits and emits [`ExternalChange`]s with the new disk hash.
//! The adapter owns the vault root so it can relativize absolute watcher paths and
//! compute the hash; the app compares against the buffer's baseline (§7).

use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver};
use std::time::Duration;

use notify::RecursiveMode;
use notify_debouncer_mini::{new_debouncer, DebouncedEvent};

use crate::domain::errors::VaultError;
use crate::domain::note::ContentHash;
use crate::domain::session::ExternalChange;
use crate::domain::vault::RelativeNotePath;
use crate::ports::FileWatcher;

pub struct FileWatch {
    root: PathBuf,
}

impl FileWatch {
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }
}

impl FileWatcher for FileWatch {
    fn watch(&self, paths: &[PathBuf]) -> Result<Receiver<ExternalChange>, VaultError> {
        let (tx, rx) = mpsc::channel();
        let root = self.root.clone();

        let mut debouncer = new_debouncer(Duration::from_millis(300), move |res| {
            let Ok(events) = res else { return };
            for ev in events {
                forward(&root, &tx, ev);
            }
        })
        .map_err(|e| VaultError::Io(std::io::Error::other(format!("watch init failed: {e}"))))?;

        for p in paths {
            let mode = if p.is_dir() {
                RecursiveMode::Recursive
            } else {
                RecursiveMode::NonRecursive
            };
            debouncer
                .watcher()
                .watch(p, mode)
                .map_err(|e| VaultError::Io(std::io::Error::other(format!("watch failed: {e}"))))?;
        }

        // The watcher is process-scoped; keep the debouncer alive for the lifetime
        // of the program. (A bounded leak; acceptable for MVP — one watcher/run.)
        Box::leak(Box::new(debouncer));
        Ok(rx)
    }
}

fn forward(root: &std::path::Path, tx: &mpsc::Sender<ExternalChange>, ev: DebouncedEvent) {
    let Some(rel) = relativize(root, &ev.path) else {
        return;
    };
    // The watcher recursively observes the WHOLE vault, so without this gate a
    // write to `.onote/index.sqlite-wal`, `.obsidian/*`, `.git/*`,
    // `node_modules/*` (or any `.md` shipped under those dirs) would surface as
    // an ExternalChange — `sync_index_for` then inserts ghost rows that
    // `list_notes` skips but `fuzzy_titles`/`full_text_search` surface. Mirror
    // the `walk_md` admission policy (filesystem_vault.rs): drop any path whose
    // relativized form has a segment in SKIP_DIRS or starts with `.` (dotfile /
    // dotdir sweep), and require a `.md` extension.
    let rel_str = rel.as_str();
    if rel_str
        .split(['/', '\\'])
        .any(|seg| seg.starts_with('.') || crate::infra::filesystem_vault::SKIP_DIRS.contains(&seg))
    {
        return;
    }
    if ev.path.extension().and_then(|e| e.to_str()) != Some("md") {
        return;
    }
    let new_disk_hash = match std::fs::read(&ev.path) {
        Ok(b) => ContentHash::of_bytes(&b),
        Err(_) => ContentHash::of_bytes(b""), // deleted / unreadable
    };
    let _ = tx.send(ExternalChange {
        note_path: rel,
        new_disk_hash,
    });
}

fn relativize(root: &std::path::Path, abs: &std::path::Path) -> Option<RelativeNotePath> {
    let rel = abs.strip_prefix(root).ok()?;
    let s = rel.to_string_lossy().replace('\\', "/");
    RelativeNotePath::from_user(&s).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use notify_debouncer_mini::{DebouncedEvent, DebouncedEventKind};
    use tempfile::tempdir;

    /// Round-9 regression guard: `forward` must mirror the `walk_md` admission
    /// policy (filesystem_vault.rs) — drop any event whose relativized path has a
    /// segment in SKIP_DIRS (`.git`/`.obsidian`/`.onote`/`node_modules`) or starts
    /// with `.`, and any non-`.md` file. Without this gate, a write to
    /// `.onote/index.sqlite-wal`, `.obsidian/*`, `.git/*`, `node_modules/*`
    /// (or any `.md` under those dirs) would surface as an `ExternalChange`, and
    /// `sync_index_for` would insert ghost rows that `list_notes` skips but
    /// `fuzzy_titles`/`full_text_search` surface.
    #[test]
    fn forward_filters_skip_dirs_dotfiles_and_non_md() {
        let dir = tempdir().expect("tempdir");
        let root = dir.path().to_path_buf();
        // A real note so `std::fs::read` in `forward` returns a real hash.
        std::fs::create_dir_all(root.join("Notes")).unwrap();
        std::fs::write(root.join("Notes").join("real.md"), "# real").unwrap();

        let (tx, rx) = mpsc::channel();

        // These three must be DROPPED.
        forward(
            &root,
            &tx,
            DebouncedEvent::new(
                root.join(".onote").join("index.sqlite-wal"),
                DebouncedEventKind::Any,
            ),
        );
        forward(
            &root,
            &tx,
            DebouncedEvent::new(
                root.join(".obsidian").join("config"),
                DebouncedEventKind::Any,
            ),
        );
        forward(
            &root,
            &tx,
            DebouncedEvent::new(root.join("Notes").join("x.txt"), DebouncedEventKind::Any),
        );
        assert!(
            rx.try_recv().is_err(),
            "skip-dir / dotfile / non-md events must NOT be forwarded",
        );

        // A real note must be FORWARDED.
        forward(
            &root,
            &tx,
            DebouncedEvent::new(root.join("Notes").join("real.md"), DebouncedEventKind::Any),
        );
        let got = rx.try_recv().expect("real .md event must be forwarded");
        assert_eq!(got.note_path.as_str(), "Notes/real.md");
    }
}

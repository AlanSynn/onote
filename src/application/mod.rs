//! Application layer (`CLAUDE.md` §3.2). Orchestrates domain + ports; owns no IO.
//!
//! `App` is constructed with concrete trait-object adapters (`AppDeps`). Use cases
//! live in [`ops`]. The open note + its §7 hashes are tracked here so both the CLI
//! and TUI share one save/conflict path.

pub mod ops;

use std::sync::{Arc, Mutex};

use chrono::{DateTime, Local, Utc};

use crate::config::Config;
use crate::domain::note::ContentHash;
use crate::domain::vault::RelativeNotePath;
use crate::ports::{
    AttachmentStore, BackupService, Clipboard, FileWatcher, ImageRenderer, MarkdownLinkExtractor,
    MarkdownRenderer, NoteIndex, ShareServer, UriLauncher, VaultRepository,
};

/// Re-export the clock port for convenience.
pub use crate::ports::Clock;

/// Fixed clock for tests.
#[cfg(test)]
pub struct FixedClock(pub DateTime<Utc>);
#[cfg(test)]
impl Clock for FixedClock {
    fn now(&self) -> DateTime<Utc> {
        self.0
    }
}

/// All adapter dependencies. Required ones are non-optional; optional services
/// (backup/share/watch/gui-launch) degrade gracefully when absent.
pub struct AppDeps {
    pub vault: Arc<dyn VaultRepository>,
    pub index: Arc<dyn NoteIndex>,
    pub attachments: Arc<dyn AttachmentStore>,
    pub clipboard: Arc<dyn Clipboard>,
    pub markdown: Arc<dyn MarkdownRenderer>,
    pub link_extractor: Arc<dyn MarkdownLinkExtractor>,
    pub image_renderer: Arc<dyn ImageRenderer>,
    pub backup: Option<Arc<dyn BackupService>>,
    pub watcher: Option<Arc<dyn FileWatcher>>,
    pub launcher: Option<Arc<dyn UriLauncher>>,
    pub share_server: Option<Arc<dyn ShareServer>>,
    pub clock: Arc<dyn Clock>,
}

/// The note currently held open, plus its §7 conflict hashes.
#[derive(Debug, Clone)]
pub struct OpenNote {
    pub path: RelativeNotePath,
    /// Disk hash at open time — the optimistic-concurrency baseline.
    pub opened_hash: ContentHash,
}

/// Application facade.
pub struct App {
    config: Config,
    deps: AppDeps,
    current: Mutex<Option<OpenNote>>,
}

impl App {
    pub fn new(config: Config, deps: AppDeps) -> Self {
        Self {
            config,
            deps,
            current: Mutex::new(None),
        }
    }

    pub fn config(&self) -> &Config {
        &self.config
    }

    /// Same-crate access to wired ports (`ops.rs` use cases). Deliberately
    /// `pub(crate)` — exposing it crate-wide would invite the UI/CLI layer to
    /// bypass application use cases and reach adapters directly, breaking the
    /// §2.2 dispatch boundary (UI Event → AppAction → use case → Domain → Port).
    pub(crate) fn deps(&self) -> &AppDeps {
        &self.deps
    }

    pub fn now(&self) -> DateTime<Utc> {
        self.deps.clock.now()
    }

    /// Same instant as [`now`](Self::now) in the LOCAL timezone — use for
    /// human-facing wall-clock labels (daily-note date, backup timestamp). Epoch
    /// `.timestamp()` values stay UTC.
    pub fn now_local(&self) -> DateTime<Local> {
        self.deps.clock.now_local()
    }

    /// Remember the open note + baseline hash (for §7 save).
    pub(crate) fn set_current(&self, note: OpenNote) {
        match self.current.lock() {
            Ok(mut g) => *g = Some(note),
            // A poisoned mutex means a prior panic poisoned the guard. The
            // `opened_hash` baseline is what makes `write_note` detect external
            // edits (§7); losing it silently degrades conflict detection, so
            // surface the degradation instead of no-oping quietly.
            Err(_) => {
                tracing::error!("current-note mutex poisoned; §7 conflict baseline unavailable")
            }
        }
    }

    pub(crate) fn current(&self) -> Option<OpenNote> {
        match self.current.lock() {
            Ok(g) => g.clone(),
            Err(_) => {
                tracing::error!("current-note mutex poisoned; §7 conflict baseline unavailable");
                None
            }
        }
    }

    pub(crate) fn clear_current(&self) {
        match self.current.lock() {
            Ok(mut g) => *g = None,
            Err(_) => {
                tracing::error!("current-note mutex poisoned; §7 conflict baseline unavailable")
            }
        }
    }
}

//! Infrastructure adapters — concrete implementations of the ports (`CLAUDE.md`
//! §3.2 `infra/`). Each adapter owns one external concern.

pub mod attachment_store;
pub mod clipboard;
pub mod clock;
pub mod file_watch;
pub mod filesystem_vault;
pub mod git_cli;
pub mod http_share;
pub mod index_location;
pub mod logging;
pub mod markdown;
pub mod null_index;
pub mod obsidian_uri;
pub mod sqlite_index;
pub mod terminal_image;

pub use attachment_store::FilesystemAttachmentStore;
pub use clipboard::ArboardClipboard;
pub use clock::SystemClock;
pub use file_watch::FileWatch;
pub use filesystem_vault::FilesystemVault;
pub use git_cli::GitCliBackup;
pub use http_share::HttpShareServer;
pub use index_location::{resolve_index_location, IndexLocation};
pub use markdown::ComrakMarkdown;
pub use null_index::NullNoteIndex;
pub use obsidian_uri::ObsidianLauncher;
pub use sqlite_index::SqliteNoteIndex;
pub use terminal_image::TerminalImage;

use std::sync::Arc;

use anyhow::{Context, Result};

use crate::application::AppDeps;
use crate::config::Config;
use crate::ports::{Clock, MarkdownLinkExtractor, MarkdownRenderer, NoteIndex};

/// Wire the default adapter set from a config + the resolved index location.
///
/// `vault_root` must exist; callers ensure it. The index is a required
/// `AppDeps` field, but when no writable location exists (`IndexLocation::
/// Indexless`) a [`NullNoteIndex`] stands in so onote runs with search disabled
/// rather than aborting — §6.1: the index is derived cache, never truth.
/// Optional services (backup/share/watcher/launcher) are constructed eagerly
/// here but degrade gracefully inside `App` when their underlying tooling (git,
/// clipboard, network) is unavailable.
pub fn build_deps(config: &Config, index_location: &IndexLocation) -> Result<AppDeps> {
    let vault_root = config.vault.clone();

    let vault = Arc::new(FilesystemVault::new(vault_root.clone()));
    let index: Arc<dyn NoteIndex> = match index_location {
        IndexLocation::Vault(p) | IndexLocation::Cache(p) => {
            Arc::new(SqliteNoteIndex::new(p).context("failed to open note index")?)
        }
        IndexLocation::Indexless => {
            tracing::warn!("running without a local index (read-only vault); fuzzy open and full-text search are disabled");
            Arc::new(NullNoteIndex)
        }
    };

    let markdown: Arc<dyn MarkdownRenderer> = Arc::new(ComrakMarkdown::new());
    let link_extractor: Arc<dyn MarkdownLinkExtractor> = Arc::new(ComrakMarkdown::new());
    let clock: Arc<dyn Clock> = Arc::new(SystemClock);

    let attachments = Arc::new(FilesystemAttachmentStore::new(
        vault_root.clone(),
        config.attachment_dir.clone(),
        link_extractor.clone(),
        clock.clone(),
    ));

    let clipboard = Arc::new(ArboardClipboard::new().context("clipboard unavailable")?);
    let image_renderer = Arc::new(TerminalImage::new());

    let backup = Arc::new(GitCliBackup::new(
        vault_root.clone(),
        config.backup_remote.clone(),
    ));
    let watcher = Arc::new(FileWatch::new(vault_root.clone()));
    let share_server =
        Arc::new(HttpShareServer::new(vault_root.clone()).context("share server init failed")?);
    let launcher = Arc::new(ObsidianLauncher::new(
        config.open_gui_command.clone(),
        config
            .vault
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default(),
    ));

    Ok(AppDeps {
        vault,
        index,
        attachments,
        clipboard,
        markdown,
        link_extractor,
        image_renderer,
        backup: Some(backup),
        watcher: Some(watcher),
        launcher: Some(launcher),
        share_server: Some(share_server),
        clock,
    })
}

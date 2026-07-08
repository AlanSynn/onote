//! Infrastructure adapters — concrete implementations of the ports (`CLAUDE.md`
//! §3.2 `infra/`). Each adapter owns one external concern.

pub mod attachment_store;
pub mod clipboard;
pub mod clock;
pub mod file_watch;
pub mod filesystem_vault;
pub mod git_cli;
pub mod http_share;
pub mod logging;
pub mod markdown;
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
pub use markdown::ComrakMarkdown;
pub use obsidian_uri::ObsidianLauncher;
pub use sqlite_index::SqliteNoteIndex;
pub use terminal_image::TerminalImage;

use std::path::Path;
use std::sync::Arc;

use anyhow::{Context, Result};

use crate::application::AppDeps;
use crate::config::Config;
use crate::ports::{Clock, MarkdownLinkExtractor, MarkdownRenderer};

/// Wire the default adapter set from a config + the index DB path.
///
/// `vault_root` must exist; callers ensure it. Optional services (backup/share/
/// watcher/launcher) are constructed eagerly here but degrade gracefully inside
/// `App` when their underlying tooling (git, clipboard, network) is unavailable.
pub fn build_deps(config: &Config, index_db: &Path) -> Result<AppDeps> {
    let vault_root = config.vault.clone();

    let vault = Arc::new(FilesystemVault::new(vault_root.clone()));
    let index = Arc::new(SqliteNoteIndex::new(index_db).context("failed to open note index")?);

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

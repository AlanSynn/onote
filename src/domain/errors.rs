//! Typed domain errors per bounded context (`CLAUDE.md` §3.1, §2.11).
//!
//! Domain/internal boundaries use `thiserror`; the CLI/TUI top level uses `anyhow`.

use thiserror::Error;

/// Top-level error alias used at the application boundary.
#[derive(Debug, Error)]
pub enum OnoteError {
    #[error(transparent)]
    Vault(#[from] VaultError),

    #[error(transparent)]
    Note(#[from] NoteError),

    #[error(transparent)]
    Index(#[from] IndexError),

    #[error(transparent)]
    Attachment(#[from] AttachmentError),

    #[error(transparent)]
    Clipboard(#[from] ClipboardError),

    #[error(transparent)]
    Share(#[from] ShareError),

    #[error(transparent)]
    Backup(#[from] BackupError),

    #[error(transparent)]
    Config(#[from] ConfigError),
}

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("config read failed: {0}")]
    Read(String),
    #[error("config parse failed: {0}")]
    Parse(String),
    #[error("invalid value for `{field}`: {reason}")]
    Invalid { field: String, reason: String },
}

#[derive(Debug, Error)]
pub enum VaultError {
    #[error("vault root does not exist: {0}")]
    NotFound(String),
    #[error("path escapes vault root: {0}")]
    Escape(String),
    #[error("note not found: {0}")]
    NoteNotFound(String),
    #[error("entry already exists: {0}")]
    AlreadyExists(String),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("path not utf-8: {0}")]
    NonUtf8(String),
}

#[derive(Debug, Error)]
pub enum NoteError {
    #[error("empty note title")]
    EmptyTitle,
    #[error("frontmatter parse error: {0}")]
    Frontmatter(String),
}

#[derive(Debug, Error)]
pub enum IndexError {
    #[error("sqlite error: {0}")]
    Sqlite(String),
}

#[derive(Debug, Error)]
pub enum AttachmentError {
    #[error("unsupported image mime: {0}")]
    UnsupportedMime(String),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("image decode error: {0}")]
    Decode(String),
    #[error("attachment not found: {0}")]
    NotFound(String),
}

#[derive(Debug, Error)]
pub enum ClipboardError {
    #[error("clipboard unavailable: {0}")]
    Unavailable(String),
}

#[derive(Debug, Error)]
pub enum ShareError {
    #[error("share server error: {0}")]
    Server(String),
    #[error("share already running")]
    AlreadyRunning,
    #[error("share not running")]
    NotRunning,
}

#[derive(Debug, Error)]
pub enum BackupError {
    #[error("git failed ({0})")]
    Git(String),
    #[error("not a git repository")]
    NotARepo,
}

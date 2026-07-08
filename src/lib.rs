//! `onote` — terminal-native, Obsidian-compatible Markdown vault client.
//!
//! Layering (see `CLAUDE.md` §3.2): `domain` ← `application` ← `ports` ← `infra`;
//! `ui`/`cli` depend on `application`. Domain knows nothing about IO/TUI/SQLite/Git.

pub mod application;
pub mod cli;
pub mod config;
pub mod domain;
pub mod infra;
pub mod ports;
pub mod ui;

pub use domain::errors::{ConfigError, OnoteError};

/// Convenience prelude for application/ui glue code.
pub mod prelude {
    pub use crate::config::Config;
    pub use crate::domain::note::{ContentHash, NoteDocument, NoteSummary, NoteTitle};
    pub use crate::domain::vault::{RelativeNotePath, VaultPath};
}

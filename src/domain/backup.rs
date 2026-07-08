//! Backup bounded context (`CLAUDE.md` §3.1 Backup).
//!
//! Git-backed, read-only-with-respect-to-note-content. Backup must never rewrite
//! note bytes and must never block editing.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum GitStatus {
    #[default]
    Clean,
    Dirty,
    NoRepo,
    Error,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct BackupState {
    pub status: GitStatus,
    pub ahead: u32,
    pub behind: u32,
    pub dirty_files: u32,
    pub branch: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BackupMessage(pub String);

impl BackupMessage {
    /// Default auto-backup message: `onote backup: YYYY-MM-DD HH:MM`.
    pub fn auto(prefix: &str, timestamp_label: String) -> Self {
        Self(format!("{prefix}: {timestamp_label}"))
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct BackupReport {
    pub committed: bool,
    pub pushed: bool,
    pub pulled: bool,
    pub message: String,
    pub conflicts: Vec<String>,
}

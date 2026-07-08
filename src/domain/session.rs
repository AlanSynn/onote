//! Sessions bounded context (`CLAUDE.md` §3.1 Sessions, §7).
//!
//! Owns local multi-terminal coordination. The §7 save/conflict algorithm
//! lives in `FilesystemVault::write_note` + `application::ops::SaveOutcome`;
//! this module carries the `ExternalChange` a file watcher emits when another
//! process mutates the open note.

use serde::{Deserialize, Serialize};

use super::note::ContentHash;
use super::vault::RelativeNotePath;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionMode {
    #[default]
    Edit,
    Follow,
    Takeover,
    #[serde(rename = "conflict-copy")]
    ConflictCopy,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EditSession {
    pub session_id: String,
    pub note_path: RelativeNotePath,
    pub pid: u32,
    pub mode: SessionMode,
    pub opened_at: i64,
    pub last_seen_at: i64,
}

/// An external change observed by the file watcher.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExternalChange {
    pub note_path: RelativeNotePath,
    pub new_disk_hash: ContentHash,
}

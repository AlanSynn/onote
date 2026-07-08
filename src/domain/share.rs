//! Share bounded context (`CLAUDE.md` §3.1 Share).
//!
//! Read-only delivery. A share references an immutable snapshot, never the live
//! mutable editor buffer.

use serde::{Deserialize, Serialize};

use super::vault::RelativeNotePath;

/// An immutable snapshot of a note rendered for sharing.
#[derive(Debug, Clone)]
pub struct ShareSnapshot {
    pub note_path: RelativeNotePath,
    pub title: String,
    /// Pre-rendered read-only HTML.
    pub html: String,
    /// Relative attachment dir (e.g. `Attachments`); attachments served by the
    /// share server are confined here.
    pub attachment_dir: String,
}

/// Opaque, URL-safe token gating access to a share session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShareToken(pub String);

impl ShareToken {
    /// Generate a random token (caller injects randomness; domain stays pure).
    pub fn from_random(bytes: &[u8]) -> Self {
        use base64::Engine;
        Self(base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes))
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SharePolicy {
    pub port: u16,
    pub allow_lan: bool,
}

impl SharePolicy {
    pub fn new(port: u16, allow_lan: bool) -> Self {
        Self { port, allow_lan }
    }
}

#[derive(Debug, Clone)]
pub struct ShareSession {
    pub id: String,
    pub token: ShareToken,
    pub local_url: String,
    pub lan_url: Option<String>,
}

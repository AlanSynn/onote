//! Vault bounded context (`CLAUDE.md` §3.1 Vault).
//!
//! Owns the Markdown folder. Every note path is relative to the vault root and
//! must never escape it.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::errors::VaultError;

/// Absolute, validated path to the vault root directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VaultPath(PathBuf);

impl VaultPath {
    /// Validate that `root` exists and is a directory. Does NOT require it to be
    /// a git repo. Used by both real and test fakes.
    pub fn new(root: impl Into<PathBuf>) -> Result<Self, VaultError> {
        let root = root.into();
        if !root.is_dir() {
            return Err(VaultError::NotFound(root.display().to_string()));
        }
        Ok(Self(root))
    }

    /// Construct without filesystem validation — for in-memory/fake tests where the
    /// directory will be created later.
    pub fn new_unchecked(root: impl Into<PathBuf>) -> Self {
        Self(root.into())
    }

    pub fn as_path(&self) -> &Path {
        &self.0
    }

    pub fn into_inner(self) -> PathBuf {
        self.0
    }

    /// Join a relative note path, producing an absolute path inside the vault.
    /// Returns `Escape` if the relative path would leave the vault root.
    pub fn join(&self, rel: &RelativeNotePath) -> Result<PathBuf, VaultError> {
        rel.resolve_within(&self.0)
    }
}

/// A note/attachment path expressed relative to the vault root.
///
/// Canonicalized: no leading `/`, no `..` components, no embedded absolute paths.
/// Constructing it from untrusted input ([`RelativeNotePath::from_user`]) enforces
/// the §3.1 "must not escape the vault root" rule.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct RelativeNotePath(PathBuf);

impl RelativeNotePath {
    /// Build from already-trusted components (e.g. produced internally).
    pub fn new(rel: impl Into<PathBuf>) -> Result<Self, VaultError> {
        let rel = rel.into();
        Self::validate(&rel)?;
        Ok(Self(rel))
    }

    /// Build from untrusted user input; strips leading slashes and rejects traversal.
    pub fn from_user(input: &str) -> Result<Self, VaultError> {
        let stripped = input.trim_start_matches(['/', '\\']);
        let path = PathBuf::from(stripped);
        Self::validate(&path)?;
        Ok(Self(path))
    }

    fn validate(rel: &Path) -> Result<(), VaultError> {
        if rel.is_absolute() {
            return Err(VaultError::Escape(rel.display().to_string()));
        }
        for comp in rel.components() {
            use std::path::Component;
            match comp {
                Component::Normal(_) | Component::CurDir => {}
                Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                    return Err(VaultError::Escape(rel.display().to_string()));
                }
            }
        }
        Ok(())
    }

    pub fn as_path(&self) -> &Path {
        &self.0
    }

    /// String form using `/` separators (portable across platforms).
    pub fn as_str(&self) -> String {
        self.0.to_string_lossy().replace('\\', "/")
    }

    /// Resolve to an absolute path under `root`, re-checking escape after join
    /// AND after resolving symlinks (§3.1 "must not escape the vault root").
    ///
    /// Returns the *lexical* `root.join(self)` so callers that `strip_prefix`
    /// against a non-canonical root keep working. The canonicalization is purely
    /// a gate: a symlink planted inside the vault that points outside (e.g. via
    /// a tampered `git pull`) is rejected here.
    pub fn resolve_within(&self, root: &Path) -> Result<PathBuf, VaultError> {
        let joined = root.join(&self.0);
        // Cheap lexical guard (the `..`/absolute checks already ran in `validate`).
        if !joined.ancestors().any(|a| a == root) {
            return Err(VaultError::Escape(self.as_str()));
        }
        let canon_root = match std::fs::canonicalize(root) {
            Ok(c) => c,
            Err(_) => {
                // Root doesn't exist (e.g. a synthetic test path). There is no
                // vault to defend; the lexical guard above already confines
                // `joined` under `root`.
                return Ok(joined);
            }
        };
        let canon_target =
            canonical_target(&joined).map_err(|_| VaultError::Escape(self.as_str()))?;
        if !canon_target.starts_with(&canon_root) {
            return Err(VaultError::Escape(self.as_str()));
        }
        Ok(joined)
    }

    /// Sibling path with a new extension (used for conflict copies / daily notes).
    pub fn with_extension(&self, ext: &str) -> Result<Self, VaultError> {
        Self::new(self.0.with_extension(ext))
    }

    /// Change the file stem.
    pub fn with_stem(&self, new_stem: &str) -> Result<Self, VaultError> {
        let mut p = self.0.clone();
        p.set_file_name(new_stem);
        if let Some(ext) = self.0.extension() {
            p.set_extension(ext);
        }
        Self::new(p)
    }

    /// Filename stem (no extension), used as a fallback note title.
    pub fn stem(&self) -> String {
        self.0
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default()
    }
}

/// Canonicalize `joined` enough to detect escape, without requiring the leaf to
/// exist (first-write case).
///
/// - A **dangling symlink** at the leaf (`metadata` fails but `symlink_metadata`
///   succeeds) is an error: it could point outside the vault.
/// - Otherwise canonicalize the **longest existing ancestor** (the file itself
///   if present, else the nearest existing directory). Non-existent tails have
///   no symlink to follow yet, so the existing-ancestor check is sufficient.
fn canonical_target(joined: &Path) -> std::io::Result<PathBuf> {
    // Dangling symlink at the leaf → refuse.
    if std::fs::metadata(joined).is_err() && std::fs::symlink_metadata(joined).is_ok() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "dangling symlink",
        ));
    }
    // Walk up to the longest existing ancestor and canonicalize it.
    let mut probe = joined.to_path_buf();
    while !probe.as_os_str().is_empty() {
        if let Ok(c) = std::fs::canonicalize(&probe) {
            return Ok(c);
        }
        if !probe.pop() {
            break;
        }
    }
    Ok(joined.to_path_buf())
}

/// One node in the vault tree for the Explorer drawer (`CLAUDE.md` §3.1 Vault,
/// §3.2 `note_drawer`). Pure domain data — the UI layer decides glyphs,
/// indentation, and selection styling; this type knows nothing of ratatui or the
/// filesystem. Built by `VaultRepository::list_tree` (infra walks disk; this is
/// the serializable result, not the walk).
///
/// `name` is the DISPLAY name: a folder's name as-is, or a note's STEM (filename
/// minus `.md`) — matching the Obsidian/IDE convention of hiding the extension
/// in a file tree. The full path (with extension) stays in `rel_path` so opening
/// the note routes through the normal `read_note` flow (no parallel open path).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VaultEntry {
    pub name: String,
    pub rel_path: RelativeNotePath,
    pub kind: EntryKind,
    /// Folder children (folders-first + alphabetical, established by the infra
    /// adapter). Empty for notes and for folders containing no `.md` notes.
    pub children: Vec<VaultEntry>,
}

/// Folder vs note (`CLAUDE.md` §3.1). A folder with no `.md` descendants still
/// appears (empty `children`) — the Explorer renders it as a collapsible node.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryKind {
    Folder,
    Note,
}

/// User-facing vault layout knobs (subset of `config.toml` that affects paths).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct VaultLayout {
    /// Root directory of the vault.
    pub root: PathBuf,
    /// Where pasted images land, relative to root. Default `Attachments`.
    #[serde(default = "default_attachment_dir")]
    pub attachment_dir: String,
    /// Where daily notes live, relative to root. Default `Daily`.
    #[serde(default = "default_daily_dir")]
    pub daily_dir: String,
    /// Default note opened by bare `onote`. Default `Scratch.md`.
    #[serde(default = "default_note")]
    pub default_note: String,
}

fn default_attachment_dir() -> String {
    "Attachments".to_string()
}
fn default_daily_dir() -> String {
    "Daily".to_string()
}
fn default_note() -> String {
    "Scratch.md".to_string()
}

impl Default for VaultLayout {
    fn default() -> Self {
        Self {
            root: PathBuf::from("."),
            attachment_dir: default_attachment_dir(),
            daily_dir: default_daily_dir(),
            default_note: default_note(),
        }
    }
}

impl VaultLayout {
    pub fn attachment_dir_relative(&self) -> Result<RelativeNotePath, VaultError> {
        RelativeNotePath::new(&self.attachment_dir)
    }

    pub fn daily_dir_relative(&self) -> Result<RelativeNotePath, VaultError> {
        RelativeNotePath::new(&self.daily_dir)
    }

    pub fn default_note_relative(&self) -> Result<RelativeNotePath, VaultError> {
        RelativeNotePath::from_user(&self.default_note)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_traversal_and_absolute() {
        // Traversal (`..`) is always rejected.
        assert!(RelativeNotePath::from_user("../etc/passwd").is_err());
        assert!(RelativeNotePath::from_user("a/../../b").is_err());
        assert!(RelativeNotePath::from_user("Notes/idea.md").is_ok());
        // Leading slashes are stripped (cosmetic) → safe in-vault relative path;
        // the real escape guard is `..` rejection + `resolve_within`.
        assert!(RelativeNotePath::from_user("/etc/passwd").is_ok());
        assert!(RelativeNotePath::from_user("/Notes/idea.md").is_ok());
        // And a stripped leading slash cannot escape via resolve_within:
        let root = PathBuf::from("/tmp/vault");
        let rel = RelativeNotePath::from_user("/etc/passwd").unwrap();
        assert!(rel.resolve_within(&root).unwrap().starts_with("/tmp/vault"));
    }

    #[test]
    fn resolve_within_stays_in_root() {
        let root = PathBuf::from("/tmp/vault");
        let rel = RelativeNotePath::from_user("Daily/today.md").unwrap();
        let abs = rel.resolve_within(&root).unwrap();
        assert_eq!(abs, PathBuf::from("/tmp/vault/Daily/today.md"));
    }

    #[cfg(unix)]
    #[test]
    fn resolve_within_rejects_symlink_escape() {
        use std::os::unix::fs::symlink;
        let dir = tempfile::tempdir().expect("tempdir");
        let outside_dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().to_path_buf();
        // Target genuinely OUTSIDE the vault (a separate temp dir).
        let outside = outside_dir.path().join("secret.txt");
        std::fs::write(&outside, "secret").unwrap();
        // Plant a symlink inside the vault that escapes.
        symlink(&outside, root.join("escape.md")).unwrap();

        let rel = RelativeNotePath::from_user("escape.md").unwrap();
        match rel.resolve_within(&root) {
            Err(VaultError::Escape(_)) => {}
            other => panic!("expected Escape on symlink-escape, got {other:?}"),
        }
    }

    #[cfg(unix)]
    #[test]
    fn resolve_within_allows_in_vault_symlink() {
        use std::os::unix::fs::symlink;
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().to_path_buf();
        std::fs::write(root.join("real.md"), "x").unwrap();
        symlink("real.md", root.join("alias.md")).unwrap();

        let rel = RelativeNotePath::from_user("alias.md").unwrap();
        assert!(rel.resolve_within(&root).is_ok());
    }
}

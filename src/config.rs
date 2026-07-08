//! Config loading (`CLAUDE.md` §2.10).
//!
//! `~/.config/onote/config.toml`, expanded with `shellexpand` and located via
//! `directories`. Falls back to sensible defaults so bare `onote` works against a
//! default vault.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::domain::attachment::LinkStyle;
use crate::domain::errors::ConfigError;
use crate::domain::vault::VaultLayout;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    /// Vault root, `~`-expanded.
    pub vault: PathBuf,
    #[serde(default = "default_note")]
    pub default_note: String,
    #[serde(default = "default_attachment_dir")]
    pub attachment_dir: String,
    #[serde(default = "default_daily_dir")]
    pub daily_dir: String,
    #[serde(default)]
    pub image_link_style: LinkStyle,
    /// Obsidian URI template with `{vault}` (auto-encoded vault basename) and/or
    /// `{file}` (auto-encoded note path) placeholders, e.g.
    /// `obsidian://open?vault={vault}&file={file}`. A literal `vault=Name`
    /// (no placeholder) also works for explicit names.
    #[serde(default = "default_open_gui_command")]
    pub open_gui_command: String,
    #[serde(default = "default_backup_remote")]
    pub backup_remote: String,
    #[serde(default = "default_share_port")]
    pub share_port: u16,
    /// Whether `onote share` binds the LAN (`0.0.0.0`) or loopback only.
    /// Default `false` (loopback) — opting into LAN exposure must be explicit.
    #[serde(default)]
    pub share_allow_lan: bool,
}

fn default_note() -> String {
    "Scratch.md".into()
}
fn default_attachment_dir() -> String {
    "Attachments".into()
}
fn default_daily_dir() -> String {
    "Daily".into()
}
fn default_open_gui_command() -> String {
    "obsidian://open?vault={vault}&file={file}".into()
}
fn default_backup_remote() -> String {
    "origin".into()
}
fn default_share_port() -> u16 {
    7478
}

impl Config {
    /// Path to the config file: `<config_root>/onote/config.toml` where
    /// `config_root` is `$XDG_CONFIG_HOME` or `~/.config` (CLAUDE.md §2.10).
    /// Returns `None` only when no home directory can be resolved.
    ///
    /// Per the XDG Base Directory spec, `XDG_CONFIG_HOME` MUST be absolute; an
    /// empty or relative value is ignored and we fall through to `~/.config`.
    pub fn config_path() -> Option<PathBuf> {
        let config_root = match std::env::var("XDG_CONFIG_HOME") {
            Ok(xdg) if Path::new(&xdg).is_absolute() => PathBuf::from(xdg),
            _ => directories::BaseDirs::new()?.home_dir().join(".config"),
        };
        Some(config_root.join("onote").join("config.toml"))
    }

    /// Load config, expanding `~`. Missing file is OK — returns a default that
    /// points at `~/Notes/Vault`.
    pub fn load() -> Result<Self, ConfigError> {
        Self::load_from(Self::config_path().as_deref())
    }

    /// Load from an explicit path (test-friendly). `None` → defaults.
    pub fn load_from(path: Option<&Path>) -> Result<Self, ConfigError> {
        let Some(path) = path else {
            return Ok(Self::defaults());
        };
        if !path.exists() {
            return Ok(Self::defaults());
        }
        let raw = std::fs::read_to_string(path)
            .map_err(|e| ConfigError::Read(format!("{}: {e}", path.display())))?;
        let cfg: ConfigFile = toml::from_str(&raw)
            .map_err(|e| ConfigError::Parse(format!("{}: {e}", path.display())))?;
        cfg.into_config()
    }

    /// Default config: vault at `~/Notes/Vault`.
    pub fn defaults() -> Self {
        let vault = shellexpand::tilde("~/Notes/Vault").into_owned();
        Self {
            vault: PathBuf::from(vault),
            default_note: default_note(),
            attachment_dir: default_attachment_dir(),
            daily_dir: default_daily_dir(),
            image_link_style: LinkStyle::default(),
            open_gui_command: default_open_gui_command(),
            backup_remote: default_backup_remote(),
            share_port: default_share_port(),
            share_allow_lan: false,
        }
    }

    /// Project the path/layout knobs into a [`VaultLayout`].
    pub fn vault_layout(&self) -> VaultLayout {
        VaultLayout {
            root: self.vault.clone(),
            attachment_dir: self.attachment_dir.clone(),
            daily_dir: self.daily_dir.clone(),
            default_note: self.default_note.clone(),
        }
    }
}

/// On-disk shape: allows `vault` to contain `~`.
#[derive(Debug, Deserialize)]
struct ConfigFile {
    vault: String,
    #[serde(default = "default_note")]
    default_note: String,
    #[serde(default = "default_attachment_dir")]
    attachment_dir: String,
    #[serde(default = "default_daily_dir")]
    daily_dir: String,
    #[serde(default)]
    image_link_style: LinkStyle,
    #[serde(default = "default_open_gui_command")]
    open_gui_command: String,
    #[serde(default = "default_backup_remote")]
    backup_remote: String,
    #[serde(default = "default_share_port")]
    share_port: u16,
    #[serde(default)]
    share_allow_lan: bool,
}

impl ConfigFile {
    fn into_config(self) -> Result<Config, ConfigError> {
        let vault = shellexpand::tilde(&self.vault).into_owned();
        let vault = PathBuf::from(vault);
        if !vault.is_absolute() {
            return Err(ConfigError::Invalid {
                field: "vault".into(),
                reason: "must be an absolute path".into(),
            });
        }
        // Defense-in-depth for share-server attachment confinement
        // (http_share.rs `serve_attachment`): `attach_root` is computed as
        // `canon_root.join(attachment_dir)`, and the served file must stay under
        // it. A user-set `attachment_dir` of `.` makes `attach_root == vault_root`
        // so ANY in-vault file (including `Secret.md`) becomes servable as an
        // "attachment"; an absolute path or a `..` segment escapes the dir
        // outright. Reject these values at load time so the http_share.rs
        // confinement can never be bypassed by config.
        let attach = self.attachment_dir.trim();
        if attach.is_empty() || attach == "." {
            return Err(ConfigError::Invalid {
                field: "attachment_dir".into(),
                reason: "must not be empty or `.` (would expose the whole vault as attachments)"
                    .into(),
            });
        }
        if Path::new(attach).is_absolute() {
            return Err(ConfigError::Invalid {
                field: "attachment_dir".into(),
                reason: "must be a relative path under the vault root".into(),
            });
        }
        if attach.split(['/', '\\']).any(|seg| seg == "..") {
            return Err(ConfigError::Invalid {
                field: "attachment_dir".into(),
                reason: "must not contain `..` segments (would escape the attachment dir)".into(),
            });
        }
        Ok(Config {
            vault,
            default_note: self.default_note,
            attachment_dir: self.attachment_dir,
            daily_dir: self.daily_dir,
            image_link_style: self.image_link_style,
            open_gui_command: self.open_gui_command,
            backup_remote: self.backup_remote,
            share_port: self.share_port,
            share_allow_lan: self.share_allow_lan,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn parses_full_config() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let mut f = tmp.reopen().unwrap();
        writeln!(
            f,
            r#"
vault = "/tmp/vault"
default_note = "Scratch.md"
attachment_dir = "Attachments"
daily_dir = "Daily"
image_link_style = "obsidian"
open_gui_command = "obsidian://open?vault=V&file={{file}}"
backup_remote = "origin"
share_port = 8000
share_allow_lan = true
"#
        )
        .unwrap();
        let cfg = Config::load_from(Some(tmp.path())).unwrap();
        assert_eq!(cfg.vault, PathBuf::from("/tmp/vault"));
        assert_eq!(cfg.image_link_style, LinkStyle::Obsidian);
        assert_eq!(cfg.share_port, 8000);
        assert!(cfg.share_allow_lan);
    }

    #[test]
    fn defaults_bind_share_loopback_only() {
        let cfg = Config::load_from(None).unwrap();
        assert!(!cfg.share_allow_lan, "share must default to loopback");
    }

    /// Round-9 regression guard for share-server attachment confinement
    /// (`http_share.rs` `serve_attachment`): `attachment_dir` values that would
    /// widen `attach_root` to the whole vault (or escape it) must be rejected at
    /// load time, so a config-set `.`/absolute/`..` can never make note files
    /// like `Secret.md` servable as "attachments".
    #[test]
    fn rejects_dangerous_attachment_dir() {
        fn try_load(val: &str) -> Result<Config, ConfigError> {
            let tmp = tempfile::NamedTempFile::new().unwrap();
            let mut f = tmp.reopen().unwrap();
            writeln!(
                f,
                r#"
vault = "/tmp/vault"
attachment_dir = "{val}"
"#
            )
            .unwrap();
            Config::load_from(Some(tmp.path()))
        }

        for bad in ["", ".", "/abs", "../x"] {
            match try_load(bad) {
                Err(ConfigError::Invalid { field, .. }) => {
                    assert_eq!(
                        field, "attachment_dir",
                        "attachment_dir = {bad:?} should report the attachment_dir field",
                    );
                }
                other => {
                    panic!("attachment_dir = {bad:?} must be rejected as dangerous; got {other:?}",)
                }
            }
        }
    }

    #[test]
    fn missing_file_yields_default() {
        let cfg = Config::load_from(None).unwrap();
        let vault = cfg.vault.to_string_lossy();
        // The suffix check alone is satisfied by a *literal* `~/Notes/Vault` —
        // i.e. by `shellexpand::tilde` silently leaving the `~` unexpanded.
        // Prove tilde expansion actually ran: the resolved vault must be
        // absolute (the home dir is always absolute on unix) and must contain
        // no literal `~`. A regression that broke `shellexpand::tilde` (e.g.
        // swapping it for a plain `PathBuf::from`) would pass the suffix check
        // but fail these two.
        assert!(
            vault.ends_with("Notes/Vault"),
            "default vault suffix mismatch; got {vault:?}",
        );
        assert!(
            vault.starts_with('/'),
            "default vault must be absolute after tilde expansion; got {vault:?}",
        );
        assert!(
            !vault.contains('~'),
            "default vault must not contain a literal `~` after expansion; got {vault:?}",
        );
    }

    #[test]
    fn config_path_single_onote_segment() {
        // CLAUDE.md §2.10: config lives at `<config_root>/onote/config.toml`
        // — exactly one `onote` path segment between the config root and the
        // file. Skip when no home/XDG dir is resolvable on this host — but
        // fail loudly under `CI`, where HOME is always resolvable and a `None`
        // return signals a regression rather than a missing env.
        let Some(path) = Config::config_path() else {
            if std::env::var("CI").is_ok() {
                panic!("config_path returned None on CI");
            }
            eprintln!("(skipped: no config path resolvable on this host)");
            return;
        };
        assert_eq!(
            path.file_name().and_then(|s| s.to_str()),
            Some("config.toml"),
            "filename should be config.toml; got {}",
            path.display(),
        );
        let parent_name = path
            .parent()
            .and_then(|p| p.file_name())
            .and_then(|n| n.to_str());
        assert_eq!(
            parent_name,
            Some("onote"),
            "parent dir should be `onote`; got {:?} (path: {})",
            parent_name,
            path.display(),
        );
        // Grandparent must NOT also be `onote` — that would be the old
        // `onote/onote/` double-nesting from ProjectDirs.
        let grandparent_name = path
            .parent()
            .and_then(|p| p.parent())
            .and_then(|p| p.file_name())
            .and_then(|n| n.to_str());
        assert_ne!(
            grandparent_name,
            Some("onote"),
            "config path is doubly nested under `onote/onote/`; got {}",
            path.display(),
        );
    }
}

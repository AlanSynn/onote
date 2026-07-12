//! Config loading (`CLAUDE.md` §2.10).
//!
//! `~/.config/onote/config.toml`, expanded with `shellexpand` and located via
//! `directories`. Falls back to sensible defaults so bare `onote` works against a
//! default vault.

use std::collections::BTreeMap;
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
    /// User-overridable TUI keybindings (`[keymap]`), overlaid on the editor's
    /// baked defaults. Stored as opaque `"key-spec" = "action-name"` strings
    /// (config must not know `KeyCode`/TUI types — `CLAUDE.md` §1.3); the TUI
    /// layer parses them. See [`KeymapConfig`].
    #[serde(default)]
    pub keymap: KeymapConfig,
    /// Responsive pane-layout knobs (`[layout]`) — Spike 7 Explorer drawer.
    #[serde(default)]
    pub layout: LayoutConfig,
    /// Catppuccin flavor for the TUI: `"latte"` (light, default) | `"frappe"` |
    /// `"macchiato"` | `"mocha"`. Stored as a raw string — config must not know
    /// `Color`/TUI types (`CLAUDE.md` §1.3); the UI layer parses it to a theme.
    #[serde(default = "default_theme")]
    pub theme: String,
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
fn default_theme() -> String {
    "latte".into()
}

/// User-overridable keybindings (`[keymap]` in config.toml), overlaid on the
/// TUI editor's baked defaults (`CLAUDE.md` §5 KeymapRegistry).
///
/// Each entry is `"key-spec" = "action-name"`:
///
/// ```toml
/// [keymap]
/// "ctrl+s" = "save"            # rebind save (or leave at default)
/// "ctrl+shift+c" = "copy"      # copy selection
/// "ctrl+x" = "cut"             # cut selection
/// "ctrl+a" = "select_all"
/// ```
///
/// Stored as opaque strings here — config deliberately knows nothing about
/// `KeyCode` or TUI types (`CLAUDE.md` §1.3). The `ui::tui::KeymapRegistry`
/// parses each `"spec" = "action"` into a typed binding at startup. A malformed
/// spec or unknown action is skipped (with a warning), leaving the default
/// binding intact, so a typo can't brick the editor. A `BTreeMap` (not
/// `HashMap`) gives deterministic ordering and last-write-wins on duplicate
/// key-specs.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct KeymapConfig {
    /// `#[serde(flatten)]` so a user writes `[keymap]` with `"ctrl+s" = "save"`
    /// pairs DIRECTLY (not nested under a `bindings =` sub-table).
    #[serde(flatten)]
    pub bindings: BTreeMap<String, String>,
}

/// Responsive pane-layout knobs (`[layout]` in config.toml), driving the
/// basalt-style `[Explorer | Editor | Outline]` split (`CLAUDE.md` §3.2
/// `note_drawer`). Spike 7 wires the LEFT Explorer; the right Outline lands
/// later. All widths are terminal columns.
///
/// ```toml
/// [layout]
/// explorer_width            = 30   # Explorer pane width when visible
/// explorer_hidden_width     = 4    # reserved (toggle gutter, P7.2)
/// show_explorer_threshold   = 100  # auto-show Explorer at/above this width
/// ```
///
/// Below `show_explorer_threshold` the Explorer is `Hidden` and the editor
/// takes the full width (today's behavior — zero regression). The manual
/// toggle (Ctrl+E, remappable) that overrides this arrives in P7.2.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LayoutConfig {
    #[serde(default = "default_explorer_width")]
    pub explorer_width: u16,
    #[serde(default = "default_explorer_hidden_width")]
    pub explorer_hidden_width: u16,
    #[serde(default = "default_show_explorer_threshold")]
    pub show_explorer_threshold: u16,
}

impl Default for LayoutConfig {
    fn default() -> Self {
        Self {
            explorer_width: default_explorer_width(),
            explorer_hidden_width: default_explorer_hidden_width(),
            show_explorer_threshold: default_show_explorer_threshold(),
        }
    }
}

fn default_explorer_width() -> u16 {
    30
}
fn default_explorer_hidden_width() -> u16 {
    4
}
fn default_show_explorer_threshold() -> u16 {
    100
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
            keymap: KeymapConfig::default(),
            layout: LayoutConfig::default(),
            theme: default_theme(),
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
    #[serde(default)]
    keymap: KeymapConfig,
    #[serde(default)]
    layout: LayoutConfig,
    #[serde(default = "default_theme")]
    theme: String,
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
            keymap: self.keymap,
            layout: self.layout,
            theme: self.theme,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    /// A TOML-basic-string-escaped absolute vault path that satisfies the
    /// `is_absolute()` validation on every platform. Hard-coding `/tmp/vault`
    /// (as earlier revisions did) breaks on Windows, where a drive-less path is
    /// NOT absolute and is rejected at load time. The path need not exist on
    /// disk — only be absolute. Backslashes (Windows) are doubled so the value
    /// parses as a TOML basic string.
    fn abs_vault_toml() -> String {
        let p = std::env::temp_dir().join("onote-cfg-test-vault");
        p.to_string_lossy().replace('\\', "\\\\")
    }

    #[test]
    fn parses_full_config() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let mut f = tmp.reopen().unwrap();
        let vault = abs_vault_toml();
        writeln!(
            f,
            r#"
vault = "{vault}"
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
        assert_eq!(cfg.vault, std::env::temp_dir().join("onote-cfg-test-vault"));
        assert_eq!(cfg.image_link_style, LinkStyle::Obsidian);
        assert_eq!(cfg.share_port, 8000);
        assert!(cfg.share_allow_lan);
    }

    /// `[keymap]` entries parse into the opaque string map; the TUI layer (not
    /// config) interprets them. A missing `[keymap]` yields an empty map (the
    /// TUI applies its baked defaults).
    #[test]
    fn keymap_section_parses_into_string_map() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let mut f = tmp.reopen().unwrap();
        let vault = abs_vault_toml();
        writeln!(
            f,
            r#"
vault = "{vault}"
[keymap]
"ctrl+s" = "reload"
"ctrl+x" = "cut"
"ctrl+shift+c" = "copy"
"#
        )
        .unwrap();
        let cfg = Config::load_from(Some(tmp.path())).unwrap();
        assert_eq!(
            cfg.keymap.bindings.get("ctrl+s").map(String::as_str),
            Some("reload")
        );
        assert_eq!(
            cfg.keymap.bindings.get("ctrl+x").map(String::as_str),
            Some("cut")
        );
        assert_eq!(
            cfg.keymap.bindings.get("ctrl+shift+c").map(String::as_str),
            Some("copy")
        );
    }

    /// `[layout]` knobs parse into `LayoutConfig`; a missing table yields the
    /// defaults (Explorer 30 cols, threshold 100). The TUI layer reads these to
    /// decide the basalt-style `[Explorer | Editor]` split (Spike 7).
    #[test]
    fn layout_section_parses() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let mut f = tmp.reopen().unwrap();
        let vault = abs_vault_toml();
        writeln!(
            f,
            r#"
vault = "{vault}"
[layout]
explorer_width = 42
show_explorer_threshold = 120
"#
        )
        .unwrap();
        let cfg = Config::load_from(Some(tmp.path())).unwrap();
        assert_eq!(cfg.layout.explorer_width, 42);
        assert_eq!(cfg.layout.show_explorer_threshold, 120);
        // Unspecified key falls back to its default, not 0.
        assert_eq!(cfg.layout.explorer_hidden_width, 4);
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
            let vault = abs_vault_toml();
            writeln!(
                f,
                r#"
vault = "{vault}"
attachment_dir = "{val}"
"#
            )
            .unwrap();
            Config::load_from(Some(tmp.path()))
        }

        // `/abs` is absolute on Unix but NOT on Windows (no drive letter), so
        // `Path::is_absolute()` would let it through the absolute-attachment_dir
        // guard on Windows. Pick a value the host considers absolute so the
        // guard is exercised on every platform.
        let abs = if cfg!(windows) { "C:/abs" } else { "/abs" };
        for bad in ["", ".", abs, "../x"] {
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
        // Default vault is `~/Notes/Vault` with `~` tilde-expanded. A *literal*
        // unexpanded `~/Notes/Vault` would also end in `Notes/Vault`, so prove
        // expansion actually ran. The checks are component/`Path`-level, not
        // string-level: `starts_with('/')` / a forward-slash `ends_with` would
        // pass on Unix but FAIL on Windows, where the separator is `\` and the
        // home dir is a drive-rooted absolute path. A regression that broke
        // `shellexpand::tilde` (e.g. a plain `PathBuf::from`) passes the suffix
        // check but fails the absolute + no-`~` checks.
        assert!(
            cfg.vault.is_absolute(),
            "default vault must be absolute after tilde expansion; got {:?}",
            cfg.vault,
        );
        // `Path::ends_with` compares components and treats `/` and `\` alike on
        // Windows, so this is separator-agnostic.
        assert!(
            cfg.vault.ends_with(Path::new("Notes/Vault")),
            "default vault must end in Notes/Vault; got {:?}",
            cfg.vault,
        );
        assert!(
            !cfg.vault.to_string_lossy().contains('~'),
            "default vault must not contain a literal `~` after expansion; got {:?}",
            cfg.vault,
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

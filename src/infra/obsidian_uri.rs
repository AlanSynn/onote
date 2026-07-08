//! `obsidian://` URI launcher (`CLAUDE.md` §2.10, §3.3 `UriLauncher`).
//!
//! Opens a note in the Obsidian GUI by shelling out to the platform's "open"
//! helper. The URI is built from a configured template such as
//! `obsidian://open?vault={vault}&file={file}`, with two placeholders:
//!
//! - `{vault}`: replaced with the percent-encoded vault NAME (the basename of
//!   the vault root, e.g. `Sam's Vault` → `Sam%27s%20Vault`). Use this so the
//!   user never has to hand-percent-encode a vault name containing spaces,
//!   apostrophes, or CJK in their config.
//! - `{file}`: replaced with the percent-encoded note path (with any trailing
//!   `.md` stripped).
//!
//! Backward compatibility: a template that omits `{vault}` (e.g. a literal
//! `vault=Notes`) is left untouched, since `str::replace` is a no-op when the
//! placeholder is absent.

use std::io;
use std::process::Command;

use crate::domain::errors::VaultError;
use crate::domain::vault::RelativeNotePath;
use crate::ports::UriLauncher;

/// Launches the Obsidian GUI for a given note via the `obsidian://` URI scheme.
///
/// `template` is the raw `open_gui_command` from config; it may contain a
/// `{file}` placeholder (substituted with the encoded note path) and/or a
/// `{vault}` placeholder (substituted with the encoded vault name).
/// `vault_name` is the vault root's basename, used to fill `{vault}`.
pub struct ObsidianLauncher {
    template: String,
    vault_name: String,
}

impl ObsidianLauncher {
    pub fn new(template: String, vault_name: String) -> Self {
        Self {
            template,
            vault_name,
        }
    }

    /// Build the final `obsidian://` URI for `note_path` by percent-encoding
    /// the vault name and file component and substituting them into the
    /// template. `{vault}` is substituted first, then `{file}`, so a note path
    /// can never clobber a partially-substituted vault placeholder. Pure helper
    /// (no I/O) so it can be unit-tested without invoking `open`.
    fn build_uri(&self, note_path: &RelativeNotePath) -> String {
        let vault = encode_file(&self.vault_name);
        let file = encode_file(&note_path.as_str());
        self.template
            .replace("{vault}", &vault)
            .replace("{file}", &file)
    }
}

impl UriLauncher for ObsidianLauncher {
    fn open(&self, note_path: &RelativeNotePath) -> Result<(), VaultError> {
        let uri = self.build_uri(note_path);

        let status = match std::env::consts::OS {
            "macos" | "linux" => {
                let launcher = if std::env::consts::OS == "macos" {
                    "open"
                } else {
                    "xdg-open"
                };
                Command::new(launcher).arg(&uri).status()
            }
            // `cmd /C start "" "<uri>"`: the empty title arg keeps `start`
            // happy, and the URI is passed as a single argument. The `file=`
            // component is fully percent-encoded (see [`encode_file`]), so a
            // hostile note path cannot smuggle a `cmd` metacharacter (`&`,
            // `|`, `%`) into the URI. Any literal `&` left in the URI comes
            // from the user's OWN config template (e.g. `...vault=V&file=…`),
            // which is trusted, not from note input.
            "windows" => Command::new("cmd").args(["/C", "start", "", &uri]).status(),
            _ => Err(io::Error::other(format!(
                "unsupported platform for obsidian launch: {uri}"
            ))),
        };

        match status {
            Ok(s) if s.success() => Ok(()),
            Ok(_) => Err(VaultError::Io(io::Error::other(format!(
                "failed to launch {uri}"
            )))),
            Err(_) => Err(VaultError::Io(io::Error::other(format!(
                "failed to launch {uri}"
            )))),
        }
    }
}

/// Strip a trailing `.md`, then percent-encode the path for the `file=` query
/// parameter of the `obsidian://open` URI.
///
/// Encodes per RFC 3986: keeps unreserved chars (`A-Za-z0-9-._~`) and `/` (so
/// subdirectories survive), percent-encoding every other byte (including UTF-8
/// for multibyte chars). This is stricter than a "spaces and `#`" pass because
/// the result is handed to a shell on Windows — a note path containing `&`,
/// `%`, `|`, etc. must NOT reach `cmd.exe` as a live metacharacter. Note `%` is
/// encoded to `%25`, which also neutralizes cmd's `%VAR%` expansion (the
/// inserted `25` shifts var-name parsing, e.g. `%PATH%` → `%25PATH%25`). No code
/// execution results even when the result is unquoted on the command line.
/// Intentionally avoids pulling in a URL-encoding crate.
fn encode_file(path: &str) -> String {
    let stripped = path.strip_suffix(".md").unwrap_or(path);

    let mut out = String::with_capacity(stripped.len());
    for ch in stripped.chars() {
        match ch {
            'A'..='Z' | 'a'..='z' | '0'..='9' | '-' | '.' | '_' | '~' | '/' => out.push(ch),
            other => {
                let mut buf = [0u8; 4];
                for b in other.encode_utf8(&mut buf).as_bytes() {
                    out.push_str(&format!("%{b:02X}"));
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_uri_fills_and_encodes_vault_placeholder() {
        // A vault name with an apostrophe and a space is percent-encoded
        // (`'` → `%27`, ` ` → `%20`), so the user never has to hand-encode it.
        let launcher = ObsidianLauncher::new(
            "obsidian://open?vault={vault}&file={file}".to_owned(),
            "Sam's Vault".to_owned(),
        );
        let path = RelativeNotePath::from_user("Notes/Inbox.md").unwrap();
        assert_eq!(
            launcher.build_uri(&path),
            "obsidian://open?vault=Sam%27s%20Vault&file=Notes/Inbox"
        );
    }

    #[test]
    fn build_uri_encodes_space_and_strips_md() {
        let launcher = ObsidianLauncher::new(
            "obsidian://open?vault=V&file={file}".to_owned(),
            "My Vault".to_owned(),
        );
        let path = RelativeNotePath::from_user("Notes/My Note.md").unwrap();
        assert_eq!(
            launcher.build_uri(&path),
            "obsidian://open?vault=V&file=Notes/My%20Note"
        );
    }

    #[test]
    fn build_uri_encodes_hash() {
        let launcher = ObsidianLauncher::new(
            "obsidian://open?vault=V&file={file}".to_owned(),
            "My Vault".to_owned(),
        );
        let path = RelativeNotePath::from_user("Tags/#ideas.md").unwrap();
        assert_eq!(
            launcher.build_uri(&path),
            "obsidian://open?vault=V&file=Tags/%23ideas"
        );
    }

    #[test]
    fn build_uri_keeps_non_md_extension() {
        let launcher = ObsidianLauncher::new(
            "obsidian://open?vault=V&file={file}".to_owned(),
            "My Vault".to_owned(),
        );
        // `.markdown` is not stripped — only the literal `.md` suffix is.
        let path = RelativeNotePath::from_user("Notes/Draft.markdown").unwrap();
        assert_eq!(
            launcher.build_uri(&path),
            "obsidian://open?vault=V&file=Notes/Draft.markdown"
        );
    }

    #[test]
    fn build_uri_no_extension() {
        let launcher = ObsidianLauncher::new(
            "obsidian://open?vault=V&file={file}".to_owned(),
            "My Vault".to_owned(),
        );
        let path = RelativeNotePath::from_user("Notes/Inbox").unwrap();
        assert_eq!(
            launcher.build_uri(&path),
            "obsidian://open?vault=V&file=Notes/Inbox"
        );
    }

    #[test]
    fn build_uri_encodes_amp_and_percent() {
        // Shell-injection metacharacters from a note path are percent-encoded,
        // never handed raw to cmd.exe / the shell.
        let launcher = ObsidianLauncher::new(
            "obsidian://open?vault=V&file={file}".to_owned(),
            "My Vault".to_owned(),
        );
        let path = RelativeNotePath::from_user("A & B/50%.md").unwrap();
        assert_eq!(
            launcher.build_uri(&path),
            "obsidian://open?vault=V&file=A%20%26%20B/50%25"
        );
    }

    #[test]
    fn build_uri_encodes_multibyte_as_utf8() {
        let launcher = ObsidianLauncher::new(
            "obsidian://open?vault=V&file={file}".to_owned(),
            "My Vault".to_owned(),
        );
        // '語' is U+8A9E → UTF-8 E8 AA 9E.
        let path = RelativeNotePath::from_user("Notes/語.md").unwrap();
        assert_eq!(
            launcher.build_uri(&path),
            "obsidian://open?vault=V&file=Notes/%E8%AA%9E"
        );
    }
}

//! CLI definition (`CLAUDE.md` §8) via `clap`.

use clap::{Parser, Subcommand};

/// Terminal-native, Obsidian-compatible Markdown vault client.
#[derive(Parser, Debug)]
#[command(
    name = "onote",
    version,
    about = "Terminal-native, Obsidian-compatible Markdown vault client",
    after_help = "Config: ~/.config/onote/config.toml (XDG-aware; set XDG_CONFIG_HOME to relocate)\n\
                  Environment:\n  \
                  RUST_LOG        tracing filter override (default: warn)\n  \
                  ONOTE_LOG_DIR   override the log directory (absolute, no '..')"
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Option<Command>,
}

impl Cli {
    /// The resolved command, defaulting to [`Command::Run`] (bare TUI).
    pub fn command_or_default(self) -> Command {
        self.command.unwrap_or(Command::Run)
    }
}

#[derive(Subcommand, Debug)]
pub enum Command {
    /// Open the bare TUI on the default note.
    Run,
    /// Open the default scratch note (TUI). Alias for `run` when the configured
    /// default note is `Scratch.md`.
    Scratch,
    /// Open today's daily note (TUI).
    Today,
    /// Create a new note and open it (TUI).
    New {
        /// Title for the new note (slugified to a `.md` filename).
        title: String,
    },
    /// Fuzzy-open a note by title and edit it (TUI).
    Open {
        /// Fuzzy-match query against note titles.
        query: String,
    },
    /// Start a read-only share server for the current note.
    Share,
    /// Commit / push / pull the vault via git.
    Backup {
        /// Push to the remote after committing.
        #[arg(long)]
        push: bool,
        /// Pull with --ff-only before committing.
        #[arg(long)]
        pull: bool,
    },
    /// Open the default (or fuzzy-matched) note in the Obsidian GUI.
    Gui {
        /// Fuzzy query; with no query, opens the default note.
        query: Option<String>,
    },
    /// Image operations.
    Img {
        #[command(subcommand)]
        cmd: ImgCmd,
    },
    /// Copy the current note to the clipboard.
    Copy {
        /// Copy as Markdown text (the default when no flag is given).
        #[arg(long)]
        md: bool,
        /// Copy as rendered HTML.
        #[arg(long)]
        html: bool,
        /// Copy as rich text (writes the same HTML flavor as --html; full RTF
        /// is future work per CLAUDE.md §2.9).
        #[arg(long)]
        rich: bool,
    },
    /// Print a shell completion script to stdout (redirect to install, e.g.
    /// `onote completions zsh > "${fpath[1]}/_onote"`).
    Completions {
        /// Shell to generate completions for.
        shell: clap_complete::Shell,
    },
    /// Print the most recent onote log file to stdout (path on stderr).
    Log,
}

#[derive(Subcommand, Debug)]
pub enum ImgCmd {
    /// Paste an image from the clipboard; prints the insertion token.
    Paste,
}

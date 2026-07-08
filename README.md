<div align="center">

```text
╭───────────────────────────────────────────────────────────╮
│                                                           │
│   ❯ onote▮                                                │
│   a terminal scratchpad for your obsidian vault           │
│                                                           │
╰───────────────────────────────────────────────────────────╯
```

**A lightweight terminal client for an Obsidian-compatible Markdown vault.**
Not an Obsidian replacement — a fast, local-first, terminal-native surface.

[![CI](https://github.com/AlanSynn/onote/actions/workflows/ci.yml/badge.svg)](https://github.com/AlanSynn/onote/actions/workflows/ci.yml)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)
[![Rust](https://img.shields.io/badge/rust-%E2%89%A5%201.82-orange.svg)](https://www.rust-lang.org/)
[![Platform](https://img.shields.io/badge/platform-macOS%20%7C%20Linux-lightgrey.svg)](#install)
[![Status](https://img.shields.io/badge/status-MVP-yellow.svg)](#status--non-goals)

`Rust` · `ratatui` · `crossterm` · `local-first` · `obsidian-compatible` · `markdown`

</div>

---

> **Why onote?** Obsidian is your library; onote is the notebook in your back pocket.
> The vault — plain Markdown files, `Attachments/`, and `.obsidian/` — stays the
> **source of truth**. onote just gives you a fast, terminal-native way to read, edit,
> paste images, share over HTTP/QR, and back up to Git without ever leaving the keyboard.
> No parallel storage model. No lock-in. No replacement.

## Table of contents

- [What it's for](#what-its-for)
- [Install](#install)
- [First run](#first-run)
- [Commands](#commands)
- [Configuration](#configuration)
- [Architecture](#architecture)
- [Design guarantees](#design-guarantees)
- [Theming](#theming)
- [Development](#development)
- [Status & non-goals](#status--non-goals)
- [License](#license)

## What it's for

onote is the right tool when you want to:

- ✍️  jot a **global scratch note** without breaking flow
- 📝  do **quick Markdown editing** in the terminal
- 🖼️  get an **image-aware terminal preview** of pasted images
- 🔗  spin up a **read-only QR / web share** of a note on localhost or LAN
- 🗃️  **back the vault up to GitHub** with one command
- 🚪  pop the **same note open in the Obsidian GUI** when you need the full surface

The vault directory is plain Markdown plus an attachments folder — onote reads and writes those files directly.

```text
~/Notes/Vault/
  Scratch.md
  Inbox.md
  Daily/
  Notes/
  Attachments/
  .obsidian/      ← Obsidian's config
  .onote/         ← onote's derived cache (SQLite index, sessions) — excluded from backup
```

## Install

> Requires **Rust ≥ 1.82** (edition 2021). Built and tested on **macOS arm64**;
> **Linux** should work out of the box — `crossterm` and `notify` are cross-platform.

**1. From source (dev / latest)**

```bash
git clone https://github.com/AlanSynn/onote.git
cd onote
cargo install --path . --locked
# or, if you have `just`:
just install
```

**2. One-line install** (builds from source via `install.sh`; lands the binary in `~/.local/bin`):

```bash
curl -L https://raw.githubusercontent.com/AlanSynn/onote/main/install.sh | sh
```

**3. Build it yourself**

```bash
git clone https://github.com/AlanSynn/onote.git
cd onote
cargo build --release
# binary lands in target/release/onote
```

## First run

onote reads its config from `~/.config/onote/config.toml`. It is XDG-aware — set
`XDG_CONFIG_HOME` to relocate it.

Minimal config:

```toml
vault = "~/Notes/Vault"
default_note = "Scratch.md"
```

On first run, onote initializes the vault directory **and** creates a `.onote/`
cache directory inside the vault. Obsidian ignores dotfolders, so the two coexist
cleanly.

## Commands

| Command                              | What it does                                                                |
| ------------------------------------ | --------------------------------------------------------------------------- |
| `onote` · `onote run`                | Open the bare TUI on the default note                                       |
| `onote scratch`                      | Open the default scratch note                                               |
| `onote today`                        | Open today's daily note                                                     |
| `onote new "robot idea"`             | Create a new note (slugified filename) and open it                          |
| `onote open "robot"`                 | Fuzzy-open a note by title; disambiguates multiple matches                  |
| `onote share`                        | Start a read-only HTTP share server for the current note (prints QR)        |
| `onote backup [--push] [--pull]`     | Git commit / push / pull the vault (excludes the `.onote/` cache)           |
| `onote gui [query]`                  | Open the (default or fuzzy-matched) note in Obsidian via `obsidian://`      |
| `onote img paste`                    | Paste a clipboard image into `Attachments/`; prints the insertion token     |
| `onote copy [--md\|--html\|--rich]`  | Copy the current note to the clipboard                                      |
| `onote completions <shell>`          | Print a shell completion script to stdout (e.g. `zsh`, `bash`, `fish`)      |
| `onote log`                          | Print the most recent onote log file to stdout (path on stderr)             |

`onote --version` and `onote --help` work (built with `clap`).

## Configuration

Full example, all keys:

```toml
vault             = "~/Notes/MainVault"
default_note      = "Scratch.md"
attachment_dir    = "Attachments"
daily_dir         = "Daily"
image_link_style  = "markdown"                                    # markdown | obsidian
open_gui_command  = "obsidian://open?vault={vault}&file={file}"
backup_remote     = "origin"
share_port        = 7478
share_allow_lan   = false                                         # loopback by default; opt into LAN
```

| Key                 | Meaning                                                                  |
| ------------------- | ------------------------------------------------------------------------ |
| `vault`             | Path to the Obsidian-compatible vault root                               |
| `default_note`      | Note opened by `onote` / `onote scratch`                                 |
| `attachment_dir`    | Where pasted images land                                                 |
| `daily_dir`         | Where `onote today` writes                                               |
| `image_link_style`  | `markdown` (portable) or `obsidian` (`![[…]]`)                           |
| `open_gui_command`  | `obsidian://` URI template; `{vault}` and `{file}` are substituted       |
| `backup_remote`     | Git remote used by `onote backup`                                        |
| `share_port`        | Port the read-only share server listens on                               |
| `share_allow_lan`   | `false` = loopback only; `true` = bind LAN                               |

## Architecture

onote is strict about layering. **The hard rule: the domain knows nothing about
the TUI, SQLite, Git, Ratatui, or the clipboard.** Every external concern lives
behind a port, implemented by a swappable infrastructure adapter.

```text
   ┌──────────────────────────────────┐
   │             DOMAIN               │
   │   vault · note · attachment      │
   │   session · share · backup       │
   └────────────────┬─────────────────┘
                    │  application
                    │  depends on  ↓
   ┌────────────────┴─────────────────┐
   │           APPLICATION            │
   │   open_note · save_note          │
   │   paste_image · share_note       │
   │   backup_vault · search …        │
   └────────────────┬─────────────────┘
                    │  use cases depend
                    │  on port traits  ↓
   ┌────────────────┴─────────────────┐
   │              PORTS               │
   │   VaultRepository · NoteIndex    │
   │   Clipboard · ShareServer …      │
   └───┬──────────────────────────┬───┘
       │ implemented by           │ implemented by
       ▼                          ▼
   ┌──────────────────────────────────┐
   │          INFRASTRUCTURE          │
   │   filesystem_vault · sqlite      │
   │   comrak · axum · git_cli        │
   │   arboard · notify · image       │
   └──────────────────────────────────┘

   ┌──────────┐                              ┌──────────┐
   │   cli    │  ──────── uses ──────►       │   tui    │
   │ (clap)   │                              │(ratatui) │
   └────┬─────┘                              └────┬─────┘
        └──────────►  application  ◄──────────────┘
```

**Key crates**

| Crate                          | Role                                                                  |
| ------------------------------ | --------------------------------------------------------------------- |
| `ratatui` + `crossterm`        | TUI layout, widgets, input, alternate screen                          |
| `comrak`                       | CommonMark + GFM Markdown parsing & rendering                         |
| `rusqlite` + FTS5              | Note index and full-text search cache                                 |
| `nucleo-matcher`               | fzf-style fuzzy matching for `onote open`                             |
| `notify`                       | File watching — detects external edits (Obsidian, Git, other editors) |
| `ratatui-image` + `image`      | In-terminal image preview (Sixel / Kitty / iTerm2)                    |
| `axum` + `qr2term`             | Read-only HTTP share server + terminal QR output                      |
| `git` (CLI)                    | Vault backup — `git2` is a future optional backend                    |
| `arboard`                      | Cross-platform clipboard (text, HTML, image)                          |

## Design guarantees

These are load-bearing promises, not aspirations:

- 🟢 **Local-first.** Read, edit, save, paste, search, preview, share, and back up all work offline. Only `git push` / `git pull` and public tunneling need a network.
- 🟢 **Obsidian-compatible, not Obsidian-dependent.** Understands `[[wikilinks]]`, `![[embeds]]`, `#tags`, frontmatter, daily notes, and the attachments folder — but works on any plain Markdown directory.
- 🟢 **Optimistic concurrency with conflict detection.** Every buffer tracks `opened_hash` vs `current_disk_hash`. onote **never** silently overwrites an external edit — it enters a `ChangedExternally` state and offers reload / merge / conflict-copy.
- 🟢 **Share is read-only by default.** The share server serves a snapshot behind a **tokenized URL**, bound to **loopback** unless you explicitly opt into LAN.
- 🟢 **Backup never touches note content.** `onote backup` commits your Markdown as-is and **excludes the derived `.onote/` SQLite cache**.
- 🟢 **Portable image links.** Pasted images default to standard Markdown `![](Attachments/…)`; Obsidian-style `![[…]]` is opt-in.

## Theming

onote deliberately **does not impose a color theme**. It inherits your terminal's
palette, so whatever color remapping your terminal does (truecolor, base16,
gruvbox, catppuccin, …) applies transparently. There is no in-app theme engine —
by design.

## Development

The project uses [`just`](https://github.com/casey/just) as a task runner.

```bash
brew install just     # or: cargo install just
```

| Recipe        | What it runs                                          |
| ------------- | ----------------------------------------------------- |
| `just ci`     | The full gate: `fmt-check` + `clippy -D warnings` + tests |
| `just test`   | Test suite                                            |
| `just clippy` | Clippy with `-D warnings`                             |
| `just fmt`    | Format the tree                                       |
| `just release`| Release build                                         |
| `just run`    | Build + run the TUI                                   |

State: **100+ tests**, clippy `-D warnings` clean.

## Status & non-goals

onote is an **MVP**. The following are explicitly **out of scope** (see `CLAUDE.md` §10):

- real-time remote collaboration
- graph view
- full Obsidian plugin compatibility
- WYSIWYG / rich-text Markdown editing
- mobile or web client
- AI features

## License

MIT — see [`LICENSE`](LICENSE).

---

> 📐 **Architecture & full design spec.** The complete product definition, bounded
> contexts, port contracts, data model, conflict-handling algorithm, and engineering
> rationale live in [`CLAUDE.md`](CLAUDE.md).

<div align="center">

```text
╭───────────────────────────────────────────────────────────╮
│                                                           │
│                         ❯ onote▮                          │
│       a terminal scratchpad for your obsidian vault       │
│                                                           │
╰───────────────────────────────────────────────────────────╯
```

**A lightweight terminal client for an Obsidian-compatible Markdown vault.**
Not an Obsidian replacement — a fast, local-first, terminal-native surface.

[![CI](https://github.com/AlanSynn/onote/actions/workflows/ci.yml/badge.svg)](https://github.com/AlanSynn/onote/actions/workflows/ci.yml)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)
[![Rust](https://img.shields.io/badge/rust-%E2%89%A5%201.82-orange.svg)](https://www.rust-lang.org/)
[![Platform](https://img.shields.io/badge/platform-macOS%20%7C%20Linux%20%7C%20Windows-lightgrey.svg)](#install)
[![Status](https://img.shields.io/badge/status-MVP-yellow.svg)](#status--non-goals)

`Rust` · `ratatui` · `crossterm` · `local-first` · `obsidian-compatible` · `markdown`

</div>

---

> **Why onote?** Obsidian is your library; onote is the notebook in your back pocket.
> The vault — plain Markdown files, `Attachments/`, and `.obsidian/` — stays the
> **source of truth**. onote just gives you a fast way to read, edit,
> paste images, share over HTTP/QR, and back up to Git without ever leaving the keyboard.
> No parallel storage model. No lock-in. No replacement.

## Table of contents

- [What it's for](#what-its-for)
- [Install](#install)
- [First run](#first-run)
- [Commands](#commands)
- [Keyboard shortcuts](#keyboard-shortcuts)
- [Configuration](#configuration)
- [Architecture](#architecture)
- [Design guarantees](#design-guarantees)
- [Theming](#theming)
- [Development](#development)
- [Status & non-goals](#status--non-goals)
- [License](#license)

## What it's for

onote is the right tool when you want to:

- jot a **global scratch note** without breaking flow
- do **quick Markdown editing** in the terminal
- get an **image-aware terminal preview** of pasted images
- spin up a **read-only QR / web share** of a note on localhost or LAN
- **back the vault up to GitHub** with one command
- pop the **same note open in the Obsidian GUI** when you need the full surface

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

> Prebuilt binaries ship for **macOS** (arm64 + x86_64), **Windows** (x86_64), and
> **Linux x86_64** (a fully static musl build plus an apt `.deb`, no runtime deps).
> Building from source needs **Rust ≥ 1.82**; `crossterm`, `notify`, and `getrandom`
> are cross-platform.

**Homebrew** — recommended on macOS

```bash
brew tap alansynn/tap
brew install onote
```

> The tap formula **builds from source** (it pulls `rust` as a build dep, ~2 min) —
> it does not consume the prebuilt tarballs. For an instant binary on macOS, grab
> the `onote-aarch64-apple-darwin.tar.gz` (or `x86_64`) from the
> [Releases page](https://github.com/AlanSynn/onote/releases/latest) instead.

**Debian / Ubuntu `.deb`** — recommended on Linux x86_64

The release ships a fully **static musl binary** wrapped in a `.deb` with **no
runtime dependencies** — it installs on any Debian, Ubuntu, or Mint regardless of
glibc version. Download and install in one line:

```bash
curl -L https://github.com/AlanSynn/onote/releases/latest/download/onote-x86_64-linux.deb -o /tmp/onote.deb
sudo dpkg -i /tmp/onote.deb
```

(Or grab it from the [Releases page](https://github.com/AlanSynn/onote/releases/latest).)

**Scoop** — recommended on Windows

```powershell
scoop bucket add alansynn/scoop-onote
scoop install onote
```

**One-line installer** — any Linux (or any platform with Rust); downloads the
**prebuilt static binary** on x86_64 Linux, falls back to a from-source build elsewhere

```bash
curl -L https://raw.githubusercontent.com/AlanSynn/onote/main/install.sh | sh
```

On x86_64 Linux this skips the ~2 min `cargo build` and lands the binary in
`~/.local/bin`; other platforms build the pinned release via `cargo`. (Pin a
specific version with `ONOTE_TAG=v0.x.y`; force a source build with `--from-source`.)

**Build from source** — dev / latest

```bash
git clone https://github.com/AlanSynn/onote.git
cd onote
cargo install --path . --locked
# or, if you have `just`:  just install
```

Verify the install:

```bash
onote --version
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

## Keyboard shortcuts

The editor resolves every keystroke to a logical action through a
`KeymapRegistry` (`CLAUDE.md` §5), so **every binding below is remappable** —
see [Configuration → Keymap](#keymap). Selection is **grapheme-accurate**: a
combining mark (`e` + ◌́) or a ZWJ emoji is one selectable unit, never split.

**Editing**

| Key                | Action                                   |
| ------------------ | ---------------------------------------- |
| (type any key)     | Insert a character                       |
| `Enter`            | Newline                                  |
| `Tab`              | Insert tab                               |
| `Backspace`        | Delete the char before the caret         |
| `Delete`           | Forward-delete the char after the caret  |
| `Ctrl+S`           | Save (`write_note`, never silent-overwrite) |
| `Ctrl+R`           | Reload — discard the buffer, re-read disk   |
| `Ctrl+K`           | Conflict copy — save as `*.conflict.md`     |

**Navigation**

| Key            | Action                            |
| -------------- | --------------------------------- |
| `←` `→` `↑` `↓`| Move the caret                    |
| `Home` / `End` | Jump to line start / end          |
| `Ctrl+←` / `Ctrl+→` | Jump a word (skips whitespace, punctuation) |

**Selection** — typing, `Enter`, `Backspace`, or `Delete` **replaces** the
active selection; `Esc` clears it.

| Key                  | Action                                   |
| -------------------- | ---------------------------------------- |
| `Shift+←/→/↑/↓`      | Extend the selection                     |
| `Shift+Home` / `Shift+End` | Extend to line start / end          |
| `Ctrl+Shift+←` / `Ctrl+Shift+→` | Extend by a word             |
| `Ctrl+A`             | Select all                               |
| Mouse drag           | Select a region (grapheme-snapped)       |
| `Ctrl+Shift+C`       | **Copy** the selection                   |
| `Ctrl+X`             | **Cut** the selection (deletes on copy)  |
| `Esc`                | Clear the selection                      |

**Notes & app**

| Key      | Action                                              |
| -------- | --------------------------------------------------- |
| `Ctrl+O` | Fuzzy-open a note                                   |
| `Ctrl+G` | Follow the note link under the caret (`[[wikilink]]` / Markdown link) |
| `Ctrl+P` | Paste a clipboard image → `Attachments/` + token    |
| `Ctrl+D` | Delete the image token under the caret              |
| `Ctrl+Q` | Quit                                                |

**Explorer** — the left pane auto-shows on wide terminals (≥ `show_explorer_threshold`
cols; see [Configuration → Layout](#layout)). `Ctrl+E` toggles it anywhere and
moves focus into it. When focused, keystrokes route to the tree instead of the
editor; `Esc` returns focus to the editor.

| Key        | Action                                                              |
| ---------- | ------------------------------------------------------------------ |
| `Ctrl+E`   | Toggle the Explorer pane + move focus into / out of it             |
| `↑` `↓`    | Move the selection                                                  |
| `←` `→`    | Collapse / expand the selected folder                              |
| `Enter`    | Folder: toggle expand · Note: open it (focus returns to the editor) |
| `n`        | New note (in the selected folder, or beside the selection)          |
| `N`        | New folder                                                          |
| `r`        | Rename the selected note or folder                                  |
| `d`        | Delete the selected note or folder (asks to confirm)               |
| `Esc`      | Back to the editor                                                  |

> The Explorer file-op keys (`n` `N` `r` `d`) are raw, not remappable — they're
> intercepted only while the Explorer is focused, so they never collide with
> typing in the editor. The confirm prompt takes `y`/`Enter` or `n`/`Esc`.

> `Ctrl+C` **also** quits (cancel muscle memory). Copy is `Ctrl+Shift+C` — the
> `Shift` bit makes it a distinct combo, so the two never clash.

## Configuration

Full example, all keys:

```toml
vault             = "~/Notes/Vault"
default_note      = "Scratch.md"
attachment_dir    = "Attachments"
daily_dir         = "Daily"
image_link_style  = "markdown"                                    # markdown | obsidian
open_gui_command  = "obsidian://open?vault={vault}&file={file}"
backup_remote     = "origin"
share_port        = 7478
share_allow_lan   = false                                         # loopback by default; opt into LAN

# [layout] drives the responsive Explorer drawer: it auto-shows at/above
# `show_explorer_threshold` cols, and Ctrl+E toggles it at any width.
[layout]
explorer_width          = 30      # Explorer pane width when visible
show_explorer_threshold = 100     # auto-show at/above this terminal width
explorer_hidden_width   = 4       # reserved (future toggle-gutter width)

# [keymap] overrides the editor's baked keybindings — see "Keymap" below.
# A malformed spec or unknown action is skipped (with a warning), so a typo
# can't brick the editor: the default binding survives.
[keymap]
"ctrl+x" = "cut"              # rebind cut
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
| `keymap`            | `[keymap]` table of `"key-spec" = "action"` overrides (see below)        |
| `layout`            | `[layout]` table of responsive-Explorer knobs (see below)                |

### Layout

The `[layout]` table drives the responsive Explorer drawer (the left pane,
basalt-style). All widths are terminal columns.

```toml
[layout]
explorer_width          = 30   # Explorer pane width when visible
show_explorer_threshold = 100  # auto-show at/above this terminal width
explorer_hidden_width   = 4    # reserved (future toggle-gutter width when hidden)
```

| Key                     | Default | Meaning                                              |
| ----------------------- | ------- | ---------------------------------------------------- |
| `explorer_width`        | `30`    | Explorer pane width (columns) when visible           |
| `show_explorer_threshold` | `100` | Auto-show the Explorer at/above this terminal width  |
| `explorer_hidden_width` | `4`     | Reserved — future toggle-gutter width when hidden    |

Below `show_explorer_threshold` the Explorer is hidden and the editor takes the
full row — byte-identical to a pre-Explorer build (zero regression). At/above
the threshold it auto-shows; `Ctrl+E` overrides the auto policy at any width.
See [Keyboard shortcuts → Explorer](#keyboard-shortcuts).

### Keymap

The `[keymap]` table remaps any editor binding. Each entry is a
`"key-spec" = "action-name"` pair, overlaid on the baked defaults:

```toml
[keymap]
"ctrl+s"         = "save"          # leave save where it is (or move it)
"ctrl+shift+c"   = "copy"          # copy the selection
"ctrl+x"         = "cut"           # cut the selection
"ctrl+a"         = "select_all"
"ctrl+left"      = "word_left"
"shift+left"     = "select_left"
```

**Key-spec syntax** — a `+`-joined combo:

- **Modifiers:** `ctrl` (or `control`), `alt` (or `option`, `meta`), `shift`.
- **Key name:** `enter` / `return`, `tab`, `backspace` / `bs`, `esc` /
  `escape`, `delete` / `del`, `insert` / `ins`, `home`, `end`,
  `pageup` / `pgup`, `pagedown` / `pgdn`, `left` `right` `up` `down`,
  `space` / `spacebar`, `f1`…`f12`, or a single literal character (e.g. `"s"`).
- Letter keys are case-insensitive: `"ctrl+S"` ≡ `"ctrl+s"`.

**Action names** (the full vocabulary; aliases after `/`):

| Action                          | Default binding       |
| ------------------------------- | --------------------- |
| `quit`                          | `Ctrl+Q`, `Ctrl+C`    |
| `save`                          | `Ctrl+S`              |
| `reload`                        | `Ctrl+R`              |
| `open_fuzzy` / `open`           | `Ctrl+O`              |
| `open_link`                     | `Ctrl+G`              |
| `paste_image` / `paste`         | `Ctrl+P`              |
| `delete_image_token` / `delete_image` | `Ctrl+D`        |
| `conflict_copy`                 | `Ctrl+K`              |
| `enter` / `newline`             | `Enter`               |
| `tab`                           | `Tab`                 |
| `backspace`                     | `Backspace`           |
| `delete_forward` / `delete`     | `Delete`              |
| `move_left` `move_right` `move_up` `move_down` | `←` `→` `↑` `↓` |
| `move_home` / `home`            | `Home`                |
| `move_end` / `end`              | `End`                 |
| `select_left` `select_right` `select_up` `select_down` | `Shift+←/→/↑/↓` |
| `select_home` `select_end`    | `Shift+Home`, `Shift+End` |
| `select_all`                    | `Ctrl+A`              |
| `select_word_left` / `select_word_right` | `Ctrl+Shift+←` / `Ctrl+Shift+→` |
| `word_left` / `word_right`      | `Ctrl+←` / `Ctrl+→`   |
| `copy`                          | `Ctrl+Shift+C`        |
| `cut`                           | `Ctrl+X`              |
| `clear_selection` / `deselect`  | `Esc`                 |

A spec that fails to parse, or an unknown action, is logged and **skipped** —
the default binding for that key stays intact, so a typo can never leave you
without a working editor.

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

**Key dependencies**

| Crate                          | Role                                                                  |
| ------------------------------ | --------------------------------------------------------------------- |
| `ratatui` + `crossterm`        | TUI layout, widgets, input, alternate screen                          |
| `comrak`                       | CommonMark + GFM Markdown parsing & rendering                         |
| `rusqlite` + FTS5              | Note index and full-text search cache                                 |
| `nucleo-matcher`               | fzf-style fuzzy matching for `onote open`                             |
| `notify`                       | File watching — detects external edits (Obsidian, Git, other editors) |
| `ratatui-image` + `image`      | In-terminal image preview (Sixel / Kitty / iTerm2)                    |
| `axum` + `qr2term`             | Read-only HTTP share server + terminal QR output                      |
| `git` (CLI)                    | Vault backup via system `git`                    |
| `arboard`                      | Cross-platform clipboard (text, HTML, image)                          |

## Design guarantees

These are load-bearing promises, not aspirations:

- 🟢 **Local-first.** Read, edit, save, paste, search, preview, share, and back up all work offline. Only `git push` / `git pull` and public tunneling need a network.
- 🟢 **Obsidian-compatible, not Obsidian-dependent.** Understands `[[wikilinks]]`, `![[embeds]]`, `#tags`, frontmatter, daily notes, and the attachments folder — but works on any plain Markdown directory.
- 🟢 **No silent overwrites.** Every buffer tracks `opened_hash` vs `current_disk_hash`; on detecting an external edit, onote enters a `ChangedExternally` state and offers reload / merge / conflict-copy rather than clobbering it.
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

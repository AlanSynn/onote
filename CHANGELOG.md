# Changelog

All notable changes to `onote` are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added
- _Nothing yet._

### Changed
- _Nothing yet._

### Fixed
- _Nothing yet._

## [0.3.0] - 2026-07-11

Editor maturity release: in-vault navigation, a file-ops Explorer drawer, text
selection, and body search — the TUI graduates from a single-pane scratchpad to
a navigable vault surface.

### Added

- **In-vault note navigation** (Spike 8): `Ctrl+G` follows the `[[wikilink]]` or
  Markdown link under the caret (title-exact resolution; ambiguous/unknown
  targets fall back to the fuzzy picker); `Ctrl+B` jumps back through the
  navigation stack (link-follow / fuzzy-open / Explorer-open all push). Closes
  the link-follow loop.
- **Explorer file-ops drawer** (Spike 7): a left-pane vault tree that auto-shows
  on wide terminals (≥ `show_explorer_threshold`). Create note / new folder /
  rename / delete via prompt + confirm modals; two-way sync — the current note
  is marked, and fuzzy/Explorer opens reveal it. `Ctrl+E` toggles visibility
  and focus from either pane.
- **Text block selection**: grapheme-snapped keyboard selection (`Shift+arrows`,
  `Ctrl+←/→` and `Shift+Ctrl+←/→` word motions) and mouse-drag selection with
  reverse-video render, drag-past-viewport autoscroll, and cut / copy / delete /
  replace-on-Enter operations. The selection lifecycle is data-safe across
  buffer swaps, reloads, and mode changes.
- **Configurable keymap** (`CLAUDE.md` §5): every edit-mode keybinding is
  remappable via a `[keymap]` table in `config.toml`.
- **`Ctrl+F` body search** in the TUI — surfaces the FTS5 match `snippet`, the
  §6.2 search index that was previously reachable only programmatically.
- **`onote tags`** — lists `#tag` counts across the vault (Obsidian tag
  convention, §1.2).
- **Explicit overwrite** (`Ctrl+Shift+K`) for §7 conflict resolution — the
  escape hatch the spec mandates ("never default to overwrite, but make it
  available"); distinct from `Ctrl+K` conflict-copy by the SHIFT bit.
- **Recency tiebreak for fuzzy open**: opening a note stamps `recent_notes`
  (§6.2), so equal-score fuzzy matches float the recently-used note to the top.
- Snapshot tests pinning the Explorer split geometry / responsive layout.

### Changed

- **§3.2 module split**: the `ui::tui` god-module is split into focused `editor`
  / `keymap` / `render` / `note_drawer` modules; `EditorState` tightened to
  `pub(super)` and `App::deps` to `pub(crate)`.
- **Domain purity**: `slugify` moved out of infra into the domain;
  image-preview delegated to `resolve_within`.
- CI clippy (`-D warnings`) and test gates are now required checks.

### Fixed

- **Observability**: WAL journal-mode fallback and current-note mutex-poison
  failures now emit a `tracing` warning instead of silently no-op'ing — the §7
  conflict baseline is no longer invisibly lost.
- **Selection data-safety**: stale selection anchors are cleared on buffer swap,
  reload, and editor-surface exit; selected text is re-normalized after clamping
  (defense-in-depth).

### Dependencies

- `cargo update`: 15 patch/minor bumps within existing semver bounds
  (bytemuck, bytes, exr, memchr, zerocopy, …); no major-version jumps.

## [0.2.1] - 2026-07-08

Portable Linux release: easy `apt`-style install plus a distro-agnostic static binary.

### Added

- **Portable Linux target** (`x86_64-unknown-linux-musl`): a fully static binary
  with no glibc runtime dependency, so the same artifact runs on any Linux
  distro (fixes the glibc ≥ 2.39 requirement of the GNU build on older
  Ubuntu/Debian). Shipped as `onote-x86_64-unknown-linux-musl.tar.gz`.
- **apt `.deb` package** (`onote-x86_64-linux.deb`): wraps the static musl
  binary with no runtime dependencies — `sudo dpkg -i onote-x86_64-linux.deb`
  installs `/usr/bin/onote` on any Debian/Ubuntu/Mint. SLSA build provenance is
  attached like every other release asset.
- **`install.sh` prebuilt fast path**: on x86_64 Linux the installer now
  downloads and installs the matching static musl binary directly, skipping the
  ~2 min `cargo build`; every other platform (and any download failure) falls
  back to building the pinned release from source. Pass `--from-source` to force
  a local build.

### Changed

- CI gains a `linux-musl` job and the Release workflow gains a `linux-portable`
  job so the musl compile and `.deb` packaging are validated on every push, not
  only at tag time.

## [0.2.0] - 2026-07-08

Cross-platform release: Windows joins macOS and Linux as a first-class target.

### Added

- **Windows target** (`x86_64-pc-windows-msvc`) in CI and the Release workflow;
  the release now ships `onote-x86_64-pc-windows-msvc.zip` alongside the macOS
  (arm64 + x86_64) and Linux (x86_64) tarballs, each with SLSA build provenance.
- **Homebrew tap** consolidated to a generic `alansynn/homebrew-tap`
  (`brew tap alansynn/tap && brew install onote`) so the tap can host formulae
  for more than just onote.
- **Scoop bucket** (`alansynn/scoop-onote`) for Windows: `scoop bucket add
  alansynn/scoop-onote && scoop install onote`.

### Changed

- Share-server token now sourced from the OS CSPRNG via `getrandom`
  (Unix `/dev/urandom`/`getentropy`, Windows `BCryptGenRandom`) instead of a
  Unix-only `/dev/urandom` read — the tokenized share URL now works on Windows.

## [0.1.0] - 2026-07-07

First release. A terminal-native scratchpad for an Obsidian-compatible Markdown
vault — local-first, single binary, no network required for core use.

### Added

- Terminal-native TUI editor (ratatui + crossterm) operating directly on an
  Obsidian-compatible Markdown vault; the vault folder is the source of truth.
- Fuzzy title open (`nucleo`) and full-text search over note bodies via SQLite
  FTS5 (`rusqlite`), backed by a derived `.onote/index.sqlite` cache.
- Clipboard image paste into the configured `Attachments/` directory with both
  standard Markdown (`![](Attachments/...)`) and Obsidian (`![[...]]`) image
  link styles.
- In-terminal image preview (`ratatui-image`) with a filename fallback for
  terminals without a supported graphics protocol.
- Read-only HTTP share server (`axum`) rendering the current note to HTML and
  resolving local image paths, with a tokenized share URL and a printed QR code
  (`qr2term`). Loopback by default; LAN exposure is opt-in.
- Git backup (commit / push / pull `--ff-only`) excluding the derived `.onote/`
  cache; backup never blocks or mutates note content.
- `obsidian://` GUI launch with `{vault}` and `{file}` template placeholders.
- Daily notes under the configured `Daily/` directory.
- File watching (`notify`) with external-edit conflict detection using
  optimistic concurrency on content hashes — never a silent overwrite
  (see `CLAUDE.md` §7).
- `onote completions <shell>` (print a shell completion script to stdout) and
  `onote log` (print the most recent onote log file to stdout) subcommands.
- `just` task runner recipes (`just ci`, `just run`, `just release`, ...) and
  an `install.sh` installer script.
- MIT license.

[Unreleased]: https://github.com/AlanSynn/onote/compare/v0.3.0...HEAD
[0.3.0]: https://github.com/AlanSynn/onote/releases/tag/v0.3.0
[0.2.1]: https://github.com/AlanSynn/onote/releases/tag/v0.2.1
[0.2.0]: https://github.com/AlanSynn/onote/releases/tag/v0.2.0
[0.1.0]: https://github.com/AlanSynn/onote/releases/tag/v0.1.0

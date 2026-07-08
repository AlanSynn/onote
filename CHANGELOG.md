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
  alansynn/onote && scoop install onote`.

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

[Unreleased]: https://github.com/AlanSynn/onote/compare/v0.2.0...HEAD
[0.2.0]: https://github.com/AlanSynn/onote/releases/tag/v0.2.0
[0.1.0]: https://github.com/AlanSynn/onote/releases/tag/v0.1.0

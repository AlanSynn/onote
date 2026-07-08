# Contributing to onote

Thanks for taking the time to contribute to `onote` — a terminal-native,
Obsidian-compatible Markdown vault client. This guide is short on purpose;
when in doubt, open an issue and ask.

## Prerequisites

- **Rust >= 1.82** (stable toolchain; matches `rust-version` in `Cargo.toml`).
- **[just](https://github.com/casey/just)** — `brew install just` or `cargo install just`.
- **git**.

## Getting started

```bash
git clone https://github.com/AlanSynn/onote.git
cd onote
just ci      # verify the full gate passes locally
just run     # launch the TUI against your configured vault
```

Configure the vault path and other settings in `~/.config/onote/config.toml`
(see `README.md` and `CLAUDE.md`).

## Before submitting a PR

- Run **`just ci`** locally. It mirrors CI exactly (`fmt-check` + `clippy
  -D warnings` + tests) — if it passes locally, it passes in CI.
- Add tests for new behavior.
- Keep clippy clean under **`-D warnings`**.
- Do **not** introduce `unwrap` / `expect` / `panic!` outside test code.

## Architecture

`onote` follows strict domain-driven layering. **Read `CLAUDE.md` first** —
it is the authoritative design spec. The load-bearing rules:

- **Domain knows nothing** about TUI, SQLite, Git, Ratatui, or the clipboard.
  Add a new backend via a **port** (trait under `ports/`) plus an infra adapter,
  never by editing application use-cases directly.
- **S7 — never overwrite**: saves use optimistic concurrency on content hashes;
  an external change triggers a conflict state, never a silent overwrite.
- **S3.1 — vault confinement**: every note/attachment path is relative to the
  vault root; operations must not escape it.
- **Share is read-only**, tokenized, and loopback by default (LAN opt-in).

## Commits & releases

- Releases follow [Keep a Changelog](https://keepachangelog.com/) (`CHANGELOG.md`)
  and [Semantic Versioning](https://semver.org/).
- **Add a user-facing change under the `[Unreleased]` section of `CHANGELOG.md`
  in the same PR that introduces it** — that keeps the changelog honest at
  release time instead of being reconstructed from memory.
- Git tags track the `Cargo.toml` version (e.g. `v0.1.0`).

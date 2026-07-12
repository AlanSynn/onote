---
title: Getting Started
description: "Install onote in 30 seconds, open your first note, and learn the local-first promise, config location, and vault layout."
section: Get Started
order: 1
---

# Getting Started

`onote` is a lightweight, terminal-native client for an Obsidian-compatible
Markdown vault. It is not an Obsidian replacement — it is a fast, local-first
surface that reads and writes the same plain `.md` files Obsidian uses. It works
on **any** Markdown folder; a `.obsidian/` directory is not required.

## The local-first promise

Every core operation works offline (`CLAUDE.md` §1.1):

- read, edit, and save notes
- paste and preview images
- fuzzy and full-text search the vault
- back up to a local Git repository

Network operations — `git push`, `git pull`, and public share tunneling — are
optional and never block editing.

## 30-second quickstart

Install the prebuilt binary (macOS recommended; see [Install](./install.md) for
every other platform):

```bash
brew install alansynn/tap/onote
```

Point onote at a Markdown folder and launch it:

```bash
onote
```

On first run, onote opens the configured default note (`Scratch.md`) and
**creates it if absent** (`src/application/ops.rs`, `open_default`). It also
creates a `.onote/` state directory inside the vault. Start typing. `Ctrl+S`
saves. `Ctrl+Q` quits.

```text
╭──────────────────────────────────────╮
│ ~/Notes/Vault/Scratch.md             │
├──────────────────────────────────────┤
│ # Scratch                            │
│                                      │
│ ▮                                    │
├──────────────────────────────────────┤
│ saved                                │
╰──────────────────────────────────────╯
```

## Configuration

onote reads its config from `~/.config/onote/config.toml`, located via the XDG
Base Directory spec — set `XDG_CONFIG_HOME` (it must be absolute) to relocate
it (`src/config.rs`). A missing file is fine: defaults point the vault at
`~/Notes/Vault`.

```toml
vault        = "~/Notes/Vault"
default_note = "Scratch.md"
```

## Vault layout

Markdown files are the source of truth; `.onote/` holds only derived state (an
SQLite index and session records) and is excluded from backup. Obsidian ignores
dotfolders, so the two coexist cleanly.

```text
~/Notes/Vault/
  Scratch.md
  Inbox.md
  Daily/
  Notes/
  Attachments/
  .obsidian/   ← Obsidian config (optional)
  .onote/      ← onote derived cache, excluded from backup
```

> Next: master the [editor](./editor.md) surface, or browse every [command](./commands.md).

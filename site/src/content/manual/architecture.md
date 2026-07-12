---
title: Architecture
description: "A layered Rust app — a pure domain core, application use-cases that depend on port traits, and swappable infrastructure adapters."
section: Project
order: 1
---

# Architecture

onote is strict about layering. The hard rule from the design brief (`CLAUDE.md`
§1.3): **the domain knows nothing about the TUI, SQLite, Git, Ratatui, or the
clipboard.** Every external concern lives behind a port, implemented by a
swappable infrastructure adapter.

## Layers

```text
   ┌───────────────────────────────────┐
   │              DOMAIN               │
   │  vault · note · attachment        │
   │  session · share · backup         │
   └─────────────────┬─────────────────┘
                     │ depends on ↓
   ┌─────────────────┴─────────────────┐
   │            APPLICATION            │
   │  open_note · save_note            │
   │  paste_image · share_note         │
   │  backup_vault · search_notes …    │
   └─────────────────┬─────────────────┘
                     │ uses port traits ↓
   ┌─────────────────┴─────────────────┐
   │              PORTS                │
   │  VaultRepository · NoteIndex      │
   │  AttachmentStore · Clipboard      │
   │  ShareServer · BackupService …    │
   └───┬───────────────────────────┬───┘
       │ implemented by            │ implemented by
       ▼                           ▼
   ┌───────────────────────────────────┐
   │           INFRASTRUCTURE          │
   │  filesystem_vault · sqlite_index  │
   │  comrak · axum · git_cli          │
   │  arboard · notify · image         │
   └───────────────────────────────────┘
```

**Domain** (`src/domain/`) holds the bounded contexts — `vault`, `note`,
`attachment`, `session`, `share`, `backup` — plus typed errors. It owns note
identity, vault path policy, attachment references, edit-session state, share
snapshots, and backup reports. It imports no TUI, database, Git, or clipboard
crate.

**Application** (`src/application/`) contains the use cases — `open_note`,
`save_note`, `create_note`, `search_notes`, `paste_image`, `copy_note`,
`share_note`, `backup_vault`, `resolve_conflict`, `open_in_obsidian`. Each use
case depends on **port traits**, never on a concrete library.

**Ports** (`src/ports/`) are small interfaces: `VaultRepository`, `NoteIndex`,
`AttachmentStore`, `Clipboard`, `ShareServer`, `BackupService`, `FileWatcher`,
and `UriLauncher`. A new backend is added by implementing a port, not by editing
use cases.

**Infrastructure** (`src/infra/`) adapts libraries to the ports:
`filesystem_vault`, `sqlite_index`, `markdown` (Comrak), `macos_clipboard`
(arboard), `terminal_image` (ratatui-image), `http_share` (Axum), `git_cli`
(system `git`), `file_watch` (notify), and `obsidian_uri`.

**UI and CLI** (`src/ui/tui`, `src/cli`) depend on the application layer. They
dispatch actions to use cases and never reach into infrastructure directly.

## Data model: files are the source of truth

Markdown files are authoritative. The SQLite database under `.onote/` is a
**derived cache and session-state store**, never the canonical copy
(`CLAUDE.md` §6.1). Its schema (`CLAUDE.md` §6.2):

| Table              | Purpose                                          |
| ------------------ | ------------------------------------------------ |
| `notes`            | Title, content hash, modified/indexed timestamps |
| `notes_fts`        | FTS5 virtual table over path, title, body        |
| `attachments`      | Image metadata (mime, dimensions, size)          |
| `note_attachments` | Note-to-attachment reference graph               |
| `sessions`         | Multi-terminal edit-session coordination         |
| `recent_notes`     | Recents list                                     |

Reindexing rebuilds these tables from the Markdown on disk; deleting the
database loses no note content.

See [Design Guarantees](./guarantees.md) for the load-bearing promises this
layering enforces.

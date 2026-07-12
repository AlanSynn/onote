---
title: Design Guarantees
description: "onote's load-bearing promises — local-first, no silent overwrites, read-only share, content-safe backup — and its non-goals."
section: Project
order: 2
---

# Design Guarantees

These are load-bearing promises, not aspirations. Each follows from the
architecture in [Architecture](./architecture.md) and the product brief
(`CLAUDE.md`).

## Local-first

Every core operation works without network access (`CLAUDE.md` §1.1): read,
edit, save, insert an image, search the vault, preview an image, share on
localhost/LAN, and commit a backup. Only `git push`, `git pull`, public
tunneling, and Obsidian Sync are network-dependent — and they never block
editing.

## Obsidian-compatible, not Obsidian-dependent

onote understands `[[wikilinks]]`, `![[embeds]]`, `#tags`, frontmatter, daily
notes, and the attachments folder — but it works on any plain Markdown
directory. No `.obsidian/` config is required (`CLAUDE.md` §1.2).

## No silent overwrites

Every editor buffer tracks four hashes (`CLAUDE.md` §7): `opened_hash`,
`current_disk_hash`, `buffer_hash`, and `last_saved_hash`. On save:

```text
if disk_hash == opened_hash:
    write the file, update opened_hash
else:
    enter ChangedExternally state
    offer reload / merge / conflict copy / overwrite
```

The default action is **reload or conflict copy**. onote **never defaults to
overwrite** — a save never clobbers an external edit silently.

## Vault ops stay inside the vault

Every note path is relative to the vault root, attachment paths are relative by
default, and **vault operations must not escape the vault root** (`CLAUDE.md`
§3.1, Vault context).

## Backup is content-safe

`onote backup` never changes note content. It can commit generated metadata only
if configured, and it **must not run automatically during text entry**
(`CLAUDE.md` §3.1, Backup context). The derived `.onote/` SQLite cache is
excluded from the commit, so only your Markdown is backed up.

## Share is read-only and tokenized

The share server serves a **snapshot**, not a live mutable buffer; the share URL
is **tokenized**; and the server stops on command or process exit (`CLAUDE.md`
§3.1, Share context). Loopback is the default — opting into LAN exposure is
explicit.

## Non-goals

The following are explicitly out of scope for the MVP (`CLAUDE.md` §10):

- real-time remote collaboration
- graph view
- full Obsidian plugin compatibility
- rich-text editing / WYSIWYG Markdown
- database-backed proprietary note format
- mobile client or web editor
- AI features

onote is a terminal-native scratchpad for the vault, not a terminal clone of
Obsidian.

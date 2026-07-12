---
title: Editor
description: "The onote editor surface: path bar, status line, save/reload/conflict flow, grapheme-aware selection, and responsive wrap."
section: Editor
order: 1
---

# Editor

The editor is `onote`'s main surface: a top path bar, the editor body, and a
bottom status line. It edits the vault's Markdown files directly — there is no
parallel buffer format.

```text
┌────────────────────────────────────────────────────┐
│ Notes/2026/07/robot-idea.md                        │  path bar
├────────────────────────────────────────────────────┤
│ │ # Robot idea                                     │
│ │ A quick sketch from this morning.                │  editor body
│ │ [image: img-20260712-0903.png]                   │
├────────────────────────────────────────────────────┤
│ unsaved                                            │  status line
└────────────────────────────────────────────────────┘
```

- **Path bar** — the vault-relative path, bold and ellipsized so the filename
  survives on a narrow terminal (`…/dir/file.md`); never right-clipped.
- **Editor body** — the Markdown text, with image embeds as
  `[image: filename.png]` tokens.
- **Status line** — the save-state label, hints, and toasts.

## Status line

The status reflects the sync model (`SyncStatus`):

| Label | Meaning |
|---|---|
| `clean` | Buffer matches disk. |
| `unsaved` | Edits pending a save. |
| `saving…` | Write in flight. |
| `changed externally` | Another process touched the file. |
| `CONFLICT: …` | Save refused — disk diverged from the baseline. |

## Edit, save, reload

Type to edit. `Ctrl+S` saves. The buffer records an `opened_hash` baseline; a
save writes only when disk still matches that baseline, then advances it.
`Ctrl+R` reloads, discarding the buffer.

## Conflict handling

A file watcher observes the vault. When the open note's disk hash diverges from
`opened_hash`, the status flips to `changed externally`. If a save then detects
the mismatch it refuses to write, and the status becomes `CONFLICT`, offering
three explicit exits:

- `Ctrl+R` — reload (discard the buffer)
- `Ctrl+K` — save a conflict copy
- `Ctrl+Shift+K` — overwrite disk deliberately

`onote` never defaults to overwrite. The two-modifier `Ctrl+Shift+K` is an
intentional escape hatch; a terminal that cannot distinguish the SHIFT bit
reports plain `Ctrl+K` (conflict copy), so overwrite stays unreachable rather
than misfiring.

## Grapheme-aware selection and responsive wrap

Selection endpoints snap to grapheme boundaries, so multi-byte and combining
characters move as units. Visual layout is grapheme-aware soft-wrap: every frame
the editor re-wraps the buffer at the current width, reflowing on resize with no
clipping. Wide CJK characters and `[image: …]` glyphs wrap atomically.

## Navigation

- `Ctrl+O` — fuzzy-open by title
- `Ctrl+F` — full-text body search (FTS5)
- `Ctrl+G` — follow the link under the caret
- `Ctrl+B` — jump back

See the full [Keymap](./keymap.md) and the [Explorer](./explorer.md) drawer.

---
title: Explorer
description: "The onote Explorer drawer: toggle, vault tree, raw file-op keys (new/rename/delete), confirm prompts, and responsive layout."
section: Editor
order: 3
---

# Explorer

The Explorer is the LEFT vault-tree pane: a navigable outline of the vault's
folders and notes. It auto-shows on wide terminals and collapses on narrow ones,
so the editor keeps the full row when space is tight.

```text
┌─ Vault ──────────┬──────────────────────────────┐
│ › Daily/         │ # Robot idea                 │
│   2026-07-12.md  │                              │
│   Notes/         │ A quick sketch.              │
│ ‣ robot-idea.md  │                              │
└──────────────────┴──────────────────────────────┘
```

## Toggle and focus

`Ctrl+E` (`ToggleExplorer`) flips the pane's visibility and moves focus into
it. The key is pane-agnostic — it works from either pane. Below the auto-show
threshold the editor occupies the whole content row, byte-identical to a
pre-Explorer build (zero regression).

## Tree navigation

With the Explorer focused, arrows move through the tree: `Up`/`Down` move the
selection, `Left` collapses the selected folder, `Right` expands it, and
`Enter` toggles expand or opens the selected note.

## File operations

File-op keys are **raw and fixed** — not part of the keymap registry, because
binding `n` / `r` / `d` globally would break typing those letters in the editor.
They fire only while the Explorer is focused and visible:

| Key | Action |
|---|---|
| `n` | New note (name prompt) |
| `N` or `Shift+n` | New folder (name prompt) |
| `r` | Rename selected (prompt prefilled with the current name) |
| `d` | Delete selected (y/n confirm) |

`Shift+n` arrives as `N` or `n` plus the SHIFT bit depending on the terminal;
both map to "new folder".

## Prompts and confirm

- **Name prompt** — printable characters append, `Backspace` pops, `Enter`
  commits, `Esc` cancels. On a commit error (e.g. a rename onto a busy target —
  `onote` never silently overwrites), the prompt stays open with the input
  preserved so the name can be edited and retried.
- **Delete confirm** — `y` or `Enter` deletes; `n` or `Esc` cancels.

After any op the tree refreshes, and the editor follows when the open note moved
or was deleted.

## Layout knobs

The `[layout]` table tunes the pane (`../layout.md`):

```toml
[layout]
explorer_width          = 30    # pane width when visible
explorer_hidden_width   = 4     # reserved toggle gutter
show_explorer_threshold = 100   # auto-show at/above this width
```

The default threshold is 100, so a typical 80-column terminal stays editor-only.
`Ctrl+E` overrides the auto policy either way — force-on when hidden, force-off
when visible.

See the [Editor](./editor.md) surface and the full [Keymap](./keymap.md).

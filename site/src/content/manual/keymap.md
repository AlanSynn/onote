---
title: Keymap
description: "onote's [keymap] system: the action vocabulary, key-spec grammar, case-insensitive matching, and skip-on-malformed overrides."
section: Editor
order: 2
---

# Keymap

Every edit-mode keystroke resolves to a logical `Action` through a single
`KeymapRegistry` (`src/ui/tui/keymap.rs`). No binding is hardcoded in a `match`
arm — each one is remappable from a `[keymap]` table in `config.toml`, layered
over baked defaults.

## Action vocabulary

The registry defines one `Action` per logical, input-device-independent editor
command (lines 20–79):

- **Global**: `Quit`, `Save`, `Reload`, `OpenFuzzy`, `OpenSearch`, `PasteImage`,
  `DeleteImageToken`, `ConflictCopy`, `Overwrite`, `ToggleExplorer`,
  `OpenLink`, `GoBack`
- **Editing**: `InsertChar`, `Enter`, `Backspace`, `Tab`
- **Cursor motion**: `MoveLeft` / `Right` / `Up` / `Down` / `Home` / `End`
- **Selection**: `SelectLeft` / `Right` / `Up` / `Down` / `Home` / `End`,
  `SelectAll`, `ClearSelection`
- **Clipboard / delete**: `Copy`, `Cut`, `DeleteForward`
- **Word motion**: `WordLeft`, `WordRight`, `SelectWordLeft`, `SelectWordRight`

`InsertChar` is the universal text-entry fallback, not a registered binding: a
printable character with no `Ctrl`/`Alt` modifier inserts itself.

## Key-spec grammar

A `[keymap]` key is a `+`-joined spec parsed by `parse_key_spec` (lines 255–271):

```text
modifier + modifier + key
```

- **Modifiers** (case-insensitive): `ctrl` / `control`, `alt` / `option` /
  `meta`, `shift`.
- **Key name**: `enter`/`return`, `tab`, `backspace`/`bs`, `esc`/`escape`,
  `delete`/`del`, `insert`/`ins`, `home`, `end`, `pageup`/`pgup`,
  `pagedown`/`pgdn`, `left`/`right`/`up`/`down`, `space`/`spacebar`,
  `f1`–`f12`, or a single literal character.

Parsing is case-insensitive throughout. Letter keys fold to lowercase for
matching, while the SHIFT modifier bit is preserved on the combo — so `Ctrl+C`
(quit) stays distinct from `Ctrl+Shift+C` (copy), and `Ctrl+K` (conflict-copy)
from `Ctrl+Shift+K` (overwrite).

Action names accept both `snake_case` and `kebab-case`, with aliases (`open`,
`search`/`find`, `paste`, `back`, `deselect`, `delete`).

## Overrides and graceful degradation

User entries overlay the defaults (`apply_overrides`, lines 216–232). A
malformed key spec or unknown action is **skipped with a warning**, not fatal —
the default binding for that key survives, so a typo can never brick the editor.

```toml
[keymap]
"ctrl+s" = "save"            # leave at default, or rebind
"ctrl+shift+c" = "copy"      # copy selection
"ctrl+x" = "cut"             # cut selection
"ctrl+a" = "select_all"
"f4" = "copy"                # add a brand-new binding
```

See the [Editor](./editor.md) surface for how these actions behave, and
[Getting Started](./getting-started.md) to locate `config.toml`.

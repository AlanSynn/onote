---
title: Layout
description: "The [layout] table and the responsive Explorer drawer — thresholds, widths, and the zero-regression guarantee for wide terminals."
section: Configure
order: 2
---

# Layout

The `[layout]` table drives the responsive Explorer drawer — the basalt-style
`[Explorer | Editor]` split (`src/config.rs`, `LayoutConfig`). All values are
terminal columns.

```toml
[layout]
explorer_width          = 30   # Explorer pane width when visible
show_explorer_threshold = 100  # auto-show at/above this terminal width
explorer_hidden_width   = 4    # reserved (future toggle-gutter width)
```

| Key | Default | Meaning |
| --- | --- | --- |
| `explorer_width` | `30` | Explorer pane width (columns) when visible. |
| `show_explorer_threshold` | `100` | Auto-show the Explorer at/above this terminal width. |
| `explorer_hidden_width` | `4` | Reserved — future toggle-gutter width when hidden. |

Each key defaults independently via `#[serde(default = …)]` on its field
(`src/config.rs`); omitting one falls back to its own default, not `0`.

## Responsive behavior

Below `show_explorer_threshold` columns the Explorer is `Hidden` and the
[editor](./editor.md) takes the full row — byte-identical to a pre-Explorer
build. At/above the threshold the Explorer auto-shows at `explorer_width`
columns. The key guarantee: **zero regression** above the threshold. The
auto-hide is purely additive, a new behavior for narrow terminals; everything
wide-terminal users already had is unchanged.

`Ctrl+E` overrides the auto policy at any width: it toggles the
[Explorer](./explorer.md) and moves focus into or out of it. So a
narrow-terminal user can still summon the tree on demand, and a wide-terminal
user can dismiss it.

## Overriding the threshold

Lower `show_explorer_threshold` to make the Explorer appear on narrower
terminals; raise it to keep the editor full-width until there is room. To pin
the Explorer off until explicitly toggled, set the threshold above your widest
terminal. The Explorer file-op keys (`n` `N` `r` `d`) are raw, not remappable —
they are intercepted only while the Explorer is focused, so they never collide
with editor typing.

See [Configuration](./configuration.md) for the full key reference.

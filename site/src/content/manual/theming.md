---
title: Theming
description: "onote ships four canonical Catppuccin flavors as exact RGB palettes behind semantic UI roles; set one via the theme key."
section: Reference
order: 2
---

# Theming

onote's color system is [Catppuccin](https://catppuccin.com/palette/). The TUI
embeds all four canonical flavors — **Latte**, **Frappé**, **Macchiato**, and
**Mocha** — as exact `Color::Rgb` constants (the v0.3 hexes), then projects each
palette onto a small set of semantic UI roles that the renderers consume. Raw
palette slots are private; a role can be remapped in one place without touching
render code. The implementation lives in `src/ui/tui/theme.rs`.

## Choosing a flavor

Set `theme` in `config.toml` (see [Configuration](./configuration.md)):

```toml
theme = "mocha"   # latte | frappe | macchiato | mocha
```

Accepted values are `latte`, `frappe`, `macchiato`, and `mocha`, matched
**case-insensitively** (`Flavor::parse` lowercases and trims the input). The
default is **Latte** — a fixed, predictable light theme. Frappé, Macchiato, and
Mocha are the three dark flavors.

An unknown or empty value falls back to **Latte** rather than erroring. A theme
typo must never block startup, so a misconfigured `theme` key silently selects
the light default instead of failing the launch.

## Semantic roles

Renderers never read palette slots directly; they call role accessors. Each role
maps to a fixed Catppuccin color across all four flavors:

| Role             | Catppuccin slot | Used for                                      |
| ---------------- | --------------- | --------------------------------------------- |
| `accent()`       | Lavender        | Titles, headers, prompt cursor (theme.rs:262) |
| `glyph()`        | Mauve           | Inline image-embed glyphs (theme.rs:278)      |
| `link()`         | Blue            | Links, tags, attachment paths, folder names   |
| `success()`      | Green           | Success / clean status                        |
| `warning()`      | Yellow          | Warnings, fuzzy-picker accent                 |
| `error()`        | Red             | Errors                                        |
| `info()`         | Sky             | Body-search picker accent                     |
| `text()`         | Text            | Default body and name text                    |
| `muted()`        | Subtext0        | Hints, snippets, empty-state lines            |
| `border()`       | Overlay1        | Pane and widget borders                       |
| `bar_bg()`       | Surface1        | Path and status bar background                |
| `selection_bg()` | Surface2        | Explorer list-item highlight background       |

Every accessor returns a `Color::Rgb` — no `Reset` or named-color fallback leaks
into the render path, so the chosen flavor is always the one rendered.

## Painted vs. unpainted surfaces

Terminal colors are normally managed by the terminal itself, so **large surfaces
are left unpainted** — the editor body shows your terminal's background, keeping
onote cohesive in both light and dark terminals without fighting your palette.
Only the bars (path and status) and accents (titles, borders, selection, status
labels) are themed.

This docs site uses the same Latte/Mocha palette, so light and dark modes here
mirror the in-app flavors.

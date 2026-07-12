---
title: Configuration
description: "Every config.toml key with its real default, the file location, and the dangerous attachment_dir values rejected at load time."
section: Configure
order: 1
---

# Configuration

`onote` reads its config from `<config_root>/onote/config.toml`, where
`config_root` is `$XDG_CONFIG_HOME` (when set to an absolute path) or
`~/.config` (`src/config.rs`, `Config::config_path`). On Linux and macOS that
resolves to `~/.config/onote/config.toml`. A relative or empty `XDG_CONFIG_HOME`
is ignored and the `~/.config` fallback is used, per the XDG spec. A missing
file is fine — onote ships with sensible defaults and bare `onote` runs against
`~/Notes/Vault`.

The `vault` value may contain `~`; it is expanded with `shellexpand::tilde` at
load time (`src/config.rs`, `ConfigFile::into_config`) and must be absolute
after expansion, or load fails.

## Keys

Every key below exists in `src/config.rs` (`Config` and `ConfigFile`); defaults
are cited from the `default_*` functions there.

| Key | Type | Default | Notes |
| --- | --- | --- | --- |
| `vault` | string (path) | `~/Notes/Vault` | Vault root; `~`-expanded; must be absolute. |
| `default_note` | string | `Scratch.md` | Opened by `onote` / `onote scratch`. |
| `attachment_dir` | string | `Attachments` | Relative under vault; see rejection rules below. |
| `daily_dir` | string | `Daily` | Where `onote today` writes. |
| `image_link_style` | `markdown` \| `obsidian` | `markdown` | `![](…)` (portable) or `![[…]]`. |
| `open_gui_command` | string | `obsidian://open?vault={vault}&file={file}` | `{vault}` / `{file}` substituted. |
| `backup_remote` | string | `origin` | Git remote for `onote backup`. |
| `share_port` | integer | `7478` | Port for the read-only share server. |
| `share_allow_lan` | boolean | `false` | `false` = loopback; `true` = bind LAN (`0.0.0.0`). |
| `theme` | string | `latte` | `latte` \| `frappe` \| `macchiato` \| `mocha`; case-insensitive; unknown → `latte`. |
| `[keymap]` | table | empty | Editor binding overrides; see [Keymap](./keymap.md). |
| `[layout]` | table | see [Layout](./layout.md) | Responsive Explorer knobs. |

`attachment_dir` is rejected at load time if it is empty, `.` (would expose the
whole vault as attachments), an absolute path, or contains a `..` segment
(`src/config.rs`, `into_config`). This is defense-in-depth for the share
server's attachment confinement — a config-set `.`/absolute/`..` can never make
note files like `Secret.md` servable as "attachments".

## Example

A complete `config.toml` (`src/config.rs` exercises every key):

```toml
vault             = "~/Notes/Vault"
default_note      = "Scratch.md"
attachment_dir    = "Attachments"
daily_dir         = "Daily"
image_link_style  = "markdown"
open_gui_command  = "obsidian://open?vault={vault}&file={file}"
backup_remote     = "origin"
share_port        = 7478
share_allow_lan   = false
theme             = "latte"

[layout]
explorer_width          = 30
show_explorer_threshold = 100
explorer_hidden_width   = 4
```

Themes are covered in [Theming](./theming.md).

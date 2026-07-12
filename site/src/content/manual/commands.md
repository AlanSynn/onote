---
title: Commands
description: "Reference for every onote subcommand — run, scratch, today, new, open, tags, share, backup, gui, img, copy, completions, log."
section: Reference
order: 1
---

# Commands

Each subcommand is a variant of the `Command` enum in `src/cli/mod.rs`. Bare
`onote` resolves to `Run`; `--version` and `--help` work on any invocation.

## Overview

| Command                              | What it does                                                  |
| ------------------------------------ | ------------------------------------------------------------- |
| `onote` · `onote run`                | Open the bare TUI on the default note                         |
| `onote scratch`                      | Open the default scratch note                                 |
| `onote today`                        | Open today's daily note                                       |
| `onote new "<title>"`                | Create a note (slugified filename) and open it                |
| `onote open "<query>"`               | Fuzzy-open a note by title                                    |
| `onote tags`                         | List every `#tag` with per-tag note counts                    |
| `onote share`                        | Start a read-only HTTP share server (prints QR)               |
| `onote backup [--push] [--pull]`     | Git commit / push / pull the vault                            |
| `onote gui [query]`                  | Open the note in the Obsidian GUI                             |
| `onote img paste`                    | Paste a clipboard image; prints the insertion token           |
| `onote copy [--md\|--html\|--rich]`  | Copy the current note to the clipboard                        |
| `onote completions <shell>`          | Print a shell completion script to stdout                     |
| `onote log`                          | Print the most recent log file (path on stderr)               |

## `run` / bare `onote`

Opens the TUI on `default_note` — bare `onote` resolves here.

```bash
onote
```

## `scratch`

Opens the default scratch note (alias for `run` when the default is `Scratch.md`).

```bash
onote scratch
```

## `today`

Opens (creating if absent) today's daily note under `daily_dir`.

```bash
onote today
```

## `new "<title>"`

Creates a note and opens it; the title slugifies to a `.md` filename.

```bash
onote new "robot idea"
```

## `open "<query>"`

Fuzzy-matches the query against note titles; multiple matches are disambiguated.

```bash
onote open "robot"
```

## `tags`

Lists every `#tag` with per-tag note counts.

```bash
onote tags
```

## `share`

Starts a read-only HTTP server for the current note and prints a QR code. See
[Share](./share.md).

```bash
onote share
```

## `backup [--push] [--pull]`

Commits the vault via git. `--pull` runs `git pull --ff-only` before committing;
`--push` pushes to `backup_remote` afterward. See [Backup](./backup.md).

```bash
onote backup --pull --push
```

## `gui [query]`

Opens the default note — or the fuzzy-matched `query` — in the Obsidian GUI via
`open_gui_command` (see [Configuration](./configuration.md)).

```bash
onote gui "robot"
```

## `img paste`

Pastes a clipboard image into `attachment_dir` and prints the Markdown insertion
token. See [Images](./images.md).

```bash
onote img paste
```

## `copy [--md|--html|--rich]`

Copies the current note to the clipboard. `--md` (the default) copies Markdown
text; `--html` copies rendered HTML; `--rich` writes the same HTML flavor as
rich text (full RTF is future work).

```bash
onote copy --html
```

## `completions <shell>`

Prints a shell completion script to stdout. `<shell>` is any
`clap_complete::Shell` — `bash`, `elvish`, `fish`, `powershell`, or `zsh`.
Redirect to install:

```bash
onote completions zsh > "${fpath[1]}/_onote"
```

## `log`

Prints the most recent log file to stdout; the file path goes to stderr.

```bash
onote log
```

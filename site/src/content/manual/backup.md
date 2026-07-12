---
title: Backup
description: "onote backup commits your vault to git without touching note content, with optional push and pull --ff-only, and always excludes the .onote cache."
section: Features
order: 3
---

# Backup

`onote backup` keeps your vault under version control by shelling out to system
`git`. It stages, commits, pushes, and pulls — and nothing else.

```bash
onote backup           # commit new changes
onote backup --push    # commit, then push to the remote
onote backup --pull    # pull --ff-only, then commit
```

A bare `onote backup` only creates a commit; it never pushes. `--pull` runs
`git pull --ff-only` first so the commit lands on top of the latest remote
history. `--push` runs `git push` after the commit. When there is nothing to
commit, the command prints `nothing to commit (working tree clean)` and exits
cleanly — the canonical no-op is not an error.

## Hard guarantees

Backup is deliberately separated from editing:

- **It never changes note content.** The adapter only runs `git add`, `commit`,
  `push`, and `pull`; it never opens a note for writing.
- **It never runs automatically during text entry.** Backup is a command you
  invoke explicitly, so an in-flight edit session is never interrupted.
- **The `.onote/` cache is excluded.** The derived SQLite index (`index.sqlite`
  and its WAL/SHM siblings) is staged out via a non-mutating `:(exclude).onote`
  pathspec, so no cache churn enters your history and no `.gitignore` is written
  into your vault.

## Remote and conflicts

The remote name is `backup_remote` (default `origin`). Push handles a
non-fast-forward gracefully — it reports `not pushed (non-fast-forward; pull
first)` rather than clobbering remote history. Pull uses `--ff-only`; if git
reports conflicts, the command prints the count so you can resolve them in a
normal Git workflow. See [Guarantees](./guarantees.md) for the full conflict
algorithm, and [Configuration](./configuration.md) for the remote setting.

---
title: Images
description: "Paste a clipboard image into a note, preview it in the terminal, and manage attachments with Markdown or Obsidian link syntax."
section: Features
order: 1
---

# Images

`onote` treats images as first-class vault citizens: they live as real files under
your attachment directory, and the note body carries a plain Markdown (or Obsidian)
link to them. No base64 blobs, no parallel asset store.

## Paste from the clipboard

From the terminal:

```bash
onote img paste
```

This reads the image on the clipboard, persists it, and prints the insertion token
to insert into a note. From inside the editor, `Ctrl+P` does the same thing and
drops the token at the caret in one step.

## Attachment names and link style

Every pasted image gets a deterministic, timestamped name so re-pastes never
collide and `git` diffs stay readable:

```text
Attachments/2026/07/img-20260707-120301.png
```

The filename is `img-YYYYMMDD-HHMMSS.<ext>` and it lands in a `YYYY/MM` subpath
under `attachment_dir` (default `Attachments`). The inserted link follows your
`image_link_style`:

- `markdown` (default) — portable: `![](Attachments/2026/07/img-20260707-120301.png)`
- `obsidian` — wiki embed: `![[Attachments/2026/07/img-20260707-120301.png]]`

Both styles are parsed back identically when the note is rendered. See
[Configuration](./configuration.md) to change the directory or link style.

## In-terminal preview

Image tokens render as a compact glyph in the editor so prose layout is
unaffected. With the caret on an image line, `Enter` opens a full-screen preview
overlay rendered through `ratatui-image`, which speaks the terminal's native
graphics protocol (Sixel, Kitty, or iTerm2) where available.

On a terminal without a graphics protocol, the overlay degrades gracefully to
metadata — filename, dimensions, and byte size — with a copy-to-clipboard action,
so you never lose the reference.

## Removing an image token

`Ctrl+D` deletes the image token under the caret from the note body. The token
text is removed; the attachment file itself stays on disk so other notes that
reference it keep working. Re-running `onote backup` will then commit the edited
note.

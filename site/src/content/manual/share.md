---
title: Share
description: "onote share starts a read-only, tokenized HTTP server for the current note, with a local URL, optional LAN URL, and QR code."
section: Features
order: 2
---

# Share

`onote share` renders the current note as static HTML and serves it from a local
HTTP server so you can hand it to a phone or a coworker without leaving the
terminal.

```bash
onote share
```

The command prints the local URL, copies it to the clipboard, and (on a real
terminal) prints a QR code you can scan:

```text
http://127.0.0.1:7478/a1b2c3...
sharing read-only. press Enter to stop…
```

Press `Enter` (or close stdin) to stop the server and tear down the snapshot.

## Read-only and tokenized

Share is strictly read-only delivery, never collaborative editing. Every route is
gated by a random token in the URL path — a wrong or missing token returns `404`,
and the token is compared in constant time so its existence is never confirmed.
There is no static-file fallback over the vault: `.git/`, `.obsidian/`, and
`.onote/index.sqlite` are unreachable. Attachment serving is confined to your
`attachment_dir`, canonicalized, and rejects dotfiles and path traversal.

## A snapshot, not a live buffer

The server references an immutable in-memory snapshot of the note taken at start
time — the HTML, the title, and the attachment directory. Edits you make to the
note after starting the server are not reflected until you stop and re-share. This
keeps the rendered page stable for however long the URL is open.

## Binding: loopback or LAN

By default the server binds `127.0.0.1` only, so the URL is reachable solely on
your machine. Set `share_allow_lan = true` to bind `0.0.0.0`; `onote` then also
prints a LAN URL using your primary network IP. The port is `share_port` (default
`7478`). See [Configuration](./configuration.md).

Only one share runs at a time; starting another reports an error until the first
is stopped.

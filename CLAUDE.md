# onote Architecture Design

## 0. Product Definition

`onote` is a lightweight terminal client for an Obsidian-compatible Markdown vault.

It is not a full Obsidian replacement. It is a fast, local-first, terminal-native note surface for:

- global scratch notes
- quick Markdown editing
- image-aware terminal preview
- read-only QR/web sharing
- GitHub backup
- opening the same note in Obsidian GUI when needed

The source of truth is the vault directory:

```text
~/Notes/Vault/
  Scratch.md
  Inbox.md
  Daily/
  Notes/
  Attachments/
  .obsidian/
  .onote/
```

Obsidian already stores notes as local Markdown files inside a vault, so `onote` should operate directly on those files rather than inventing a parallel storage model. This makes image links, Git backup, external editor interoperability, and Obsidian desktop/mobile compatibility much cleaner than an Apple Notes backend. Obsidian’s help describes vault data as local folders containing Markdown files and attachments. ([Docs.rs](https://docs.rs/comrak))

## 1. Core Design Principles

### 1.1 Local-first

All core operations work without network access.

```text
read note
edit note
save note
insert image
search vault
preview image
share on localhost/LAN
commit backup
```

Network-dependent operations are optional:

```text
git push
git pull
external tunnel for public sharing
Obsidian Sync, if the user uses it separately
```

### 1.2 Obsidian-compatible, not Obsidian-dependent

`onote` should work on any Markdown folder, but it should understand common Obsidian conventions:

```text
[[wikilinks]]
![[embedded images]]
#tags
frontmatter
daily notes
attachments folder
.obsidian config
```

The default image link style should be standard Markdown for portability:

```md
![](Attachments/2026/07/img-20260707-120301.png)
```

But Obsidian-style embed syntax should be supported:

```md
![[Attachments/2026/07/img-20260707-120301.png]]
```

### 1.3 Small core, many replaceable adapters

The app should be strict about architecture:

```text
Domain does not know TUI.
Domain does not know SQLite.
Domain does not know Git.
Domain does not know Ratatui.
Domain does not know macOS clipboard.
```

External dependencies belong in infrastructure adapters.

### 1.4 Prefer libraries over custom code

Use libraries aggressively for small parts:

- TUI layout and widgets
- terminal backend
- file watching
- Markdown parsing/rendering
- image rendering
- fuzzy search
- SQLite
- Git
- QR code rendering
- clipboard
- config parsing
- logging/tracing
- HTTP share server
- command parsing
- path handling
- serialization
- error handling
- time/date handling

Custom code should focus on product-specific glue:

```text
note model
vault indexing policy
session coordination
image token UX
drawer/modal UI state
share snapshot policy
conflict handling
```

## 2. Recommended Stack

### 2.1 Language

Use Rust.

Reasons:

- single-binary distribution
- fast startup
- strong type system
- good TUI ecosystem
- good filesystem and CLI support
- safe concurrency model
- suitable for local-first tools

Distribution target (release workflow attaches per-target tarballs to each
`v*` GitHub Release; a Homebrew tap is the preferred macOS path):

```bash
# macOS arm64 — fetch the release tarball and extract the binary
curl -L https://github.com/AlanSynn/onote/releases/latest/download/onote-aarch64-apple-darwin.tar.gz | tar xz
install -m 0755 onote ~/bin/onote
```

### 2.2 TUI

Use:

```toml
ratatui = "*"
crossterm = "*"
```

`ratatui` provides the TUI widget/layout layer, while `crossterm` provides terminal input/output, theme, keyboard, mouse, and alternate-screen support. Ratatui is explicitly positioned as a Rust crate for building terminal user interfaces with widgets and dynamic layouts. ([Ratatui](https://ratatui.rs/))

The UI layer should not directly edit files. It dispatches application commands.

```text
UI Event
  → AppAction
  → Application Use Case
  → Domain
  → Port
  → Infrastructure Adapter
```

### 2.3 Markdown

Use:

```toml
comrak = "*"
```

`comrak` is a CommonMark and GitHub Flavored Markdown-compatible parser/renderer, which is the right default because notes need task lists, tables, strikethrough, autolinks, and GitHub-compatible rendering. ([Docs.rs](https://docs.rs/comrak))

Responsibilities:

```text
parse Markdown
extract headings
extract links
extract image references
render preview HTML for share mode
render preview text for TUI mode
```

Do not write a Markdown parser.

### 2.4 Image Rendering

Use:

```toml
ratatui-image = "*"
image = "*"
```

`ratatui-image` supports multiple terminal graphics protocols, including Sixel, Kitty, and iTerm2 backends, with fallback behavior. ([GitHub](https://github.com/ratatui/ratatui-image))

Image rendering policy:

```text
Editor surface:
  show [image: filename.png]

Hover/focus:
  show small overlay if terminal supports it

Enter/Space:
  open full image preview modal

Fallback:
  show filename, size, dimensions, and open/copy actions
```

### 2.5 File Watching

Use:

```toml
notify = "*"
```

`notify` is the standard cross-platform filesystem notification library for Rust and is appropriate for detecting external edits from Obsidian, another terminal, or Git operations. ([Docs.rs](https://docs.rs/notify))

Responsibilities:

```text
watch current note
watch vault index-relevant paths
watch attachment directory
debounce updates
detect external modifications
trigger conflict state
```

### 2.6 Search

Use:

```toml
nucleo = "*"
```

`nucleo` is a high-performance fuzzy matcher written in Rust and designed for fzf/skim-like matching in TUI applications. ([GitHub](https://github.com/helix-editor/nucleo))

Use cases:

```text
open note
search note title
search recent files
command picker, if needed later
```

For full-text search:

```toml
rusqlite = "*"
```

Use SQLite FTS5 through `rusqlite`.

`rusqlite` is an ergonomic SQLite wrapper for Rust and is widely used for embedded SQLite access. ([Crates.io](https://crates.io/crates/rusqlite))

### 2.7 Git Backup

Start with shelling out to `git`.

```text
git status
git add .
git commit -m "onote backup: 2026-07-07 12:41"
git push
git pull --ff-only
```

Use `git2` only after the CLI flow is stable. The `git2` crate binds to libgit2 and exposes repository management APIs, but it adds implementation complexity and authentication handling. ([Docs.rs](https://docs.rs/git2))

Policy:

```text
MVP:
  use system git

v2:
  optional git2 backend

Never:
  make Git sync block note editing
```

### 2.8 QR and Sharing

Use:

```toml
axum = "*"
tokio = "*"
tower-http = "*"
qr2term = "*"
```

`qr2term` is a small Rust QR renderer for terminal output. ([Crates.io](https://crates.io/crates/qr2term))

Share mode:

```text
onote share
  starts local HTTP server
  renders current note as read-only HTML
  resolves local image paths
  prints local URL
  prints LAN URL
  prints QR code
```

Share should not be collaborative editing. It is read-only delivery.

### 2.9 Clipboard

Use a macOS-specific helper.

Possible libraries/paths:

```text
arboard
copypasta
objc2 / cocoa / swift helper
```

Preferred implementation:

```text
Rust main binary
  +
small macOS clipboard adapter
```

Required operations:

```text
read image from clipboard
write image to clipboard
write Markdown text
write HTML rich text
copy image file
copy note as text/html
```

Do not rely only on `pbpaste`/`pbcopy` for images. The macOS clipboard distinguishes text, HTML, RTF, file references, TIFF, PNG, and other data types.

### 2.10 Config

Use:

```toml
serde = "*"
toml = "*"
directories = "*"
shellexpand = "*"
```

Config file:

```text
~/.config/onote/config.toml
```

Example:

```toml
vault = "~/Notes/MainVault"
default_note = "Scratch.md"
attachment_dir = "Attachments"
daily_dir = "Daily"
image_link_style = "markdown" # markdown | obsidian
open_gui_command = "obsidian://open?vault=MainVault&file={file}"
backup_remote = "origin"
share_port = 7478
```

### 2.11 Error Handling and Logging

Use:

```toml
anyhow = "*"
thiserror = "*"
tracing = "*"
tracing-subscriber = "*"
```

Policy:

```text
Domain errors:
  typed with thiserror

Application boundary:
  typed where recoverable

CLI/TUI top-level:
  anyhow is acceptable

Logs:
  ~/.local/state/onote/onote.log
```

## 3. Domain-Driven Design

## 3.1 Bounded Contexts

### Vault

Owns the Markdown folder.

Entities:

```text
Vault
VaultPath
NotePath
AttachmentPath
VaultConfig
```

Rules:

```text
Every note path is relative to the vault root.
No absolute attachment paths in Markdown by default.
Vault operations must not escape the vault root.
```

### Notes

Owns note identity, title, body, metadata, and edit state.

Entities:

```text
Note
NoteId
NoteTitle
MarkdownBody
NoteSummary
NoteFrontmatter
```

Value objects:

```text
RelativeNotePath
ContentHash
ModifiedTime
CursorPosition
```

### Attachments

Owns images and binary files.

Entities:

```text
Attachment
ImageAttachment
AttachmentReference
```

Rules:

```text
Images are stored under attachment_dir.
Inserted images get deterministic timestamped names.
Deleting an image token may optionally delete the file if no other note references it.
```

### Sessions

Owns local multi-terminal coordination.

Entities:

```text
EditSession
SessionId
SessionMode
SessionLock
ExternalChange
```

Modes:

```text
edit
follow
takeover
conflict-copy
```

### Share

Owns read-only delivery.

Entities:

```text
ShareSession
ShareToken
ShareSnapshot
SharePolicy
```

Rules:

```text
Share is read-only by default.
Share session references a snapshot, not a live mutable editor buffer, unless explicitly configured.
Share URL should be tokenized.
Share server should stop on command or process exit.
```

### Backup

Owns Git backup state.

Entities:

```text
BackupState
GitStatus
BackupRemote
BackupReport
```

Rules:

```text
Backup never changes note content.
Backup can commit generated metadata only if configured.
Backup must not run automatically during text entry.
```

## 3.2 Layering

```text
src/
  domain/
    vault.rs
    note.rs
    attachment.rs
    session.rs
    share.rs
    backup.rs
    errors.rs

  application/
    open_note.rs
    save_note.rs
    create_note.rs
    search_notes.rs
    paste_image.rs
    copy_note.rs
    share_note.rs
    backup_vault.rs
    resolve_conflict.rs
    open_in_obsidian.rs

  ports/
    vault_repository.rs
    note_index.rs
    attachment_store.rs
    file_watcher.rs
    clipboard.rs
    image_renderer.rs
    share_server.rs
    backup_service.rs
    uri_launcher.rs

  infra/
    filesystem_vault/
    sqlite_index/
    markdown/
    macos_clipboard/
    terminal_image/
    http_share/
    git_cli/
    file_watch/
    obsidian_uri/

  ui/
    tui/
      app.rs
      layout.rs
      editor.rs
      note_drawer.rs
      preview_drawer.rs
      share_drawer.rs
      image_overlay.rs
      status_line.rs
      mouse.rs

  cli/
    args.rs
    commands.rs
```

## 3.3 Ports

### VaultRepository

```rust
trait VaultRepository {
    fn list_notes(&self) -> Result<Vec<NoteSummary>, VaultError>;
    fn read_note(&self, path: &RelativeNotePath) -> Result<NoteDocument, VaultError>;
    fn write_note(&self, note: &NoteDocument, expected_hash: Option<ContentHash>)
        -> Result<WriteResult, VaultError>;
    fn create_note(&self, draft: NewNote) -> Result<RelativeNotePath, VaultError>;
    fn delete_note(&self, path: &RelativeNotePath) -> Result<(), VaultError>;
}
```

### NoteIndex

```rust
trait NoteIndex {
    fn refresh_note(&self, note: &NoteDocument) -> Result<(), IndexError>;
    fn remove_note(&self, path: &RelativeNotePath) -> Result<(), IndexError>;
    fn fuzzy_titles(&self, query: &str) -> Result<Vec<NoteSummary>, IndexError>;
    fn full_text_search(&self, query: &str) -> Result<Vec<SearchHit>, IndexError>;
}
```

### AttachmentStore

```rust
trait AttachmentStore {
    fn save_image(&self, image: ImageData) -> Result<Attachment, AttachmentError>;
    fn resolve(&self, reference: &AttachmentReference) -> Result<Attachment, AttachmentError>;
    fn is_referenced_elsewhere(&self, attachment: &Attachment) -> Result<bool, AttachmentError>;
}
```

### Clipboard

```rust
trait Clipboard {
    fn read_text(&self) -> Result<Option<String>, ClipboardError>;
    fn read_image(&self) -> Result<Option<ImageData>, ClipboardError>;
    fn write_text(&self, text: &str) -> Result<(), ClipboardError>;
    fn write_html(&self, html: &str, plain_text: &str) -> Result<(), ClipboardError>;
    fn write_image(&self, image: &ImageData) -> Result<(), ClipboardError>;
}
```

### ShareServer

```rust
trait ShareServer {
    fn start(&self, snapshot: ShareSnapshot, policy: SharePolicy)
        -> Result<ShareSession, ShareError>;
    fn stop(&self, session: ShareSessionId) -> Result<(), ShareError>;
}
```

### BackupService

```rust
trait BackupService {
    fn status(&self) -> Result<BackupState, BackupError>;
    fn commit(&self, message: BackupMessage) -> Result<BackupReport, BackupError>;
    fn push(&self) -> Result<BackupReport, BackupError>;
    fn pull_ff_only(&self) -> Result<BackupReport, BackupError>;
}
```

## 4. SOLID Application

### Single Responsibility

Each module owns one reason to change.

```text
filesystem_vault changes when file layout changes.
sqlite_index changes when search/index schema changes.
tui/editor changes when editing UX changes.
http_share changes when share rendering changes.
git_cli changes when backup behavior changes.
```

Do not let the TUI manipulate files directly.

### Open/Closed

New backends should be added through ports.

Examples:

```text
GitCliBackup → Git2Backup
ComrakMarkdownRenderer → alternate renderer
MacClipboard → LinuxClipboard
FilesystemVault → RemoteReadonlyVault
```

Application logic should remain stable.

### Liskov Substitution

Every implementation of a port must preserve contract behavior.

Example:

```text
write_note(expected_hash)
  must detect external modification
  must not silently overwrite if hash differs
```

This contract must hold whether the backend is filesystem, test fake, or future sync adapter.

### Interface Segregation

Do not make one huge `AppServices` trait.

Use small ports:

```text
VaultRepository
NoteIndex
AttachmentStore
Clipboard
ShareServer
BackupService
FileWatcher
UriLauncher
```

### Dependency Inversion

Application use cases depend on traits, not concrete libraries.

```text
SaveNoteUseCase<VaultRepository, NoteIndex>
PasteImageUseCase<Clipboard, AttachmentStore, VaultRepository>
ShareNoteUseCase<MarkdownRenderer, ShareServer>
```

## 5. DRY Policy

Avoid duplication in:

```text
path normalization
Markdown image parsing
status state formatting
note title extraction
frontmatter parsing
share HTML rendering
attachment path resolution
keyboard binding definitions
```

Centralize these:

```text
PathPolicy
MarkdownLinkExtractor
StatusModel
TitleExtractor
AttachmentResolver
KeymapRegistry
```

Do not duplicate UI state strings across drawers/status/modals.

Use one source:

```rust
enum SyncStatus {
    Clean,
    Dirty,
    Saving,
    ChangedExternally,
    Conflict,
    Error(String),
}
```

Render it differently depending on available width.

## 6. Data Model

### 6.1 Filesystem Source of Truth

Markdown files are authoritative.

SQLite is cache/index/session state.

```text
Vault files:
  source of truth

.onote/index.sqlite:
  derived index
  search cache
  session state
  recent files
  UI metadata
```

### 6.2 SQLite Tables

```sql
CREATE TABLE notes (
  path TEXT PRIMARY KEY,
  title TEXT NOT NULL,
  content_hash TEXT NOT NULL,
  modified_at INTEGER NOT NULL,
  indexed_at INTEGER NOT NULL,
  pinned INTEGER NOT NULL DEFAULT 0
);

CREATE VIRTUAL TABLE notes_fts USING fts5(
  path,
  title,
  body
);

CREATE TABLE attachments (
  path TEXT PRIMARY KEY,
  mime TEXT,
  width INTEGER,
  height INTEGER,
  size_bytes INTEGER,
  created_at INTEGER
);

CREATE TABLE note_attachments (
  note_path TEXT NOT NULL,
  attachment_path TEXT NOT NULL,
  PRIMARY KEY (note_path, attachment_path)
);

CREATE TABLE sessions (
  session_id TEXT PRIMARY KEY,
  note_path TEXT NOT NULL,
  pid INTEGER NOT NULL,
  mode TEXT NOT NULL,
  opened_at INTEGER NOT NULL,
  last_seen_at INTEGER NOT NULL
);

CREATE TABLE recent_notes (
  path TEXT PRIMARY KEY,
  opened_at INTEGER NOT NULL
);
```

## 7. Conflict Handling

Every editor buffer records:

```text
opened_hash
current_disk_hash
buffer_hash
last_saved_hash
```

Save algorithm:

```text
if disk_hash == opened_hash:
  write file
  update opened_hash
else:
  enter ChangedExternally state
  offer reload / merge / conflict copy / overwrite
```

Default action:

```text
reload or conflict copy
```

Never default to overwrite.

## 8. Commands

```bash
onote
onote scratch
onote today
onote new "robot idea"
onote open "robot"
onote share
onote backup
onote gui
onote img paste
onote copy --md
onote copy --html
onote copy --rich
```

## 9. MVP Implementation Order

### Spike 1: Vault Core

```text
config
vault path
list notes
open note
edit note
save note
file watcher
external change detection
```

### Spike 2: TUI Minimal Editor

```text
single-pane editor
top path line
bottom status line
mouse scroll
note drawer
fuzzy open
```

### Spike 3: Images

```text
parse image links
display [image: filename]
paste clipboard image
save into Attachments/
insert Markdown image link
preview overlay
copy image
delete image token
```

### Spike 4: Share

```text
render Markdown to HTML
serve current note
resolve images
show URL
show QR
copy URL
stop server
```

### Spike 5: Backup

```text
git status
commit
push
pull --ff-only
conflict warning
```

### Spike 6: Polish

```text
small terminal layout
drawer resize
session coordination
daily note
Obsidian URI open
```

## 10. Non-goals

Do not implement these in MVP:

```text
real-time remote collaboration
graph view
full Obsidian plugin compatibility
rich-text editing
WYSIWYG Markdown
database-backed proprietary note format
mobile client
web editor
AI features
```

## 11. Final Engineering Position

The correct architecture is:

```text
Obsidian-compatible vault as source of truth
Rust single binary
Ratatui/crossterm TUI
Comrak Markdown
SQLite FTS index
Notify file watcher
Ratatui-image preview
Axum local share server
qr2term QR output
Git CLI backup
macOS clipboard adapter
```

The product should feel like a terminal-native scratchpad, not a terminal clone of Obsidian.
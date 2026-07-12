//! End-to-end integration: real filesystem + SQLite + comrak adapters wired into
//! the `App`, with no-op fakes for clipboard/share/watch/launcher/backup (those
//! need a GUI/network session). Exercises the §7 save/conflict flow.

use std::path::PathBuf;
use std::sync::mpsc;
use std::sync::Arc;

use onote::application::ops::SaveOutcome;
use onote::application::{App, AppDeps};
use onote::config::Config;
use onote::domain::attachment::{AttachmentReference, ImageData, LinkStyle};
use onote::domain::errors::{AttachmentError, BackupError, ClipboardError, ShareError, VaultError};
use onote::domain::note::MarkdownBody;
use onote::infra::{
    ComrakMarkdown, FilesystemAttachmentStore, FilesystemVault, SqliteNoteIndex, SystemClock,
};
use onote::ports::{
    AttachmentStore, BackupService, Clipboard, Clock, FileWatcher, ImageRenderer, LoadedImage,
    MarkdownLinkExtractor, MarkdownRenderer, ShareServer, UriLauncher,
};

// ── no-op fakes for session-bound services ─────────────────────────────────────

struct NoopClipboard;
impl Clipboard for NoopClipboard {
    fn read_text(&self) -> Result<Option<String>, ClipboardError> {
        Ok(None)
    }
    fn read_image(&self) -> Result<Option<ImageData>, ClipboardError> {
        Ok(None)
    }
    fn write_text(&self, _text: &str) -> Result<(), ClipboardError> {
        Ok(())
    }
    fn write_html(&self, _html: &str, _plain: &str) -> Result<(), ClipboardError> {
        Ok(())
    }
    fn write_image(&self, _image: &ImageData) -> Result<(), ClipboardError> {
        Ok(())
    }
}

/// Clipboard fake that yields a fixed image from `read_image` (used to drive the
/// Spike-3 paste-image flow end-to-end — `NoopClipboard` returns `None` there).
/// Text/HTML/image writes are accepted and discarded: the round-trip asserts on
/// disk + token, not on clipboard state.
struct FakeClipboard {
    image: Option<ImageData>,
}
impl FakeClipboard {
    fn with_image(image: ImageData) -> Self {
        Self { image: Some(image) }
    }
}
impl Clipboard for FakeClipboard {
    fn read_text(&self) -> Result<Option<String>, ClipboardError> {
        Ok(None)
    }
    fn read_image(&self) -> Result<Option<ImageData>, ClipboardError> {
        Ok(self.image.clone())
    }
    fn write_text(&self, _text: &str) -> Result<(), ClipboardError> {
        Ok(())
    }
    fn write_html(&self, _html: &str, _plain: &str) -> Result<(), ClipboardError> {
        Ok(())
    }
    fn write_image(&self, _image: &ImageData) -> Result<(), ClipboardError> {
        Ok(())
    }
}

/// Clipboard fake that records every `write_text` call into a shared cell.
/// Backs the P4 copy/cut integration test: it proves the editor's
/// `Copy`/`Cut` actions route the selection through `App::copy_text` → the
/// `Clipboard` port → this sink (the App-free unit tests cover the dispatch
/// logic; this covers the real wiring). `Mutex` (not `RefCell`) because the
/// port is `&self` and the App holds the clipboard behind an `Arc<dyn
/// Clipboard>` shared across calls.
struct CapturingClipboard {
    written: std::sync::Mutex<Vec<String>>,
}
impl CapturingClipboard {
    fn new() -> Self {
        Self {
            written: std::sync::Mutex::new(Vec::new()),
        }
    }
    fn writes(&self) -> Vec<String> {
        self.written.lock().expect("capture lock poisoned").clone()
    }
}
impl Clipboard for CapturingClipboard {
    fn read_text(&self) -> Result<Option<String>, ClipboardError> {
        Ok(None)
    }
    fn read_image(&self) -> Result<Option<ImageData>, ClipboardError> {
        Ok(None)
    }
    fn write_text(&self, text: &str) -> Result<(), ClipboardError> {
        self.written
            .lock()
            .expect("capture lock poisoned")
            .push(text.to_string());
        Ok(())
    }
    fn write_html(&self, _html: &str, _plain: &str) -> Result<(), ClipboardError> {
        Ok(())
    }
    fn write_image(&self, _image: &ImageData) -> Result<(), ClipboardError> {
        Ok(())
    }
}

struct NoopShare;
impl ShareServer for NoopShare {
    fn start(
        &self,
        _snapshot: onote::domain::share::ShareSnapshot,
        _policy: onote::domain::share::SharePolicy,
    ) -> Result<onote::domain::share::ShareSession, ShareError> {
        Err(ShareError::Server("share disabled in test".into()))
    }
    fn stop(&self) -> Result<(), ShareError> {
        Ok(())
    }
    fn local_url(&self) -> Option<String> {
        None
    }
}

struct NoopWatcher;
impl FileWatcher for NoopWatcher {
    fn watch(
        &self,
        _paths: &[PathBuf],
    ) -> Result<mpsc::Receiver<onote::domain::session::ExternalChange>, VaultError> {
        Ok(mpsc::channel().1)
    }
}

struct NoopLauncher;
impl UriLauncher for NoopLauncher {
    fn open(&self, _note_path: &onote::domain::vault::RelativeNotePath) -> Result<(), VaultError> {
        Err(VaultError::Io(std::io::Error::other(
            "launcher disabled in test",
        )))
    }
}

struct NoopBackup;
impl BackupService for NoopBackup {
    fn status(&self) -> Result<onote::domain::backup::BackupState, BackupError> {
        Ok(onote::domain::backup::BackupState::default())
    }
    fn commit(
        &self,
        _message: onote::domain::backup::BackupMessage,
    ) -> Result<onote::domain::backup::BackupReport, BackupError> {
        Ok(onote::domain::backup::BackupReport::default())
    }
    fn push(&self) -> Result<onote::domain::backup::BackupReport, BackupError> {
        Ok(onote::domain::backup::BackupReport::default())
    }
    fn pull_ff_only(&self) -> Result<onote::domain::backup::BackupReport, BackupError> {
        Ok(onote::domain::backup::BackupReport::default())
    }
}

struct NoopImage;
impl ImageRenderer for NoopImage {
    fn load(&self, _abs: &std::path::Path) -> Result<LoadedImage, AttachmentError> {
        Err(AttachmentError::NotFound("disabled in test".into()))
    }
}

// (attachment store fakes unused here; real adapter exercised in unit tests.)

#[allow(dead_code)]
fn _unused_traits(
    _a: &dyn AttachmentStore,
    _b: &dyn MarkdownLinkExtractor,
    _c: &dyn MarkdownRenderer,
    _d: &dyn Clock,
) {
}

fn build_app_with_clipboard(dir: &std::path::Path, clipboard: Arc<dyn Clipboard>) -> App {
    let config = Config {
        vault: dir.to_path_buf(),
        default_note: "Scratch.md".into(),
        attachment_dir: "Attachments".into(),
        daily_dir: "Daily".into(),
        image_link_style: LinkStyle::Markdown,
        open_gui_command: "obsidian://open?vault=V&file={file}".into(),
        backup_remote: "origin".into(),
        share_port: 7478,
        share_allow_lan: false,
        keymap: Default::default(),
        layout: Default::default(),
        theme: "latte".into(),
    };
    let index_db = dir.join(".onote").join("index.sqlite");
    std::fs::create_dir_all(dir.join(".onote")).unwrap();

    let vault = Arc::new(FilesystemVault::new(dir.to_path_buf()));
    let index = Arc::new(SqliteNoteIndex::new(&index_db).unwrap());
    let renderer: Arc<dyn MarkdownRenderer> = Arc::new(ComrakMarkdown::new());
    let link_extractor: Arc<dyn MarkdownLinkExtractor> = Arc::new(ComrakMarkdown::new());
    let clock: Arc<dyn Clock> = Arc::new(SystemClock);

    let attachments = Arc::new(FilesystemAttachmentStore::new(
        dir.to_path_buf(),
        "Attachments".into(),
        link_extractor.clone(),
        clock.clone(),
    ));

    App::new(
        config,
        AppDeps {
            vault,
            index,
            attachments,
            clipboard,
            markdown: renderer,
            link_extractor,
            image_renderer: Arc::new(NoopImage),
            backup: Some(Arc::new(NoopBackup)),
            watcher: Some(Arc::new(NoopWatcher)),
            launcher: Some(Arc::new(NoopLauncher)),
            share_server: Some(Arc::new(NoopShare)),
            clock,
        },
    )
}

fn build_app(dir: &std::path::Path) -> App {
    build_app_with_clipboard(dir, Arc::new(NoopClipboard))
}

#[test]
fn open_default_creates_then_saves_roundtrip() {
    let tmp = tempfile::tempdir().unwrap();
    let app = build_app(tmp.path());

    // First open bootstraps Scratch.md.
    let doc = app.open_default().unwrap();
    let original = doc.body.as_str().to_string();
    assert!(original.contains("Scratch"));

    // Edit + save → Written.
    let edited = format!("{original}\n\n## a new idea\n");
    match app.save_current(&edited).unwrap() {
        SaveOutcome::Written(_) => {}
        other => panic!("expected Written, got {other:?}"),
    }

    // Re-open from disk → persisted.
    let reopened = app.open_default().unwrap();
    assert!(reopened.body.as_str().contains("a new idea"));
}

#[test]
fn external_change_surfaces_conflict_not_overwrite() {
    let tmp = tempfile::tempdir().unwrap();
    let app = build_app(tmp.path());

    let doc = app.open_default().unwrap();
    let original = doc.body.as_str().to_string();

    // Simulate an external edit (Obsidian / another terminal) on disk.
    let scratch = tmp.path().join("Scratch.md");
    std::fs::write(&scratch, "# FOREIGN EDIT\n").unwrap();

    // Saving our (now-stale) buffer must NOT overwrite — §7.
    match app.save_current(&original).unwrap() {
        SaveOutcome::Conflict { .. } => {}
        other => panic!("expected Conflict, got {other:?}"),
    }
    // Disk still has the foreign content.
    assert_eq!(
        std::fs::read_to_string(&scratch).unwrap(),
        "# FOREIGN EDIT\n"
    );
}

#[test]
fn write_conflict_copy_preserves_original_and_writes_sibling() {
    // §7 conflict-copy resolution has zero coverage — only detection does. This
    // exercises the data-safety-critical invariant: the ORIGINAL note file on
    // disk is never touched, and the buffer lands in a `*.conflict-*.md` sibling.
    let tmp = tempfile::tempdir().unwrap();
    let app = build_app(tmp.path());

    // Mirror external_change_surfaces_conflict_not_overwrite's conflict setup:
    // open the default note, then mutate disk so our buffer is stale.
    let doc = app.open_default().unwrap();
    let buffer_body = format!("{}\n## my unsaved edit\n", doc.body.as_str());
    let scratch = tmp.path().join("Scratch.md");
    std::fs::write(&scratch, "# FOREIGN EDIT\n").unwrap();
    let pre_call_disk = std::fs::read(&scratch).unwrap();

    // Resolution: write the stale buffer to a sibling, leave the original alone.
    let copy_path = app.write_conflict_copy(&buffer_body).unwrap();

    // ORIGINAL note file on disk is byte-for-byte unchanged (data safety).
    assert_eq!(
        std::fs::read(&scratch).unwrap(),
        pre_call_disk,
        "conflict-copy must never modify the original note file"
    );

    // A sibling `*.conflict-*.md` exists and holds the buffer body verbatim.
    assert!(
        copy_path.as_str().contains(".conflict-"),
        "expected a conflict-*.md sibling path, got {}",
        copy_path.as_str()
    );
    let copy_abs = tmp.path().join(copy_path.as_str());
    assert!(copy_abs.exists(), "conflict copy should exist on disk");
    assert_eq!(
        std::fs::read_to_string(&copy_abs).unwrap(),
        buffer_body,
        "conflict copy must contain the buffer body verbatim"
    );
}

#[test]
fn search_and_fuzzy_find_created_notes() {
    let tmp = tempfile::tempdir().unwrap();
    let app = build_app(tmp.path());

    let robot = app.create_note("Robot Idea", None).unwrap();
    let _ = app.open_note(&robot).unwrap();
    // Keep the H1 so the derived title stays "Robot Idea" after save.
    app.save_current("# Robot Idea\n\nbuild a robot arm with servos")
        .unwrap();

    let hits = app.search("servos").unwrap();
    assert!(hits.iter().any(|h| h.path.as_str() == robot.as_str()));

    let fuzzy = app.fuzzy("rob").unwrap();
    assert!(fuzzy.iter().any(|s| s.title == "Robot Idea"));
}

#[test]
fn daily_note_path_is_dated() {
    let tmp = tempfile::tempdir().unwrap();
    let app = build_app(tmp.path());
    let path = app.daily_note_path().unwrap();
    assert!(path.as_str().starts_with("Daily/"));
    assert!(path.as_str().ends_with(".md"));
    // Opening it bootstraps the file.
    let _doc = app.open_daily().unwrap();
    assert!(tmp.path().join(path.as_str()).exists());
}

#[test]
fn link_extraction_round_trips_through_markdown() {
    let m = ComrakMarkdown::new();
    let body = MarkdownBody("![](Attachments/2026/07/x.png)".into());
    let refs = m.extract_attachment_links(&body);
    assert_eq!(refs.len(), 1);
    assert_eq!(refs[0].target.as_str(), "Attachments/2026/07/x.png");
    let _html = m.render_html(&body); // smoke
    let _ = AttachmentReference::render_token(LinkStyle::Markdown, &refs[0].target);
}

#[test]
fn paste_image_roundtrip_persists_attachment_and_resolves() {
    // Spike-3 image flow end-to-end (CLAUDE.md §3.1): clipboard → save_image →
    // token in buffer → save note → reopen from disk → extracted link points at
    // the file that actually exists under Attachments/<YYYY>/<MM>/. This is the
    // only test that exercises App::paste_image (NoopClipboard makes it a no-op
    // everywhere else).
    let tmp = tempfile::tempdir().unwrap();
    // The store writes `bytes` verbatim (no decode), so a minimal PNG header is
    // sufficient — mirrors the attachment_store unit test's image fixture.
    let image = ImageData {
        bytes: vec![0x89, b'P', b'N', b'G'],
        mime: "image/png".into(),
        width: 1,
        height: 1,
    };
    let app = build_app_with_clipboard(tmp.path(), Arc::new(FakeClipboard::with_image(image)));

    // Open the default note so there is a current note to paste into.
    let doc = app.open_default().unwrap();
    let original = doc.body.as_str().to_string();

    // Paste → returns a non-empty token + a vault-relative attachment path.
    let pasted = app
        .paste_image()
        .expect("paste_image must not error")
        .expect("paste_image must return Some when the clipboard holds an image");
    assert!(
        !pasted.token.is_empty(),
        "paste must yield a non-empty insertion token"
    );
    // build_app sets LinkStyle::Markdown → token shape is `![](path)`, NOT the
    // Obsidian `![[path]]` embed form.
    assert!(
        pasted.token.starts_with("![](") && pasted.token.ends_with(')'),
        "Markdown link-style token must be `![](...)`, got: {}",
        pasted.token
    );

    // §3.1 deterministic naming: Attachments/<YYYY>/<MM>/img-<ts>.<ext>. The
    // harness uses the real SystemClock, so the date segments are derived from
    // the returned path rather than hard-coded.
    let att_rel = pasted.attachment.path.as_str();
    let segs: Vec<&str> = att_rel.split('/').collect();
    assert_eq!(
        segs.len(),
        4,
        "expected Attachments/<YYYY>/<MM>/<file>, got: {att_rel}"
    );
    assert_eq!(segs[0], "Attachments");
    assert!(
        segs[1].len() == 4 && segs[1].chars().all(|c| c.is_ascii_digit()),
        "year segment must be 4 digits, got: {}",
        segs[1]
    );
    assert!(
        segs[2].len() == 2 && segs[2].chars().all(|c| c.is_ascii_digit()),
        "month segment must be 2 digits, got: {}",
        segs[2]
    );

    // The image file physically exists on disk at the reported path.
    let att_abs = tmp.path().join(pasted.attachment.path.as_path());
    assert!(
        att_abs.exists(),
        "pasted image file must exist on disk at {}",
        att_abs.display()
    );

    // Drive a save so the token is persisted into the note body (paste returns
    // the token; it does not edit the buffer itself).
    let body = format!("{original}\n\n{}\n", pasted.token);
    match app.save_current(&body).unwrap() {
        SaveOutcome::Written(_) => {}
        other => panic!("expected Written, got {other:?}"),
    }

    // Reopen from disk: the body still carries the token, and the link
    // extractor resolves it back to the same vault-relative attachment path.
    let reopened = app.open_default().unwrap();
    let reopened_body = reopened.body.as_str();
    assert!(
        reopened_body.contains(&pasted.token),
        "persisted note must still contain the image token after reopen"
    );
    let refs = app.attachment_links(reopened_body);
    assert_eq!(
        refs.len(),
        1,
        "exactly one attachment reference must be extracted after reopen"
    );
    assert_eq!(
        refs[0].target.as_str(),
        pasted.attachment.path.as_str(),
        "extracted reference must target the saved attachment"
    );
}

#[cfg(unix)]
#[test]
fn image_preview_rejects_symlink_that_escapes_vault() {
    // CLAUDE.md §3.1 "must not escape the vault root". RelativeNotePath blocks
    // `..` by construction; the real gap is a SYMLINK planted inside the vault
    // (e.g. via a tampered `git pull`) that points at a file outside.
    // image_preview delegates its confinement to RelativeNotePath::resolve_within
    // (the single vault-escape guard), so this also covers that delegation.
    use std::os::unix::fs::symlink;

    use onote::domain::vault::RelativeNotePath;

    let dir = tempfile::tempdir().unwrap();
    // A genuinely OUTSIDE-the-vault target: a separate tempdir, NOT a subdir of
    // the vault (a sibling file inside the vault would not actually escape).
    let outside_dir = tempfile::tempdir().unwrap();
    let outside = outside_dir.path().join("secret.txt");
    std::fs::write(&outside, "secret").unwrap();

    let app = build_app(dir.path());

    // Plant a symlink inside the vault's attachment tree that escapes to the
    // outside file. The exact subpath under Attachments/ is irrelevant to
    // RelativeNotePath::resolve_within (canonicalize + starts_with on the
    // resolved target), so a flat layout is sufficient.
    let attach_dir = dir.path().join("Attachments");
    std::fs::create_dir_all(&attach_dir).unwrap();
    let escape = attach_dir.join("escape.png");
    symlink(&outside, &escape).unwrap();
    assert!(
        escape.exists(),
        "test setup: symlink must resolve to its target"
    );

    let rel = RelativeNotePath::new("Attachments/escape.png")
        .expect("in-vault relative path must construct");

    // The preview must refuse to read through the escaped symlink.
    let err = app
        .image_preview(&rel)
        .expect_err("image_preview must reject a symlink that escapes the vault root");
    let msg = format!("{err:#}");
    assert!(
        msg.contains("escapes vault root") || msg.to_lowercase().contains("escape"),
        "error should report a vault escape, got: {msg}",
    );
}

/// `create_note` must index the note so it's searchable WITHOUT a follow-up
/// `open_note` (round-7 Finding 3: the prior implementation only indexed on
/// open, masking the gap in every other test by opening first).
#[test]
fn create_note_is_searchable_without_open() {
    let tmp = tempfile::tempdir().unwrap();
    let app = build_app(tmp.path());

    let path = app.create_note("servos spec", None).expect("create note");
    // No open_note in between — the index must already know about it.
    let hits = app.search("servos").expect("search");
    assert!(
        hits.iter().any(|h| h.path == path),
        "freshly-created note must be searchable without an open; got {:?}",
        hits.iter().map(|h| h.path.as_str()).collect::<Vec<_>>()
    );
}

/// `sync_index_for` is the single primitive that keeps the index tied to disk
/// for an externally-changed path — the fix for round-7 Finding 4 (external
/// edits/deletes from Obsidian / `git pull` / another terminal leaving stale or
/// ghost rows). Exercises both the update and delete paths directly, without the
/// file watcher (whose events aren't deterministically reproducible in a unit test).
#[test]
fn sync_index_for_reflects_external_edit_then_delete() {
    let tmp = tempfile::tempdir().unwrap();
    let app = build_app(tmp.path());

    // Seed + index a note (create_note indexes it — see test above).
    let path = app.create_note("draft note", None).expect("create note");
    assert!(!app.search("draft").expect("search").is_empty());

    // External edit on disk (simulating Obsidian / git pull): add a unique term
    // the index does NOT yet know. sync_index_for must pick it up.
    let abs = tmp.path().join(path.as_str());
    std::fs::write(&abs, "# draft note\n\nUNIQUE_EXTERNAL_TOKEN\n").unwrap();
    app.sync_index_for(&path);
    assert!(
        app.search("UNIQUE_EXTERNAL_TOKEN")
            .expect("search")
            .iter()
            .any(|h| h.path == path),
        "sync_index_for must reflect an external edit in search"
    );

    // External delete on disk: sync_index_for must evict the ghost row so search
    // no longer returns a hit for a file that no longer exists.
    std::fs::remove_file(&abs).unwrap();
    app.sync_index_for(&path);
    assert!(
        app.search("draft")
            .expect("search")
            .iter()
            .all(|h| h.path != path),
        "sync_index_for must remove a deleted note from the index (no ghost hits)"
    );
}

/// Round-8 CRITICAL #3 regression: an existing Obsidian vault's notes must be
/// searchable immediately on startup. The index is a derived cache (§6) that
/// starts empty on a fresh DB — without `reindex_all`, `onote open`/`gui`/Ctrl+O/
/// FTS find nothing until each note is opened once. Seeds notes directly on disk
/// (bypassing the app, simulating a pre-existing vault onote has never indexed),
/// then asserts `reindex_all` discovers them.
#[test]
fn reindex_all_makes_preexisting_disk_notes_searchable() {
    let tmp = tempfile::tempdir().unwrap();
    let app = build_app(tmp.path());

    // Write notes directly to disk — the index has never seen them.
    std::fs::write(
        tmp.path().join("Legacy.md"),
        "# Legacy Note\n\nancient wisdom\n",
    )
    .unwrap();
    std::fs::create_dir_all(tmp.path().join("Notes")).unwrap();
    std::fs::write(
        tmp.path().join("Notes/Old.md"),
        "# Old Idea\n\nvintage thoughts\n",
    )
    .unwrap();

    // Before reindex: the derived cache is empty, so neither note is found.
    assert!(
        app.search("ancient").expect("search").is_empty(),
        "fresh index must not contain pre-existing disk notes"
    );

    // reindex_all walks the vault and rebuilds the cache from disk.
    app.reindex_all().expect("reindex");

    // Both pre-existing notes are now FTS-searchable + title-fuzzable.
    assert!(
        app.search("ancient")
            .expect("search")
            .iter()
            .any(|h| h.path.as_str() == "Legacy.md"),
        "reindex_all must index a root-level pre-existing note"
    );
    assert!(
        app.search("vintage")
            .expect("search")
            .iter()
            .any(|h| h.path.as_str() == "Notes/Old.md"),
        "reindex_all must index a nested pre-existing note"
    );
    assert!(
        app.fuzzy("Legacy")
            .expect("fuzzy")
            .iter()
            .any(|s| s.title == "Legacy Note"),
        "reindex_all must populate title fuzzy search"
    );

    // And a note deleted from disk since the last index must be evicted.
    std::fs::remove_file(tmp.path().join("Legacy.md")).unwrap();
    app.reindex_all().expect("reindex after delete");
    assert!(
        app.search("ancient")
            .expect("search")
            .iter()
            .all(|h| h.path.as_str() != "Legacy.md"),
        "reindex_all must evict a note deleted from disk"
    );
}

/// P4 plan §5 integration surface: the `Copy`/`Cut` actions route the selection
/// through `App::copy_text` → the `Clipboard` port. The dispatch logic (select
/// → text → copy → delete) is unit-tested App-free in `ui::tui`; this test pins
/// the WIRING — that `copy_text` actually reaches the `Clipboard` impl the App
/// was built with. `CapturingClipboard` records the write so we assert the
/// exact substring landed in the OS clipboard, not a no-op.
#[test]
fn copy_text_routes_selection_through_clipboard_port() {
    let tmp = tempfile::tempdir().unwrap();
    let clip = Arc::new(CapturingClipboard::new());
    let app = build_app_with_clipboard(tmp.path(), Arc::clone(&clip) as Arc<dyn Clipboard>);

    // The editor's Copy/Cut call `app.copy_text(&selected_text)`. We can't reach
    // the private editor from here, so drive the App boundary directly: this is
    // the exact line the Copy action runs (dispatch_edit → copy_selection →
    // app.copy_text). Verifying it captures proves the port wiring the actions
    // depend on.
    app.copy_text("selected substring")
        .expect("copy_text must not error");
    assert_eq!(
        clip.writes(),
        vec!["selected substring".to_string()],
        "copy_text must write through the Clipboard port the App was built with"
    );

    // A second call appends (the port is stateless per-write), proving the
    // capture isn't a one-shot artifact.
    app.copy_text("more").unwrap();
    assert_eq!(
        clip.writes(),
        vec!["selected substring".to_string(), "more".to_string()],
        "the Clipboard port must record each write, not just the first"
    );
}

// ── Spike 7 P7.4: Explorer file ops (create_folder / rename / delete) ─────────
//
// End-to-end through the real FilesystemVault + SqliteNoteIndex adapters. The
// infra unit tests pin the vault-escape / never-overwrite guards; these cover
// the app-layer concerns the UI depends on: index sync (§6), and the open-note
// follow-through when a rename/move relocates the editor's note.

#[test]
fn create_folder_appears_in_tree() {
    use onote::domain::vault::{EntryKind, RelativeNotePath};
    let tmp = tempfile::tempdir().unwrap();
    let app = build_app(tmp.path());

    let folder = RelativeNotePath::new("Inbox").unwrap();
    app.create_folder(&folder).unwrap();
    let folder_names: Vec<String> = app
        .list_vault_tree()
        .unwrap()
        .iter()
        .filter(|e| e.kind == EntryKind::Folder)
        .map(|e| e.name.clone())
        .collect();
    assert!(
        folder_names.contains(&"Inbox".to_string()),
        "created folder should appear in the vault tree; got {folder_names:?}"
    );
    assert!(tmp.path().join("Inbox").is_dir());
}

#[test]
fn rename_entry_note_moves_file_follows_current_and_reindexes() {
    use onote::domain::vault::{EntryKind, RelativeNotePath};
    let tmp = tempfile::tempdir().unwrap();
    let app = build_app(tmp.path());

    let path = app.create_note("Robot Idea", None).unwrap();
    app.open_note(&path).unwrap();
    assert_eq!(app.current_note().unwrap().path.as_str(), "robot-idea.md");

    let from = RelativeNotePath::new("robot-idea.md").unwrap();
    let out = app
        .rename_entry(&from, "Cyber Bot", EntryKind::Note)
        .unwrap();
    assert_eq!(out.new_path.as_str(), "cyber-bot.md");

    // File moved on disk.
    assert!(!tmp.path().join("robot-idea.md").exists());
    assert!(tmp.path().join("cyber-bot.md").exists());

    // The open note followed (relocated_current + app.current both report it).
    assert_eq!(
        out.relocated_current
            .as_ref()
            .map(|p| p.as_str())
            .as_deref(),
        Some("cyber-bot.md"),
    );
    assert_eq!(app.current_note().unwrap().path.as_str(), "cyber-bot.md");

    // Index tracks the NEW path (body still says "Robot Idea") and dropped the old.
    let hits = app.search("Robot").unwrap();
    assert!(
        hits.iter().any(|h| h.path.as_str() == "cyber-bot.md"),
        "renamed note searchable under its new path"
    );
    assert!(
        hits.iter().all(|h| h.path.as_str() != "robot-idea.md"),
        "old path no longer indexed"
    );
}

#[test]
fn rename_entry_folder_remaps_nested_current() {
    use onote::domain::vault::{EntryKind, RelativeNotePath};
    let tmp = tempfile::tempdir().unwrap();
    let app = build_app(tmp.path());

    let folder = RelativeNotePath::new("Projects").unwrap();
    app.create_folder(&folder).unwrap();
    let path = app.create_note("Alpha", Some(&folder)).unwrap();
    app.open_note(&path).unwrap();
    assert_eq!(
        app.current_note().unwrap().path.as_str(),
        "Projects/alpha.md"
    );

    // Renaming the ANCESTOR folder must remap the nested open note's path.
    let from = RelativeNotePath::new("Projects").unwrap();
    let out = app
        .rename_entry(&from, "Archive", EntryKind::Folder)
        .unwrap();
    assert_eq!(out.new_path.as_str(), "Archive");
    assert_eq!(
        out.relocated_current
            .as_ref()
            .map(|p| p.as_str())
            .as_deref(),
        Some("Archive/alpha.md"),
    );
    assert_eq!(
        app.current_note().unwrap().path.as_str(),
        "Archive/alpha.md",
    );
    assert!(tmp.path().join("Archive/alpha.md").exists());
    assert!(!tmp.path().join("Projects").exists());
}

#[test]
fn rename_entry_refuses_overwrite_and_preserves_source() {
    use onote::domain::vault::{EntryKind, RelativeNotePath};
    let tmp = tempfile::tempdir().unwrap();
    let app = build_app(tmp.path());

    app.create_note("First", None).unwrap(); // first.md
    app.create_note("Second", None).unwrap(); // second.md
    let from = RelativeNotePath::new("first.md").unwrap();
    // Rename to "Second" → target second.md already exists → §7 refuse.
    let res = app.rename_entry(&from, "Second", EntryKind::Note);
    assert!(
        res.is_err(),
        "rename onto a busy target must error (§7 never overwrite)"
    );
    // Neither file was clobbered.
    assert!(tmp.path().join("first.md").exists());
    assert!(tmp.path().join("second.md").exists());
}

#[test]
fn delete_entry_note_clears_current_and_evicts_index() {
    use onote::domain::vault::{EntryKind, RelativeNotePath};
    let tmp = tempfile::tempdir().unwrap();
    let app = build_app(tmp.path());

    let path = app.create_note("Ghost Note", None).unwrap();
    app.open_note(&path).unwrap();
    assert!(
        !app.search("Ghost").unwrap().is_empty(),
        "note indexed before delete"
    );

    let rel = RelativeNotePath::new("ghost-note.md").unwrap();
    let deleted_current = app.delete_entry(&rel, EntryKind::Note).unwrap();
    assert!(deleted_current, "the open note was the one deleted");
    assert!(
        app.current_note().is_none(),
        "current cleared after deleting the open note"
    );
    assert!(!tmp.path().join("ghost-note.md").exists());
    assert!(
        app.search("Ghost").unwrap().is_empty(),
        "deleted note no longer searchable"
    );
}

#[test]
fn delete_entry_folder_removes_subtree_and_reindexes() {
    use onote::domain::vault::{EntryKind, RelativeNotePath};
    let tmp = tempfile::tempdir().unwrap();
    let app = build_app(tmp.path());

    let folder = RelativeNotePath::new("Tmp").unwrap();
    app.create_folder(&folder).unwrap();
    app.create_note("One", Some(&folder)).unwrap();
    app.create_note("Two", Some(&folder)).unwrap();
    assert_eq!(app.search("One").unwrap().len(), 1);
    assert_eq!(app.search("Two").unwrap().len(), 1);

    let deleted_current = app.delete_entry(&folder, EntryKind::Folder).unwrap();
    assert!(
        !deleted_current,
        "the open note was not under the deleted folder"
    );
    assert!(!tmp.path().join("Tmp").exists(), "folder subtree removed");
    // Reindex evicted both nested notes.
    assert!(app.search("One").unwrap().is_empty());
    assert!(app.search("Two").unwrap().is_empty());
}

/// P7.4 regression: deleting a FOLDER that contains the OPEN note must report
/// `deleted_current = true` (not just an exact path match) and clear current.
/// Without nested detection the editor keeps a `state.path` at a now-deleted
/// file and the next save silently re-creates it (§7 baseline corruption). This
/// is the delete-side parity of `relocate_current`'s nested handling on rename.
#[test]
fn delete_entry_folder_containing_current_is_detected() {
    use onote::domain::vault::{EntryKind, RelativeNotePath};
    let tmp = tempfile::tempdir().unwrap();
    let app = build_app(tmp.path());

    // Seed the default note (so the UI's post-delete `open_default` has somewhere
    // to land) and a folder holding a nested note we then open.
    app.open_default().unwrap();
    let folder = RelativeNotePath::new("Tmp").unwrap();
    app.create_folder(&folder).unwrap();
    let nested = app.create_note("Nested", Some(&folder)).unwrap();
    app.open_note(&nested).unwrap();
    assert_eq!(app.current_note().unwrap().path.as_str(), "Tmp/nested.md");

    let deleted_current = app.delete_entry(&folder, EntryKind::Folder).unwrap();
    assert!(
        deleted_current,
        "open note nested under the deleted folder must count as deleted"
    );
    assert!(
        app.current_note().is_none(),
        "current cleared after the open note's file was removed with the folder"
    );
    assert!(!tmp.path().join("Tmp").exists(), "folder subtree removed");
    // Reindex evicted the nested note (no ghost hit).
    assert!(app.search("Nested").unwrap().is_empty());
}

// ── Spike 8: note-link resolution (`[[wikilink]]` / md-link → note) ──────────

use onote::application::ops::LinkResolution;
use onote::domain::vault::RelativeNotePath;

/// `[[Robot]]` with a single note titled "Robot" resolves to that note's path.
#[test]
fn resolve_note_link_finds_unique_title() {
    let tmp = tempfile::tempdir().unwrap();
    let app = build_app(tmp.path());
    let created = app.create_note("Robot", None).unwrap();

    match app.resolve_note_link("Robot").unwrap() {
        LinkResolution::Found(path) => assert_eq!(path, created),
        other => panic!("expected Found, got {other:?}"),
    }
    // Case-insensitive + trimmed.
    assert!(matches!(
        app.resolve_note_link("  robot ").unwrap(),
        LinkResolution::Found(_)
    ));
}

/// `[[Robot]]` is NOT silently opened for a near-match titled "Robotics" — the
/// resolver is title-exact (Obsidian semantics), so it returns NotFound and the
/// caller can fall back to a seeded fuzzy picker.
#[test]
fn resolve_note_link_rejects_fuzzy_near_match() {
    let tmp = tempfile::tempdir().unwrap();
    let app = build_app(tmp.path());
    app.create_note("Robotics", None).unwrap();

    assert_eq!(
        app.resolve_note_link("Robot").unwrap(),
        LinkResolution::NotFound
    );
}

/// Two notes sharing the exact title in different folders are ambiguous — the
/// caller must disambiguate (§8).
#[test]
fn resolve_note_link_ambiguous_on_duplicate_title() {
    let tmp = tempfile::tempdir().unwrap();
    let app = build_app(tmp.path());
    let a = RelativeNotePath::new("a").unwrap();
    let b = RelativeNotePath::new("b").unwrap();
    app.create_folder(&a).unwrap();
    app.create_folder(&b).unwrap();
    app.create_note("Robot", Some(&a)).unwrap();
    app.create_note("Robot", Some(&b)).unwrap();

    match app.resolve_note_link("Robot").unwrap() {
        LinkResolution::Ambiguous(cands) => assert_eq!(cands.len(), 2),
        other => panic!("expected Ambiguous, got {other:?}"),
    }
}

#[test]
fn resolve_note_link_unknown_target_is_not_found() {
    let tmp = tempfile::tempdir().unwrap();
    let app = build_app(tmp.path());
    app.create_note("Robot", None).unwrap();

    assert_eq!(
        app.resolve_note_link("Ghost").unwrap(),
        LinkResolution::NotFound
    );
    // Empty/whitespace target is NotFound, not an error.
    assert_eq!(
        app.resolve_note_link("   ").unwrap(),
        LinkResolution::NotFound
    );
}

#[test]
fn all_tags_counts_across_notes_and_dedupes_per_note() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    // `# Alpha` is a heading (space after #), not a tag; inline `#idea` etc. are.
    std::fs::write(root.join("Alpha.md"), "# Alpha\n\n#idea and #robot\n").unwrap();
    std::fs::write(root.join("Beta.md"), "# Beta\n\n#idea #todo\n").unwrap();
    // A tag repeated within ONE note counts the note once, not twice.
    std::fs::write(root.join("Gamma.md"), "# Gamma\n\n#robot #robot\n").unwrap();

    let app = build_app(root);
    let tags = app.all_tags().unwrap();
    let count_of = |name: &str| -> usize {
        tags.iter()
            .find(|t| t.tag == name)
            .map(|t| t.count)
            .unwrap_or(0)
    };
    // idea: Alpha + Beta = 2. robot: Alpha + Gamma = 2. todo: Beta = 1.
    assert_eq!(count_of("idea"), 2);
    assert_eq!(count_of("robot"), 2);
    assert_eq!(count_of("todo"), 1);
    // Sorted by count desc, then tag asc: idea(2) before robot(2) before todo(1).
    let names: Vec<&str> = tags.iter().map(|t| t.tag.as_str()).collect();
    assert_eq!(names, vec!["idea", "robot", "todo"]);
}

#[test]
fn all_tags_empty_when_no_notes_tagged() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    std::fs::write(root.join("Plain.md"), "# Plain\n\nno tags here\n").unwrap();

    let app = build_app(root);
    assert!(app.all_tags().unwrap().is_empty());
}

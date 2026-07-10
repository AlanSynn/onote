//! Minimal Ratatui/Crossterm editor (`CLAUDE.md` §2.2, Spike 2).
//!
//! A single-pane text editor over the open note: path bar on top, editor in the
//! middle, a centralized status line at the bottom. Ctrl+S saves (§7 conflict
//! surfaced in the status), Ctrl+O fuzzy-opens, Ctrl+P pastes an image token,
//! Ctrl+D deletes the image token under the cursor, Ctrl+R reloads, Ctrl+K
//! writes a conflict copy, Ctrl+Q quits. Enter over an image line opens the
//! full-screen preview modal (§2.4); mouse wheel scrolls the viewport.
//!
//! Text selection (block-select interaction): Shift+arrows extend a selection
//! (grapheme-accurate), Ctrl+A selects all, mouse drag selects, Ctrl+Shift+C
//! copies and Ctrl+X cuts the selection, and Ctrl+Left/Right jump by word
//! (Ctrl+Shift+Left/Right extend by word). Typing/Backspace/Delete over a
//! selection replaces it. All editor (edit-mode) bindings are remappable via
//! the `[keymap]` config table (`CLAUDE.md` §5 KeymapRegistry); the fuzzy
//! picker and image-preview modal keys are fixed (not in the registry).
//!
//! External edits (Obsidian, another terminal, `git pull`) are detected via the
//! file watcher (§2.5/§7): the status flips to "changed externally" so the user
//! reloads rather than silently clobbering disk.
//!
//! Image preview (§2.4): pressing Enter on a line containing an image embed
//! opens a centered modal that renders the picture via `ratatui-image` when the
//! terminal speaks a graphics protocol (Kitty/iTerm2/Sixel), and otherwise shows
//! the filename, dimensions, size, and an "open in GUI" action. In the editor
//! body itself, each image-embed token is shown as an inline `[image: name]`
//! glyph (§2.4 "editor surface") while the underlying buffer stays untouched —
//! editing is transparent (the real Markdown token is what gets saved).
//!
//! # Deferred MVP polish (intentionally not implemented here)
//!
//! - **§2.4 "Hover/focus small overlay"** is deferred: the tier-2 inline image
//!   surface is skipped because the full-screen preview modal (Enter/Space)
//!   already covers the need. The editor surface uses the `[image: name]` glyph
//!   and leaves the pop-over for a later round.
//! - **§3.2 note/preview/share drawers** are deferred: the MVP ships a single
//!   editor pane (path bar + editor + status line) rather than the multi-drawer
//!   layout. Drawers will layer on once the single-pane flow is stable.

use std::io::{self, Stdout};
use std::sync::mpsc::Receiver;
use std::time::Duration;

use anyhow::Result;
use crossterm::cursor::Show;
use crossterm::event::{
    self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEvent, KeyEventKind,
    MouseButton, MouseEvent, MouseEventKind,
};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::backend::CrosstermBackend;
use ratatui::style::Color;
// ratatui-image stays in the UI layer: the domain/ports never see it
// (`CLAUDE.md` §1.3, §2.4). `Picker` detects the terminal graphics protocol.
use ratatui::Terminal as RatatuiTerminal;
use ratatui_image::picker::Picker;
use ratatui_image::protocol::StatefulProtocol;

use crate::application::ops::SaveOutcome;
use crate::application::App;
use crate::domain::note::NoteDocument;
use crate::domain::session::ExternalChange;
use crate::domain::vault::RelativeNotePath;

mod editor;
mod keymap;
mod render;
use editor::*;
use keymap::*;
use render::*;

type Terminal = RatatuiTerminal<CrosstermBackend<Stdout>>;

/// Centralized sync status (DRY, `CLAUDE.md` §5) — one source rendered to fit.
///
/// Mirrors the §5 model exactly: `Clean / Dirty / Saving / ChangedExternally /
/// Conflict / Error(String)`. `Saving` is reserved for the future async-write
/// window — because saves are currently synchronous it is set and then
/// overwritten within a single frame, so the UI may never observe it mid-flight.
/// The variant exists now so the model is correct and future-proof.
#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(dead_code)]
pub(super) enum SyncStatus {
    Clean,
    Dirty,
    Saving,
    Conflict,
    ChangedExternally,
    Error(String),
}

impl SyncStatus {
    fn label(&self) -> String {
        match self {
            Self::Clean => "clean".into(),
            Self::Dirty => "unsaved".into(),
            Self::Saving => "saving…".into(),
            Self::Conflict => "CONFLICT: ^R reload / ^K conflict-copy".into(),
            Self::ChangedExternally => "changed externally".into(),
            Self::Error(e) => format!("error: {e}"),
        }
    }

    fn color(&self) -> Color {
        match self {
            Self::Clean => Color::Green,
            Self::Dirty => Color::Yellow,
            Self::Saving => Color::Cyan,
            Self::Conflict | Self::ChangedExternally | Self::Error(_) => Color::Red,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Mode {
    Edit,
    FuzzyOpen,
}

/// Full-screen image preview modal (`CLAUDE.md` §2.4 Enter/Space action).
///
/// `Rendered` holds a live [`StatefulProtocol`] the `StatefulImage` widget
/// resizes to fit the modal — only used when the terminal speaks a graphics
/// protocol (Kitty/iTerm2/Sixel). `Fallback` is the spec'd text surface
/// (filename, dimensions, size, open action) for terminals without one.
///
/// Both variants carry the validated vault-relative `path` (drives the
/// open-in-GUI / copy actions) SEPARATELY from the `display_name` basename
/// (drives the modal title). Splitting these keeps a deep attachment path
/// (e.g. `Attachments/2026/07/img-x.png`) from overflowing the title bar while
/// still letting actions target the exact file — the two concerns must not
/// share one string, or a "show only basename in title" tweak would silently
/// break the action target.
pub(super) enum ImageOverlay {
    Rendered {
        proto: StatefulProtocol,
        path: RelativeNotePath,
        display_name: String,
        width: u32,
        height: u32,
        size: u64,
    },
    Fallback {
        path: RelativeNotePath,
        display_name: String,
        width: u32,
        height: u32,
        size: u64,
        reason: String,
    },
}

impl ImageOverlay {
    /// The validated vault-relative path — drives the open-in-GUI / copy actions.
    fn path(&self) -> &RelativeNotePath {
        match self {
            Self::Rendered { path, .. } | Self::Fallback { path, .. } => path,
        }
    }

    /// One-line header: `display_name · WxH · size` (`CLAUDE.md` §2.4 fallback
    /// info). Formats the BASENAME (not the full vault-relative path) so a deep
    /// attachment path can't overflow the title bar; the full path remains
    /// reachable via the fallback body and the action commands.
    fn header(&self) -> String {
        let (display_name, w, h, size) = match self {
            Self::Rendered {
                display_name,
                width,
                height,
                size,
                ..
            }
            | Self::Fallback {
                display_name,
                width,
                height,
                size,
                ..
            } => (display_name, width, height, size),
        };
        format!("{display_name}  ·  {w}×{h}  ·  {}", format_size(*size))
    }
}

/// Human-readable byte size (B / KiB / MiB).
pub(super) fn format_size(bytes: u64) -> String {
    const KIB: u64 = 1024;
    const MIB: u64 = KIB * 1024;
    if bytes >= MIB {
        format!("{:.1} MiB", bytes as f64 / MIB as f64)
    } else if bytes >= KIB {
        format!("{:.1} KiB", bytes as f64 / KIB as f64)
    } else {
        format!("{bytes} B")
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Control {
    Continue,
    Quit,
}

/// RAII guard that restores the terminal on drop — runs on both normal return
/// and panic unwind, so a panic inside the loop can never strand the user in raw
/// alternate-screen mode with no cursor.
struct TerminalGuard;

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let _ = execute!(
            io::stdout(),
            LeaveAlternateScreen,
            DisableMouseCapture,
            Show
        );
    }
}

/// Run the editor until the user quits. The terminal is restored on exit via
/// [`TerminalGuard`] (Drop), which also covers panics.
pub fn run(app: &App, initial: NoteDocument) -> Result<()> {
    // Fail with a readable message instead of crossterm's raw `errno ENXIO`
    // ("Device not configured") when stdout isn't a real terminal — e.g.
    // `onote | cat`, cron, or SSH with no pty. Without this, `enable_raw_mode`
    // returns an opaque os error with zero context.
    use std::io::IsTerminal;
    if !io::stdout().is_terminal() {
        return Err(anyhow::anyhow!(
            "onote needs an interactive terminal; stdout is not a TTY"
        ));
    }
    enable_raw_mode()?;
    let _guard = TerminalGuard;
    let mut stdout = io::stdout();
    let _ = execute!(stdout, EnterAlternateScreen, EnableMouseCapture);
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let mut state = EditorState::from_doc(initial);
    // Overlay user `[keymap]` overrides on the baked defaults (§5 KeymapRegistry).
    state.keymap = KeymapRegistry::from_config(&app.config().keymap);
    // Detect a graphics protocol (Kitty/iTerm2/Sixel) for the image modal.
    // `from_query_stdio` probes the terminal; `None` ⇒ text fallback (§2.4).
    // Must run after entering the alternate screen so the probe sequences land
    // in the right context. Failure is non-fatal.
    state.picker = Picker::from_query_stdio().ok();
    // §2.5/§7: watch the vault root so external edits surface in the status.
    // Failure to start the watcher is non-fatal — save-time conflict detection
    // (§7 optimistic concurrency) still protects data.
    let watcher_rx = app.watch(&[app.config().vault.clone()]).ok().flatten();
    main_loop(app, &mut terminal, &mut state, watcher_rx)
}

fn main_loop(
    app: &App,
    terminal: &mut Terminal,
    state: &mut EditorState,
    watcher_rx: Option<Receiver<ExternalChange>>,
) -> Result<()> {
    loop {
        terminal.draw(|f| render(app, state, f))?;
        if event::poll(Duration::from_millis(250))? {
            match event::read()? {
                Event::Key(key) => {
                    if key.kind == KeyEventKind::Press
                        && handle_event(app, state, key)? == Control::Quit
                    {
                        return Ok(());
                    }
                }
                // Spike 2 mouse scroll: nudge the viewport without moving the
                // cursor (reader-style). Captured via EnableMouseCapture in run().
                Event::Mouse(mouse) => handle_mouse(app, state, mouse),
                _ => {}
            }
        }
        // Drain external-edit notifications (non-blocking).
        //
        // Two jobs per change: (1) keep the search index in sync with disk for
        // EVERY changed path — not just the open note — so a `git pull`,
        // Obsidian edit, or external delete in another terminal doesn't leave
        // stale rows or ghost hits (§6 index tracks source-of-truth files); and
        // (2) flip the status to ChangedExternally only when the OPEN note's
        // disk hash diverges from its open baseline (§7).
        if let Some(rx) = &watcher_rx {
            while let Ok(change) = rx.try_recv() {
                app.sync_index_for(&change.note_path);
                if let Some(open) = app.current() {
                    if open.path == change.note_path
                        && change.new_disk_hash != open.opened_hash
                        && state.status != SyncStatus::Conflict
                    {
                        state.status = SyncStatus::ChangedExternally;
                    }
                }
            }
        }
        if let Some((at, _)) = state.message {
            if at.elapsed() > Duration::from_secs(3) {
                state.message = None;
            }
        }
    }
}

fn handle_event(app: &App, state: &mut EditorState, key: KeyEvent) -> Result<Control> {
    // Global quit — resolved through the keymap and checked BEFORE the modal
    // dispatch so it works from any mode (including the image overlay). Mirrors
    // the old hardcoded Ctrl+Q / Ctrl+C check (now configurable via `[keymap]`).
    if state.keymap.action_for(&key) == Some(Action::Quit) {
        return Ok(Control::Quit);
    }
    // Image preview modal intercepts all other keys while open.
    if state.overlay.is_some() {
        return handle_overlay_event(app, state, key);
    }
    match state.mode {
        Mode::FuzzyOpen => handle_fuzzy_event(app, state, key),
        Mode::Edit => {
            let ctrl = handle_edit_event(app, state, key)?;
            // Re-anchor the viewport on the cursor after any edit-key move.
            state.adjust_scroll(state.view_height);
            Ok(ctrl)
        }
    }
}

fn handle_edit_event(app: &App, state: &mut EditorState, key: KeyEvent) -> Result<Control> {
    // Every edit-mode key resolves through the keymap (§5 KeymapRegistry,
    // contract C8): no key is matched directly here. Unhandled keys (e.g.
    // Ctrl+J, Shift+Tab) fall through to a no-op.
    let Some(action) = state.keymap.action_for(&key) else {
        return Ok(Control::Continue);
    };
    dispatch_edit(app, state, action)
}

/// Dispatch a resolved [`Action`] in edit mode. App-dependent commands (save,
/// open, paste, image-token, enter→image) call their handlers here; App-free
/// mutations route through [`EditorState::apply_action`] so they stay
/// unit-testable without an `App`. Copy/Cut/selection/word actions resolve here
/// but land their behavior in P2/P4/P5 (until then `apply_action` no-ops them).
fn dispatch_edit(app: &App, state: &mut EditorState, action: Action) -> Result<Control> {
    match action {
        Action::Save => save(app, state),
        Action::OpenFuzzy => {
            // Leaving the editor surface → drop the selection (C5 lifecycle).
            state.clear_selection();
            state.mode = Mode::FuzzyOpen;
            state.fuzzy_query.clear();
            refresh_fuzzy(app, state)?;
            Ok(Control::Continue)
        }
        Action::PasteImage => paste_image(app, state),
        Action::Reload => reload(app, state),
        Action::ConflictCopy => conflict_copy(app, state),
        // ^D deletes the image token under the cursor (Spike 3).
        Action::DeleteImageToken => delete_image_token(app, state),
        // Enter on an image line opens the preview modal; otherwise newline.
        Action::Enter => {
            // C6/C5: with a selection, Enter replaces it with the newline (then
            // clears the anchor) — type-over-replace parity. Done BEFORE the
            // image-overlay probe so a selection on an image line still replaces
            // rather than opening the preview. Enter is dispatched here (not via
            // `apply_action`'s C6 catch-all) because the overlay probe is
            // App-dependent, so the centralized guard can't see it.
            if let Some(sel) = state.selection() {
                let (start, end) = sel.normalized();
                state.delete_range(start, end);
                state.insert_newline();
                return Ok(Control::Continue);
            }
            if !try_open_image_overlay(app, state)? {
                state.insert_newline();
            }
            Ok(Control::Continue)
        }
        // ^Shift+C copies the selection to the clipboard (Ctrl+C stays quit).
        // The clipboard write is injected so `copy_selection` is App-free-testable.
        Action::Copy => copy_selection(state, |t| app.copy_text(t)),
        // ^X cuts: copy the selection, then delete the range in one step.
        Action::Cut => cut_selection(state, |t| app.copy_text(t)),
        // All App-free editor mutations (insert, motion, tab, backspace,
        // forward-delete, selection). Word-motion lands in P5.
        _ => {
            state.apply_action(action);
            Ok(Control::Continue)
        }
    }
}

/// Copy the active selection to the clipboard via the injected `copy` sink.
///
/// The `copy` closure returns a `Result` so the caller supplies the real
/// clipboard write (`app.copy_text`) in production and a capturing sink in
/// unit tests — `copy_selection`/`cut_selection` stay free of `App`, so the
/// "selection → clipboard" path is tested without building the 8-dep
/// `AppDeps` graph (CLAUDE.md §1.3; the §9 guard test comment applies the same
/// "heavy App paths go to the integration harness" rationale to the editor).
/// `E: Display` lets the test sink fail with any error type, matching how
/// `anyhow::Error` renders in the production toast.
fn copy_selection<E: std::fmt::Display>(
    state: &mut EditorState,
    copy: impl FnOnce(&str) -> Result<(), E>,
) -> Result<Control> {
    match state.selected_text() {
        Some(text) => match copy(&text) {
            Ok(()) => state.toast("copied"),
            Err(e) => state.toast(format!("copy failed: {e}")),
        },
        None => state.toast("nothing selected"),
    }
    Ok(Control::Continue)
}

/// Cut: copy the selection, then delete its range in one step. Like
/// [`copy_selection`], the clipboard write is injected so this is App-free
/// testable. The delete happens only on a successful copy (CLAUDE.md §3.1 —
/// never silently lose data): a clipboard failure leaves the selection intact.
fn cut_selection<E: std::fmt::Display>(
    state: &mut EditorState,
    copy: impl FnOnce(&str) -> Result<(), E>,
) -> Result<Control> {
    if let Some(text) = state.selected_text() {
        match copy(&text) {
            Ok(()) => {
                if let Some(sel) = state.selection() {
                    let (start, end) = sel.normalized();
                    state.delete_range(start, end);
                }
                state.toast("cut");
            }
            Err(e) => state.toast(format!("cut failed: {e}")),
        }
    } else {
        state.toast("nothing selected");
    }
    Ok(Control::Continue)
}

/// Keys while the image preview modal is open (`CLAUDE.md` §2.4).
fn handle_overlay_event(app: &App, state: &mut EditorState, key: KeyEvent) -> Result<Control> {
    match key.code {
        KeyCode::Esc | KeyCode::Enter | KeyCode::Char('q') | KeyCode::Char(' ') => {
            state.overlay = None;
        }
        // 'o' opens the image in the Obsidian GUI (§2.4 fallback "open" action).
        KeyCode::Char('o') => {
            if let Some(rel) = overlay_rel(state) {
                match app.open_in_gui(&rel) {
                    Ok(()) => state.toast("opened in GUI"),
                    Err(e) => state.toast(format!("gui open failed: {e}")),
                }
            }
        }
        // 'c' copies the image to the clipboard (§2.4 fallback "copy" action).
        KeyCode::Char('c') => {
            if let Some(rel) = overlay_rel(state) {
                match app.copy_image(&rel) {
                    Ok(()) => state.toast("copied image"),
                    Err(e) => state.toast(format!("copy failed: {e}")),
                }
            }
        }
        _ => {}
    }
    Ok(Control::Continue)
}

/// The vault-relative path of the image currently in the preview modal, if any.
///
/// The overlay already stores a validated `RelativeNotePath`, so this is a
/// cheap clone — no re-parse. The previous form round-tripped the path through
/// a `name: String` + `RelativeNotePath::from_user`, which coupled the action
/// target to the title display string.
fn overlay_rel(state: &EditorState) -> Option<RelativeNotePath> {
    state.overlay.as_ref().map(|ov| ov.path().clone())
}

/// If the current line embeds an image, open the preview modal. Returns whether
/// an overlay was produced (so the caller can skip inserting a newline).
fn try_open_image_overlay(app: &App, state: &mut EditorState) -> Result<bool> {
    let line = state.cur_line().to_string();
    let Some(rf) = app.attachment_links(&line).into_iter().next() else {
        return Ok(false);
    };
    // The reference's `target` is already a validated `RelativeNotePath` (the
    // same one handed to `image_preview`) — clone it for the overlay's action
    // target instead of re-parsing a display string. `display_name` is the
    // basename (reusing the module's `basename()` helper — DRY) for the title.
    let path = rf.target.clone();
    let display_name = basename(&rf.target.as_str()).to_string();
    let loaded = match app.image_preview(&rf.target) {
        Ok(l) => l,
        Err(e) => {
            state.toast(format!("image load failed: {e}"));
            // Still open a fallback so the user sees the broken reference.
            state.overlay = Some(ImageOverlay::Fallback {
                path,
                display_name,
                width: 0,
                height: 0,
                size: 0,
                reason: format!("{e}"),
            });
            return Ok(true);
        }
    };
    let width = loaded.width;
    let height = loaded.height;
    let size = loaded.size_bytes;

    // Build the overlay. A live protocol requires a detected graphics protocol
    // AND a decodable image (capped — see `decode_limits`); else degrade to text.
    let overlay = match (&state.picker, decode_for_render(&loaded.bytes)) {
        (Some(picker), Some(dyn_img)) => {
            let proto = picker.new_resize_protocol(dyn_img);
            ImageOverlay::Rendered {
                proto,
                path,
                display_name,
                width,
                height,
                size,
            }
        }
        (None, _) => ImageOverlay::Fallback {
            path,
            display_name,
            width,
            height,
            size,
            reason: "no graphics protocol in this terminal".to_string(),
        },
        (Some(_), None) => ImageOverlay::Fallback {
            path,
            display_name,
            width,
            height,
            size,
            reason: "image decode failed (or too large)".to_string(),
        },
    };
    state.overlay = Some(overlay);
    Ok(true)
}

/// Full decode of `bytes` for rendering, with a decompression-bomb cap
/// (max 8000×8000, 256 MiB). `None` on any decode error or limit breach.
fn decode_for_render(bytes: &[u8]) -> Option<image::DynamicImage> {
    let mut reader = image::ImageReader::new(std::io::Cursor::new(bytes))
        .with_guessed_format()
        .ok()?;
    // `limits()` mutates in place (no builder return), then decode with the cap.
    reader.limits(decode_limits());
    reader.decode().ok()
}

/// Decode resource caps: defends against a crafted image (e.g. a 65535×65535
/// PNG IHDR) OOMing the TUI on preview (`CLAUDE.md` §3.1 local-first, but the
/// vault can receive a hostile image via `git pull`).
fn decode_limits() -> image::Limits {
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(8_000);
    limits.max_image_height = Some(8_000);
    limits.max_alloc = Some(256 * 1024 * 1024);
    limits
}

/// Remove the first image-attachment token on the cursor line from the buffer.
/// Uses the token's PARSED style (not the configured one) so it deletes
/// Obsidian `![[…]]` embeds even when `image_link_style = markdown`. Only the
/// first occurrence is removed. (File deletion is intentionally NOT done here —
/// `CLAUDE.md` §3.1 makes it optional and gated on `is_referenced_elsewhere`.)
fn delete_image_token(app: &App, state: &mut EditorState) -> Result<Control> {
    // B3: deleting an image token mutates the line + caret but bypasses
    // apply_action's C6 guard (it's an App-dependent command). Drop any
    // selection so its anchor can't go stale against the shortened line.
    state.clear_selection();
    let line = state.cur_line().to_string();
    let Some(rf) = app.attachment_links(&line).into_iter().next() else {
        state.toast("no image token on this line");
        return Ok(Control::Continue);
    };
    let token = crate::domain::attachment::AttachmentReference::render_token(rf.style, &rf.target);
    let Some(line_idx) = state.lines.get(state.cy).cloned() else {
        return Ok(Control::Continue);
    };
    if let Some(byte) = line_idx.find(&token) {
        let char_start = char_count(&line_idx[..byte]);
        let char_end = char_start + char_count(&token);
        if let Some(line) = state.lines.get_mut(state.cy) {
            let b0 = char_to_byte(line, char_start);
            let b1 = char_to_byte(line, char_end);
            line.replace_range(b0..b1, "");
            // Park the cursor at the deletion point.
            state.cx = char_start.min(char_count(line));
            state.mark_dirty();
            state.toast(format!("removed {}", rf.target.as_str()));
        }
    } else {
        state.toast("token not found on line");
    }
    Ok(Control::Continue)
}

/// Mouse handling (Spike 2 wheel scroll + P3 selection). Wheel scrolls the
/// viewport reader-style; left-button Down/Drag/Up perform click-to-move and
/// drag-to-select. Down sets anchor=head at the hit cell (a pure click just
/// moves the caret — anchor==head → empty selection); Drag/Moved move only the
/// head (the caret), so the selection grows from the anchor. Dragging past the
/// viewport edges autoscrolls (C3). Clicks outside the editor rect are ignored.
fn handle_mouse(app: &App, state: &mut EditorState, mouse: MouseEvent) {
    // The char-column resolver is the ONLY App dependency in the mouse path
    // (glyph substitution). Injecting it lets `handle_mouse_with` (the whole
    // Down/Drag/Up state machine) be unit-tested App-free with a plain resolver.
    handle_mouse_with(state, mouse, |line, d| display_col_to_char(app, line, d));
}

/// App-free core of [`handle_mouse`] (testable): same Down/Drag/Up selection
/// state machine, but resolves a hit column via `char_at` instead of `App`.
fn handle_mouse_with<R>(state: &mut EditorState, mouse: MouseEvent, char_at: R)
where
    R: Fn(&str, usize) -> usize,
{
    match mouse.kind {
        MouseEventKind::ScrollUp => state.scroll = state.scroll.saturating_sub(3),
        MouseEventKind::ScrollDown => {
            let max = state.lines.len().saturating_sub(state.view_height.max(1));
            state.scroll = (state.scroll + 3).min(max);
        }
        MouseEventKind::Down(MouseButton::Left) => {
            if let Some(p) = mouse_to_pos_with(state, mouse.column, mouse.row, &char_at) {
                state.cy = p.line;
                state.cx = p.col;
                // Anchor == head == click: a click with no drag is an empty
                // selection (just a moved caret); the first Drag leaves the
                // anchor here and extends the head away from it.
                state.selection_anchor = Some(p);
                state.mouse_dragging = true;
                state.adjust_scroll(state.view_height);
            }
        }
        MouseEventKind::Drag(MouseButton::Left) if state.mouse_dragging => {
            mouse_drag_to_with(state, mouse.column, mouse.row, &char_at);
        }
        // Some terminals report `Moved` (not `Drag`) while a button is held;
        // treat it as a drag when we know the button is down (mouse_dragging).
        MouseEventKind::Moved if state.mouse_dragging => {
            mouse_drag_to_with(state, mouse.column, mouse.row, &char_at);
        }
        MouseEventKind::Up(MouseButton::Left) => state.mouse_dragging = false,
        _ => {}
    }
}

/// Map a screen (column, row) to a buffer [`Pos`], or `None` if it's outside
/// the editor inner rect. The hit column becomes a char index via `char_at`
/// (the inverse display-col map, C2, grapheme-snapped C7). Row → absolute line
/// via the scroll offset, clamped to the last line (a hit below the viewport
/// pins to the last line while autoscroll catches up).
fn mouse_to_pos_with<R>(state: &EditorState, col: u16, row: u16, char_at: &R) -> Option<Pos>
where
    R: Fn(&str, usize) -> usize,
{
    if col < state.editor_x || row < state.editor_y {
        return None;
    }
    let dx = (col - state.editor_x) as usize;
    let dy = (row - state.editor_y) as usize;
    let last = state.lines.len().saturating_sub(1);
    let line_idx = state.scroll.saturating_add(dy).min(last);
    let line = state.lines.get(line_idx)?;
    let char_col = char_at(line, dx.min(state.editor_width as usize));
    Some(Pos {
        line: line_idx,
        col: char_col,
    })
}

/// Extend the selection head to the drag position, autoscrolling when the drag
/// leaves the viewport edges (C3). Only the head (caret) moves — the anchor set
/// on Down is untouched, so the selection tracks the pointer from the anchor.
fn mouse_drag_to_with<R>(state: &mut EditorState, col: u16, row: u16, char_at: &R)
where
    R: Fn(&str, usize) -> usize,
{
    let bottom = state.editor_y.saturating_add(state.view_height as u16);
    if row >= bottom && state.scroll + state.view_height < state.lines.len() {
        state.scroll += 1; // drag below the viewport → scroll down (C3)
    } else if row < state.editor_y && state.scroll > 0 {
        state.scroll -= 1; // drag above the viewport → scroll up (C3)
    }
    if let Some(p) = mouse_to_pos_with(state, col, row, char_at) {
        state.cy = p.line;
        state.cx = p.col;
    }
    // A drag that left the editor horizontally (col < editor_x or past width)
    // still keeps the head's line; mouse_to_pos_with returns None there, so the
    // caret stays put — acceptable, since vertical autoscroll is the common case.
}

fn handle_fuzzy_event(app: &App, state: &mut EditorState, key: KeyEvent) -> Result<Control> {
    match key.code {
        KeyCode::Esc => state.mode = Mode::Edit,
        KeyCode::Enter => {
            if let Some(sel) = state.fuzzy_results.get(state.fuzzy_sel).cloned() {
                let doc = app.open_note(&sel.path)?;
                state.reload(doc);
                state.mode = Mode::Edit;
                state.toast(format!("opened {}", sel.path.as_str()));
            }
        }
        KeyCode::Backspace => {
            state.fuzzy_query.pop();
            refresh_fuzzy(app, state)?;
        }
        KeyCode::Up => {
            if state.fuzzy_sel > 0 {
                state.fuzzy_sel -= 1;
            }
        }
        KeyCode::Down => {
            if state.fuzzy_sel + 1 < state.fuzzy_results.len() {
                state.fuzzy_sel += 1;
            }
        }
        KeyCode::Char(c) => {
            state.fuzzy_query.push(c);
            refresh_fuzzy(app, state)?;
        }
        _ => {}
    }
    Ok(Control::Continue)
}

fn refresh_fuzzy(app: &App, state: &mut EditorState) -> Result<()> {
    state.fuzzy_results = app.fuzzy(&state.fuzzy_query)?;
    state.fuzzy_sel = 0;
    Ok(())
}

fn save(app: &App, state: &mut EditorState) -> Result<Control> {
    // Model the §5 "Saving" window before the synchronous write. Saves are
    // currently blocking, so this is overwritten within the same frame and the
    // UI may never render it — but the model is now correct and ready for an
    // async-write path (§5). No artificial delay is introduced.
    state.status = SyncStatus::Saving;
    // Data-safety (§1.1 local-first): a transient save error (disk full,
    // permission denied, watcher hiccup) must NOT propagate. The old `?` tore
    // down the loop, the guard restored the terminal, and the user's unsaved
    // buffer was discarded with only an eprintln to a restored terminal. Catch
    // it here, surface it in the status line, and keep the buffer intact so the
    // user can retry or copy their text out.
    match app.save_current(&state.body()) {
        Ok(SaveOutcome::Written(_) | SaveOutcome::NoChange) => {
            state.status = SyncStatus::Clean;
            state.toast("saved");
        }
        Ok(SaveOutcome::Conflict { .. }) => {
            state.status = SyncStatus::Conflict;
            state.toast("conflict — not overwritten");
        }
        Err(e) => {
            // `anyhow::Error` Display is the single top-level message (not the
            // full `{:?}` chain), so this stays short and user-readable.
            state.status = SyncStatus::Error(e.to_string());
            state.toast(format!("save failed: {e}"));
        }
    }
    Ok(Control::Continue)
}

fn reload(app: &App, state: &mut EditorState) -> Result<Control> {
    let doc = app.reload_current()?;
    state.reload(doc);
    state.toast("reloaded");
    Ok(Control::Continue)
}

fn conflict_copy(app: &App, state: &mut EditorState) -> Result<Control> {
    let copy = app.write_conflict_copy(&state.body())?;
    state.toast(format!("wrote conflict copy {}", copy.as_str()));
    Ok(Control::Continue)
}

fn paste_image(app: &App, state: &mut EditorState) -> Result<Control> {
    match app.paste_image()? {
        Some(pasted) => {
            // B2 (C6 parity): paste_image calls insert_char directly (bypassing
            // apply_action's centralized replace-on-type guard), so an ACTIVE
            // selection must be deleted here first — otherwise the token is
            // inserted at the caret with the selection untouched, and the next
            // keystroke would C6-delete the stale range. The latent-anchor case
            // is handled by insert_char itself (B1).
            if let Some(sel) = state.selection() {
                let (s, e) = sel.normalized();
                state.delete_range(s, e);
            }
            for ch in format!("{} ", pasted.token).chars() {
                state.insert_char(ch);
            }
            state.toast(format!("pasted {}", pasted.attachment.path.as_str()));
        }
        None => state.toast("no image on clipboard"),
    }
    Ok(Control::Continue)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::KeyModifiers;

    #[test]
    fn format_size_buckets_correctly() {
        assert_eq!(format_size(0), "0 B");
        assert_eq!(format_size(512), "512 B");
        assert_eq!(format_size(1024), "1.0 KiB");
        assert_eq!(format_size(1536), "1.5 KiB");
        assert_eq!(format_size(1_048_576), "1.0 MiB");
        assert_eq!(format_size(5_242_880), "5.0 MiB");
    }

    #[test]
    fn overlay_header_includes_dims_and_size() {
        let ov = ImageOverlay::Fallback {
            path: RelativeNotePath::new("Attachments/seal.png").expect("test path"),
            display_name: "seal.png".into(),
            width: 800,
            height: 600,
            size: 4096,
            reason: "none".into(),
        };
        // Title formats the basename (display_name), not the full path — no overflow.
        assert_eq!(ov.header(), "seal.png  ·  800×600  ·  4.0 KiB");
        // Actions still target the validated vault-relative path.
        assert_eq!(ov.path().as_str(), "Attachments/seal.png");
    }

    /// §5 SyncStatus model: `Saving` exists, renders distinctly, and is Cyan so
    /// it cannot be confused with `Dirty` (Yellow) or `Clean` (Green).
    #[test]
    fn sync_status_saving_variant_is_distinct() {
        assert_eq!(SyncStatus::Saving.label(), "saving…");
        assert_eq!(SyncStatus::Saving.color(), Color::Cyan);
        // All five sibling variants present (§5 model is complete).
        assert_ne!(SyncStatus::Saving, SyncStatus::Clean);
        assert_ne!(SyncStatus::Saving, SyncStatus::Dirty);
        assert_ne!(SyncStatus::Saving, SyncStatus::Conflict);
        assert_ne!(SyncStatus::Saving, SyncStatus::ChangedExternally);
    }

    // ── Selection: mouse Down/Drag/Up (P3, contracts C2/C3) ────────────────
    //
    // Drives the App-free `handle_mouse_with` with a plain-text resolver
    // (no glyphs → empty spans; grapheme snap is identity on ASCII) so the
    // full Down→Drag→Up state machine is covered without building an `App`.

    /// A resolver equivalent to the real one for plain text: inverse map (no
    /// glyphs) + grapheme snap.
    fn plain_resolver(line: &str, display_col: usize) -> usize {
        let c = display_col_to_char_from_spans(line, &[], display_col);
        snap_to_grapheme_start(line, c)
    }

    /// Editor state for `body` with a fake inner rect at (x,y) of `w`×`h`.
    fn mouse_state(body: &str, x: u16, y: u16, w: u16, h: usize) -> EditorState {
        let mut s = state_from_body(body);
        s.editor_x = x;
        s.editor_y = y;
        s.editor_width = w;
        s.view_height = h;
        s
    }

    fn left(kind: MouseEventKind, col: u16, row: u16) -> MouseEvent {
        MouseEvent {
            kind,
            column: col,
            row,
            modifiers: KeyModifiers::NONE,
        }
    }

    #[test]
    fn mouse_down_drag_selects_a_range() {
        let mut s = mouse_state("hello\nworld", 5, 3, 80, 20);
        // Down at col 7 (= dx 2) on line 0 → anchor + caret at (0,2).
        handle_mouse_with(
            &mut s,
            left(MouseEventKind::Down(MouseButton::Left), 7, 3),
            plain_resolver,
        );
        assert_eq!(s.cy, 0);
        assert_eq!(s.cx, 2);
        assert!(s.mouse_dragging);
        assert_eq!(s.selection_anchor, Some(Pos { line: 0, col: 2 }));
        // Drag to col 11 (= dx 6) on line 1 → head moves; anchor stays.
        handle_mouse_with(
            &mut s,
            left(MouseEventKind::Drag(MouseButton::Left), 11, 4),
            plain_resolver,
        );
        let sel = s.selection().expect("drag produced a selection");
        assert_eq!(sel.anchor, Pos { line: 0, col: 2 }, "anchor fixed at Down");
        assert_eq!(
            sel.head,
            Pos { line: 1, col: 5 },
            "head = 'world' end (dx clamps)"
        );
        assert_eq!(sel.normalized().0, Pos { line: 0, col: 2 });
    }

    #[test]
    fn mouse_click_without_drag_just_moves_caret() {
        let mut s = mouse_state("hello", 5, 3, 80, 20);
        // Down then Up at the same cell: anchor==head → empty (no selection),
        // caret moved to the click.
        handle_mouse_with(
            &mut s,
            left(MouseEventKind::Down(MouseButton::Left), 8, 3),
            plain_resolver,
        );
        handle_mouse_with(
            &mut s,
            left(MouseEventKind::Up(MouseButton::Left), 8, 3),
            plain_resolver,
        );
        assert!(!s.mouse_dragging);
        assert_eq!(s.cx, 3, "caret moved to the click (dx 3)");
        assert!(s.selection().is_none(), "a pure click is not a selection");
    }

    /// Clicks outside the editor inner rect are ignored (no caret/anchor move).
    #[test]
    fn mouse_click_outside_editor_rect_is_ignored() {
        let mut s = mouse_state("hello", 5, 3, 80, 20);
        let (cy, cx) = (s.cy, s.cx);
        // col 2 < editor_x 5 → outside.
        handle_mouse_with(
            &mut s,
            left(MouseEventKind::Down(MouseButton::Left), 2, 3),
            plain_resolver,
        );
        // row 1 < editor_y 3 → outside.
        handle_mouse_with(
            &mut s,
            left(MouseEventKind::Down(MouseButton::Left), 7, 1),
            plain_resolver,
        );
        assert_eq!(
            (s.cy, s.cx),
            (cy, cx),
            "outside-rect clicks do not move the caret"
        );
        assert!(s.selection_anchor.is_none());
        assert!(!s.mouse_dragging);
    }

    /// Dragging past the bottom edge autoscrolls the viewport (C3).
    #[test]
    fn mouse_drag_past_bottom_autoscrolls() {
        // 6 lines, viewport shows 3 (rows 3..6). editor_y=3, view_height=3.
        let mut s = mouse_state("a\nb\nc\nd\ne\nf", 0, 3, 80, 3);
        assert_eq!(s.scroll, 0);
        // Start a drag on the top visible line, then drag to row 10 (well past
        // the bottom at row 6).
        handle_mouse_with(
            &mut s,
            left(MouseEventKind::Down(MouseButton::Left), 0, 3),
            plain_resolver,
        );
        handle_mouse_with(
            &mut s,
            left(MouseEventKind::Drag(MouseButton::Left), 0, 10),
            plain_resolver,
        );
        assert_eq!(
            s.scroll, 1,
            "drag past the bottom edge scrolled down one line"
        );
        assert_eq!(s.cy, 5, "head pinned to the last line");
    }

    // ── Copy / Cut dispatch (P4, plan §5 integration surface) ─────────────
    //
    // `copy_selection`/`cut_selection` take an injected clipboard sink, so the
    // "selection → clipboard" path is tested App-free (no 8-dep `AppDeps`).

    /// Clipboard-failure stand-in for the "cut preserves buffer on error" test.
    /// Defined at module scope so the `Display` impl lives next to it (can't
    /// impl a trait for a type from outside its defining scope).
    struct ClipBoom;

    impl std::fmt::Display for ClipBoom {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            write!(f, "ClipBoom")
        }
    }

    /// Copy captures the normalized selection text and leaves the buffer intact.
    #[test]
    fn copy_selection_captures_text_and_preserves_buffer() {
        use std::cell::RefCell;
        let mut s = select_line_range("hello", 1, 3); // select "el"
        let captured = RefCell::new(String::new());
        let ctrl = copy_selection(&mut s, |t| {
            *captured.borrow_mut() = t.to_string();
            Ok::<(), std::convert::Infallible>(())
        })
        .unwrap();
        assert_eq!(ctrl, Control::Continue);
        assert_eq!(&*captured.borrow(), "el", "captured the selected span");
        assert_eq!(s.body(), "hello", "copy never mutates the buffer");
        assert!(s.selection().is_some(), "copy keeps the selection active");
        assert_eq!(
            s.message.as_ref().unwrap().1,
            "copied",
            "toast confirms the copy"
        );
    }

    /// Copy with nothing selected toasts "nothing selected" and copies nothing.
    #[test]
    fn copy_selection_with_no_selection_toasts_and_copies_nothing() {
        use std::cell::RefCell;
        let mut s = state_from_body("hello");
        let captured = RefCell::new(None::<String>);
        copy_selection(&mut s, |t| {
            *captured.borrow_mut() = Some(t.to_string());
            Ok::<(), std::convert::Infallible>(())
        })
        .unwrap();
        assert!(captured.borrow().is_none(), "sink was never called");
        assert_eq!(s.message.as_ref().unwrap().1, "nothing selected");
    }

    /// Cut captures the text AND deletes the range, collapsing the caret.
    #[test]
    fn cut_selection_copies_then_deletes() {
        use std::cell::RefCell;
        let mut s = select_line_range("hello", 1, 3); // select "el"
        let captured = RefCell::new(String::new());
        let ctrl = cut_selection(&mut s, |t| {
            *captured.borrow_mut() = t.to_string();
            Ok::<(), std::convert::Infallible>(())
        })
        .unwrap();
        assert_eq!(ctrl, Control::Continue);
        assert_eq!(&*captured.borrow(), "el", "cut copied the span first");
        assert_eq!(s.body(), "hlo", "then deleted the range");
        assert_eq!(s.cx, 1, "caret at the deletion start");
        assert!(s.selection().is_none(), "cut clears the selection");
        assert_eq!(s.message.as_ref().unwrap().1, "cut");
    }

    /// CLAUDE.md §3.1: a clipboard failure must NOT delete the selection's text
    /// (never silently lose data). `cut_selection` leaves the buffer intact and
    /// toasts the failure.
    #[test]
    fn cut_selection_preserves_buffer_on_clipboard_error() {
        let mut s = select_line_range("hello", 1, 3); // select "el"
        let ctrl = cut_selection(&mut s, |_| Err(ClipBoom)).unwrap();
        assert_eq!(ctrl, Control::Continue);
        assert_eq!(s.body(), "hello", "failed copy did not delete anything");
        assert!(
            s.selection().is_some(),
            "selection survives a clipboard failure"
        );
        assert_eq!(
            s.message.as_ref().unwrap().1,
            "cut failed: ClipBoom",
            "failure rendered via Display into the toast"
        );
    }
}

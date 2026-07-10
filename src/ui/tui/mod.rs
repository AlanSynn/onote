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
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Wrap};
use ratatui::Frame;
use ratatui::Terminal as RatatuiTerminal;
// ratatui-image stays in the UI layer: the domain/ports never see it
// (`CLAUDE.md` §1.3, §2.4). `Picker` detects the terminal graphics protocol;
// `StatefulImage` + `Resize::Fit` scale the preview to the modal area.
use ratatui_image::picker::Picker;
use ratatui_image::protocol::StatefulProtocol;
use ratatui_image::{Resize, StatefulImage};
// Display-width for the editor cursor column (`CLAUDE.md` §2.4): a char's
// terminal column span (CJK = 2, emoji = 2, ASCII = 1) differs from its byte
// or char count, so the cursor x must come from `UnicodeWidthStr`, not `cx`.
// `UnicodeWidthChar` is the per-char form used by the inverse map (P3, C2).
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::application::ops::SaveOutcome;
use crate::application::App;
use crate::domain::note::NoteDocument;
use crate::domain::session::ExternalChange;
use crate::domain::vault::RelativeNotePath;

mod editor;
mod keymap;
use editor::*;
use keymap::*;

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

/// Render the §9 small-terminal guard message. App-free signature
/// (`&mut Frame` only) so a `ratatui::backend::TestBackend` unit test can
/// exercise the render path — panic-safety + buffer content — without
/// constructing a full `App` (which carries 8+ adapter deps and belongs in an
/// integration harness).
fn render_too_small(frame: &mut Frame) {
    let area = frame.area();
    let msg = Paragraph::new("terminal too small (need ≥20×3)").alignment(Alignment::Center);
    frame.render_widget(msg, area);
}

fn render(app: &App, state: &mut EditorState, frame: &mut Frame) {
    let area = frame.area();
    // Spike-6 small-terminal guard (`CLAUDE.md` §9): the 3-row layout
    // (1 + Min(1) + 1) garbles below 3 rows or 20 columns. Skip the normal
    // layout, fuzzy popup, and overlay, and show a single centered message.
    if area.height < 3 || area.width < 20 {
        // Ratatui auto-hides the cursor for any frame where no position is set
        // (`Terminal::draw` ⇒ `None => hide_cursor()`), so this branch leaves a
        // clean cursor-less surface — no explicit `Hide` needed here.
        render_too_small(frame);
        return;
    }
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Min(1),
            Constraint::Length(1),
        ])
        .split(area);

    render_path_bar(state, frame, chunks[0]);
    // `render_editor` records viewport height into `state.view_height` for the
    // mouse-scroll clamp. Cursor-follow (`adjust_scroll`) runs after each edit
    // key, not here, so a reader can mouse-scroll away from the cursor.
    render_editor(app, state, frame, chunks[1]);
    render_status(state, frame, chunks[2]);

    if state.mode == Mode::FuzzyOpen {
        render_fuzzy_popup(state, frame);
    }
    if state.overlay.is_some() {
        render_image_overlay(state, frame);
    }
}

fn render_path_bar(state: &EditorState, frame: &mut Frame, area: Rect) {
    let line = Line::from(vec![Span::styled(
        format!(" {} ", state.path.as_str()),
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD),
    )]);
    let para = Paragraph::new(line).style(Style::default().bg(Color::DarkGray));
    frame.render_widget(para, area);
}

fn render_editor(app: &App, state: &mut EditorState, frame: &mut Frame, area: Rect) -> usize {
    let block = Block::default()
        .borders(Borders::LEFT)
        .border_style(Style::default().fg(Color::DarkGray));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let height = inner.height as usize;
    state.view_height = height;
    // Stash the inner rect so mouse events (which arrive in screen coords) can
    // map back to a buffer cell (P3). Origin + width; height == view_height.
    state.editor_x = inner.x;
    state.editor_y = inner.y;
    state.editor_width = inner.width;

    // Each buffer line is exactly one visual row — NO wrapping — so a logical
    // line's screen row is exact and the cursor column maps cleanly to a display
    // column. Image-embed tokens render as an inline `[image: name]` glyph
    // (§2.4 "editor surface"); the buffer itself is untouched (transparent
    // editing — the real token is what gets saved). The active selection (if
    // any) is reconstructed once and rendered as reverse video per line (C4);
    // `line_idx` is ABSOLUTE (scroll offset + visible index) so selection line
    // ranges line up with the buffer, not the viewport.
    let selection = state.selection();
    let width = inner.width as usize;
    let visible: Vec<Line> = state
        .lines
        .iter()
        .skip(state.scroll)
        .take(height)
        .enumerate()
        .map(|(i, l)| render_line(app, l, state.scroll + i, selection, width))
        .collect();
    let para = Paragraph::new(visible);
    frame.render_widget(para, inner);

    // Place the terminal cursor on the current char's DISPLAY column. `cx` is a
    // char index into the buffer line; we translate it through glyph
    // substitution + unicode display width so wide chars (CJK/emoji = 2 cols)
    // and image glyphs land in the right column. Only positioned when the
    // cursor's row is inside the viewport — a reader can mouse-scroll away from
    // it (§2.4/§9) and the cursor must not be drawn on a stale row.
    if state.mode == Mode::Edit && state.cy >= state.scroll && state.cy < state.scroll + height {
        let line = state.cur_line();
        let col = cursor_display_col(app, line, state.cx);
        let x = inner.x + col.min(inner.width as usize) as u16;
        let y = inner.y + (state.cy - state.scroll) as u16;
        frame.set_cursor_position((x, y));
    }
    height
}

fn render_status(state: &EditorState, frame: &mut Frame, area: Rect) {
    // Honest keymap: `Enter` both inserts a newline (normal lines) and opens
    // the image preview (image-embed lines), so advertise its dual role rather
    // than the misleading "Enter=image". Esc is intentionally NOT a quit key
    // (see `handle_edit_event`), so it's omitted. `^K` shows contextually in
    // the CONFLICT status label. Wheel scroll is implied for a terminal mouse.
    //
    // When a selection is active, swap in a compact selection hint so the
    // copy/cut/replace keys are discoverable exactly when they matter (the
    // full hint is too long for narrow terminals; the contextual swap is the
    // width-aware compromise). Ctrl+C stays "quit", so copy is ^Shift+C.
    let hint = if state.selection().is_some() {
        " selection: ^Shift+C copy · ^X cut · type=replace · Esc clear"
    } else {
        " ^S save · ^O open · ^P paste · ^D del-img · ^R reload · Enter newline/image · ^Q quit"
    };
    let line = Line::from(vec![
        Span::styled(
            format!(" {} ", state.status.label()),
            Style::default().fg(state.status.color()),
        ),
        Span::styled(hint.to_string(), Style::default().fg(Color::DarkGray)),
        Span::raw(if let Some((_, m)) = &state.message {
            format!("   {m}")
        } else {
            String::new()
        }),
    ]);
    let para = Paragraph::new(line)
        .style(Style::default().bg(Color::Black))
        .alignment(Alignment::Left);
    frame.render_widget(para, area);
    let _ = state.title; // (reserved for future breadcrumb)
}

fn render_fuzzy_popup(state: &EditorState, frame: &mut Frame) {
    let area = centered_rect(70, 50, frame.area());
    let title = format!(" open: {} ", state.fuzzy_query);
    let block = Block::default().borders(Borders::ALL).title(Span::styled(
        title,
        Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::BOLD),
    ));
    frame.render_widget(Clear, area);

    let rows = area.height.saturating_sub(2) as usize;
    let items: Vec<Line> = state
        .fuzzy_results
        .iter()
        .take(rows)
        .enumerate()
        .map(|(i, n)| {
            let marker = if i == state.fuzzy_sel { "› " } else { "  " };
            let text = format!("{marker}{} — {}", n.title, n.path.as_str());
            let style = if i == state.fuzzy_sel {
                Style::default().fg(Color::Black).bg(Color::Yellow)
            } else {
                Style::default()
            };
            Line::from(Span::styled(text, style))
        })
        .collect();
    let para = Paragraph::new(items).block(block);
    frame.render_widget(para, area);
}

/// Inline image-glyph render (`CLAUDE.md` §2.4 "editor surface"): each embed
/// token shows as `[image: name]` while the buffer keeps the real Markdown.
fn glyph_for(name: &str) -> String {
    format!("[image: {name}]")
}

/// Non-overlapping, byte-sorted `(start, end, glyph)` spans of image-embed
/// tokens on `line`. Tokens are matched in the buffer's PARSED style, not the
/// configured one, so Obsidian `![[…]]` embeds are found regardless of config.
/// Each reference's token is located at ALL of its non-overlapping occurrences,
/// so a line embedding the same image twice gets two glyphs.
fn glyph_spans(app: &App, line: &str) -> Vec<(usize, usize, String)> {
    let mut spans: Vec<(usize, usize, String)> = Vec::new();
    for rf in app.attachment_links(line) {
        let token =
            crate::domain::attachment::AttachmentReference::render_token(rf.style, &rf.target);
        let glyph = glyph_for(basename(&rf.target.as_str()));
        let mut from = 0usize;
        while let Some(rel) = line[from..].find(&token) {
            let start = from + rel;
            let end = start + token.len();
            from = end;
            // Skip occurrences overlapping an already-found span (nested/dup refs).
            if spans
                .iter()
                .any(|(s, e, _): &(usize, usize, String)| start < *e && end > *s)
            {
                continue;
            }
            spans.push((start, end, glyph.clone()));
        }
    }
    spans.sort_by_key(|(s, _, _)| *s);
    spans
}

/// Render a buffer line with image tokens shown as inline `[image: name]` glyphs,
/// and the active selection in reverse video (C4). `line_idx` is the ABSOLUTE
/// line index (so `line_selection` can compare against selection line ranges);
/// `selection` is the reconstructed editor selection; `width` is the viewport
/// width (a blank line inside a multiline selection renders a full-width
/// highlight bar — C4).
fn render_line(
    app: &App,
    line: &str,
    line_idx: usize,
    selection: Option<Selection>,
    width: usize,
) -> Line<'static> {
    let spans = glyph_spans(app, line);
    let ls = line_selection(line_idx, char_count(line), selection);
    render_line_from_spans(line, &spans, ls, width)
}

/// Kind of a render segment: a raw text slice from the buffer, or a substituted
/// image glyph (which does NOT echo the buffer bytes — it shows `[image: name]`).
#[derive(Debug, Clone)]
enum SegKind {
    Text,
    Glyph(String),
}

/// One contiguous render segment with both its BYTE range (to slice the buffer
/// for `Text`) and its CHAR range (to test selection overlap — selection cols
/// are char indices, C1). Kept private to [`render_line_from_spans`].
struct Seg {
    byte_a: usize,
    byte_b: usize,
    char_a: usize,
    char_b: usize,
    kind: SegKind,
}

/// Pure core of [`render_line`] (no `App` → unit-testable). Builds the line's
/// spans with image glyphs in magenta and the selection's char range on this
/// line in reverse video, per C4: glyph ranges are atomic (any overlap → whole
/// glyph reverses); text is split at the selection boundaries; a selected BLANK
/// line renders a full-width reversed bar so it's visible inside a multiline
/// selection.
fn render_line_from_spans(
    line: &str,
    spans: &[(usize, usize, String)],
    ls: LineSel,
    width: usize,
) -> Line<'static> {
    let reverse = Style::default().add_modifier(Modifier::REVERSED);
    let glyph_style = Style::default().fg(Color::Magenta);
    let glyph_style_sel = glyph_style.patch(Style::default().add_modifier(Modifier::REVERSED));

    // Selection char-range on this line. `sel_b = usize::MAX` makes the
    // "to-EOL" / Full cases overlap-any-end without special-casing.
    let sel_active = !matches!(ls, LineSel::None);
    let (sel_a, sel_b) = match ls {
        LineSel::None => (1usize, 0usize), // a > b → never overlaps
        LineSel::Range(a, b) => (a, b),
        LineSel::Full => (0, usize::MAX),
    };
    let overlaps = |c_a: usize, c_b: usize| c_a < sel_b && c_b > sel_a;

    // 1. Build segments (text gaps + glyphs) in document order with char ranges.
    let mut segs: Vec<Seg> = Vec::new();
    let mut cur_byte = 0usize;
    let mut char_cursor = 0usize;
    for (start, end, glyph) in spans {
        if *start > cur_byte {
            let text = &line[cur_byte..*start];
            let n = char_count(text);
            segs.push(Seg {
                byte_a: cur_byte,
                byte_b: *start,
                char_a: char_cursor,
                char_b: char_cursor + n,
                kind: SegKind::Text,
            });
            char_cursor += n;
            // `cur_byte` is advanced to `*end` below; the gap up to `*start`
            // is consumed by this segment, so no separate update is needed.
        }
        let n = char_count(&line[*start..*end]);
        segs.push(Seg {
            byte_a: *start,
            byte_b: *end,
            char_a: char_cursor,
            char_b: char_cursor + n,
            kind: SegKind::Glyph(glyph.clone()),
        });
        char_cursor += n;
        cur_byte = *end;
    }
    if cur_byte < line.len() {
        let text = &line[cur_byte..line.len()];
        let n = char_count(text);
        segs.push(Seg {
            byte_a: cur_byte,
            byte_b: line.len(),
            char_a: char_cursor,
            char_b: char_cursor + n,
            kind: SegKind::Text,
        });
    }

    // 2. Style each segment against the selection.
    let mut out: Vec<Span<'static>> = Vec::new();
    for seg in &segs {
        match &seg.kind {
            SegKind::Glyph(g) => {
                let style = if sel_active && overlaps(seg.char_a, seg.char_b) {
                    glyph_style_sel
                } else {
                    glyph_style
                };
                out.push(Span::styled(g.clone(), style));
            }
            SegKind::Text => {
                let text = &line[seg.byte_a..seg.byte_b];
                let seg_chars = seg.char_b - seg.char_a;
                if !sel_active || !overlaps(seg.char_a, seg.char_b) {
                    out.push(Span::raw(text.to_string()));
                    continue;
                }
                // Split this text segment at the selection boundaries (char
                // offsets within the segment), translating to bytes (C1).
                let local_a = sel_a.saturating_sub(seg.char_a).min(seg_chars);
                let local_b = sel_b.saturating_sub(seg.char_a).min(seg_chars);
                let b_lo = char_to_byte(text, local_a);
                let b_hi = char_to_byte(text, local_b);
                if local_a > 0 {
                    out.push(Span::raw(text[..b_lo].to_string()));
                }
                if local_b > local_a {
                    out.push(Span::styled(text[b_lo..b_hi].to_string(), reverse));
                }
                if local_b < seg_chars {
                    out.push(Span::raw(text[b_hi..].to_string()));
                }
            }
        }
    }

    // 3. A selected BLANK line has no segments — render a full-width reversed
    // bar so it's visible inside a multiline selection (C4).
    if out.is_empty() && sel_active && matches!(ls, LineSel::Full) {
        let bar = " ".repeat(width.max(1));
        out.push(Span::styled(bar, reverse));
    }

    Line::from(out)
}

/// Display column of the buffer char at index `cx` on `line`, accounting for
/// unicode display width AND glyph substitution. When the cursor sits inside a
/// token (only reachable by arrowing into a glyph mid-token), it snaps to that
/// glyph's right edge — where it lands after the next keystroke anyway.
fn cursor_display_col(app: &App, line: &str, cx: usize) -> usize {
    display_col_from_spans(line, &glyph_spans(app, line), cx)
}

/// Pure core of [`cursor_display_col`]: the display column of char `cx` given
/// precomputed glyph spans. Separated from `app` so it is unit-testable.
fn display_col_from_spans(line: &str, spans: &[(usize, usize, String)], cx: usize) -> usize {
    let byte_cx = char_to_byte(line, cx.min(char_count(line)));
    let mut col = 0usize;
    let mut cur = 0usize;
    for (start, end, glyph) in spans {
        if *start >= byte_cx {
            break; // token at/after the cursor — stop before it.
        }
        let text_end = (*start).min(byte_cx);
        if text_end > cur {
            col += UnicodeWidthStr::width(&line[cur..text_end]);
        }
        // Token begins before the cursor → count its glyph's display width.
        col += UnicodeWidthStr::width(glyph.as_str());
        cur = *end;
        if *end > byte_cx {
            break; // cursor is inside this token.
        }
    }
    if cur < byte_cx {
        col += UnicodeWidthStr::width(&line[cur..byte_cx]);
    }
    col
}

/// Inverse of [`display_col_from_spans`] (contract C2): the CHAR index whose
/// display cell contains `display_col`. Walks the same text/glyph layout but
/// sums widths until the target cell is reached.
///
/// Rules (C2): a wide char's mid-cell click snaps LEFT (returns that char's
/// index); a click anywhere on an image-glyph returns the token's START char
/// (the glyph is atomic — same rule as render C4); a click past EOL clamps to
/// `char_count(line)`. `display_col` is a display column (terminal cells), not
/// a char index.
fn display_col_to_char_from_spans(
    line: &str,
    spans: &[(usize, usize, String)],
    display_col: usize,
) -> usize {
    let mut col = 0usize; // running display width consumed
    let mut char_idx = 0usize; // running char count consumed
    let mut cur_byte = 0usize;
    for (start, end, glyph) in spans {
        if *start > cur_byte {
            let text = &line[cur_byte..*start];
            if let Some(off) = char_at_display(text, col, display_col) {
                return char_idx + off;
            }
            col += UnicodeWidthStr::width(text);
            char_idx += char_count(text);
            // `cur_byte` is advanced to `*end` below; the gap up to `*start` is
            // consumed here, so no separate update is needed.
        }
        let glyph_w = UnicodeWidthStr::width(glyph.as_str());
        if display_col < col + glyph_w {
            // Click lands on the glyph → its token's start char (atomic, C2/C4).
            return char_idx;
        }
        col += glyph_w;
        char_idx += char_count(&line[*start..*end]);
        cur_byte = *end;
    }
    if cur_byte < line.len() {
        let text = &line[cur_byte..line.len()];
        if let Some(off) = char_at_display(text, col, display_col) {
            return char_idx + off;
        }
        char_idx += char_count(text);
    }
    // Past EOL → clamp to the char count.
    char_idx
}

/// Char offset within `text` of the cell containing `display_col`, given the
/// display width `col0` already consumed before `text`. Wide-char mid-cell →
/// snaps LEFT (returns that char's offset); zero-width chars are stepped over
/// (their cell is empty, so a click can't land on them). `None` if `display_col`
/// is past `text`'s last cell.
fn char_at_display(text: &str, col0: usize, display_col: usize) -> Option<usize> {
    let mut col = col0;
    for (off, c) in text.chars().enumerate() {
        let w = UnicodeWidthChar::width(c).unwrap_or(0);
        // Invariant: display_col >= col (we'd have returned earlier otherwise),
        // so the click is on this char's cell iff display_col < col + w (w>0).
        if display_col < col + w {
            return Some(off);
        }
        col += w;
    }
    None
}

/// Inverse of [`cursor_display_col`] for mouse hits: the char index at display
/// column `display_col` on `line`, grapheme-snapped (C7) so a click lands on a
/// grapheme boundary (a click on the right half of a ZWJ emoji still anchors at
/// its start).
fn display_col_to_char(app: &App, line: &str, display_col: usize) -> usize {
    let spans = glyph_spans(app, line);
    let c = display_col_to_char_from_spans(line, &spans, display_col);
    snap_to_grapheme_start(line, c)
}

/// Trailing path segment of an attachment target (file basename).
fn basename(path: &str) -> &str {
    path.rsplit(['/', '\\']).next().unwrap_or(path)
}

/// Full-screen image preview modal (`CLAUDE.md` §2.4). Renders the image via
/// `ratatui-image` when a graphics protocol is available, else a text surface
/// with the spec'd filename / dimensions / size / open action.
fn render_image_overlay(state: &mut EditorState, frame: &mut Frame) {
    let area = centered_rect(80, 70, frame.area());
    frame.render_widget(Clear, area);
    let Some(ov) = state.overlay.as_mut() else {
        return;
    };
    let header = ov.header();
    let block = Block::default().borders(Borders::ALL).title(Span::styled(
        format!(" image: {header} · Esc close · o open GUI "),
        Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::BOLD),
    ));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    match ov {
        ImageOverlay::Rendered { proto, .. } => {
            let img = StatefulImage::default().resize(Resize::Fit(None));
            frame.render_stateful_widget(img, inner, proto);
        }
        ImageOverlay::Fallback {
            path,
            width,
            height,
            size,
            reason,
            ..
        } => {
            let lines = vec![
                Line::from(Span::styled(
                    format!(" {}", path.as_str()),
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                )),
                Line::from(""),
                Line::from(format!(" dimensions : {width}×{height}")),
                Line::from(format!(" size       : {}", format_size(*size))),
                Line::from(""),
                Line::from(Span::styled(
                    format!(" no inline preview — {reason}"),
                    Style::default().fg(Color::DarkGray),
                )),
                Line::from(""),
                Line::from("  o             open in Obsidian GUI"),
                Line::from("  Esc / Enter   close"),
            ];
            frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), inner);
        }
    }
}

/// Centered popup rect (percent of width / height).
fn centered_rect(percent_x: u16, percent_y: u16, area: Rect) -> Rect {
    let pop_w = area.width * percent_x / 100;
    let pop_h = area.height * percent_y / 100;
    let x = area.x + area.width.saturating_sub(pop_w) / 2;
    let y = area.y + area.height.saturating_sub(pop_h) / 2;
    Rect::new(x, y, pop_w, pop_h)
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

    #[test]
    fn centered_rect_is_centered() {
        let area = Rect::new(0, 0, 100, 40);
        let r = centered_rect(80, 50, area);
        assert_eq!(r.width, 80);
        assert_eq!(r.height, 20);
        // 100→80 leaves 20, split 10/10 → x == 10.
        assert_eq!(r.x, 10);
        // 40→20 leaves 20, split 10/10 → y == 10.
        assert_eq!(r.y, 10);
    }

    #[test]
    fn glyph_for_wraps_basename() {
        assert_eq!(glyph_for("seal.png"), "[image: seal.png]");
        assert_eq!(glyph_for("a b.png"), "[image: a b.png]");
    }

    #[test]
    fn basename_strips_directories() {
        assert_eq!(basename("Attachments/2026/07/img-x.png"), "img-x.png");
        assert_eq!(basename("top.png"), "top.png");
        // Windows separator tolerated.
        assert_eq!(basename("Attachments\\sub\\win.png"), "win.png");
        assert_eq!(basename(""), "");
    }

    #[test]
    fn display_col_ascii_no_spans() {
        // No glyph spans: column == char count (== byte count for ASCII).
        assert_eq!(display_col_from_spans("hello", &[], 0), 0);
        assert_eq!(display_col_from_spans("hello", &[], 2), 2);
        assert_eq!(display_col_from_spans("hello", &[], 5), 5);
        // Past-end clamps to end.
        assert_eq!(display_col_from_spans("hi", &[], 99), 2);
    }

    #[test]
    fn display_col_wide_char_counts_columns() {
        // CJK ideograph is 2 display columns; cursor after it lands at col 2.
        assert_eq!(display_col_from_spans("語", &[], 1), 2);
        // Mixed: 'a' (1) + '語' (2) → cursor at char 2 is column 3.
        assert_eq!(display_col_from_spans("a語", &[], 2), 3);
    }

    #[test]
    fn display_col_substitutes_glyph_for_token() {
        // Buffer: "x ![](img.png) y"; token "![](img.png)" is 12 bytes at 2..14.
        // Glyph "[image: img.png]" has display width 16.
        let line = "x ![](img.png) y";
        let glyph = glyph_for("img.png");
        let spans = vec![(2usize, 14usize, glyph.clone())];
        // Before token (char 0) → col 0; at 'x'+space (char 2, byte 2) → col 2.
        assert_eq!(display_col_from_spans(line, &spans, 0), 0);
        assert_eq!(display_col_from_spans(line, &spans, 2), 2);
        // Right after the token (byte 14) → 2 (prefix) + 16 (glyph) = 18.
        let after = char_count(&line[..14]);
        assert_eq!(display_col_from_spans(line, &spans, after), 18);
        // Inside the token snaps to the glyph's right edge (18).
        assert_eq!(display_col_from_spans(line, &spans, 5), 18);
    }

    #[test]
    fn display_col_handles_two_adjacent_glyphs() {
        // Two identical tokens: "![](a.png)![](a.png)"; each token is 10 bytes
        // (glyph "[image: a.png]" is 14 wide). Spans (0,10) and (10,20).
        let line = "![](a.png)![](a.png)";
        let g = glyph_for("a.png");
        let spans = vec![(0usize, 10usize, g.clone()), (10usize, 20usize, g)];
        assert_eq!(display_col_from_spans(line, &spans, 0), 0);
        // Right after the first token (char 10) → 14.
        assert_eq!(display_col_from_spans(line, &spans, 10), 14);
        // Right after the second token (char 20) → 28.
        assert_eq!(display_col_from_spans(line, &spans, 20), 28);
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

    /// `TestBackend` render of the §9 small-terminal guard: proves the guard's
    /// message renders panic-free in a tiny (10×2) area and lands in the drawn
    /// buffer. Calls the App-free `render_too_small` directly — the full
    /// `render(app, state, frame)` routing (which also exercises the
    /// `area.height < 3 || area.width < 20` decision) needs a full `App` (8+
    /// adapter deps) and so is left to an integration harness.
    #[test]
    fn small_terminal_guard_renders_message_without_panic() {
        use ratatui::backend::TestBackend;
        let backend = TestBackend::new(10, 2); // width<20, height<3
        let mut terminal = ratatui::Terminal::new(backend).expect("test terminal");
        terminal
            .draw(render_too_small)
            .expect("guard render must not panic in a tiny area");
        let rendered: String = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<Vec<&str>>()
            .join("");
        // The full "terminal too small (need …)" message is wider than the
        // 10-col test area, so ratatui truncates it to the leading columns.
        // Assert the leading word ("terminal") is present — that proves the
        // guard branch fired and rendered its message without panicking.
        assert!(
            rendered.contains("terminal"),
            "guard message missing from drawn buffer: {rendered:?}"
        );
    }

    // ── Selection: reverse-video render (P2, contract C4) ──────────────────
    //
    // The full `render(app, state, frame)` path needs a full `App` (8+ adapter
    // deps) and is left to the integration harness — same boundary the §9
    // small-terminal guard test uses. The reverse-video LOGIC lives entirely in
    // the App-free `render_line_from_spans` core, which we exercise here by
    // inspecting the returned spans' styles.

    /// Concatenate the contents of every reversed span on the line.
    fn reversed_contents(line: &Line) -> String {
        line.spans
            .iter()
            .filter(|s| s.style.add_modifier.contains(Modifier::REVERSED))
            .map(|s| s.content.as_ref())
            .collect()
    }

    #[test]
    fn render_reverses_partial_text_selection() {
        // "hello", select cols [1,3) = "el" → reversed; the rest stays raw.
        let line = render_line_from_spans("hello", &[], LineSel::Range(1, 3), 10);
        assert_eq!(reversed_contents(&line), "el");
        let all: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(all, "hello");
    }

    #[test]
    fn render_reverses_whole_line_and_blank_bar() {
        // Full line → all text reversed.
        let full = render_line_from_spans("hi", &[], LineSel::Full, 10);
        assert_eq!(reversed_contents(&full), "hi");
        // Blank line in a multiline selection → full-width reversed bar (C4).
        let bar = render_line_from_spans("", &[], LineSel::Full, 5);
        assert_eq!(
            reversed_contents(&bar),
            "     ",
            "blank selected line pads to width"
        );
    }

    /// An image glyph is ATOMIC under selection: any overlap reverses the whole
    /// `[image: …]` token (C4), never a partial slice of its glyph text.
    #[test]
    fn render_glyph_is_atomic_under_selection() {
        // Glyph covers buffer chars [1,4) ("bcd" → shown as "[img]"); select only
        // char [2,3) — strictly inside the glyph. The whole glyph reverses, the
        // leading "a" (char 0, outside the range) does not.
        let spans = [(1usize, 4usize, "[img]".to_string())];
        let line = render_line_from_spans("abcdef", &spans, LineSel::Range(2, 3), 10);
        let glyph_span = line
            .spans
            .iter()
            .find(|s| s.content.as_ref() == "[img]")
            .expect("glyph present");
        assert!(
            glyph_span.style.add_modifier.contains(Modifier::REVERSED),
            "whole glyph reverses on partial overlap"
        );
        let a_span = line
            .spans
            .iter()
            .find(|s| s.content.as_ref() == "a")
            .expect("leading text present");
        assert!(
            !a_span.style.add_modifier.contains(Modifier::REVERSED),
            "char outside selection is not reversed"
        );
    }

    // ── Selection: inverse display-col→char map (P3, contract C2) ──────────

    /// Round-trip on plain text (no glyphs): for every char index,
    /// `inverse(forward(c)) == c`. CJK (2-wide) chars are clean inverses at
    /// their left edges; a mid-cell click snaps LEFT.
    #[test]
    fn inverse_map_round_trips_plain_text() {
        for (line, len) in [("abc", 3usize), ("你好", 2usize), ("a好b", 3usize)] {
            for c in 0..=len {
                let d = display_col_from_spans(line, &[], c);
                let back = display_col_to_char_from_spans(line, &[], d);
                assert_eq!(
                    back, c,
                    "round-trip char {c} on {line:?}: fwd={d} back={back}"
                );
            }
        }
        // CJK mid-cell snaps LEFT (the wide char's own index).
        assert_eq!(display_col_to_char_from_spans("你好", &[], 1), 0);
        assert_eq!(display_col_to_char_from_spans("你好", &[], 3), 1);
        // Past EOL clamps to char_count.
        assert_eq!(display_col_to_char_from_spans("abc", &[], 99), 3);
    }

    /// An image glyph is atomic in the inverse map too: a click anywhere on its
    /// cells returns the token's START char (C2/C4), never an interior char.
    #[test]
    fn inverse_map_glyph_is_atomic_token_start() {
        // Glyph over chars [1,3) ("bc" → "[img]", 5 cells). Layout:
        //   a@col0 · glyph@col1..6 (chars 1,2) · d@col6 (char 3)
        // char 2 ('c') lives inside the glyph and is unreachable as a hit.
        let spans = [(1usize, 3usize, "[img]".to_string())];
        let line = "abcd";
        // Any of the glyph's 5 cells → token start char (1).
        for d in 1..6 {
            assert_eq!(
                display_col_to_char_from_spans(line, &spans, d),
                1,
                "glyph cell {d} → token start char"
            );
        }
        // Surrounding text maps to its own chars.
        assert_eq!(display_col_to_char_from_spans(line, &spans, 0), 0, "'a'");
        assert_eq!(display_col_to_char_from_spans(line, &spans, 6), 3, "'d'");
        assert_eq!(
            display_col_to_char_from_spans(line, &spans, 99),
            4,
            "past EOL clamps to char_count"
        );
    }

    /// A combining mark is zero-width: its cell can't be hit, so a click past
    /// the base char's cell clamps to the line end (the grapheme-snap applied
    /// in `display_col_to_char` then bounds it to a grapheme edge at the call
    /// site — tested via the pure map here without snap).
    #[test]
    fn inverse_map_skips_zero_width_combining_mark() {
        let line = "e\u{0301}"; // 'e' (1 cell) + combining acute (0 cells)
        assert_eq!(
            display_col_to_char_from_spans(line, &[], 0),
            0,
            "base char cell"
        );
        // Col 1 is past the single visible cell → clamp to char_count (2).
        assert_eq!(display_col_to_char_from_spans(line, &[], 1), 2);
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

//! Minimal Ratatui/Crossterm editor (`CLAUDE.md` §2.2, Spike 2).
//!
//! A single-pane text editor over the open note: path bar on top, editor in the
//! middle, a centralized status line at the bottom. Ctrl+S saves (§7 conflict
//! surfaced in the status), Ctrl+O fuzzy-opens, Ctrl+P pastes an image token,
//! Ctrl+D deletes the image token under the cursor, Ctrl+R reloads, Ctrl+K
//! writes a conflict copy, Ctrl+Q quits. Enter over an image line opens the
//! full-screen preview modal (§2.4); mouse wheel scrolls the viewport.
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
use std::time::{Duration, Instant};

use anyhow::Result;
use crossterm::cursor::Show;
use crossterm::event::{
    self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEvent, KeyEventKind,
    KeyModifiers, MouseEvent, MouseEventKind,
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
use unicode_width::UnicodeWidthStr;

use crate::application::ops::SaveOutcome;
use crate::application::App;
use crate::domain::note::NoteDocument;
use crate::domain::session::ExternalChange;
use crate::domain::vault::RelativeNotePath;

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
enum SyncStatus {
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
enum Mode {
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
enum ImageOverlay {
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
fn format_size(bytes: u64) -> String {
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

pub struct EditorState {
    path: RelativeNotePath,
    title: String,
    lines: Vec<String>,
    cx: usize,
    cy: usize,
    scroll: usize,
    status: SyncStatus,
    mode: Mode,
    fuzzy_query: String,
    fuzzy_results: Vec<crate::domain::note::NoteSummary>,
    fuzzy_sel: usize,
    message: Option<(Instant, String)>,
    /// Terminal graphics-protocol detector (`None` when none is available, e.g.
    /// a plain `dumb` terminal or piped stdout). The image modal degrades to a
    /// text fallback in that case (`CLAUDE.md` §2.4).
    picker: Option<Picker>,
    /// Active full-screen image preview, if any.
    overlay: Option<ImageOverlay>,
    /// Editor viewport height from the last render (for mouse-scroll clamping).
    view_height: usize,
}

impl EditorState {
    fn from_doc(doc: NoteDocument) -> Self {
        let title = doc.title.as_str().to_string();
        let path = doc.path.clone();
        let lines = if doc.body.as_str().is_empty() {
            vec![String::new()]
        } else {
            doc.body.as_str().split('\n').map(String::from).collect()
        };
        Self {
            path,
            title,
            lines,
            cx: 0,
            cy: 0,
            scroll: 0,
            status: SyncStatus::Clean,
            mode: Mode::Edit,
            fuzzy_query: String::new(),
            fuzzy_results: Vec::new(),
            fuzzy_sel: 0,
            message: None,
            picker: None,
            overlay: None,
            view_height: 0,
        }
    }

    fn body(&self) -> String {
        self.lines.join("\n")
    }

    fn reload(&mut self, doc: NoteDocument) {
        self.title = doc.title.as_str().to_string();
        self.path = doc.path.clone();
        self.lines = if doc.body.as_str().is_empty() {
            vec![String::new()]
        } else {
            doc.body.as_str().split('\n').map(String::from).collect()
        };
        self.cx = 0;
        self.cy = 0;
        self.scroll = 0;
        self.status = SyncStatus::Clean;
    }

    fn cur_line(&self) -> &str {
        self.lines.get(self.cy).map(|s| s.as_str()).unwrap_or("")
    }

    fn mark_dirty(&mut self) {
        if self.status != SyncStatus::Conflict {
            self.status = SyncStatus::Dirty;
        }
    }

    fn insert_char(&mut self, c: char) {
        if let Some(line) = self.lines.get_mut(self.cy) {
            // cx is a CHAR index; clamp + translate to a byte boundary before
            // mutating so multibyte sequences (CJK, emoji) never split a code
            // point (which would panic `String::insert`).
            self.cx = self.cx.min(char_count(line));
            let byte_idx = char_to_byte(line, self.cx);
            line.insert(byte_idx, c);
            self.cx += 1;
        }
        self.mark_dirty();
    }

    fn insert_newline(&mut self) {
        if self.cy >= self.lines.len() {
            return;
        }
        let line = self.lines[self.cy].clone();
        let byte_idx = char_to_byte(&line, self.cx.min(char_count(&line)));
        let (left, right) = line.split_at(byte_idx);
        self.lines[self.cy] = left.to_string();
        self.lines.insert(self.cy + 1, right.to_string());
        self.cy += 1;
        self.cx = 0;
        self.mark_dirty();
    }

    fn backspace(&mut self) {
        if self.cx == 0 {
            if self.cy > 0 {
                // Merge into the previous line; cursor lands at its end (char count).
                let prev_len = char_count(&self.lines[self.cy - 1]);
                let cur = self.lines.remove(self.cy);
                self.cy -= 1;
                self.lines[self.cy].push_str(&cur);
                self.cx = prev_len;
                self.mark_dirty();
            }
            return;
        }
        if let Some(line) = self.lines.get_mut(self.cy) {
            // Remove the code point immediately before the cursor (char-based,
            // not byte-based) so emoji/CJK delete as one unit.
            let end_byte = char_to_byte(line, self.cx);
            let start_byte = char_to_byte(line, self.cx - 1);
            line.replace_range(start_byte..end_byte, "");
            self.cx -= 1;
        }
        self.mark_dirty();
    }

    fn move_cursor(&mut self, code: KeyCode) {
        match code {
            KeyCode::Left => {
                if self.cx > 0 {
                    self.cx -= 1;
                } else if self.cy > 0 {
                    self.cy -= 1;
                    self.cx = char_count(self.cur_line());
                }
            }
            KeyCode::Right => {
                let len = char_count(self.cur_line());
                if self.cx < len {
                    self.cx += 1;
                } else if self.cy + 1 < self.lines.len() {
                    self.cy += 1;
                    self.cx = 0;
                }
            }
            KeyCode::Up => {
                if self.cy > 0 {
                    self.cy -= 1;
                    self.cx = self.cx.min(char_count(self.cur_line()));
                }
            }
            KeyCode::Down => {
                if self.cy + 1 < self.lines.len() {
                    self.cy += 1;
                    self.cx = self.cx.min(char_count(self.cur_line()));
                }
            }
            KeyCode::Home => self.cx = 0,
            KeyCode::End => self.cx = char_count(self.cur_line()),
            _ => {}
        }
    }

    fn adjust_scroll(&mut self, view_height: usize) {
        if view_height == 0 {
            return;
        }
        if self.cy < self.scroll {
            self.scroll = self.cy;
        }
        if self.cy >= self.scroll + view_height {
            self.scroll = self.cy + 1 - view_height;
        }
    }

    fn toast(&mut self, msg: impl Into<String>) {
        self.message = Some((Instant::now(), msg.into()));
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
                Event::Mouse(mouse) => handle_mouse(state, mouse),
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
    if key.modifiers.contains(KeyModifiers::CONTROL) {
        match key.code {
            // Quit works even with the overlay up.
            KeyCode::Char('q') | KeyCode::Char('c') => return Ok(Control::Quit),
            _ => {}
        }
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
    if key.modifiers.contains(KeyModifiers::CONTROL) {
        match key.code {
            KeyCode::Char('s') => return save(app, state),
            KeyCode::Char('o') => {
                state.mode = Mode::FuzzyOpen;
                state.fuzzy_query.clear();
                refresh_fuzzy(app, state)?;
                return Ok(Control::Continue);
            }
            KeyCode::Char('p') => return paste_image(app, state),
            KeyCode::Char('r') => return reload(app, state),
            KeyCode::Char('k') => return conflict_copy(app, state),
            // ^D deletes the image token under the cursor (Spike 3).
            KeyCode::Char('d') => return delete_image_token(app, state),
            _ => return Ok(Control::Continue),
        }
    }

    match key.code {
        KeyCode::Char(c) => state.insert_char(c),
        // Enter on an image line opens the preview modal; otherwise newline.
        KeyCode::Enter => {
            if !try_open_image_overlay(app, state)? {
                state.insert_newline();
            }
        }
        KeyCode::Backspace => state.backspace(),
        KeyCode::Tab => {
            state.insert_char(' ');
            state.insert_char(' ');
        }
        KeyCode::Left
        | KeyCode::Right
        | KeyCode::Up
        | KeyCode::Down
        | KeyCode::Home
        | KeyCode::End => state.move_cursor(key.code),
        // `Esc` deliberately does nothing in edit mode: it's the universal
        // "cancel / back out" key, so binding it to quit (like a prior version
        // did) would discard unsaved work when a user reflexively taps it.
        // Quit is `^Q` / `^C` (above). `Esc` still closes the fuzzy picker and
        // the image modal (those handlers bind it explicitly).
        _ => {}
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

/// Reader-style viewport scroll on mouse wheel (Spike 2).
fn handle_mouse(state: &mut EditorState, mouse: MouseEvent) {
    match mouse.kind {
        MouseEventKind::ScrollUp => state.scroll = state.scroll.saturating_sub(3),
        MouseEventKind::ScrollDown => {
            let max = state.lines.len().saturating_sub(state.view_height.max(1));
            state.scroll = (state.scroll + 3).min(max);
        }
        _ => {}
    }
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

    // Each buffer line is exactly one visual row — NO wrapping — so a logical
    // line's screen row is exact and the cursor column maps cleanly to a display
    // column. Image-embed tokens render as an inline `[image: name]` glyph
    // (§2.4 "editor surface"); the buffer itself is untouched (transparent
    // editing — the real token is what gets saved).
    let visible: Vec<Line> = state
        .lines
        .iter()
        .skip(state.scroll)
        .take(height)
        .map(|l| render_line(app, l))
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
    let hint =
        " ^S save · ^O open · ^P paste · ^D del-img · ^R reload · Enter newline/image · ^Q quit";
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

/// Number of Unicode scalar values in `s` (cursor model is char-index, not byte).
fn char_count(s: &str) -> usize {
    s.chars().count()
}

/// Byte offset of the `char_idx`-th char in `s`, or `s.len()` if past the end.
/// All cursor edits route through this so a multibyte code point is never split.
fn char_to_byte(s: &str, char_idx: usize) -> usize {
    s.char_indices()
        .nth(char_idx)
        .map(|(b, _)| b)
        .unwrap_or_else(|| s.len())
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

/// Render a buffer line with image tokens shown as inline `[image: name]` glyphs.
fn render_line(app: &App, line: &str) -> Line<'static> {
    let spans = glyph_spans(app, line);
    if spans.is_empty() {
        return Line::from(line.to_string());
    }
    let glyph_style = Style::default().fg(Color::Magenta);
    let mut out: Vec<Span<'static>> = Vec::new();
    let mut cur = 0usize;
    for (start, end, glyph) in spans {
        if start > cur {
            out.push(Span::raw(line[cur..start].to_string()));
        }
        out.push(Span::styled(glyph, glyph_style));
        cur = end;
    }
    if cur < line.len() {
        out.push(Span::raw(line[cur..].to_string()));
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

    /// The whole reason cx is char-indexed: a 4-byte emoji is one cursor step,
    /// and its byte offset must round to a char boundary for `String::insert`.
    #[test]
    fn char_to_byte_maps_emoji_boundary() {
        let s = "a😀b"; // 1 + 4 + 1 bytes
        assert_eq!(char_count(s), 3);
        assert_eq!(char_to_byte(s, 0), 0); // 'a'
        assert_eq!(char_to_byte(s, 1), 1); // emoji start (not mid-codepoint)
        assert_eq!(char_to_byte(s, 2), 5); // 'b'
        assert_eq!(char_to_byte(s, 3), s.len()); // past end → len
        assert_eq!(char_to_byte(s, 99), s.len());
    }

    #[test]
    fn char_count_handles_empty_and_ascii() {
        assert_eq!(char_count(""), 0);
        assert_eq!(char_count("hello"), 5);
        assert_eq!(char_count("日本語"), 3);
    }

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
}

//! Minimal Ratatui/Crossterm editor (`CLAUDE.md` §2.2, Spike 2).
//!
//! A single-pane text editor over the open note: path bar on top, editor in the
//! middle, a centralized status line at the bottom. Ctrl+S saves (§7 conflict
//! surfaced in the status), Ctrl+O fuzzy-opens, Ctrl+P pastes an image token,
//! Ctrl+D deletes the image token under the cursor, Ctrl+R reloads, Ctrl+K
//! writes a conflict copy, Ctrl+Q quits. Enter over an image line opens the
//! full-screen preview modal (§2.4); mouse wheel scrolls the viewport.
//!
//! Explorer drawer (§3.2 `note_drawer`, Spike 7): on wide terminals (≥
//! `show_explorer_threshold` cols, default 100) a LEFT vault-tree pane auto-
//! shows beside the editor. Ctrl+E toggles its visibility + focus; when
//! focused, arrows navigate, Left/Right collapse/expand folders, Enter opens a
//! note (or toggles a folder), Esc returns focus to the editor. File ops
//! (P7.4): `n` new note, `N` new folder, `r` rename, `d` delete (y/n confirm) —
//! raw keys, only while the Explorer is focused, so they never collide with
//! editor typing. Pane-agnostic keys (Save/Reload/Open/…) work from either pane.
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
//! - **§3.2 preview/share drawers** are deferred: the LEFT Explorer drawer ships
//!   in Spike 7; the RIGHT outline + the share drawer layer on later.

use std::io::{self, Stdout};
use std::sync::mpsc::Receiver;
use std::time::Duration;

use anyhow::Result;
use crossterm::cursor::Show;
use crossterm::event::{
    self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEvent, KeyEventKind,
    KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
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

use crate::application::ops::{LinkResolution, SaveOutcome};
use crate::application::App;
use crate::domain::note::NoteDocument;
use crate::domain::session::ExternalChange;
use crate::domain::vault::{EntryKind, RelativeNotePath};

mod editor;
mod keymap;
mod layout;
mod note_drawer;
mod render;
use editor::*;
use keymap::*;
use layout::explorer_effective_visibility;
use note_drawer::ActivePane;
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
            Self::Conflict => "CONFLICT: ^R reload / ^K copy / ^Shift+K overwrite".into(),
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
    /// Name-entry modal for Explorer file ops (Spike 7 P7.4): `n` / `N` / `r`.
    /// Fixed keys (Esc/Enter/Backspace/printable), not in the keymap — mirrors
    /// the fuzzy picker.
    Prompt,
    /// y/n delete confirmation (Spike 7 P7.4): `d`. Fixed keys.
    Confirm,
}

/// Which Explorer file op a `Mode::Prompt` is collecting a name for (Spike 7
/// P7.4). New-note/new-folder start empty; rename is prefilled with the entry's
/// display name (note stem / folder name).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum PromptKind {
    NewNote,
    NewFolder,
    Rename,
}

/// A raw (non-keymapped) Explorer file-op key (Spike 7 P7.4). Dispatched only
/// when the Explorer is focused + visible, so the letters `n` / `r` / `d` can't
/// collide with editor typing — they're intercepted before the keymap resolves
/// them to `InsertChar`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FileOp {
    NewNote,
    NewFolder,
    Rename,
    Delete,
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
    // Spike 7 P7.2/P7.3: seed the Explorer tree, then two-way-sync the initial
    // note — select + reveal it so the pane opens already pointing at the note
    // the editor is showing. A load failure is non-fatal: the editor works
    // without it, and the pane refreshes on the first watch event.
    if let Ok(tree) = app.list_vault_tree() {
        state.explorer.set_tree(tree);
        // `path.as_str()` allocates (`RelativeNotePath` accessor); bind the owned
        // `String` so it outlives the borrowed `&str` we hand to `select_note`.
        let initial = state.path.as_str();
        state.explorer.select_note(&initial);
    }
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
            // `tree_dirty` collapses a burst of changes into ONE vault re-walk,
            // so a multi-file `git pull` / Obsidian Sync batch refreshes the
            // Explorer once, not once per file.
            let mut tree_dirty = false;
            while let Ok(change) = rx.try_recv() {
                app.sync_index_for(&change.note_path);
                tree_dirty = true;
                if let Some(open) = app.current() {
                    if open.path == change.note_path
                        && change.new_disk_hash != open.opened_hash
                        && state.status != SyncStatus::Conflict
                    {
                        state.status = SyncStatus::ChangedExternally;
                    }
                }
            }
            // Refresh the Explorer tree once per change-batch. `set_tree`
            // preserves the expanded-folder set + re-derives selection by
            // `rel_path`, so an external edit/rename/delete doesn't jump the
            // cursor or collapse the user's open folders.
            if tree_dirty {
                if let Ok(tree) = app.list_vault_tree() {
                    state.explorer.set_tree(tree);
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
        Mode::Prompt => handle_prompt_event(app, state, key),
        Mode::Confirm => handle_confirm_event(app, state, key),
        Mode::Edit => handle_edit_mode(app, state, key),
    }
}

/// Edit-mode key dispatch, pane-aware (Spike 7 P7.2). Pane-AGNOSTIC actions
/// (Save/Reload/Open/Paste/ConflictCopy/Overwrite/GoBack/DeleteImageToken/Copy/Cut
/// and ToggleExplorer) dispatch from either pane. Pane-SPECIFIC actions (motion,
/// Enter, Esc) go to the editor normally, OR — when the Explorer is focused —
/// are reinterpreted as tree nav (same physical keys, different intent), so the
/// keymap never needs per-pane bindings.
fn handle_edit_mode(app: &App, state: &mut EditorState, key: KeyEvent) -> Result<Control> {
    // Focus-guard: a focused-but-invisible Explorer would trap keystrokes (the
    // terminal narrowed past the auto-show threshold, or the user forced it
    // hidden). Drop focus to the editor so no key is lost to an invisible pane.
    let visible = explorer_effective_visibility(
        state.frame_width,
        &app.config().layout,
        state.explorer_visible_override,
    );
    if !visible && state.active_pane == ActivePane::Explorer {
        state.active_pane = ActivePane::Editor;
    }

    // Spike 7 P7.4: Explorer file-op keys are RAW (not keymapped) so the letters
    // n / r / d can't collide with editor typing — intercepted here, only when
    // the Explorer is focused (the focus-guard above guarantees it's visible).
    // `n` / `N` / `r` open a name prompt; `d` opens a y/n delete confirm.
    if state.active_pane == ActivePane::Explorer {
        if let Some(op) = explorer_fileop(&key) {
            return start_fileop(state, op);
        }
    }

    // Every edit-mode key resolves through the keymap (§5 KeymapRegistry,
    // contract C8). Unhandled keys fall through to a no-op.
    let Some(action) = state.keymap.action_for(&key) else {
        return Ok(Control::Continue);
    };

    match action {
        Action::ToggleExplorer => {
            toggle_explorer(state);
            return Ok(Control::Continue);
        }
        // Pane-agnostic commands work regardless of focus.
        Action::Save
        | Action::Reload
        | Action::OpenFuzzy
        | Action::PasteImage
        | Action::DeleteImageToken
        | Action::ConflictCopy
        | Action::Overwrite
        | Action::GoBack
        | Action::Copy
        | Action::Cut => return dispatch_and_scroll(app, state, action),
        _ => {}
    }

    // Pane-specific actions. Explorer focus → reinterpret as tree nav; else the
    // normal editor dispatch.
    if state.active_pane == ActivePane::Explorer {
        // Enter on a note opens it (focus returns to the editor). Enter on a
        // folder, and all other pane-specific keys, route through tree nav.
        // `.map(str::to_owned)` ends the `&state.explorer` borrow before the
        // mutable `open_from_explorer` borrow below.
        if action == Action::Enter && state.explorer.selected_kind() == Some(EntryKind::Note) {
            if let Some(rel_str) = state.explorer.selected_rel_path().map(str::to_owned) {
                let rel = RelativeNotePath::from_user(&rel_str)?;
                return open_relative(app, state, &rel);
            }
        }
        apply_explorer_action(state, action);
        return Ok(Control::Continue);
    }
    dispatch_and_scroll(app, state, action)
}

/// Dispatch an editor action, then re-anchor the viewport on the cursor.
fn dispatch_and_scroll(app: &App, state: &mut EditorState, action: Action) -> Result<Control> {
    let ctrl = dispatch_edit(app, state, action)?;
    state.adjust_scroll(state.view_height);
    Ok(ctrl)
}

/// Cycle Explorer visibility + focus (Ctrl+E). From the editor, show + focus
/// the Explorer (force-visible even on a narrow terminal — explicit choice);
/// from the Explorer, hide it and return focus to the editor.
fn toggle_explorer(state: &mut EditorState) {
    if state.active_pane == ActivePane::Explorer {
        state.explorer_visible_override = Some(false);
        state.active_pane = ActivePane::Editor;
    } else {
        state.explorer_visible_override = Some(true);
        state.active_pane = ActivePane::Explorer;
    }
}

/// Reinterpret an editor motion/Enter/Esc action as Explorer tree nav (Spike 7
/// P7.2). Enter on a folder toggles its expand/collapse (the note-open Enter is
/// handled in `handle_edit_mode` before this runs).
fn apply_explorer_action(state: &mut EditorState, action: Action) {
    use Action::*;
    match action {
        MoveUp | SelectUp => state.explorer.up(),
        MoveDown | SelectDown => state.explorer.down(),
        MoveLeft | SelectLeft | WordLeft | SelectWordLeft => state.explorer.collapse_selected(),
        MoveRight | SelectRight | WordRight | SelectWordRight => state.explorer.expand_selected(),
        Enter => state.explorer.toggle_expand_selected(),
        // Esc (ClearSelection in the editor) returns focus to the editor; the
        // Explorer stays visible.
        ClearSelection => state.active_pane = ActivePane::Editor,
        _ => {}
    }
}

// ── Explorer file ops (Spike 7 P7.4) ─────────────────────────────────────────
//
// Raw (non-keymapped) keys, only while the Explorer is focused. `n`/`N`/`r` open
// a name-entry prompt modal; `d` opens a y/n delete confirm. The ops route
// through the App use cases (create_note / create_folder / rename_entry /
// delete_entry), then refresh the tree and follow the editor when the open note
// moved or was deleted. The keys are fixed (not in the keymap registry) for the
// same reason the fuzzy picker's are: they're pane/modal-local, and binding `n`
// / `r` / `d` globally would break typing those letters in the editor.

/// Resolve a raw key to an Explorer file op, or `None` if it isn't one. Shift+n
/// arrives as either `Char('N')` or `Char('n')` + the SHIFT bit depending on the
/// terminal, so both map to "new folder"; plain `n` is "new note".
fn explorer_fileop(key: &KeyEvent) -> Option<FileOp> {
    let shift = key.modifiers.contains(KeyModifiers::SHIFT);
    match key.code {
        KeyCode::Char('N') => Some(FileOp::NewFolder),
        KeyCode::Char('n') if shift => Some(FileOp::NewFolder),
        KeyCode::Char('n') => Some(FileOp::NewNote),
        KeyCode::Char('r') if !shift => Some(FileOp::Rename),
        KeyCode::Char('d') if !shift => Some(FileOp::Delete),
        _ => None,
    }
}

/// Begin a file op: open the prompt/confirm modal. Rename/delete no-op with a
/// toast when nothing is selected (new note/folder can run against the root even
/// on an empty vault).
fn start_fileop(state: &mut EditorState, op: FileOp) -> Result<Control> {
    if matches!(op, FileOp::Delete | FileOp::Rename) && state.explorer.selected_rel_path().is_none()
    {
        state.toast("nothing selected");
        return Ok(Control::Continue);
    }
    match op {
        FileOp::Delete => state.mode = Mode::Confirm,
        FileOp::NewNote | FileOp::NewFolder => {
            state.prompt_kind = Some(match op {
                FileOp::NewNote => PromptKind::NewNote,
                FileOp::NewFolder => PromptKind::NewFolder,
                // Delete is handled above; NewNote/NewFolder are the only others.
                _ => unreachable!("FileOp::Delete handled above"),
            });
            state.prompt_input.clear();
            state.mode = Mode::Prompt;
        }
        FileOp::Rename => {
            // Prefill with the entry's display name (note stem / folder name) so a
            // rename is an edit, not a retype. `.map(str::to_owned)` ends the
            // `&state.explorer` borrow before the field writes below.
            if let Some(name) = state.explorer.selected_display_name().map(str::to_owned) {
                state.prompt_kind = Some(PromptKind::Rename);
                state.prompt_input = name;
                state.mode = Mode::Prompt;
            }
        }
    }
    Ok(Control::Continue)
}

/// Name-prompt modal keys (Spike 7 P7.4). Fixed (not keymapped): printable chars
/// append, Backspace pops, Enter commits, Esc cancels. On a commit ERROR the
/// modal stays open with the input preserved so the user edits + retries — e.g.
/// a rename onto a busy target (§7 never-overwrite) must not discard the typed
/// name.
fn handle_prompt_event(app: &App, state: &mut EditorState, key: KeyEvent) -> Result<Control> {
    match key.code {
        KeyCode::Esc => close_prompt(state),
        KeyCode::Enter => {
            let Some(kind) = state.prompt_kind else {
                close_prompt(state);
                return Ok(Control::Continue);
            };
            let input = state.prompt_input.clone();
            match commit_prompt(app, state, kind, &input) {
                Ok(()) => close_prompt(state),
                Err(e) => state.toast(format!("{e}")),
            }
        }
        KeyCode::Backspace => {
            state.prompt_input.pop();
        }
        // Skip control chars + Ctrl/Alt combos so they don't pollute the name
        // (Ctrl+Q still quits — checked globally before mode dispatch).
        KeyCode::Char(c)
            if !c.is_control()
                && !key.modifiers.contains(KeyModifiers::CONTROL)
                && !key.modifiers.contains(KeyModifiers::ALT) =>
        {
            state.prompt_input.push(c);
        }
        _ => {}
    }
    Ok(Control::Continue)
}

/// y/n delete-confirm keys (Spike 7 P7.4). y/Enter deletes; n/Esc cancels.
fn handle_confirm_event(app: &App, state: &mut EditorState, key: KeyEvent) -> Result<Control> {
    match key.code {
        KeyCode::Char('y') | KeyCode::Char('Y') | KeyCode::Enter => {
            if let Err(e) = confirm_delete(app, state) {
                state.toast(format!("{e}"));
            }
            state.mode = Mode::Edit;
        }
        KeyCode::Esc | KeyCode::Char('n') | KeyCode::Char('N') => state.mode = Mode::Edit,
        _ => {}
    }
    Ok(Control::Continue)
}

/// Drop the prompt modal and clear its buffer.
fn close_prompt(state: &mut EditorState) {
    state.prompt_kind = None;
    state.prompt_input.clear();
    state.mode = Mode::Edit;
}

/// Execute the committed prompt: create a note/folder or rename, then refresh the
/// tree and follow the editor when its note moved. Toasts the result.
fn commit_prompt(app: &App, state: &mut EditorState, kind: PromptKind, input: &str) -> Result<()> {
    match kind {
        PromptKind::NewNote => {
            let folder = target_parent(state)?;
            let new_path = app.create_note(input, folder.as_ref())?;
            // The user created a note to edit it → open + reveal + focus editor.
            let doc = app.open_note(&new_path)?;
            state.reload(doc);
            state.active_pane = ActivePane::Editor;
            refresh_tree(app, state);
            let s = new_path.as_str();
            state.explorer.select_note(&s);
            state.toast(format!("created {s}"));
        }
        PromptKind::NewFolder => {
            let parent = target_parent(state)?;
            let path = compose_folder_path(parent.as_ref(), input)?;
            app.create_folder(&path)?;
            refresh_tree(app, state);
            let s = path.as_str();
            // select_note reveals the new folder (expands ancestors); the cursor
            // stays in the Explorer for further ops.
            state.explorer.select_note(&s);
            state.toast(format!("created {s}"));
        }
        PromptKind::Rename => {
            // Capture selection BEFORE any mutable op (ends the explorer borrow).
            let Some(rel) = state.explorer.selected_rel_path().map(str::to_owned) else {
                anyhow::bail!("nothing selected");
            };
            let Some(ekind) = state.explorer.selected_kind() else {
                anyhow::bail!("nothing selected");
            };
            let from = RelativeNotePath::from_user(&rel)?;
            let outcome = app.rename_entry(&from, input, ekind)?;
            // If the rename relocated the OPEN note, reload the editor from its new
            // path so `state.path` + the §7 save baseline stay valid.
            if let Some(relocated) = outcome.relocated_current.as_ref() {
                let doc = app.open_note(relocated)?;
                state.reload(doc);
            }
            refresh_tree(app, state);
            let s = outcome.new_path.as_str();
            state.explorer.select_note(&s);
            state.toast(format!("renamed → {s}"));
        }
    }
    Ok(())
}

/// Run the confirmed delete, then refresh the tree and load the default note when
/// the open note was the one removed (the editor must never point at a deleted
/// file — §7). Re-selects the editor's current note so the pane tracks it.
fn confirm_delete(app: &App, state: &mut EditorState) -> Result<()> {
    let Some(rel) = state.explorer.selected_rel_path().map(str::to_owned) else {
        anyhow::bail!("nothing selected");
    };
    let Some(kind) = state.explorer.selected_kind() else {
        anyhow::bail!("nothing selected");
    };
    let path = RelativeNotePath::from_user(&rel)?;
    let deleted_current = app.delete_entry(&path, kind)?;
    if deleted_current {
        // The open note's file is gone → fall back to the default so the buffer
        // isn't orphaned. A failure here is non-fatal (toast); the editor keeps
        // its buffer and the user can save it elsewhere.
        match app.open_default() {
            Ok(doc) => state.reload(doc),
            Err(e) => state.toast(format!("open default failed: {e}")),
        }
    }
    refresh_tree(app, state);
    // `state.path` is now the surviving current note (the default after a
    // current-delete, else unchanged) — reveal it so the pane tracks the editor.
    let cur = state.path.as_str();
    state.explorer.select_note(&cur);
    state.toast(format!("deleted {rel}"));
    Ok(())
}

/// Parent folder a new Explorer entry should land in: the selected folder (new
/// entry goes INSIDE it), the selected note's parent (sibling), or the vault root
/// (`None`) when nothing is selected or the selection is a root-level note.
fn target_parent(state: &EditorState) -> Result<Option<RelativeNotePath>> {
    let Some(rel) = state.explorer.selected_rel_path().map(str::to_owned) else {
        return Ok(None);
    };
    match state.explorer.selected_kind() {
        Some(EntryKind::Folder) => Ok(Some(RelativeNotePath::from_user(&rel)?)),
        Some(EntryKind::Note) => match std::path::Path::new(&rel).parent() {
            Some(par) if !par.as_os_str().is_empty() => Ok(Some(RelativeNotePath::new(par)?)),
            _ => Ok(None),
        },
        None => Ok(None),
    }
}

/// Compose a new folder path `parent/leaf` (or just `leaf` at the root). Folder
/// names are kept verbatim (matching `rename_entry`'s folder rule); §3.1
/// confinement holds because `RelativeNotePath::new` rejects `..` segments.
fn compose_folder_path(parent: Option<&RelativeNotePath>, leaf: &str) -> Result<RelativeNotePath> {
    let mut pb = parent
        .map(|p| p.as_path().to_path_buf())
        .unwrap_or_default();
    pb.push(leaf);
    Ok(RelativeNotePath::new(pb)?)
}

/// Re-walk the vault and refill the Explorer tree (source of truth = files, §6).
/// `set_tree` preserves the expanded-folder set + re-derives selection by
/// `rel_path`, so a create/rename/delete doesn't jump the cursor or collapse the
/// user's open folders.
fn refresh_tree(app: &App, state: &mut EditorState) {
    if let Ok(tree) = app.list_vault_tree() {
        state.explorer.set_tree(tree);
    }
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
        // ^Shift+K overwrites external changes (§7 explicit escape hatch).
        Action::Overwrite => force_overwrite(app, state),
        // ^B jumps back to the previous note (Spike 8 back-nav).
        Action::GoBack => go_back(app, state),
        // ^G follows the [[wikilink]] / Markdown link under the caret (Spike 8).
        Action::OpenLink => open_link(app, state),
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
                state.push_history(&sel.path);
                let doc = app.open_note(&sel.path)?;
                state.reload(doc);
                // Spike 7 P7.3 two-way sync: the fuzzy open changed the editor's
                // note, so move the Explorer cursor onto it (expanding ancestors
                // to reveal it) — the pane tracks the editor without manual nav.
                // `path.as_str()` allocates; bind once and reuse for the toast.
                let rel = sel.path.as_str();
                state.explorer.select_note(&rel);
                state.mode = Mode::Edit;
                state.toast(format!("opened {rel}"));
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

/// Swap the editor to `rel` (open + reload + focus editor) WITHOUT touching the
/// note-history stack. Shared core of `open_relative` (which records history)
/// and `go_back` (which consumes it).
fn load_note(app: &App, state: &mut EditorState, rel: &RelativeNotePath) -> Result<Control> {
    let doc = app.open_note(rel)?;
    state.reload(doc);
    state.active_pane = ActivePane::Editor;
    Ok(Control::Continue)
}

/// Open a note by relative path into the editor: load via the use case, swap
/// the buffer, focus the editor, toast. Shared by the Explorer's Enter-on-note
/// and the Spike-8 link-follow (`Ctrl+G`) — hence the generic name. Records the
/// previous note on the back-nav stack (`Ctrl+B`).
fn open_relative(app: &App, state: &mut EditorState, rel: &RelativeNotePath) -> Result<Control> {
    state.push_history(rel);
    load_note(app, state, rel)?;
    state.toast(format!("opened {}", rel.as_str()));
    Ok(Control::Continue)
}

/// Back-nav (`Ctrl+B`): pop the jump-stack and return to the predecessor note.
/// Does NOT re-record history (otherwise back would immediately re-push the
/// note it just left). Empty stack is a harmless toast.
fn go_back(app: &App, state: &mut EditorState) -> Result<Control> {
    match state.note_history.pop() {
        Some(prev) => {
            load_note(app, state, &prev)?;
            state.toast("back");
            Ok(Control::Continue)
        }
        None => {
            state.toast("nothing to go back to");
            Ok(Control::Continue)
        }
    }
}

/// Spike 8: follow the note link under the caret (`Ctrl+G`). Resolves the
/// `[[wikilink]]` / Markdown-link target; a unique match opens it (reusing the
/// Explorer open path), and an ambiguous/unknown match falls back to the fuzzy
/// picker seeded with the target so the user disambiguates (`CLAUDE.md` §8). No
/// link under the caret is a harmless toast.
fn open_link(app: &App, state: &mut EditorState) -> Result<Control> {
    let target = match state.link_under_caret() {
        Some(t) => t,
        None => {
            state.toast("no note link under caret");
            return Ok(Control::Continue);
        }
    };
    match app.resolve_note_link(&target)? {
        LinkResolution::Found(rel) => open_relative(app, state, &rel),
        // Ambiguous (several notes share the title) or unknown → let the user
        // pick: seed the fuzzy picker with the link target.
        LinkResolution::Ambiguous(_) | LinkResolution::NotFound => {
            state.clear_selection();
            state.mode = Mode::FuzzyOpen;
            state.fuzzy_query = target;
            refresh_fuzzy(app, state)?;
            Ok(Control::Continue)
        }
    }
}

fn conflict_copy(app: &App, state: &mut EditorState) -> Result<Control> {
    let copy = app.write_conflict_copy(&state.body())?;
    state.toast(format!("wrote conflict copy {}", copy.as_str()));
    Ok(Control::Continue)
}

/// §7 resolution: explicit overwrite of external changes. Writes the buffer
/// verbatim, bypassing the `opened_hash` baseline (`force_overwrite_current`).
/// Only meaningful from `SyncStatus::Conflict`; elsewhere it's a no-op toast.
/// §7 forbids *defaulting* to overwrite — this fires only on the explicit
/// `Ctrl+Shift+K` chord, never automatically.
fn force_overwrite(app: &App, state: &mut EditorState) -> Result<Control> {
    if state.status != SyncStatus::Conflict {
        state.toast("no conflict to overwrite");
        return Ok(Control::Continue);
    }
    match app.force_overwrite_current(&state.body()) {
        Ok(_) => {
            state.status = SyncStatus::Clean;
            state.toast("overwrote external changes");
        }
        Err(e) => {
            state.status = SyncStatus::Error(e.to_string());
            state.toast(format!("overwrite failed: {e}"));
        }
    }
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

// ── Explorer file ops (Spike 7 P7.4): pure helper unit tests ────────────────
//
// The App-dependent commit/confirm handlers belong in the integration harness
// (they need the 8-dep `App`); these pin the three PURE policies the UI depends
// on — the raw key→op map, the new-folder path (incl. §3.1 escape rejection),
// and "where does a new entry land" (inside a folder / beside a note / root).

#[cfg(test)]
mod fileop_tests {
    use super::*;
    use crate::domain::vault::{EntryKind, RelativeNotePath, VaultEntry};
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    /// The raw Explorer key→op map (P7.4 user-facing contract): plain `n`→note,
    /// `N` or Shift+`n`→folder, `r`→rename, `d`→delete; anything else falls
    /// through to the keymap. The capital-`N` + shift-`n` dual covers both ways a
    /// terminal may report Shift+n.
    #[test]
    fn explorer_fileop_maps_raw_keys() {
        let none = KeyModifiers::NONE;
        let shift = KeyModifiers::SHIFT;
        let k = |code, mods| explorer_fileop(&KeyEvent::new(code, mods));
        assert_eq!(k(KeyCode::Char('n'), none), Some(FileOp::NewNote));
        assert_eq!(k(KeyCode::Char('N'), none), Some(FileOp::NewFolder));
        assert_eq!(k(KeyCode::Char('n'), shift), Some(FileOp::NewFolder));
        assert_eq!(k(KeyCode::Char('r'), none), Some(FileOp::Rename));
        assert_eq!(k(KeyCode::Char('d'), none), Some(FileOp::Delete));
        // Shift+r / Shift+d / unrelated letters / non-char keys are NOT file ops.
        assert_eq!(k(KeyCode::Char('r'), shift), None);
        assert_eq!(k(KeyCode::Char('d'), shift), None);
        assert_eq!(k(KeyCode::Char('x'), none), None);
        assert_eq!(k(KeyCode::Enter, none), None);
        assert_eq!(k(KeyCode::Left, none), None);
    }

    /// A new-folder path is `parent/leaf` (or just `leaf` at the root); a `..`
    /// leaf is rejected by `RelativeNotePath::new` so the create can't escape the
    /// vault root (§3.1).
    #[test]
    fn compose_folder_path_joins_and_rejects_escape() {
        let root_leaf = compose_folder_path(None, "Inbox").expect("root leaf");
        assert_eq!(root_leaf.as_str(), "Inbox");
        let parent = RelativeNotePath::new("Projects").expect("parent");
        let nested = compose_folder_path(Some(&parent), "Alpha").expect("nested");
        assert_eq!(nested.as_str(), "Projects/Alpha");
        // `..` is a path segment RelativeNotePath::new refuses — confinement holds.
        assert!(
            compose_folder_path(Some(&parent), "..").is_err(),
            "`..` leaf must be rejected (§3.1 vault escape)"
        );
        assert!(
            compose_folder_path(None, "..").is_err(),
            "bare `..` leaf must be rejected (§3.1 vault escape)"
        );
    }

    /// Target-parent policy: a new entry goes INSIDE a selected folder, ALONGSIDE
    /// a selected note (its parent dir), or at the ROOT for a root-level note /
    /// empty selection.
    #[test]
    fn target_parent_goes_inside_folder_alongside_note_or_root() {
        let note = |rel: &str| VaultEntry {
            name: rel
                .rsplit('/')
                .next()
                .unwrap_or(rel)
                .trim_end_matches(".md")
                .to_string(),
            rel_path: RelativeNotePath::new(rel).expect("note path"),
            kind: EntryKind::Note,
            children: vec![],
        };
        let folder = |rel: &str, kids: Vec<VaultEntry>| VaultEntry {
            name: rel.rsplit('/').next().unwrap_or(rel).to_string(),
            rel_path: RelativeNotePath::new(rel).expect("folder path"),
            kind: EntryKind::Folder,
            children: kids,
        };
        let mut s = state_from_body("x");
        s.explorer.set_tree(vec![
            folder("Projects", vec![note("Projects/alpha.md")]),
            note("beta.md"),
        ]);
        // set_tree lands selection on the first row = Projects (folder) → inside.
        assert_eq!(s.explorer.selected_rel_path(), Some("Projects"));
        let inside = target_parent(&s).expect("folder selected");
        assert_eq!(
            inside.as_ref().map(|p| p.as_str()).as_deref(),
            Some("Projects"),
            "new entry inside the selected folder"
        );
        // Nested note selected → sibling parent (the folder).
        s.explorer.select_note("Projects/alpha.md");
        let sib = target_parent(&s).expect("nested note selected");
        assert_eq!(
            sib.as_ref().map(|p| p.as_str()).as_deref(),
            Some("Projects"),
            "new entry alongside the selected note (its parent)"
        );
        // Root-level note selected → None (root).
        s.explorer.select_note("beta.md");
        assert!(
            target_parent(&s).unwrap().is_none(),
            "root-level note → new entry at the vault root"
        );
    }
}

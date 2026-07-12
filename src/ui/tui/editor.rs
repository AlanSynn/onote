//! Editor state, selection model, and cursor/word primitives, extracted from the
//! original god-module per `CLAUDE.md` §3.2 (mechanical split, zero logic change).
//! App-free and unit-testable: the TUI event loop (`mod.rs`) drives
//! [`EditorState`] through `apply_action` / `dispatch_edit`.

use std::time::Instant;

use crossterm::event::KeyCode;

use ratatui_image::picker::Picker;
use unicode_segmentation::UnicodeSegmentation;

use crate::domain::note::NoteDocument;
use crate::domain::vault::RelativeNotePath;

use super::keymap::{Action, KeymapRegistry};
use super::wrap;
use super::{ImageOverlay, Mode, PromptKind, SyncStatus};

/// The editor's in-memory buffer + cursor/selection state. Constructed inside
/// `tui::run` (mod.rs) and threaded through the handlers — never leaves the
/// `ui::tui` module, so `pub(super)` (visible to the parent `ui::tui`) is the
/// tightest correct visibility. `main.rs` only calls `tui::run`.
pub(super) struct EditorState {
    pub(super) path: RelativeNotePath,
    pub(super) title: String,
    pub(super) lines: Vec<String>,
    pub(super) cx: usize,
    pub(super) cy: usize,
    pub(super) scroll: usize,
    pub(super) status: SyncStatus,
    pub(super) mode: Mode,
    pub(super) fuzzy_query: String,
    pub(super) fuzzy_results: Vec<crate::domain::note::NoteSummary>,
    pub(super) fuzzy_sel: usize,
    /// Full-text body-search picker (§2.6 FTS5). Mirrors the fuzzy fields: a
    /// query buffer + `SearchHit` results + a selection cursor. Entered via
    /// `Ctrl+F` (`Action::OpenSearch`); results come from `App::search` (FTS5
    /// `MATCH`), not fuzzy title matching, so it finds notes by BODY content.
    pub(super) search_query: String,
    pub(super) search_results: Vec<crate::domain::note::SearchHit>,
    pub(super) search_sel: usize,
    pub(super) message: Option<(Instant, String)>,
    /// Terminal graphics-protocol detector (`None` when none is available, e.g.
    /// a plain `dumb` terminal or piped stdout). The image modal degrades to a
    /// text fallback in that case (`CLAUDE.md` §2.4).
    pub(super) picker: Option<Picker>,
    /// Active full-screen image preview, if any.
    pub(super) overlay: Option<ImageOverlay>,
    /// Editor viewport height from the last render (for mouse-scroll clamping).
    pub(super) view_height: usize,
    /// Editor inner rect origin/width from the last render — used to map a mouse
    /// event's screen (column,row) to a buffer [`Pos`] (P3 mouse selection).
    /// Origin is `inner.x`/`inner.y`; width is `inner.width`.
    pub(super) editor_x: u16,
    pub(super) editor_y: u16,
    pub(super) editor_width: u16,
    /// True while the left button is held (Down…Up). Gates Drag/Moved handling
    /// and lets `Moved` (some terminals don't emit `Drag`) extend the selection.
    pub(super) mouse_dragging: bool,
    /// Resolved keybindings (`CLAUDE.md` §5 KeymapRegistry): every edit-mode
    /// key resolves through here. Defaults overlaid with `[keymap]` config.
    pub(super) keymap: KeymapRegistry,
    /// Selection anchor (`CLAUDE.md` §3.1 + plan §2A). `None` = no selection.
    /// Only the ANCHOR is stored: the moving head is always the caret (`cy`,`cx`),
    /// so the selection can never diverge from where the cursor is drawn (DRY,
    /// single source of truth — plan §2A). A full [`Selection`] is reconstructed
    /// on demand via [`EditorState::selection`]. Endpoints are grapheme-snapped
    /// (contract C7) so é / ZWJ-emoji select as one unit.
    pub(super) selection_anchor: Option<Pos>,
    /// Explorer drawer state (Spike 7 P7.2). Populated from `list_vault_tree` in
    /// `run` and refreshed on file-watch; empty until then.
    pub(super) explorer: super::note_drawer::ExplorerState,
    /// Which pane receives pane-specific keys (arrows/Enter). Pane-agnostic keys
    /// (Save/Reload/Open/…) dispatch from either.
    pub(super) active_pane: super::note_drawer::ActivePane,
    /// User toggle (Ctrl+E) overriding the auto-show width policy:
    /// `None` = auto, `Some(true)` = force visible, `Some(false)` = force hidden.
    pub(super) explorer_visible_override: Option<bool>,
    /// Last rendered frame width (set in `render`). Lets the event handler
    /// compute effective Explorer visibility for the focus-guard.
    pub(super) frame_width: u16,
    /// Active name prompt (Spike 7 P7.4 file ops): what the typed string means.
    /// `None` unless `mode == Prompt`. Set by `n`/`N`/`r` in the Explorer pane.
    pub(super) prompt_kind: Option<PromptKind>,
    /// Name-prompt input buffer (Spike 7 P7.4). Appended/popped at the END only
    /// (no mid-string cursor editing) — the block-cursor glyph in the prompt
    /// popup always renders at the tail.
    pub(super) prompt_input: String,
    /// Note-navigation jump-stack for back-nav (`Ctrl+B`). Pushed on every note
    /// switch (link-follow / Explorer-Enter / fuzzy-open); `go_back` pops LIFO.
    /// Capped at 50 — oldest drops first. `reload` is in-place, so history
    /// survives a buffer swap.
    pub(super) note_history: Vec<RelativeNotePath>,
    /// Resolved Catppuccin theme (v0.4.0). Built once in `run` from the config
    /// `theme` string; defaults to Latte here until `run` overrides it. Cheap
    /// to copy (all `Color`); renderers read role colors off it.
    pub(super) theme: super::theme::Theme,
    /// Visual-row layout (v0.4.0 F1 wrap): the flattened, grapheme-aware
    /// soft-wrap of `lines` at the last-known editor width. Recomputed every
    /// render (authoritative) and after each edit (so post-edit scroll math
    /// sees fresh wrapping). Empty until the first render. Mouse hits + scroll
    /// are measured in this visual-row space, NOT logical lines.
    pub(super) rows: Vec<wrap::Row>,
    /// Set on `Event::Resize`; consumed by the next render to clamp the cursor
    /// back into view after the width change reshapes the row layout. Keeps the
    /// "reader can scroll away from the cursor" property (adjust_scroll runs
    /// only on edits + this one-shot resize clamp, never every frame).
    pub(super) resize_pending: bool,
}

impl EditorState {
    pub(super) fn from_doc(doc: NoteDocument) -> Self {
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
            search_query: String::new(),
            search_results: Vec::new(),
            search_sel: 0,
            message: None,
            picker: None,
            overlay: None,
            view_height: 0,
            editor_x: 0,
            editor_y: 0,
            editor_width: 0,
            mouse_dragging: false,
            keymap: KeymapRegistry::defaults(),
            selection_anchor: None,
            explorer: super::note_drawer::ExplorerState::default(),
            active_pane: super::note_drawer::ActivePane::default(),
            explorer_visible_override: None,
            frame_width: 0,
            prompt_kind: None,
            prompt_input: String::new(),
            note_history: Vec::new(),
            theme: super::theme::Theme::default(),
            rows: Vec::new(),
            resize_pending: false,
        }
    }

    pub(super) fn body(&self) -> String {
        self.lines.join("\n")
    }

    pub(super) fn reload(&mut self, doc: NoteDocument) {
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
        // C5 lifecycle: a reload swaps the buffer out from under any active
        // selection, so the selection (char-indexed into the OLD lines) is
        // dropped. Never carry a stale selection across a content change.
        self.selection_anchor = None;
    }

    /// Record the current note on the back-nav stack, unless the destination is
    /// the note we're already on (a no-op open records nothing). Capped at 50.
    pub(super) fn push_history(&mut self, dest: &RelativeNotePath) {
        if self.path != *dest {
            const CAP: usize = 50;
            if self.note_history.len() >= CAP {
                self.note_history.remove(0);
            }
            self.note_history.push(self.path.clone());
        }
    }

    pub(super) fn cur_line(&self) -> &str {
        self.lines.get(self.cy).map(|s| s.as_str()).unwrap_or("")
    }

    /// Spike 8: the note-link target under the caret, if any (`[[wikilink]]` or
    /// `[label](note)` on the current line). `None` when the caret isn't on a
    /// link span. Drives the `OpenLink` action (Ctrl+G) — follow a link without a
    /// mouse. Delegates to the pure `link_target_at` so the scanning logic is
    /// unit-testable without an `EditorState`.
    pub(super) fn link_under_caret(&self) -> Option<String> {
        link_target_at(self.cur_line(), self.cx)
    }

    pub(super) fn mark_dirty(&mut self) {
        if self.status != SyncStatus::Conflict {
            self.status = SyncStatus::Dirty;
        }
    }

    pub(super) fn insert_char(&mut self, c: char) {
        // B1 defense-in-depth: any raw buffer mutation invalidates a
        // selection anchor. C6 (apply_action) deletes an ACTIVE selection
        // first; this closes the LATENT case (anchor == caret, e.g. after a
        // pure mouse click) so the anchor can't survive into a state where
        // the caret has drifted off it and a later keystroke would treat the
        // stale gap as a selection (silent data loss on the next type).
        self.clear_selection();
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

    pub(super) fn insert_newline(&mut self) {
        // B1 defense-in-depth: see `insert_char` — a buffer mutation drops any
        // latent/active selection anchor (the Enter arm in dispatch_edit deletes
        // an active selection first; this covers the latent + direct-call cases).
        self.clear_selection();
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
        // B1 defense-in-depth: see `insert_char` — drops a latent/active anchor
        // (C6 deletes an active selection first; this covers the rest).
        self.clear_selection();
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

    /// Keep the caret's VISUAL row inside the viewport (v0.4.0 F1: scroll is
    /// measured in visual rows, not logical lines — a wrapped line occupies
    /// several scrollable rows). Reads the last-computed `rows` layout; falls
    /// back to line-based clamping when `rows` is empty (before the first
    /// render) so the caret is still kept in view on the very first keystrokes.
    /// View-height 0 is a degenerate/hidden viewport → no-op.
    pub(super) fn adjust_scroll(&mut self, view_height: usize) {
        if view_height == 0 {
            return;
        }
        if let Some((vi, _)) = wrap::caret_row(&self.rows, self.cy, self.cx) {
            if vi < self.scroll {
                self.scroll = vi;
            }
            if vi >= self.scroll + view_height {
                self.scroll = vi + 1 - view_height;
            }
        } else if self.cy < self.scroll {
            self.scroll = self.cy;
        } else if self.cy >= self.scroll + view_height {
            self.scroll = self.cy + 1 - view_height;
        }
    }

    pub(super) fn toast(&mut self, msg: impl Into<String>) {
        self.message = Some((Instant::now(), msg.into()));
    }

    // ── Selection (plan §2A, contracts C5/C7) ───────────────────────────────

    /// Current caret as a [`Pos`] (the selection's moving head).
    fn caret(&self) -> Pos {
        Pos {
            line: self.cy,
            col: self.cx,
        }
    }

    /// The active selection, if any: anchor (stored) → head (caret). `None` when
    /// no anchor is set OR the anchor equals the caret (an empty selection is
    /// not a selection — C5). Reconstructed on demand so head == caret always.
    pub(super) fn selection(&self) -> Option<Selection> {
        let anchor = self.selection_anchor?;
        let sel = Selection {
            anchor,
            head: self.caret(),
        };
        (!sel.is_empty()).then_some(sel)
    }

    /// Start a selection at the caret (no-op if one is already active). The
    /// anchor is grapheme-snapped (C7) so a selection begun with the caret
    /// parked inside a multi-code-point grapheme still anchors on a boundary.
    fn ensure_selection(&mut self) {
        if self.selection_anchor.is_none() {
            let mut anchor = self.caret();
            if let Some(line) = self.lines.get(anchor.line) {
                anchor.col = snap_to_grapheme_start(line, anchor.col);
            }
            self.selection_anchor = Some(anchor);
        }
    }

    /// Drop the selection (Esc, plain move, mode switch, destructive key).
    pub(super) fn clear_selection(&mut self) {
        self.selection_anchor = None;
    }

    /// Snap the caret (selection head) to the start of its grapheme cluster on
    /// the current line. Applied after every Select* move so selection
    /// endpoints land on grapheme boundaries (C7).
    fn snap_head_to_grapheme(&mut self) {
        if let Some(line) = self.lines.get(self.cy) {
            self.cx = snap_to_grapheme_start(line, self.cx);
        }
    }

    /// Extend the selection head by one cursor move: ensure an anchor exists,
    /// move the caret the same way a plain move would, then grapheme-snap the
    /// landing (C7). The anchor is untouched, so the selection grows/shrinks
    /// toward the moving caret.
    fn extend_selection(&mut self, code: KeyCode) {
        self.ensure_selection();
        self.move_cursor(code);
        self.snap_head_to_grapheme();
    }

    /// Select the entire buffer (Ctrl+A). Anchor at (0,0); head (= caret) at the
    /// END of the last line (EOL, one past the last char), grapheme-snapped.
    /// Empty buffer → empty selection (renders as nothing, per C5).
    fn select_all(&mut self) {
        let last = self.lines.len().saturating_sub(1);
        let last_len = self.lines.get(last).map_or(0, |l| char_count(l.as_str()));
        self.selection_anchor = Some(Pos { line: 0, col: 0 });
        self.cy = last;
        self.cx = last_len;
        self.snap_head_to_grapheme();
    }

    /// Jump the caret one word in `dir` (Ctrl+Left/Right), with the same
    /// line-wrap behavior as a plain Left/Right: at a line edge, cross to the
    /// previous/next line's end/start rather than stalling. The word jump on a
    /// single line is delegated to the pure [`word_boundary`]. Does NOT touch
    /// the selection — callers clear/extend as needed (plain word-move clears,
    /// select-word-move extends).
    ///
    /// The landing is grapheme-snapped (B4): [`word_class`] treats a combining
    /// mark (e.g. U+0301) as `Punct`, so a base+combining grapheme like `é`
    /// splits into two runs and `word_boundary` can return an index INSIDE the
    /// grapheme. `snap_grapheme_dir` advances (Right) or retracts (Left) to the
    /// nearest grapheme boundary so the caret never lands mid-cluster. Line-edge
    /// wraps land on col 0 / EOL, which are always boundaries, so they're safe.
    fn move_word(&mut self, dir: WordDir) {
        let len = char_count(self.cur_line());
        match dir {
            WordDir::Left => {
                if self.cx == 0 {
                    if self.cy > 0 {
                        self.cy -= 1;
                        self.cx = char_count(self.cur_line());
                    }
                } else {
                    let line = self.cur_line().to_string();
                    let raw = word_boundary(&line, self.cx, WordDir::Left);
                    self.cx = snap_grapheme_dir(&line, raw, WordDir::Left);
                }
            }
            WordDir::Right => {
                if self.cx >= len {
                    if self.cy + 1 < self.lines.len() {
                        self.cy += 1;
                        self.cx = 0;
                    }
                } else {
                    let line = self.cur_line().to_string();
                    let raw = word_boundary(&line, self.cx, WordDir::Right);
                    self.cx = snap_grapheme_dir(&line, raw, WordDir::Right);
                }
            }
        }
    }

    /// Extend the selection head one word in `dir` (Ctrl+Shift+Left/Right):
    /// ensure an anchor, move the caret by word, then grapheme-snap the landing
    /// (C7). The anchor is untouched, so the selection extends by a whole word.
    fn extend_word(&mut self, dir: WordDir) {
        self.ensure_selection();
        self.move_word(dir);
        self.snap_head_to_grapheme();
    }

    /// Delete the half-open char range `[start, end)` (document order; swapped
    /// if reversed). Single-line splices one line; multi-line drops the covered
    /// lines and joins the end-line tail onto the start line. Every slice goes
    /// through [`char_to_byte`] (C1) so CJK/emoji never split a code point. The
    /// caret lands at `start`, the selection is cleared, and the buffer is
    /// marked dirty. An empty range is a no-op.
    pub(super) fn delete_range(&mut self, start: Pos, mut end: Pos) {
        let mut start = start;
        if start > end {
            std::mem::swap(&mut start, &mut end);
        }
        if start == end {
            return;
        }
        // Defense-in-depth: callers pass a live selection (already in-bounds),
        // but clamp both endpoints to the buffer so a stale/oversized `Pos`
        // (e.g. from a config path) can never index out of range. Clamping can
        // collapse both onto the last line with start.col > end.col, so
        // re-normalize after — otherwise the single-line branch would build a
        // reversed `b0..b1` and panic in `replace_range`.
        let mut start = self.clamp_pos(start);
        let mut end = self.clamp_pos(end);
        if start > end {
            std::mem::swap(&mut start, &mut end);
        }
        if start == end {
            return;
        }

        if start.line == end.line {
            let line = &mut self.lines[start.line];
            let b0 = char_to_byte(line, start.col);
            let b1 = char_to_byte(line, end.col);
            line.replace_range(b0..b1, "");
        } else {
            // Tail of the end line (after end.col) is spliced onto the start
            // line's prefix (before start.col); the fully-covered middle lines
            // and the end line are dropped in one drain.
            let end_line = self.lines[end.line].clone();
            let end_byte = char_to_byte(&end_line, end.col);
            let tail = end_line[end_byte..].to_string();
            self.lines.drain(start.line + 1..end.line + 1);
            let start_byte = char_to_byte(&self.lines[start.line], start.col);
            let start_line = &mut self.lines[start.line];
            // Replace [start.col, EOL) on the start line with the end-line tail.
            start_line.replace_range(start_byte.., &tail);
        }
        self.cy = start.line;
        self.cx = start.col;
        self.clear_selection();
        self.mark_dirty();
    }

    /// The selected text (normalized range), or `None` if no selection. Join
    /// char/byte boundaries via [`char_to_byte`] (C1). Used by Copy/Cut (P4).
    pub(super) fn selected_text(&self) -> Option<String> {
        let sel = self.selection()?;
        let (start, end) = sel.normalized();
        // Clamp + re-normalize for parity with `delete_range` (defense-in-depth:
        // a post-clamp collapse onto the last line could otherwise reverse the
        // range and panic the slice below). For reachable inputs the clamp is a
        // no-op — anchor/caret are kept in-bounds by every insert/delete.
        let (mut start, mut end) = (self.clamp_pos(start), self.clamp_pos(end));
        if start > end {
            std::mem::swap(&mut start, &mut end);
        }
        let mut out = String::new();
        if start.line == end.line {
            let line = &self.lines[start.line];
            out.push_str(&line[char_to_byte(line, start.col)..char_to_byte(line, end.col)]);
        } else {
            let s = &self.lines[start.line];
            out.push_str(&s[char_to_byte(s, start.col)..]);
            for ln in (start.line + 1)..end.line {
                out.push('\n');
                out.push_str(&self.lines[ln]);
            }
            out.push('\n');
            let e = &self.lines[end.line];
            out.push_str(&e[..char_to_byte(e, end.col)]);
        }
        Some(out)
    }

    /// Clamp a [`Pos`] into the live buffer: line to `[0, lines.len()-1]`, col to
    /// `[0, char_count(line)]`. Centralized here (CLAUDE.md §5) so `delete_range`
    /// and `selected_text` can't drift on the bound. `lines` is never empty (the
    /// constructor seeds `vec![String::new()]`), so indexing `[0]` is safe.
    fn clamp_pos(&self, p: Pos) -> Pos {
        let last = self.lines.len().saturating_sub(1);
        let line = p.line.min(last);
        let col = p.col.min(char_count(&self.lines[line]));
        Pos { line, col }
    }

    /// Forward-delete (Delete key) with NO selection: remove the char after the
    /// caret, or join the next line up when at EOL. The caret stays put. (A
    /// selection is handled by the C6 replace-on-type guard before this runs.)
    fn delete_forward(&mut self) {
        // B1 defense-in-depth: see `insert_char` — drops a latent/active anchor.
        self.clear_selection();
        let len = char_count(self.cur_line());
        if self.cx < len {
            if let Some(line) = self.lines.get_mut(self.cy) {
                let b0 = char_to_byte(line, self.cx);
                let b1 = char_to_byte(line, self.cx + 1);
                line.replace_range(b0..b1, "");
            }
        } else if self.cy + 1 < self.lines.len() {
            // At EOL: join the next line onto the current one (cursor unmoved).
            let next = self.lines.remove(self.cy + 1);
            if let Some(line) = self.lines.get_mut(self.cy) {
                line.push_str(&next);
            }
        }
        self.mark_dirty();
    }

    /// Apply an App-free edit action directly to the buffer. App-dependent
    /// commands (save, paste, image-token, copy/cut, enter→image) are handled
    /// in [`dispatch_edit`] BEFORE this is reached. Split out so the editor
    /// mutation path is unit-testable without a full `App`.
    pub(super) fn apply_action(&mut self, action: Action) {
        // C6 (centralized replace-on-type): a destructive/insert key with an
        // active selection REPLACES the range — delete it, then (for inserts)
        // insert at the (now-collapsed) caret. One pre-check keeps the
        // selection-aware behavior out of every individual edit method.
        if let Some(sel) = self.selection() {
            let (start, end) = sel.normalized();
            match action {
                Action::InsertChar(c) => {
                    self.delete_range(start, end);
                    self.insert_char(c);
                    return;
                }
                Action::Tab => {
                    self.delete_range(start, end);
                    self.insert_char(' ');
                    self.insert_char(' ');
                    return;
                }
                Action::Backspace | Action::DeleteForward => {
                    self.delete_range(start, end);
                    return;
                }
                _ => {}
            }
        }
        match action {
            Action::InsertChar(c) => self.insert_char(c),
            Action::Backspace => self.backspace(),
            Action::Tab => {
                self.insert_char(' ');
                self.insert_char(' ');
            }
            Action::DeleteForward => self.delete_forward(),
            // Plain motion: clear any selection, then move (C5).
            Action::MoveLeft => {
                self.clear_selection();
                self.move_cursor(KeyCode::Left);
            }
            Action::MoveRight => {
                self.clear_selection();
                self.move_cursor(KeyCode::Right);
            }
            Action::MoveUp => {
                self.clear_selection();
                self.move_cursor(KeyCode::Up);
            }
            Action::MoveDown => {
                self.clear_selection();
                self.move_cursor(KeyCode::Down);
            }
            Action::MoveHome => {
                self.clear_selection();
                self.move_cursor(KeyCode::Home);
            }
            Action::MoveEnd => {
                self.clear_selection();
                self.move_cursor(KeyCode::End);
            }
            // Selection motion: anchor fixed, head extends with the caret (C7).
            Action::SelectLeft => self.extend_selection(KeyCode::Left),
            Action::SelectRight => self.extend_selection(KeyCode::Right),
            Action::SelectUp => self.extend_selection(KeyCode::Up),
            Action::SelectDown => self.extend_selection(KeyCode::Down),
            Action::SelectHome => self.extend_selection(KeyCode::Home),
            Action::SelectEnd => self.extend_selection(KeyCode::End),
            Action::SelectAll => self.select_all(),
            Action::ClearSelection => self.clear_selection(),
            // P5 word-motion: plain word-jump clears the selection first (C5);
            // select-word extends the head by one word (C7 grapheme-snap on land).
            // move_word itself grapheme-snaps the landing (B4) — see its body.
            Action::WordLeft => {
                self.clear_selection();
                self.move_word(WordDir::Left);
            }
            Action::WordRight => {
                self.clear_selection();
                self.move_word(WordDir::Right);
            }
            Action::SelectWordLeft => self.extend_word(WordDir::Left),
            Action::SelectWordRight => self.extend_word(WordDir::Right),
            // Copy/Cut are App-dependent → handled in dispatch_edit.
            _ => {}
        }
    }
}

/// Number of Unicode scalar values in `s` (cursor model is char-index, not byte).
pub(super) fn char_count(s: &str) -> usize {
    s.chars().count()
}

/// Byte offset of the `char_idx`-th char in `s`, or `s.len()` if past the end.
/// All cursor edits route through this so a multibyte code point is never split.
pub(super) fn char_to_byte(s: &str, char_idx: usize) -> usize {
    s.char_indices()
        .nth(char_idx)
        .map(|(b, _)| b)
        .unwrap_or_else(|| s.len())
}

// ── Selection (CLAUDE.md §3.1, contracts C1/C5/C7) ──────────────────────────

/// A buffer position: `line` indexes `EditorState::lines`, `col` is a CHAR
/// index into that line (not bytes, not display columns — C1). Selection
/// endpoints are grapheme-snapped ([`snap_to_grapheme_start`]) so a boundary
/// never splits a grapheme (C7). Derives `Ord` in (line, col) order = document
/// order for the line-by-line buffer model.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
pub(super) struct Pos {
    pub(super) line: usize,
    pub(super) col: usize,
}

/// A text selection: `anchor` is where it began, `head` is the moving caret end.
/// Direction is NOT stored (DRY) — [`Selection::normalized`] returns
/// `(start, end)` in document order on demand. An empty selection
/// (`anchor == head`) means "no selection".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(super) struct Selection {
    pub(super) anchor: Pos,
    pub(super) head: Pos,
}

impl Selection {
    /// `(start, end)` in document order, regardless of which end the user
    /// dragged toward. Used by render + delete/copy so they never store or
    /// branch on direction.
    pub(super) fn normalized(&self) -> (Pos, Pos) {
        if self.anchor <= self.head {
            (self.anchor, self.head)
        } else {
            (self.head, self.anchor)
        }
    }

    fn is_empty(&self) -> bool {
        self.anchor == self.head
    }
}

/// Which part of a given line is inside `selection` (for reverse-video render).
/// `Full` = the whole line (incl. an empty line inside a multiline selection,
/// which renders a full-width highlight bar). `Range(a, b)` = chars `[a, b)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum LineSel {
    None,
    Full,
    Range(usize, usize),
}

/// Project a selection onto one line (`line_idx`, `len` = char count of that
/// line). `None` selection / empty selection → no highlight.
pub(super) fn line_selection(line_idx: usize, len: usize, selection: Option<Selection>) -> LineSel {
    let Some(sel) = selection.filter(|s| !s.is_empty()) else {
        return LineSel::None;
    };
    let (start, end) = sel.normalized();
    if line_idx < start.line || line_idx > end.line {
        return LineSel::None;
    }
    if start.line == end.line {
        // Single line: [start.col, end.col). Whole-line if it spans the line.
        if start.col == 0 && end.col >= len {
            LineSel::Full
        } else {
            LineSel::Range(start.col, end.col)
        }
    } else if line_idx == start.line {
        // First line of a multi-line selection: from start.col to EOL.
        if start.col == 0 {
            LineSel::Full
        } else {
            LineSel::Range(start.col, len)
        }
    } else if line_idx == end.line {
        // Last line: from BOL to end.col.
        if end.col >= len {
            LineSel::Full
        } else {
            LineSel::Range(0, end.col)
        }
    } else {
        // Strictly between → entire line.
        LineSel::Full
    }
}

/// Snap a char index to the START of its grapheme cluster (C7). Selection
/// endpoints pass through this so a multi-code-point grapheme (ZWJ emoji like
/// 👨‍👩‍👧, or `é` = e + U+0301 combining acute) is never split — the boundary is
/// always at a grapheme edge. `char_idx` past the end clamps to `char_count`.
pub(super) fn snap_to_grapheme_start(line: &str, char_idx: usize) -> usize {
    let target = char_idx.min(char_count(line));
    let mut cursor = 0usize; // running char offset
    for grapheme in line.graphemes(true) {
        let len = grapheme.chars().count();
        if cursor == target {
            return target; // exactly on a boundary
        }
        if cursor + len > target {
            return cursor; // target is inside this grapheme → snap to its start
        }
        cursor += len;
    }
    target
}

/// Direction for [`word_boundary`] / `move_word` (plan P5).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WordDir {
    Left,
    Right,
}

// ── Spike 8: note-link detection under the caret (`[[wikilink]]` / md-link) ──

/// If `col` (a char index) falls inside a note-link span in `line`, return that
/// link's target (`CLAUDE.md` §1.2). Pure + allocation-free unless it returns
/// `Some`, so the scan is unit-testable without an `EditorState`.
///
/// - `[[target]]` / `[[target|alias]]` → `target` (text before any `|`).
/// - `![[embed]]` is skipped (an image embed, not a note link).
/// - `[label](target)` → `target`, but external `scheme:` URLs are skipped and a
///   trailing `#fragment`/`?query` is stripped (parity with `extract_note_links`).
///
/// The caret is "on" a span when `col ∈ [start, end)` in char space.
fn link_target_at(line: &str, col: usize) -> Option<String> {
    let chars: Vec<char> = line.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        // `[[ wikilink ]]` — but not `![[` (an image embed).
        if chars[i] == '[' && chars.get(i + 1) == Some(&'[') && (i == 0 || chars[i - 1] != '!') {
            if let Some(close) = find_seq(&chars, i + 2, "]]") {
                let span_end = close + 2;
                if col_in(col, i, span_end) {
                    let inner: String = chars[i + 2..close].iter().collect();
                    let target = inner.split('|').next().unwrap_or("").trim();
                    if !target.is_empty() {
                        return Some(target.to_string());
                    }
                }
                i = span_end;
                continue;
            }
        }
        // `[label](target)` — a single `[`, not `[[`.
        if chars[i] == '[' && chars.get(i + 1) != Some(&'[') {
            if let Some(bracket) = find_char(&chars, i + 1, ']') {
                if chars.get(bracket + 1) == Some(&'(') {
                    if let Some(paren) = find_char(&chars, bracket + 2, ')') {
                        let span_end = paren + 1;
                        if col_in(col, i, span_end) {
                            let raw: String = chars[bracket + 2..paren].iter().collect();
                            if let Some(t) = strip_link_target(&raw) {
                                return Some(t);
                            }
                        }
                        i = span_end;
                        continue;
                    }
                }
            }
        }
        i += 1;
    }
    None
}

/// `col ∈ [start, end)`.
fn col_in(col: usize, start: usize, end: usize) -> bool {
    col >= start && col < end
}

/// First index at/after `from` whose chars begin `seq`; `None` if absent.
fn find_seq(chars: &[char], from: usize, seq: &str) -> Option<usize> {
    let needle: Vec<char> = seq.chars().collect();
    (from..chars.len()).find(|&start| {
        start + needle.len() <= chars.len() && chars[start..start + needle.len()] == needle[..]
    })
}

/// First index at/after `from` equal to `target`; `None` if absent.
fn find_char(chars: &[char], from: usize, target: char) -> Option<usize> {
    (from..chars.len()).find(|&i| chars[i] == target)
}

/// Normalize a Markdown-link target: trim, drop `#fragment`/`?query`, and reject
/// external `scheme:` URLs. Returns `None` if it isn't a vault note link.
fn strip_link_target(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    // A relative vault path never contains ':'; `scheme:` (http://, mailto:, …)
    // marks an external URL.
    if trimmed.is_empty() || trimmed.contains(':') {
        return None;
    }
    let bare = trimmed.split(['#', '?']).next().unwrap_or("").trim();
    if bare.is_empty() {
        None
    } else {
        Some(bare.to_string())
    }
}

/// Classification used by [`word_boundary`] to group runs. Whitespace, word
/// chars (alphanumeric + `_`), and punctuation are three distinct runs so
/// `foo, bar` has four boundaries: `foo` | `,` | (space) | `bar` — matching the
/// Ctrl+Left/Right "jump one word-or-token" feel users expect.
#[derive(PartialEq, Eq)]
enum WordClass {
    Whitespace,
    Word,
    Punct,
}

fn word_class(c: char) -> WordClass {
    if c.is_whitespace() {
        WordClass::Whitespace
    } else if c.is_alphanumeric() || c == '_' {
        WordClass::Word
    } else {
        WordClass::Punct
    }
}

/// Pure word-boundary finder for P5 word-motion. Returns the char index of the
/// next/previous word boundary on a SINGLE line (the caller handles line-wrap).
/// Symmetric:
/// - Right: skip the run under `from` (ws/word/punct), then skip any trailing
///   whitespace so the caret lands at the START of the next token (or EOL).
/// - Left: skip the whitespace before `from`, then skip one run, landing at the
///   START of the previous token (or col 0).
///
/// `from` is clamped to `[0, char_count(line)]`.
fn word_boundary(line: &str, from: usize, dir: WordDir) -> usize {
    let chars: Vec<char> = line.chars().collect();
    let n = chars.len();
    let from = from.min(n);
    match dir {
        WordDir::Right => {
            if from >= n {
                return n;
            }
            let mut i = from;
            let start = word_class(chars[i]);
            while i < n && word_class(chars[i]) == start {
                i += 1;
            }
            while i < n && matches!(word_class(chars[i]), WordClass::Whitespace) {
                i += 1;
            }
            i
        }
        WordDir::Left => {
            if from == 0 {
                return 0;
            }
            let mut i = from;
            while i > 0 && matches!(word_class(chars[i - 1]), WordClass::Whitespace) {
                i -= 1;
            }
            if i == 0 {
                return 0;
            }
            let target = word_class(chars[i - 1]);
            while i > 0 && word_class(chars[i - 1]) == target {
                i -= 1;
            }
            i
        }
    }
}

/// Snap a char index to a grapheme boundary, moving in `dir` if it lands
/// mid-cluster (B4). A combining mark makes `word_boundary` return an index
/// INSIDE a grapheme (e.g. `é` = e + U+0301); this advances past the cluster
/// for `Right` (to the next grapheme start, or EOL) and retracts to the
/// cluster's own start for `Left`. Indices already on a boundary are unchanged.
fn snap_grapheme_dir(line: &str, idx: usize, dir: WordDir) -> usize {
    let n = char_count(line);
    let idx = idx.min(n);
    // On a boundary already? (EOL is always a boundary; else the index must be
    // a grapheme start, i.e. snap-to-start is a no-op.)
    if idx == n || snap_to_grapheme_start(line, idx) == idx {
        return idx;
    }
    match dir {
        WordDir::Right => {
            // Advance past the grapheme that contains `idx` to its end (= next
            // grapheme start, or EOL).
            let mut cursor = 0;
            for g in line.graphemes(true) {
                let len = g.chars().count();
                if cursor + len > idx {
                    return (cursor + len).min(n);
                }
                cursor += len;
            }
            n
        }
        WordDir::Left => snap_to_grapheme_start(line, idx),
    }
}

/// A throwaway editor state for the App-free `apply_action` path.
#[cfg(test)]
pub(super) fn state_from_body(body: &str) -> EditorState {
    let path = RelativeNotePath::new("Scratch.md").expect("test path");
    let doc = NoteDocument::from_raw(path, body, 0);
    EditorState::from_doc(doc)
}

// ── Delete / copy / cut (P4, contracts C1/C6) ──────────────────────────

/// Select a char range `[a, b)` on line 0 of `body` (forward), then return
/// the state ready for a delete/copy assertion.
#[cfg(test)]
pub(super) fn select_line_range(body: &str, a: usize, b: usize) -> EditorState {
    let mut s = state_from_body(body);
    s.cx = a;
    s.apply_action(Action::SelectRight);
    // Extend to col b (SelectRight moves head by 1 each call from col a).
    for _ in a..b.saturating_sub(1) {
        s.apply_action(Action::SelectRight);
    }
    s
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

    /// Behavioral parity on the App-free path: `apply_action` routes the same
    /// mutations the old direct calls did (insert / motion / backspace / tab).
    #[test]
    fn apply_action_insert_move_backspace_tab() {
        let mut s = state_from_body("hi");
        s.apply_action(Action::MoveEnd);
        assert_eq!(s.cx, 2);
        s.apply_action(Action::InsertChar('!'));
        assert_eq!(s.body(), "hi!");
        assert_eq!(s.cx, 3);
        s.apply_action(Action::Backspace);
        assert_eq!(s.body(), "hi");
        assert_eq!(s.cx, 2);
        s.apply_action(Action::MoveHome);
        assert_eq!(s.cx, 0);
        s.apply_action(Action::InsertChar('>'));
        assert_eq!(s.body(), ">hi");
        s.apply_action(Action::Tab); // two spaces
        assert_eq!(s.body(), ">  hi");
    }

    // ── Selection: grapheme snap (P1, contract C7) ─────────────────────────

    /// `snap_to_grapheme_start` never lands inside a multi-code-point grapheme:
    /// a combining mark or a ZWJ-emoji sequence collapses to its leading
    /// boundary. CJK / ASCII chars are single-code-point graphemes, so snap is
    /// the identity there.
    #[test]
    fn snap_to_grapheme_boundaries() {
        // ZWJ family emoji 👨‍👩‍👧 = 5 scalar values, 1 grapheme → any interior
        // index snaps to 0; only index 0 and 5 (past-end) are valid boundaries.
        let zwj = "👨‍👩‍👧";
        assert_eq!(char_count(zwj), 5, "fixture: 5 code points");
        assert_eq!(snap_to_grapheme_start(zwj, 0), 0);
        assert_eq!(
            snap_to_grapheme_start(zwj, 1),
            0,
            "interior snaps to cluster start"
        );
        assert_eq!(snap_to_grapheme_start(zwj, 3), 0);
        assert_eq!(
            snap_to_grapheme_start(zwj, 5),
            5,
            "past-end boundary is valid"
        );
        assert_eq!(
            snap_to_grapheme_start(zwj, 99),
            5,
            "out-of-range clamps to char_count"
        );

        // Combining acute: "e" + U+0301 = 2 code points, 1 grapheme.
        let acc = "e\u{0301}";
        assert_eq!(char_count(acc), 2);
        assert_eq!(
            snap_to_grapheme_start(acc, 1),
            0,
            "between base + combining snaps to base"
        );

        // CJK + ASCII are 1-code-point-per-grapheme → identity.
        for (line, idx) in [("你好", 1usize), ("abc", 1usize), ("你好", 0usize)] {
            assert_eq!(
                snap_to_grapheme_start(line, idx),
                idx,
                "single-cp grapheme is identity"
            );
        }

        // Mixed: a mid-line ZWJ emoji cluster absorbs interior snaps but leaves
        // the surrounding ASCII boundaries alone. "ab" + 👨‍👩‍👧 + "cd".
        let mixed = "ab👨‍👩‍👧cd";
        assert_eq!(char_count(mixed), 9);
        assert_eq!(
            snap_to_grapheme_start(mixed, 2),
            2,
            "emoji cluster start is a boundary"
        );
        assert_eq!(
            snap_to_grapheme_start(mixed, 5),
            2,
            "mid-emoji snaps to cluster start"
        );
        assert_eq!(
            snap_to_grapheme_start(mixed, 7),
            7,
            "'c' after emoji is a boundary"
        );
    }

    // ── Selection: normalized + per-line projection (P1) ────────────────────

    #[test]
    fn selection_normalized_forward_and_backward() {
        let fwd = Selection {
            anchor: Pos { line: 0, col: 2 },
            head: Pos { line: 0, col: 5 },
        };
        assert_eq!(
            fwd.normalized(),
            (Pos { line: 0, col: 2 }, Pos { line: 0, col: 5 })
        );
        let bwd = Selection {
            anchor: Pos { line: 0, col: 5 },
            head: Pos { line: 0, col: 2 },
        };
        // Backward drag still yields document-order (start,end) — direction is
        // never stored (DRY, C5).
        assert_eq!(bwd.normalized(), fwd.normalized());
        let multi = Selection {
            anchor: Pos { line: 3, col: 4 },
            head: Pos { line: 1, col: 1 },
        };
        assert_eq!(
            multi.normalized(),
            (Pos { line: 1, col: 1 }, Pos { line: 3, col: 4 })
        );
        assert!(Selection {
            anchor: Pos::default(),
            head: Pos::default()
        }
        .is_empty());
    }

    /// `line_selection` projects a selection onto each line's highlight: Full
    /// (whole line / blank line in a multiline selection), Range(a,b), or None.
    #[test]
    fn line_selection_projects_to_each_line() {
        let s = |a_line, a_col, h_line, h_col| Selection {
            anchor: Pos {
                line: a_line,
                col: a_col,
            },
            head: Pos {
                line: h_line,
                col: h_col,
            },
        };
        // No / empty selection → no highlight.
        assert_eq!(line_selection(0, 5, None), LineSel::None);
        assert_eq!(
            line_selection(0, 5, Some(s(0, 2, 0, 2))),
            LineSel::None,
            "empty selection (anchor==head) is not a highlight"
        );
        // Single-line partial / whole.
        assert_eq!(
            line_selection(0, 5, Some(s(0, 1, 0, 3))),
            LineSel::Range(1, 3)
        );
        assert_eq!(line_selection(0, 5, Some(s(0, 0, 0, 5))), LineSel::Full);
        // Lines outside the range.
        assert_eq!(line_selection(9, 5, Some(s(0, 0, 2, 3))), LineSel::None);

        // Multiline selection over lines of len [5, 0, 6] starting mid line-0:
        // anchor (0,2) → head (2,4).
        let multi = s(0, 2, 2, 4);
        assert_eq!(
            line_selection(0, 5, Some(multi)),
            LineSel::Range(2, 5),
            "first line: start.col→EOL"
        );
        assert_eq!(
            line_selection(1, 0, Some(multi)),
            LineSel::Full,
            "blank middle line → full bar"
        );
        assert_eq!(
            line_selection(2, 6, Some(multi)),
            LineSel::Range(0, 4),
            "last line: BOL→end.col"
        );
        assert_eq!(
            line_selection(3, 6, Some(multi)),
            LineSel::None,
            "line after selection"
        );
    }

    // ── Selection: editor state lifecycle (P2, contracts C5/C7) ────────────

    #[test]
    fn extend_selection_anchors_then_moves_head() {
        let mut s = state_from_body("hello");
        assert!(s.selection().is_none());
        // SelectRight from col 0 → anchor (0,0); head (the caret) moves to col 1.
        s.apply_action(Action::SelectRight);
        let sel = s.selection().expect("selection active");
        assert_eq!(sel.anchor, Pos { line: 0, col: 0 });
        assert_eq!(sel.head, Pos { line: 0, col: 1 });
    }

    #[test]
    fn backward_selection_normalizes_for_render() {
        // Start at end, drag left: head ends behind anchor → normalized still
        // ascending (what render + delete rely on, C5).
        let mut s = state_from_body("hello");
        s.apply_action(Action::MoveEnd);
        assert_eq!(s.cx, 5);
        s.apply_action(Action::SelectLeft);
        let sel = s.selection().expect("selection active");
        assert_eq!(sel.anchor, Pos { line: 0, col: 5 });
        assert_eq!(sel.head, Pos { line: 0, col: 4 });
        let (start, end) = sel.normalized();
        assert!(start <= end, "normalized is document order");
        assert_eq!((start.col, end.col), (4, 5));
    }

    #[test]
    fn plain_move_clears_selection_and_insert_replaces_it() {
        let mut s = state_from_body("hello");
        s.apply_action(Action::SelectRight);
        assert!(s.selection().is_some());
        // A plain move clears the selection (does not extend it).
        s.apply_action(Action::MoveLeft);
        assert!(s.selection().is_none());
        // Re-select char 0 ('h'), then type — C6 replaces the selection: 'h' is
        // deleted and 'z' inserted in its place, caret lands after the new char.
        s.apply_action(Action::SelectRight);
        assert_eq!(
            s.selection().map(|x| x.normalized()),
            Some((Pos { line: 0, col: 0 }, Pos { line: 0, col: 1 }))
        );
        s.apply_action(Action::InsertChar('z'));
        assert!(s.selection().is_none(), "insert collapses the selection");
        assert_eq!(s.body(), "zello", "C6: typing replaces the selected char");
        assert_eq!(s.cx, 1, "caret lands after the inserted char");
    }

    #[test]
    fn clear_selection_action_drops_selection() {
        let mut s = state_from_body("hello");
        s.apply_action(Action::SelectRight);
        s.apply_action(Action::ClearSelection); // Esc
        assert!(s.selection().is_none());
    }

    #[test]
    fn select_all_selects_entire_buffer() {
        let mut s = state_from_body("ab\ncd");
        s.apply_action(Action::SelectAll);
        let sel = s.selection().expect("select-all active");
        assert_eq!(sel.anchor, Pos { line: 0, col: 0 });
        assert_eq!(
            sel.head,
            Pos { line: 1, col: 2 },
            "head at last char of last line"
        );
    }

    #[test]
    fn select_all_empty_buffer_is_empty_selection() {
        let mut s = state_from_body("");
        s.apply_action(Action::SelectAll);
        // Empty buffer → head==anchor → not a visible selection (C5).
        assert!(s.selection().is_none());
    }

    #[test]
    fn reload_drops_active_selection() {
        let mut s = state_from_body("hello");
        s.apply_action(Action::SelectRight);
        assert!(s.selection().is_some());
        // reload swaps the buffer; the selection (indexed into the old lines)
        // must not survive (C5).
        let path = RelativeNotePath::new("Scratch.md").expect("test path");
        let doc = NoteDocument::from_raw(path, "fresh", 0);
        s.reload(doc);
        assert!(s.selection().is_none());
    }

    #[test]
    fn delete_range_single_line() {
        let mut s = select_line_range("hello", 1, 3); // select "el"
        let (start, end) = s.selection().unwrap().normalized();
        s.delete_range(start, end);
        assert_eq!(s.body(), "hlo");
        assert_eq!(s.cx, 1, "caret lands at the deletion start");
        assert!(s.selection().is_none(), "delete clears the selection");
    }

    #[test]
    fn delete_range_backward_normalizes() {
        // Backward selection (anchor after head) still deletes the same span.
        let mut s = state_from_body("hello");
        s.cx = 3;
        s.selection_anchor = Some(Pos { line: 0, col: 1 }); // anchor 1, head 3
        let (start, end) = s.selection().unwrap().normalized();
        assert_eq!((start.col, end.col), (1, 3));
        s.delete_range(start, end);
        assert_eq!(s.body(), "hlo", "backward range deletes identically");
    }

    #[test]
    fn delete_range_across_lines_joins() {
        // Select line 0 col 1 → line 2 col 1 over "ab\nCD\nef": spans
        // "b" + "\nCD" + "\ne". Deleting leaves "a" + "f" (end-line tail) = "af".
        let mut s = state_from_body("ab\nCD\nef");
        s.selection_anchor = Some(Pos { line: 0, col: 1 });
        s.cy = 2;
        s.cx = 1;
        let (start, end) = s.selection().unwrap().normalized();
        s.delete_range(start, end);
        assert_eq!(
            s.body(),
            "af",
            "middle line dropped, end-line tail joined on"
        );
        assert_eq!(s.cy, 0);
        assert_eq!(s.cx, 1, "caret at the deletion start");
    }

    #[test]
    fn delete_range_empty_is_noop() {
        let mut s = state_from_body("hello");
        s.delete_range(Pos { line: 0, col: 2 }, Pos { line: 0, col: 2 });
        assert_eq!(s.body(), "hello", "empty range deletes nothing");
    }

    #[test]
    fn delete_range_multibyte_uses_char_boundaries() {
        // "你好世界", delete chars [1,3) = "好世" → "你好"... wait [1,3) = 好世, leaving 你界.
        let mut s = state_from_body("你好世界");
        s.delete_range(Pos { line: 0, col: 1 }, Pos { line: 0, col: 3 });
        assert_eq!(s.body(), "你界", "CJK deleted as whole chars, no panic");
    }

    #[test]
    fn selected_text_single_and_multiline() {
        let s = select_line_range("hello", 1, 4); // "ell"
        assert_eq!(s.selected_text().as_deref(), Some("ell"));

        let mut m = state_from_body("ab\nCD\nef");
        m.selection_anchor = Some(Pos { line: 0, col: 1 });
        m.cy = 2;
        m.cx = 1;
        // "b" + "\nCD" + "\ne" → "b\nCD\ne"
        assert_eq!(m.selected_text().as_deref(), Some("b\nCD\ne"));
    }

    #[test]
    fn selected_text_none_when_empty() {
        let s = state_from_body("hello");
        assert!(s.selected_text().is_none());
    }

    #[test]
    fn delete_forward_in_line_and_join_at_eol() {
        let mut s = state_from_body("abc");
        s.cx = 1;
        s.apply_action(Action::DeleteForward);
        assert_eq!(s.body(), "ac", "deletes the char after the caret");
        assert_eq!(s.cx, 1, "caret stays put");

        // At EOL, forward-delete joins the next line up.
        let mut s = state_from_body("ab\ncd");
        s.cx = 2; // end of "ab"
        s.apply_action(Action::DeleteForward);
        assert_eq!(s.body(), "abcd", "EOL forward-delete joins lines");
        assert_eq!(s.cx, 2, "caret stays at the join point");
    }

    /// C6 across the board: Backspace/Tab/DeleteForward on a selection all
    /// collapse to a plain delete of the range.
    #[test]
    fn destructive_keys_replace_selection() {
        // Backspace on selection just deletes (no extra char removed).
        let mut s = select_line_range("hello", 1, 3);
        s.apply_action(Action::Backspace);
        assert_eq!(s.body(), "hlo");
        assert_eq!(s.cx, 1);

        // DeleteForward on a selection deletes the range (not the next char).
        let mut s = select_line_range("hello", 1, 3);
        s.apply_action(Action::DeleteForward);
        assert_eq!(s.body(), "hlo");

        // Tab on a selection deletes the range and inserts two spaces.
        let mut s = select_line_range("hello", 1, 3);
        s.apply_action(Action::Tab);
        assert_eq!(s.body(), "h  lo");
    }

    // ── Word-motion + extend (P5, plan §4 Phase 5) ────────────────────────

    /// Pure word-boundary: right-jump lands at the start of the next token,
    /// skipping the current run + trailing whitespace.
    #[test]
    fn word_boundary_right_lands_at_next_token_start() {
        let line = "foo bar baz";
        // 0:f 1:o 2:o 3:space 4:b 5:a 6:r 7:space 8:b 9:a 10:z
        assert_eq!(word_boundary(line, 0, WordDir::Right), 4, "foo → bar");
        assert_eq!(word_boundary(line, 2, WordDir::Right), 4, "mid-foo → bar");
        assert_eq!(word_boundary(line, 4, WordDir::Right), 8, "bar → baz");
        assert_eq!(word_boundary(line, 8, WordDir::Right), 11, "baz → EOL");
        assert_eq!(word_boundary(line, 11, WordDir::Right), 11, "at EOL stays");
        // On a space, the space is its own run → skip to next token start.
        assert_eq!(word_boundary(line, 3, WordDir::Right), 4, "space → bar");
    }

    /// Punctuation is its own run, so `foo, bar` has a boundary at the comma.
    #[test]
    fn word_boundary_right_treats_punctuation_as_own_run() {
        let line = "foo, bar";
        // 0:f 1:o 2:o 3:, 4:space 5:b 6:a 7:r
        assert_eq!(word_boundary(line, 0, WordDir::Right), 3, "foo → comma");
        assert_eq!(word_boundary(line, 3, WordDir::Right), 5, "comma → bar");
    }

    /// Symmetric left-jump lands at the start of the previous token.
    #[test]
    fn word_boundary_left_lands_at_prev_token_start() {
        let line = "foo bar baz";
        assert_eq!(word_boundary(line, 11, WordDir::Left), 8, "EOL → baz start");
        assert_eq!(word_boundary(line, 8, WordDir::Left), 4, "baz → bar start");
        assert_eq!(word_boundary(line, 4, WordDir::Left), 0, "bar → foo start");
        assert_eq!(word_boundary(line, 0, WordDir::Left), 0, "col 0 stays");
    }

    /// CJK: each char is alphanumeric (a word run), so a jump moves over the
    /// whole run at once — no byte/char mismatch because indices stay char-based.
    #[test]
    fn word_boundary_handles_cjk_word_run() {
        let line = "你好 world"; // 0:你 1:好 2:space 3:w 4:o 5:r 6:l 7:d
        assert_eq!(word_boundary(line, 0, WordDir::Right), 3, "CJK run → world");
        assert_eq!(word_boundary(line, 3, WordDir::Right), 8, "world → EOL");
        assert_eq!(
            word_boundary(line, 8, WordDir::Left),
            3,
            "EOL → world start"
        );
        assert_eq!(
            word_boundary(line, 3, WordDir::Left),
            0,
            "world → CJK start"
        );
    }

    /// Ctrl+Right moves the caret by word AND clears the selection (C5).
    #[test]
    fn word_right_moves_caret_and_clears_selection() {
        let mut s = select_line_range("foo bar", 0, 2); // select "fo", caret at col 2
        s.apply_action(Action::WordRight);
        assert_eq!(s.cx, 4, "caret jumped to start of `bar`");
        assert!(s.selection().is_none(), "plain word-move clears selection");
    }

    /// Ctrl+Left moves the caret left by word and clears the selection.
    #[test]
    fn word_left_moves_caret_and_clears_selection() {
        let mut s = select_line_range("foo bar", 4, 6); // select "ba", caret at col 6
        s.apply_action(Action::WordLeft);
        assert_eq!(s.cx, 4, "caret jumped to start of `bar`");
        assert!(s.selection().is_none());
    }

    /// Ctrl+Shift+Right extends the selection by a whole word (anchor fixed).
    #[test]
    fn select_word_right_extends_head_by_word() {
        let mut s = state_from_body("foo bar baz");
        s.cx = 0;
        s.apply_action(Action::SelectWordRight); // extend 0 → 4
        assert_eq!(s.cx, 4, "head moved to start of `bar`");
        let sel = s.selection().expect("selection active");
        assert_eq!(sel.normalized().0.col, 0, "anchor stayed at 0");
        assert_eq!(sel.normalized().1.col, 4, "head at 4");
        assert_eq!(s.selected_text().as_deref(), Some("foo "));
    }

    /// Ctrl+Shift+Left extends the selection leftward by a word.
    #[test]
    fn select_word_left_extends_head_by_word() {
        let mut s = state_from_body("foo bar baz");
        s.cx = 8; // start of `baz`
        s.apply_action(Action::SelectWordLeft); // 8 → 4
        assert_eq!(s.cx, 4, "head moved to start of `bar`");
        // anchor at 8, head at 4 → normalized [4, 8) = "bar "
        assert_eq!(s.selected_text().as_deref(), Some("bar "));
    }

    /// Word-motion crosses line edges the same way Left/Right do.
    #[test]
    fn word_right_at_eol_wraps_to_next_line() {
        let mut s = state_from_body("foo\nbar"); // line0 "foo", line1 "bar"
        s.cx = 3; // at EOL of line 0
        s.apply_action(Action::WordRight);
        assert_eq!(s.cy, 1, "wrapped to next line");
        assert_eq!(s.cx, 0, "at col 0 of line 1");
    }

    // ── Validation fixes (code-review #1/#4, architect #2) ────────────────

    /// MAJOR (code-review #1): Enter replaces an active selection (C6/C5). Enter
    /// is dispatched in `dispatch_edit` (the image-overlay probe needs App), so
    /// it can't go through `apply_action`'s centralized C6 guard — the replace
    /// is handled in the Enter arm itself. Simulate that arm's selection branch.
    #[test]
    fn enter_replaces_selection_then_clears_it() {
        let mut s = select_line_range("hello", 1, 3); // select "el", caret col 3
                                                      // Mirror dispatch_edit's Enter arm: delete the range, then newline.
        let (start, end) = s.selection().unwrap().normalized();
        s.delete_range(start, end);
        s.insert_newline();
        // "el" gone → "hlo", then the newline splits after the caret (col 1).
        assert_eq!(s.body(), "h\nlo", "selection replaced by a newline");
        assert!(s.selection().is_none(), "selection cleared");
    }

    /// full-buffer delete (plan §5 "full" case, code-review #4): selecting the
    /// whole note and deleting empties the buffer to a single empty line without
    /// panicking (the clamp + re-normalize path on a whole-buffer range).
    #[test]
    fn delete_range_full_buffer_empties_to_single_line() {
        let mut s = state_from_body("ab\ncd\nef");
        s.apply_action(Action::SelectAll);
        let (start, end) = s.selection().unwrap().normalized();
        s.delete_range(start, end);
        assert_eq!(s.body(), "", "whole buffer deleted");
        assert_eq!(s.lines.len(), 1, "buffer is one empty line, never zero");
        assert_eq!(s.cx, 0);
        assert_eq!(s.cy, 0);
    }

    /// delete_range's clamp is defense-in-depth: an out-of-range `Pos` must
    /// never panic and must re-normalize so the single-line branch never sees a
    /// reversed range. Drives the post-clamp `swap` added for code-review #2.
    #[test]
    fn delete_range_clamps_oversized_positions_without_panic() {
        let mut s = state_from_body("hi"); // chars: 'h'(0) 'i'(1)
                                           // Both endpoints far out of range: clamp + re-normalize must land on
                                           // `[col1, col2)` of line 0 without panic and without a reversed slice.
                                           // start{5,99} > end{0,1} initially → swapped to {0,1}..{0,2} post-clamp.
        let start = Pos { line: 5, col: 99 };
        let end = Pos { line: 0, col: 1 };
        s.delete_range(start, end); // must not panic
                                    // Deletes char index 1 ('i') → leaves 'h'.
        assert_eq!(s.body(), "h");
    }

    // ── Adversarial re-hunt fixes (B1 data-loss, B4 grapheme) ─────────────

    /// BLOCKER (B1): a mouse click arms a LATENT anchor (anchor == caret,
    /// `selection()` returns None). Before the fix, the first typed char
    /// drifted the caret off the anchor, so the NEXT keystroke hit the C6
    /// replace-on-type guard and deleted the char just typed — silent data
    /// loss on the most common interaction (click, then type). `insert_char`
    /// now clears the latent anchor.
    #[test]
    fn latent_anchor_after_click_does_not_eat_next_typed_char() {
        let mut s = state_from_body("hello");
        // Simulate a click at col 2: caret + latent anchor both at {0,2}.
        s.cx = 2;
        s.selection_anchor = Some(Pos { line: 0, col: 2 });
        assert!(
            s.selection().is_none(),
            "a click with no drag is an empty selection"
        );
        s.apply_action(Action::InsertChar('A')); // type 'A'
        assert_eq!(s.body(), "heAllo");
        assert!(
            s.selection_anchor.is_none(),
            "latent anchor cleared by the insert"
        );
        // The second keystroke must NOT C6-delete the char just typed.
        s.apply_action(Action::InsertChar('B'));
        assert_eq!(s.body(), "heABllo", "second char must not eat the first");
    }

    /// B1 also covers backspace/delete/newline — every raw buffer mutator drops
    /// the latent anchor so a stale gap can never re-emerge as a selection.
    #[test]
    fn latent_anchor_cleared_by_backspace_and_delete_forward() {
        let mut s = state_from_body("hello");
        s.cx = 2;
        s.selection_anchor = Some(Pos { line: 0, col: 2 }); // latent click
        s.apply_action(Action::Backspace);
        assert_eq!(s.body(), "hllo");
        assert!(
            s.selection_anchor.is_none(),
            "backspace dropped latent anchor"
        );
        assert!(
            s.selection().is_none(),
            "no spurious selection after backspace"
        );

        // delete_forward from a latent anchor likewise must not later select.
        let mut s = state_from_body("hello");
        s.cx = 1;
        s.selection_anchor = Some(Pos { line: 0, col: 1 });
        s.apply_action(Action::DeleteForward);
        assert_eq!(s.body(), "hllo");
        assert!(s.selection_anchor.is_none());
    }

    /// B4 (C7): plain Ctrl+Right over a base+combining grapheme (`é` = e +
    /// U+0301) must skip the WHOLE cluster. word_class sees the combining mark
    /// as Punct, so word_boundary returns col 1 (mid-grapheme); the
    /// direction-aware snap advances past it to col 2.
    #[test]
    fn plain_word_right_skips_combining_mark_grapheme() {
        let mut s = state_from_body("e\u{0301} x"); // 'é' (2 chars), space, 'x'
        s.cx = 0;
        s.apply_action(Action::WordRight);
        assert_eq!(
            s.cx, 2,
            "word-right lands past the é grapheme, not inside it"
        );
    }

    /// B4 (C7): pure helper — snap_grapheme_dir advances forward / retracts to a
    /// boundary. Direct test of the snap primitive the word-move relies on.
    #[test]
    fn snap_grapheme_dir_advances_and_retracts() {
        let line = "e\u{0301} x"; // graphemes: é(0-1), space(2), x(3)
                                  // col 1 is inside é. Right → next boundary (2); Left → this boundary (0).
        assert_eq!(snap_grapheme_dir(line, 1, WordDir::Right), 2);
        assert_eq!(snap_grapheme_dir(line, 1, WordDir::Left), 0);
        // Already-on-boundary indices pass through unchanged.
        assert_eq!(snap_grapheme_dir(line, 0, WordDir::Right), 0);
        assert_eq!(snap_grapheme_dir(line, 2, WordDir::Right), 2);
        // EOL (4) is a boundary.
        assert_eq!(snap_grapheme_dir(line, 4, WordDir::Right), 4);
    }

    // ── Spike 8: link_target_at ─────────────────────────────────────────────

    #[test]
    fn link_target_at_wikilink_under_caret() {
        // "see [[Robot]] now"  →  [[ ]] spans cols 4..12.
        let line = "see [[Robot]] now";
        assert_eq!(link_target_at(line, 4), Some("Robot".to_string()));
        assert_eq!(link_target_at(line, 11), Some("Robot".to_string())); // last ']' - 1
        assert_eq!(link_target_at(line, 0), None, "before the link");
        assert_eq!(link_target_at(line, 13), None, "after the link");
    }

    #[test]
    fn link_target_at_strips_wikilink_alias() {
        // [[Robot|the robot note]] → target is the text before '|'.
        let line = "[[Robot|the robot note]]";
        assert_eq!(link_target_at(line, 2), Some("Robot".to_string()));
    }

    #[test]
    fn link_target_at_skips_image_embed() {
        // ![[image.png]] is an embed, not a note link — never resolves.
        let line = "![[_attachments/x.png]]";
        assert_eq!(link_target_at(line, 3), None);
    }

    #[test]
    fn link_target_at_markdown_link() {
        // "go [see](Notes/r.md)" — the whole [..](..) span is clickable.
        let line = "go [see](Notes/r.md)!";
        assert_eq!(link_target_at(line, 3), Some("Notes/r.md".to_string()));
        assert_eq!(link_target_at(line, 19), Some("Notes/r.md".to_string())); // inside target
        assert_eq!(link_target_at(line, 0), None);
    }

    #[test]
    fn link_target_at_skips_external_url() {
        let line = "[web](https://example.com/x)";
        assert_eq!(link_target_at(line, 0), None, "scheme: is not a note link");
    }

    #[test]
    fn link_target_at_strips_fragment_and_query() {
        assert_eq!(
            link_target_at("[x](Robot.md#heading)", 0),
            Some("Robot.md".to_string())
        );
        assert_eq!(
            link_target_at("[x](Robot.md?q=1)", 0),
            Some("Robot.md".to_string())
        );
    }

    #[test]
    fn link_target_at_no_link_returns_none() {
        assert_eq!(link_target_at("plain text", 3), None);
        assert_eq!(link_target_at("", 0), None);
    }

    /// Back-nav stack: a no-op open (same note) records nothing; distinct opens
    /// record predecessors LIFO; popping reverses the walk.
    #[test]
    fn push_history_records_predecessors_lifo() {
        let mut s = state_from_body("body"); // path = Scratch.md
        let mk = |p: &str| RelativeNotePath::new(p).unwrap();
        // Opening the SAME note records nothing.
        s.push_history(&mk("Scratch.md"));
        assert!(s.note_history.is_empty());
        // Walk Scratch → A → B → C, recording each predecessor.
        s.push_history(&mk("A.md"));
        s.path = mk("A.md");
        s.push_history(&mk("B.md"));
        s.path = mk("B.md");
        s.push_history(&mk("C.md"));
        assert_eq!(
            s.note_history
                .iter()
                .map(|p| p.as_str())
                .collect::<Vec<_>>(),
            vec!["Scratch.md", "A.md", "B.md"]
        );
        // LIFO pop reverses the walk.
        assert_eq!(s.note_history.pop().unwrap().as_str(), "B.md");
        assert_eq!(s.note_history.pop().unwrap().as_str(), "A.md");
        assert_eq!(s.note_history.pop().unwrap().as_str(), "Scratch.md");
        assert!(s.note_history.is_empty());
    }

    /// Back-nav stack is capped — pushing past 50 drops the oldest entry.
    #[test]
    fn push_history_caps_at_50() {
        let mut s = state_from_body("body");
        for i in 0..60 {
            s.push_history(&RelativeNotePath::new(format!("n{i}.md")).unwrap());
        }
        assert_eq!(s.note_history.len(), 50);
    }
}

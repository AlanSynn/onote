//! Terminal rendering — frame layout, editor surface, image glyphs, and the
//! display-column / inverse-map primitives, extracted from the original
//! god-module per `CLAUDE.md` §3.2 (mechanical split, zero logic change).
//! Pure of editor mutation — only reads [`super::editor::EditorState`].

use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Wrap};
use ratatui::Frame;
use ratatui_image::{Resize, StatefulImage};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::application::App;

use super::editor::{
    char_count, char_to_byte, line_selection, snap_to_grapheme_start, EditorState, LineSel,
    Selection,
};
use super::layout::{explorer_constraint, explorer_effective_visibility, Visibility};
use super::note_drawer::{render_explorer, ActivePane};
use super::{format_size, ImageOverlay, Mode};

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

pub(super) fn render(app: &App, state: &mut EditorState, frame: &mut Frame) {
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
    // Spike 7 P7.0/P7.2: responsive Explorer drawer (basalt-style; `CLAUDE.md`
    // §3.2 `note_drawer`). Effective visibility folds the user's Ctrl+E toggle
    // over the auto-show width policy. When hidden, the horizontal split is
    // skipped entirely — the editor surface + mouse cell→char map are
    // byte-identical to pre-Spike-7 (zero regression). When visible, split the
    // content row into `[explorer | editor]`; `render_editor` re-captures the
    // (now narrower) inner rect into `state.editor_x/y/width` every frame, so
    // the mouse map auto-corrects the moment the explorer renders.
    state.frame_width = area.width;
    let cfg = app.config();
    let visible = explorer_effective_visibility(
        chunks[1].width,
        &cfg.layout,
        state.explorer_visible_override,
    );
    let editor_area = if !visible {
        chunks[1]
    } else {
        let h = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([
                explorer_constraint(Visibility::Visible, &cfg.layout),
                Constraint::Min(1),
            ])
            .split(chunks[1]);
        let active = state.active_pane == ActivePane::Explorer;
        render_explorer(&mut state.explorer, frame, h[0], active);
        h[1]
    };
    // `render_editor` records viewport height into `state.view_height` for the
    // mouse-scroll clamp. Cursor-follow (`adjust_scroll`) runs after each edit
    // key, not here, so a reader can mouse-scroll away from the cursor.
    render_editor(app, state, frame, editor_area);
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
pub(super) fn display_col_to_char_from_spans(
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
pub(super) fn display_col_to_char(app: &App, line: &str, display_col: usize) -> usize {
    let spans = glyph_spans(app, line);
    let c = display_col_to_char_from_spans(line, &spans, display_col);
    snap_to_grapheme_start(line, c)
}

/// Trailing path segment of an attachment target (file basename).
pub(super) fn basename(path: &str) -> &str {
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
}

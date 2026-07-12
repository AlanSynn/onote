//! Grapheme-aware soft-wrap of editor lines into fixed-width visual rows
//! (`CLAUDE.md` §9 + F1: narrow terminals must NOT clip overflowing text).
//!
//! Pure + App-free: a wrap pass takes a source line, its image-glyph spans
//! (the App-built `[image: name]` substitution ranges from `render::glyph_spans`),
//! and a display width, and returns the visual rows that line breaks into.
//! Wrapping is by DISPLAY width (terminal cells), so 2-wide CJK/emoji and the
//! substituted image glyphs count their rendered columns, not their byte/char
//! count — a line that fits on screen never wraps, and a wide char never splits
//! (the break always lands on a grapheme boundary).
//!
//! The buffer model is untouched: `EditorState` stays `(cy, cx)` char-indexed.
//! Only RENDER (emit one `Line` per visual row), CURSOR placement, and SCROLL
//! (measured in visual rows, not logical lines) route through the row table a
//! wrap produces. Mouse hits invert through the same table (a screen row → row
//! → source char range → hit column).

use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

use super::editor::char_count;

/// One visual row of a wrapped logical line: the source char range
/// `[char_start, char_end)` (half-open) within `line_idx`. A row never splits an
/// image glyph (glyphs are atomic — same contract as `render` C4) and never
/// splits a grapheme. An empty logical line yields one empty row `[0, 0)` so its
/// screen row is still a clickable, caret-able surface.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Row {
    pub(super) line_idx: usize,
    pub(super) char_start: usize,
    pub(super) char_end: usize,
}

impl Row {
    /// `true` when source char `cx` is ON this row. Half-open `[char_start,
    /// char_end)`: a char at the trailing edge (`cx == char_end`) is the NEXT
    /// row's leading edge, so it reads as off-row here. `caret_row` resolves
    /// trailing-edge/EOL separately because that's where soft-wrap caret lands.
    pub(super) fn contains_char(&self, cx: usize) -> bool {
        cx >= self.char_start && cx < self.char_end
    }
}

/// A wrappable piece of a line: a run of source text, or one atomic image glyph
/// (its source char range replaced by the glyph string on screen). Text pieces
/// split at any grapheme boundary; glyph pieces are atomic. The glyph's display
/// width lives in `Piece::width`; the glyph TEXT isn't needed here (render
/// re-substitutes it), so the variant carries no payload.
struct Piece<'a> {
    char_start: usize,
    char_end: usize,
    width: usize,
    kind: PieceKind<'a>,
}

enum PieceKind<'a> {
    Text(&'a str),
    Glyph,
}

/// Split `line` into ordered wrappable pieces from its image-glyph spans. `spans`
/// are `(byte_start, byte_end, glyph)` — byte-sorted + non-overlapping (the
/// `render::glyph_spans` contract). Text gaps between/around glyphs become `Text`
/// pieces carrying their source slice; each glyph becomes one `Glyph` piece whose
/// `width` is the GLYPH's display width (not the token's), matching how `render`
/// draws it.
fn build_pieces<'a>(line: &'a str, spans: &[(usize, usize, String)]) -> Vec<Piece<'a>> {
    let mut out: Vec<Piece<'a>> = Vec::new();
    let mut cur_byte = 0usize;
    let mut cur_char = 0usize;
    for (start, end, glyph) in spans {
        if *start > cur_byte {
            let text = &line[cur_byte..*start];
            let n = char_count(text);
            out.push(Piece {
                char_start: cur_char,
                char_end: cur_char + n,
                width: UnicodeWidthStr::width(text),
                kind: PieceKind::Text(text),
            });
            cur_char += n;
        }
        let body = &line[*start..*end];
        let n = char_count(body);
        out.push(Piece {
            char_start: cur_char,
            char_end: cur_char + n,
            width: UnicodeWidthStr::width(glyph.as_str()),
            kind: PieceKind::Glyph,
        });
        cur_char += n;
        cur_byte = *end;
    }
    if cur_byte < line.len() {
        let text = &line[cur_byte..line.len()];
        let n = char_count(text);
        out.push(Piece {
            char_start: cur_char,
            char_end: cur_char + n,
            width: UnicodeWidthStr::width(text),
            kind: PieceKind::Text(text),
        });
    }
    out
}

/// Wrap `line` (index `line_idx`) into visual rows of at most `width` display
/// cells, given its image-glyph spans. Grapheme-aware: a break always lands on a
/// grapheme boundary; a 2-wide char that doesn't fit starts a new row; an atomic
/// glyph that doesn't fit starts a new row, and one wider than `width` occupies
/// a row alone (the terminal clips it, same as the atomic selection/clip rule).
///
/// `width == 0` (a degenerate / hidden viewport) collapses to a single row over
/// the whole line — never panics, and the line still has a clickable/scrollable
/// surface.
pub(super) fn wrap_line(
    line_idx: usize,
    line: &str,
    spans: &[(usize, usize, String)],
    width: usize,
) -> Vec<Row> {
    let total = char_count(line);
    if width == 0 || total == 0 {
        return vec![Row {
            line_idx,
            char_start: 0,
            char_end: total,
        }];
    }
    let pieces = build_pieces(line, spans);
    let mut rows: Vec<Row> = Vec::new();
    let mut row_start = 0usize;
    let mut row_end = 0usize;
    let mut row_w = 0usize;

    macro_rules! flush {
        () => {{
            if row_end > row_start {
                rows.push(Row {
                    line_idx,
                    char_start: row_start,
                    char_end: row_end,
                });
            }
            row_start = row_end;
            row_w = 0;
        }};
    }

    for p in pieces {
        match p.kind {
            PieceKind::Glyph => {
                // Atomic: if it doesn't fit in the remaining row, break first.
                if row_w > 0 && row_w + p.width > width {
                    flush!();
                }
                row_end = p.char_end;
                row_w += p.width;
                // A glyph wider than a full row leaves row_w > width; the NEXT
                // piece's break-check (or the tail push below) emits it as its
                // own row. No mid-loop auto-flush needed — that avoids a dead
                // `row_w = 0` reset when this is the last piece.
            }
            PieceKind::Text(t) => {
                let mut g_char = p.char_start;
                for g in t.graphemes(true) {
                    let gw = UnicodeWidthStr::width(g);
                    let g_len = g.chars().count();
                    if row_w > 0 && row_w + gw > width {
                        flush!();
                    }
                    row_end = g_char + g_len;
                    row_w += gw;
                    g_char += g_len;
                }
            }
        }
    }
    // Tail: emit the final accumulated row (no reset — nothing follows).
    if row_end > row_start {
        rows.push(Row {
            line_idx,
            char_start: row_start,
            char_end: row_end,
        });
    }
    if rows.is_empty() {
        // Only reachable for a line whose pieces all vanished; keep the
        // one-empty-row-per-line invariant.
        rows.push(Row {
            line_idx,
            char_start: 0,
            char_end: 0,
        });
    }
    rows
}

/// Wrap an entire buffer (`lines`) into one flat visual-row list, in document
/// order. Convenience over per-line [`wrap_line`] for the render/scroll math
/// that needs the full row table (the glyph spans are computed per line by the
/// caller via `render::glyph_spans`). `spans_for(line_idx, line) -> Vec<…>` is
/// supplied by the caller so this stays App-free + unit-testable.
pub(super) fn wrap_all<F>(lines: &[String], width: usize, mut spans_for: F) -> Vec<Row>
where
    F: FnMut(usize, &str) -> Vec<(usize, usize, String)>,
{
    let mut out: Vec<Row> = Vec::new();
    for (i, l) in lines.iter().enumerate() {
        let spans = spans_for(i, l.as_str());
        out.extend(wrap_line(i, l.as_str(), &spans, width));
    }
    out
}

/// Index (in the flattened visual-row list) of the row holding the caret at
/// `(line_idx, col)`, plus that row. A strict-interior hit (`char_start <= col <
/// char_end`) wins outright — so the caret at the first char of a wrapped row
/// lands on THAT row (col 3 on rows `[0,3)[3,6)` is row 1's 'd'). The trailing
/// edge (`col == char_end` with no strict match) covers EOL on the last row and
/// empty rows. Returns `None` when no row is on `line_idx` (e.g. an empty
/// layout).
pub(super) fn caret_row(rows: &[Row], line_idx: usize, col: usize) -> Option<(usize, &Row)> {
    let mut trailing: Option<usize> = None;
    for (i, r) in rows.iter().enumerate() {
        if r.line_idx != line_idx {
            continue;
        }
        if r.contains_char(col) {
            return Some((i, r));
        }
        if col == r.char_end {
            trailing = Some(i);
        }
    }
    trailing.map(|i| (i, &rows[i]))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_line_is_one_row() {
        let rows = wrap_line(0, "hello", &[], 80);
        assert_eq!(
            rows,
            vec![Row {
                line_idx: 0,
                char_start: 0,
                char_end: 5
            }]
        );
    }

    #[test]
    fn empty_line_is_one_empty_row() {
        // An empty logical line still occupies a screen row (clickable/scrollable).
        let rows = wrap_line(3, "", &[], 80);
        assert_eq!(
            rows,
            vec![Row {
                line_idx: 3,
                char_start: 0,
                char_end: 0
            }]
        );
    }

    /// ASCII wraps exactly at the width boundary; rows are contiguous char
    /// ranges with no overlap and no gap.
    #[test]
    fn ascii_wraps_at_width_boundary() {
        // width 3: "abcdef" → [0,3) [3,6)
        let rows = wrap_line(0, "abcdef", &[], 3);
        assert_eq!(
            rows,
            vec![
                Row {
                    line_idx: 0,
                    char_start: 0,
                    char_end: 3
                },
                Row {
                    line_idx: 0,
                    char_start: 3,
                    char_end: 6
                },
            ]
        );
    }

    /// A word longer than the row still wraps at grapheme boundaries (no
    /// mid-grapheme split; we soft-wrap, we don't refuse to wrap long words).
    #[test]
    fn long_word_wraps_at_grapheme_boundaries() {
        // width 3: "hello" → [0,3)[3,5)
        let rows = wrap_line(0, "hello", &[], 3);
        assert_eq!(
            rows.iter()
                .map(|r| (r.char_start, r.char_end))
                .collect::<Vec<_>>(),
            vec![(0, 3), (3, 5)]
        );
    }

    /// A 2-wide CJK char that doesn't fit starts a new row — wrapping is by
    /// DISPLAY width, not char count. `好b` at width 2: `好` fills the row, `b`
    /// wraps.
    #[test]
    fn wide_char_wraps_when_it_does_not_fit() {
        let rows = wrap_line(0, "好b", &[], 2);
        assert_eq!(
            rows.iter()
                .map(|r| (r.char_start, r.char_end))
                .collect::<Vec<_>>(),
            vec![(0, 1), (1, 2)]
        );
    }

    /// A 2-wide CJK char that DOES fit stays on the row: `a好b` at width 4 →
    /// `a`(1) + `好`(2) = 3, then `b`(1) = 4 exactly fits → one row.
    #[test]
    fn wide_char_that_fits_stays_on_row() {
        let rows = wrap_line(0, "a好b", &[], 4);
        assert_eq!(rows.len(), 1);
        assert_eq!((rows[0].char_start, rows[0].char_end), (0, 3));
    }

    /// An image glyph is ATOMIC under wrapping (C4 parity): it never splits
    /// across rows. Glyph over chars [1,3) with display width 5 (`[img]`), line
    /// `abcd`, width 4: `a`(1) + glyph(5) would be 6 > 4 → break before the
    /// glyph. The glyph alone (5 > 4) takes its own row, then `d` follows.
    #[test]
    fn glyph_is_atomic_takes_its_own_row_when_too_wide() {
        let line = "abcd";
        let spans = vec![(1usize, 3usize, "[img]".to_string())]; // chars [1,3)
        let rows = wrap_line(0, line, &spans, 4);
        // Row0: [0,1) = "a". Row1: [1,3) = glyph (alone). Row2: [3,4) = "d".
        assert_eq!(
            rows.iter()
                .map(|r| (r.char_start, r.char_end))
                .collect::<Vec<_>>(),
            vec![(0, 1), (1, 3), (3, 4)]
        );
    }

    /// A glyph that fits stays inline. Line `ab![](x.png) cd` is too elaborate;
    /// instead: glyph over chars [0,2) width 3, line `XY` width 5 → one row.
    #[test]
    fn glyph_that_fits_stays_inline() {
        let line = "XY";
        let spans = vec![(0usize, 2usize, "[img]".to_string())];
        let rows = wrap_line(0, line, &spans, 5);
        assert_eq!(rows.len(), 1);
        assert_eq!((rows[0].char_start, rows[0].char_end), (0, 2));
    }

    /// `width == 0` collapses to a single row over the whole line — no panic.
    #[test]
    fn zero_width_collapses_to_one_row() {
        let rows = wrap_line(0, "hello world", &[], 0);
        assert_eq!(
            rows,
            vec![Row {
                line_idx: 0,
                char_start: 0,
                char_end: 11
            }]
        );
    }

    /// `wrap_all` flattens per-line rows in document order with correct
    /// `line_idx` stamps.
    #[test]
    fn wrap_all_flattens_in_document_order() {
        let lines = vec!["ab".to_string(), "cdef".to_string()];
        let rows = wrap_all(&lines, 2, |_, _| Vec::new());
        // line0 "ab"@2 → [0,2). line1 "cdef"@2 → [0,2)[2,4).
        assert_eq!(
            rows,
            vec![
                Row {
                    line_idx: 0,
                    char_start: 0,
                    char_end: 2
                },
                Row {
                    line_idx: 1,
                    char_start: 0,
                    char_end: 2
                },
                Row {
                    line_idx: 1,
                    char_start: 2,
                    char_end: 4
                },
            ]
        );
    }

    /// `caret_row` picks the strict-interior row; the trailing edge only covers
    /// EOL on the last row (and empty rows).
    #[test]
    fn caret_row_picks_strict_then_trailing_edge() {
        // line "abcdef" @ width 3 → rows [0,3)[3,6).
        let rows = wrap_line(0, "abcdef", &[], 3);
        // Strict interior of row 0.
        let (i, _) = caret_row(&rows, 0, 1).expect("col 1 in row 0");
        assert_eq!(i, 0);
        // col 3 == row1.char_start, strict interior of row 1 ('d') → row 1.
        let (i, _) = caret_row(&rows, 0, 3).expect("col 3 in row 1");
        assert_eq!(i, 1, "col at a row start lands on that row");
        // col 4 in row 1.
        let (i, _) = caret_row(&rows, 0, 4).expect("col 4 in row 1");
        assert_eq!(i, 1);
        // EOL col 6 → trailing edge of row 1 (its char_end).
        let (i, _) = caret_row(&rows, 0, 6).expect("EOL on last row");
        assert_eq!(i, 1);
        // No row on that line.
        assert!(caret_row(&rows, 9, 0).is_none());
    }

    /// `contains_char` is half-open `[char_start, char_end)`: the trailing edge
    /// belongs to the next row, so it reads off-row here (`caret_row` owns the
    /// trailing-edge/EOL resolution).
    #[test]
    fn contains_char_half_open_excludes_tail() {
        let r = Row {
            line_idx: 0,
            char_start: 0,
            char_end: 3,
        };
        assert!(r.contains_char(0));
        assert!(r.contains_char(2));
        assert!(!r.contains_char(3), "tail edge belongs to the next row");
        assert!(!r.contains_char(4));
    }
}

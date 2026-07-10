//! Responsive pane layout (basalt-style; `CLAUDE.md` §3.2 `note_drawer`).
//!
//! Pure, stateless policy: map terminal width + `[layout]` config to an Explorer
//! [`Visibility`] and a horizontal [`Constraint`]. No mutation, no TUI state —
//! the manual toggle (Ctrl+E) that *overrides* the auto policy arrives in P7.2,
//! at which point a `LayoutState` holding user-toggled visibility is threaded
//! through the loop. Keeping P7.0 stateless means `render` can recompute every
//! frame from `frame.area().width` with no extra parameter.
//!
//! Basalt's model (the reference): each drawer has `Visibility { Hidden, Visible,
//! FullWidth }` that drives a `Constraint`; below a width the drawer collapses so
//! the editor gets the full row. onote mirrors this for the LEFT Explorer; the
//! RIGHT Outline lands in Spike 8.

use ratatui::layout::{Constraint, Direction, Layout, Rect};

use crate::config::LayoutConfig;

/// A drawer's on-screen footprint. Drives the horizontal `Constraint`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(super) enum Visibility {
    /// No pane rendered — the editor takes the full content row (today's
    /// behavior). `render` skips the horizontal split entirely in this state, so
    /// the editor surface + mouse mapping are byte-identical to pre-Spike-7.
    #[default]
    Hidden,
    /// Standard pane width (`explorer_width` cols).
    Visible,
    // NOTE: a `FullWidth` variant (editor collapses, explorer fills the row —
    // basalt's focus mode) is intentionally absent until P7.2 constructs it.
    // Adding it now would be dead code with a dead `Constraint` arm and dead
    // tests; bring it back with the focus-mode feature that first produces it.
}

/// P7.0 auto-show policy: the Explorer is `Visible` at/above
/// `show_explorer_threshold`, else `Hidden`. (P7.2's manual toggle overrides.)
///
/// Pure + total — unit-testable without an `App` or `Frame`.
pub(super) fn explorer_visibility(width: u16, cfg: &LayoutConfig) -> Visibility {
    if width >= cfg.show_explorer_threshold {
        Visibility::Visible
    } else {
        Visibility::Hidden
    }
}

/// Effective Explorer visibility, folding the user's manual toggle (Ctrl+E,
/// P7.2) over the auto-show policy. `None` = auto (width-based);
/// `Some(true)`/`Some(false)` = forced on/off. `render` calls this to decide
/// whether to split the content row; the event handler calls it for the
/// focus-guard (a focused-but-hidden explorer would trap keystrokes).
pub(super) fn explorer_effective_visibility(
    width: u16,
    cfg: &LayoutConfig,
    user_override: Option<bool>,
) -> bool {
    match user_override {
        Some(forced) => forced,
        None => explorer_visibility(width, cfg) == Visibility::Visible,
    }
}

/// The Explorer's horizontal `Constraint` for a given visibility. `Hidden` is
/// unused by `render` (it skips the split), but is provided so a future
/// toggle-gutter can collapse to `explorer_hidden_width` rather than vanish.
pub(super) fn explorer_constraint(vis: Visibility, cfg: &LayoutConfig) -> Constraint {
    match vis {
        Visibility::Hidden => Constraint::Length(cfg.explorer_hidden_width),
        Visibility::Visible => Constraint::Length(cfg.explorer_width),
    }
}

/// Split the content row (the area between the path and status bars) into
/// `[explorer | editor]` horizontal rects, or hand the editor the full row when
/// the Explorer is hidden. `render` calls this every frame with the vertical
/// layout's middle row and the user's Ctrl+E override.
///
/// Pure + ratatui-only: the horizontal geometry is exercised by unit tests
/// without a full `App` or live `Frame` (mirroring the §9 small-terminal
/// guard's App-free boundary for the full-`render` path). Returning the
/// `Option<Rect>` keeps `render`'s hidden-branch byte-identical to pre-Spike-7:
/// no split, no explorer render call, editor surface untouched.
pub(super) fn split_content_row(
    content_area: Rect,
    cfg: &LayoutConfig,
    user_override: Option<bool>,
) -> (Option<Rect>, Rect) {
    if !explorer_effective_visibility(content_area.width, cfg, user_override) {
        return (None, content_area);
    }
    let areas = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            explorer_constraint(Visibility::Visible, cfg),
            Constraint::Min(1),
        ])
        .split(content_area);
    // `Layout::split` honors `explorer_width` exactly when it fits (Length) and
    // gives the editor the remainder via `Min(1)`; indexing returns `Rect` by
    // copy out of the `Rc<[Rect]>`.
    (Some(areas[0]), areas[1])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> LayoutConfig {
        LayoutConfig {
            explorer_width: 30,
            explorer_hidden_width: 4,
            show_explorer_threshold: 100,
        }
    }

    #[test]
    fn below_threshold_is_hidden() {
        assert_eq!(explorer_visibility(99, &cfg()), Visibility::Hidden);
        assert_eq!(explorer_visibility(0, &cfg()), Visibility::Hidden);
    }

    #[test]
    fn at_or_above_threshold_is_visible() {
        assert_eq!(explorer_visibility(100, &cfg()), Visibility::Visible);
        assert_eq!(explorer_visibility(200, &cfg()), Visibility::Visible);
    }

    #[test]
    fn constraint_matches_visibility() {
        let c = cfg();
        assert_eq!(
            explorer_constraint(Visibility::Visible, &c),
            Constraint::Length(30)
        );
        // Hidden reserves the toggle-gutter width (used once P7.2 adds the gutter).
        assert_eq!(
            explorer_constraint(Visibility::Hidden, &c),
            Constraint::Length(4)
        );
    }

    /// Regression guard: the default threshold is 100, so a typical 80-col
    /// terminal stays editor-only (no surprise pane). If someone lowers the
    /// default, this test forces a deliberate update.
    #[test]
    fn default_threshold_keeps_80col_editor_only() {
        let c = LayoutConfig::default();
        assert_eq!(explorer_visibility(80, &c), Visibility::Hidden);
        assert_eq!(explorer_visibility(100, &c), Visibility::Visible);
    }

    // ── explorer_effective_visibility: the override fold render() uses ───────
    //
    // `render` and the focus-guard both call `explorer_effective_visibility`
    // (not the raw width-only `explorer_visibility`), so the Ctrl+E override
    // must fold over the auto policy exactly. These three pin the truth table.

    #[test]
    fn effective_visibility_auto_below_threshold_is_false() {
        // No override on an 80-col terminal → auto policy hides the Explorer.
        assert!(!explorer_effective_visibility(80, &cfg(), None));
    }

    #[test]
    fn effective_visibility_override_forces_on_below_threshold() {
        // Ctrl+E forces the Explorer on even on a narrow terminal.
        assert!(explorer_effective_visibility(80, &cfg(), Some(true)));
    }

    #[test]
    fn effective_visibility_override_forces_off_above_threshold() {
        // And forces it off even on a wide terminal.
        assert!(!explorer_effective_visibility(200, &cfg(), Some(false)));
    }

    // ── split_content_row: the horizontal geometry render() draws ───────────
    //
    // The full `render(app, state, frame)` path needs a full `App` (8+ adapter
    // deps) and is left to the integration harness — same boundary the §9
    // small-terminal guard uses. The split geometry it delegates to is pure
    // ratatui, so we snapshot it here at hidden / visible / forced widths.

    #[test]
    fn split_hidden_gives_editor_the_full_row() {
        // 80-col content row, default threshold 100 → hidden → no split.
        let row = Rect::new(0, 1, 80, 40);
        let (explorer, editor) = split_content_row(row, &cfg(), None);
        assert!(explorer.is_none(), "no explorer pane when hidden");
        assert_eq!(editor, row, "editor keeps the full content row");
    }

    #[test]
    fn split_visible_explorer_gets_configured_width() {
        // Wide content row → visible → explorer gets `explorer_width` (30),
        // editor gets the remainder, side by side with no gap.
        let c = cfg();
        let row = Rect::new(0, 1, 120, 40);
        let (explorer, editor) = split_content_row(row, &c, None);
        let explorer = explorer.expect("explorer pane at wide width");
        assert_eq!(explorer.width, c.explorer_width);
        assert_eq!(explorer.x, 0);
        assert_eq!(editor.x, c.explorer_width);
        assert_eq!(editor.width, 120 - c.explorer_width);
    }

    #[test]
    fn split_override_forces_split_at_narrow_width() {
        // Ctrl+E on an 80-col terminal still splits the row.
        let c = cfg();
        let row = Rect::new(0, 1, 80, 40);
        let (explorer, editor) = split_content_row(row, &c, Some(true));
        assert!(explorer.is_some(), "override forces the explorer pane on");
        assert!(
            editor.width < row.width,
            "editor shrank to make room for the explorer"
        );
    }
}

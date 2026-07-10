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

use ratatui::layout::Constraint;

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
}

//! Explorer drawer widget — the LEFT vault-tree pane (`CLAUDE.md` §3.2
//! `note_drawer`), Spike 7 P7.2.
//!
//! Pure UI: renders a `VaultEntry` tree (from the `VaultRepository::list_tree`
//! port, P7.1) as a collapsible, navigable list, and owns the Explorer-side
//! interaction state (selection, expand/collapse). No filesystem access — tree
//! data is handed in via [`ExplorerState::set_tree`], so this module is unit-
//! testable with a synthetic tree.
//!
//! Focus model: an [`ActivePane`] tag on `EditorState` selects which pane
//! receives pane-specific keys (arrows / Enter). Pane-AGNOSTIC keys (Save,
//! Reload, Open, …) dispatch from either pane. `Ctrl+E`
//! (`Action::ToggleExplorer`) cycles Explorer visibility + focus. basalt mirrors
//! this with its `Visibility` + `ActivePane` + `Tab` cycle; onote uses a
//! dedicated toggle instead of `Tab` (Tab inserts a tab char in the editor — no
//! conflict).

use std::collections::HashSet;

use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, List, ListItem, ListState};
use ratatui::Frame;

use crate::domain::vault::{EntryKind, VaultEntry};

/// Which pane receives pane-specific keys (`CLAUDE.md` §3.2; basalt `ActivePane`).
/// Pane-agnostic keys (Save/Reload/Open/…) dispatch from either; only motion +
/// Enter differ. The focused pane renders a `Thick` border, the other `Rounded`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(super) enum ActivePane {
    #[default]
    Editor,
    Explorer,
}

/// One flattened, currently-visible row of the tree (after applying
/// expand/collapse). The tree is the source of truth; `rows` is a render/ nav
/// cache rebuilt by [`ExplorerState::reflatten`] whenever the tree or the
/// expanded set changes.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Row {
    depth: usize,
    name: String,
    /// Vault-relative path (folder dir or `.md` file). Used as the stable
    /// selection identity across reflattens (an index would drift on
    /// collapse/refresh).
    rel_path: String,
    kind: EntryKind,
    expanded: bool,
}

/// Explorer interaction state. Selection is tracked by `rel_path` (not index)
/// so it survives reflatten + tree refresh from the file watcher — the row index
/// shifts, but the selected note/folder stays put.
#[derive(Debug, Default)]
pub(super) struct ExplorerState {
    tree: Vec<VaultEntry>,
    rows: Vec<Row>,
    expanded: HashSet<String>,
    list: ListState,
    /// Selected row's `rel_path` (`None` = nothing selected / empty tree).
    selected: Option<String>,
}

impl ExplorerState {
    /// Replace the tree (e.g. on first load or a file-watch refresh) and
    /// reflatten, preserving the expanded-folder set and re-deriving selection
    /// by `rel_path`. A folder that no longer exists after refresh simply drops
    /// from `expanded` on the next reflatten (harmless stale entry).
    pub(super) fn set_tree(&mut self, tree: Vec<VaultEntry>) {
        self.tree = tree;
        self.reflatten();
    }

    fn reflatten(&mut self) {
        self.rows.clear();
        for entry in &self.tree {
            flatten_into(entry, 0, &self.expanded, &mut self.rows);
        }
        // Re-derive selection by stable `rel_path`. If the selected entry
        // vanished (deleted externally) or nothing was selected, land on the
        // first row AND record its `rel_path` so `selected_rel_path()` and the
        // `ListState` never disagree (which would leave the pane focus-dead).
        // Clear selection only when the tree is genuinely empty.
        let idx = self
            .selected
            .as_deref()
            .and_then(|s| self.rows.iter().position(|r| r.rel_path == s));
        match idx {
            Some(i) => self.list.select(Some(i)),
            None if self.rows.is_empty() => {
                self.selected = None;
                self.list.select(None);
            }
            None => {
                self.selected = Some(self.rows[0].rel_path.clone());
                self.list.select(Some(0));
            }
        }
    }

    pub(super) fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    /// Currently selected note/folder's vault-relative path, if any. Used by the
    /// Enter-opens-note action (P7.2) and future file-ops (P7.4).
    pub(super) fn selected_rel_path(&self) -> Option<&str> {
        self.selected.as_deref()
    }

    /// Kind of the selected row, if any. Enter branches on this: a folder
    /// toggles expand/collapse, a note opens.
    pub(super) fn selected_kind(&self) -> Option<EntryKind> {
        self.selected_idx()
            .and_then(|i| self.rows.get(i))
            .map(|r| r.kind)
    }

    fn selected_idx(&self) -> Option<usize> {
        self.list.selected().filter(|&i| i < self.rows.len())
    }

    fn move_sel(&mut self, delta: isize) {
        if self.rows.is_empty() {
            return;
        }
        let cur = self.selected_idx().unwrap_or(0) as isize;
        let next = (cur + delta).clamp(0, self.rows.len() as isize - 1) as usize;
        self.list.select(Some(next));
        self.selected = Some(self.rows[next].rel_path.clone());
    }

    pub(super) fn up(&mut self) {
        self.move_sel(-1);
    }

    pub(super) fn down(&mut self) {
        self.move_sel(1);
    }

    /// Expand the selected folder (no-op on notes / already-expanded). Right-arrow.
    pub(super) fn expand_selected(&mut self) {
        if let Some(row) = self.selected_idx().and_then(|i| self.rows.get(i)) {
            if row.kind == EntryKind::Folder && !row.expanded {
                self.expanded.insert(row.rel_path.clone());
                self.reflatten();
            }
        }
    }

    /// Collapse the selected folder (no-op on notes / already-collapsed).
    /// Left-arrow.
    pub(super) fn collapse_selected(&mut self) {
        if let Some(row) = self.selected_idx().and_then(|i| self.rows.get(i)) {
            if row.kind == EntryKind::Folder && row.expanded {
                self.expanded.remove(&row.rel_path);
                self.reflatten();
            }
        }
    }

    /// Toggle expand/collapse on the selected folder; no-op on notes. Enter.
    pub(super) fn toggle_expand_selected(&mut self) {
        if let Some(row) = self.selected_idx().and_then(|i| self.rows.get(i)) {
            if row.kind != EntryKind::Folder {
                return;
            }
            if row.expanded {
                self.expanded.remove(&row.rel_path);
            } else {
                self.expanded.insert(row.rel_path.clone());
            }
            self.reflatten();
        }
    }
}

/// Depth-first walk emitting a [`Row`] per visible entry. Folders recurse only
/// when expanded; collapsed folders emit themselves but not their children.
fn flatten_into(entry: &VaultEntry, depth: usize, expanded: &HashSet<String>, out: &mut Vec<Row>) {
    let is_expanded =
        entry.kind == EntryKind::Folder && expanded.contains(&entry.rel_path.as_str());
    out.push(Row {
        depth,
        name: entry.name.clone(),
        rel_path: entry.rel_path.as_str(),
        kind: entry.kind,
        expanded: is_expanded,
    });
    if is_expanded {
        for child in &entry.children {
            flatten_into(child, depth + 1, expanded, out);
        }
    }
}

/// Render the Explorer pane. `active` switches the border (Thick when focused,
/// Rounded when not) — basalt's focus cue. Selection highlight + scroll live in
/// the `ListState` carried on `explorer`.
pub(super) fn render_explorer(
    explorer: &mut ExplorerState,
    frame: &mut Frame,
    area: Rect,
    active: bool,
) {
    let border_type = if active {
        BorderType::Thick
    } else {
        BorderType::Rounded
    };
    let block = Block::default()
        .borders(Borders::LEFT | Borders::TOP | Borders::BOTTOM)
        .border_type(border_type)
        .title(Line::from(Span::styled(
            " Explorer ",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )))
        .border_style(Style::default().fg(Color::DarkGray));
    // Empty vault (no `.md` notes anywhere) → show a hint instead of a bare
    // list, so the pane never reads as broken.
    if explorer.is_empty() {
        frame.render_widget(
            ratatui::widgets::Paragraph::new("(empty vault)")
                .alignment(ratatui::layout::Alignment::Center)
                .style(Style::default().fg(Color::DarkGray))
                .block(block),
            area,
        );
        return;
    }
    let items: Vec<ListItem> = explorer
        .rows
        .iter()
        .map(|r| ListItem::new(render_row(r)))
        .collect();
    let list = List::new(items)
        .block(block)
        .highlight_style(
            Style::default()
                .bg(Color::DarkGray)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("▶ ");
    frame.render_stateful_widget(list, area, &mut explorer.list);
}

/// Build one list line: depth indentation + folder glyph (▾ expanded / ▸
/// collapsed) + name, with a trailing `/` on folders. Notes indent two extra
/// columns so their names align with folder names (the glyph + its space).
fn render_row(row: &Row) -> Line<'_> {
    let indent: String = " ".repeat(row.depth * 2);
    match row.kind {
        EntryKind::Folder => {
            let glyph = if row.expanded { "▾" } else { "▸" };
            Line::from(vec![
                Span::raw(indent),
                Span::styled(glyph, Style::default().fg(Color::Yellow)),
                Span::raw(" "),
                Span::styled(
                    format!("{}/", row.name),
                    Style::default()
                        .fg(Color::Blue)
                        .add_modifier(Modifier::BOLD),
                ),
            ])
        }
        EntryKind::Note => Line::from(vec![
            Span::raw(indent),
            // Two-space pad aligns the note name under folder names (glyph col).
            Span::raw("  "),
            Span::raw(row.name.clone()),
        ]),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::vault::RelativeNotePath;

    /// Build a `VaultEntry` note from a relative path string.
    fn note(rel: &str) -> VaultEntry {
        let pb = std::path::PathBuf::from(rel);
        let name = pb
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| rel.to_string());
        VaultEntry {
            name,
            rel_path: RelativeNotePath::new(pb).unwrap(),
            kind: EntryKind::Note,
            children: vec![],
        }
    }

    /// Build a `VaultEntry` folder from its FULL vault-relative path + children.
    /// The display name is the path's basename (mirrors `walk_tree`, which sets
    /// `name` = folder name and `rel_path` = full path — so a nested folder's
    /// identity is its full path, not its basename).
    fn folder(rel: &str, children: Vec<VaultEntry>) -> VaultEntry {
        let name = std::path::Path::new(rel)
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| rel.to_string());
        VaultEntry {
            name,
            rel_path: RelativeNotePath::new(std::path::PathBuf::from(rel)).unwrap(),
            kind: EntryKind::Folder,
            children,
        }
    }

    /// `name(kind)` tuples of the currently-visible rows (collapse respected).
    fn visible(ex: &ExplorerState) -> Vec<(&str, EntryKind)> {
        ex.rows.iter().map(|r| (r.name.as_str(), r.kind)).collect()
    }

    #[test]
    fn empty_tree_has_no_selection() {
        let mut ex = ExplorerState::default();
        ex.set_tree(vec![]);
        assert!(ex.is_empty());
        assert_eq!(ex.selected_rel_path(), None);
    }

    #[test]
    fn collapsed_folders_hide_children() {
        let mut ex = ExplorerState::default();
        ex.set_tree(vec![folder(
            "Notes",
            vec![note("Notes/a.md"), note("Notes/b.md")],
        )]);
        // Folder collapsed by default → only the folder row is visible.
        assert_eq!(visible(&ex), vec![("Notes", EntryKind::Folder)]);
        assert_eq!(ex.selected_rel_path(), Some("Notes"));
    }

    #[test]
    fn expand_reveals_children() {
        let mut ex = ExplorerState::default();
        ex.set_tree(vec![folder(
            "Notes",
            vec![note("Notes/a.md"), note("Notes/b.md")],
        )]);
        ex.expand_selected();
        assert_eq!(
            visible(&ex),
            vec![
                ("Notes", EntryKind::Folder),
                ("a", EntryKind::Note),
                ("b", EntryKind::Note),
            ]
        );
        // Selection stays on the folder after expand (rel_path identity).
        assert_eq!(ex.selected_rel_path(), Some("Notes"));
    }

    #[test]
    fn collapse_hides_children_again() {
        let mut ex = ExplorerState::default();
        ex.set_tree(vec![folder("Notes", vec![note("Notes/a.md")])]);
        ex.expand_selected();
        ex.collapse_selected();
        assert_eq!(visible(&ex), vec![("Notes", EntryKind::Folder)]);
    }

    #[test]
    fn toggle_flips_expanded() {
        let mut ex = ExplorerState::default();
        ex.set_tree(vec![folder("Notes", vec![note("Notes/a.md")])]);
        ex.toggle_expand_selected(); // expand
        assert_eq!(ex.rows.len(), 2);
        ex.toggle_expand_selected(); // collapse
        assert_eq!(ex.rows.len(), 1);
    }

    #[test]
    fn nav_moves_selection_and_clamps() {
        let mut ex = ExplorerState::default();
        ex.set_tree(vec![note("a.md"), note("b.md"), note("c.md")]);
        assert_eq!(ex.selected_rel_path(), Some("a.md"));
        ex.down();
        assert_eq!(ex.selected_rel_path(), Some("b.md"));
        ex.down();
        ex.down(); // past end → clamp at c
        assert_eq!(ex.selected_rel_path(), Some("c.md"));
        ex.up();
        assert_eq!(ex.selected_rel_path(), Some("b.md"));
    }

    #[test]
    fn expand_is_noop_on_note() {
        let mut ex = ExplorerState::default();
        ex.set_tree(vec![note("a.md")]);
        ex.expand_selected(); // note → no-op, no panic
        assert_eq!(ex.rows.len(), 1);
    }

    #[test]
    fn set_tree_preserves_selection_by_relpath() {
        // Simulates a file-watch refresh: selection survives a reflatten.
        let mut ex = ExplorerState::default();
        ex.set_tree(vec![note("a.md"), note("b.md"), note("c.md")]);
        ex.down();
        ex.down(); // select c.md
        assert_eq!(ex.selected_rel_path(), Some("c.md"));
        // "Refresh" with the same tree — c.md must stay selected.
        ex.set_tree(vec![note("a.md"), note("b.md"), note("c.md")]);
        assert_eq!(ex.selected_rel_path(), Some("c.md"));
    }

    #[test]
    fn deleted_selected_falls_back_to_first_row() {
        let mut ex = ExplorerState::default();
        ex.set_tree(vec![note("a.md"), note("b.md")]);
        ex.down(); // select b.md
                   // "Refresh" with b.md gone — selection falls back to a.md (first row),
                   // never focus-dead.
        ex.set_tree(vec![note("a.md")]);
        assert_eq!(ex.selected_rel_path(), Some("a.md"));
    }

    #[test]
    fn nested_expand_visibility() {
        let mut ex = ExplorerState::default();
        // Notes/
        //   inner/
        //     deep.md
        //   top.md
        let inner = folder("Notes/inner", vec![note("Notes/inner/deep.md")]);
        let notes = folder("Notes", vec![inner, note("Notes/top.md")]);
        ex.set_tree(vec![notes]);
        // Only Notes visible.
        assert_eq!(visible(&ex), vec![("Notes", EntryKind::Folder)]);
        ex.expand_selected(); // open Notes
        assert_eq!(
            visible(&ex),
            vec![
                ("Notes", EntryKind::Folder),
                ("inner", EntryKind::Folder),
                ("top", EntryKind::Note),
            ]
        );
        // Move onto inner (index 1) and expand it.
        ex.down();
        assert_eq!(ex.selected_rel_path(), Some("Notes/inner"));
        ex.expand_selected();
        assert_eq!(
            visible(&ex),
            vec![
                ("Notes", EntryKind::Folder),
                ("inner", EntryKind::Folder),
                ("deep", EntryKind::Note),
                ("top", EntryKind::Note),
            ]
        );
    }
}

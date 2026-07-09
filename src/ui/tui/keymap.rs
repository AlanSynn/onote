use std::collections::HashMap;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::config::KeymapConfig;

// ── Keymap (CLAUDE.md §5 KeymapRegistry) ────────────────────────────────────
// Every edit-mode keystroke resolves to a logical `Action` via `KeymapRegistry`,
// so no binding is hardcoded in a `match` arm (contract C8) and any of them is
// remappable from `[keymap]` in config.toml.

/// A logical, input-device-independent editor action.
///
/// Variants whose machinery lands in later phases (`Select*`, `ClearSelection`,
/// `SelectAll`, `Copy`, `Cut`, `DeleteForward`, `Word*`) are defined now so
/// their default bindings ship from day one and are configurable immediately;
/// their dispatch behavior is filled in by that phase (P2 selection, P4
/// clipboard, P5 word-motion). Until then they resolve to a real `Action` but
/// mutate nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Action {
    // Global (work from any mode, incl. overlay — checked before modal dispatch).
    Quit,
    Save,
    Reload,
    OpenFuzzy,
    PasteImage,
    DeleteImageToken,
    ConflictCopy,
    // Plain editing.
    InsertChar(char),
    Enter,
    Backspace,
    Tab,
    // Cursor motion (clears an active selection — wired in P2).
    MoveLeft,
    MoveRight,
    MoveUp,
    MoveDown,
    MoveHome,
    MoveEnd,
    // Selection (extend the head; anchor fixed) — P2.
    SelectLeft,
    SelectRight,
    SelectUp,
    SelectDown,
    SelectHome,
    SelectEnd,
    SelectAll,
    ClearSelection, // Esc
    // Clipboard / forward-delete on a selection — P4.
    Copy,
    Cut,
    DeleteForward,
    // Word motion / word-select — P5.
    WordLeft,
    WordRight,
    SelectWordLeft,
    SelectWordRight,
}

/// A key + modifier combination — the canonical keymap lookup key.
///
/// Deliberately drops `KeyEventKind`/`KeyEventState` (which vary by terminal
/// and aren't part of "what was pressed") so the registry never depends on
/// crossterm's `KeyEvent` `Hash`/`PartialEq` semantics (which also compare
/// `kind`/`state`). `KeyCode` and `KeyModifiers` both derive `Hash`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(super) struct KeyCombo {
    code: KeyCode,
    mods: KeyModifiers,
}

impl From<&KeyEvent> for KeyCombo {
    fn from(k: &KeyEvent) -> Self {
        Self {
            code: normalized(k.code),
            mods: k.modifiers,
        }
    }
}

/// Fold ASCII letters to lowercase so a binding matches either case a terminal
/// reports — terminals disagree on whether Ctrl+Shift+C arrives as `'c'` or
/// `'C'`. Digits/punctuation keep their case (Shift+1 = `'!'`). The SHIFT
/// modifier bit is preserved on `KeyCombo.mods`, so Ctrl+C (quit) stays
/// distinct from Ctrl+Shift+C (copy).
fn normalized(code: KeyCode) -> KeyCode {
    if let KeyCode::Char(c) = code {
        if c.is_ascii_alphabetic() {
            return KeyCode::Char(c.to_ascii_lowercase());
        }
    }
    code
}

/// Build a normalized [`KeyCombo`] (used by both defaults and config parsing so
/// every entry is stored in the same canonical form the lookup uses).
fn combo(code: KeyCode, mods: KeyModifiers) -> KeyCombo {
    KeyCombo {
        code: normalized(code),
        mods,
    }
}

/// Maps key combinations to logical [`Action`]s — the single source for every
/// keybinding (`CLAUDE.md` §5). Built from baked [`KeymapRegistry::defaults`],
/// then overlaid with user `[keymap]` overrides from config.toml.
#[derive(Debug, Clone, Default)]
pub(super) struct KeymapRegistry {
    bindings: HashMap<KeyCombo, Action>,
}

impl KeymapRegistry {
    /// Baked defaults — every command has a default binding so bare `onote`
    /// works with no config. `InsertChar` is NOT here; it's the universal
    /// text-entry fallback in [`Self::action_for`].
    pub(super) fn defaults() -> Self {
        use KeyCode::*;
        let mut m: HashMap<KeyCombo, Action> = HashMap::new();
        // Globals. Quit binds to BOTH ^Q and ^C (^C is "cancel" muscle memory;
        // no copy clash because copy is ^Shift+C — a distinct combo).
        for (c, a) in [
            ('q', Action::Quit),
            ('c', Action::Quit),
            ('s', Action::Save),
            ('o', Action::OpenFuzzy),
            ('p', Action::PasteImage),
            ('d', Action::DeleteImageToken),
            ('r', Action::Reload),
            ('k', Action::ConflictCopy),
            ('a', Action::SelectAll),
            ('x', Action::Cut),
        ] {
            m.insert(combo(Char(c), KeyModifiers::CONTROL), a);
        }
        // ^Shift+C = copy (distinct from ^C = quit by the SHIFT bit).
        m.insert(
            combo(Char('c'), KeyModifiers::CONTROL | KeyModifiers::SHIFT),
            Action::Copy,
        );
        // Plain edit keys + forward-delete + Esc (deselect).
        m.insert(combo(Enter, KeyModifiers::NONE), Action::Enter);
        m.insert(combo(Tab, KeyModifiers::NONE), Action::Tab);
        m.insert(combo(Backspace, KeyModifiers::NONE), Action::Backspace);
        m.insert(combo(Delete, KeyModifiers::NONE), Action::DeleteForward);
        m.insert(combo(Esc, KeyModifiers::NONE), Action::ClearSelection);
        // Cursor motion (plain arrows / Home/End).
        m.insert(combo(Left, KeyModifiers::NONE), Action::MoveLeft);
        m.insert(combo(Right, KeyModifiers::NONE), Action::MoveRight);
        m.insert(combo(Up, KeyModifiers::NONE), Action::MoveUp);
        m.insert(combo(Down, KeyModifiers::NONE), Action::MoveDown);
        m.insert(combo(Home, KeyModifiers::NONE), Action::MoveHome);
        m.insert(combo(End, KeyModifiers::NONE), Action::MoveEnd);
        // Selection (Shift+arrow / Shift+Home/End).
        m.insert(combo(Left, KeyModifiers::SHIFT), Action::SelectLeft);
        m.insert(combo(Right, KeyModifiers::SHIFT), Action::SelectRight);
        m.insert(combo(Up, KeyModifiers::SHIFT), Action::SelectUp);
        m.insert(combo(Down, KeyModifiers::SHIFT), Action::SelectDown);
        m.insert(combo(Home, KeyModifiers::SHIFT), Action::SelectHome);
        m.insert(combo(End, KeyModifiers::SHIFT), Action::SelectEnd);
        // Word motion / word-select (Ctrl+arrow, Ctrl+Shift+arrow).
        m.insert(combo(Left, KeyModifiers::CONTROL), Action::WordLeft);
        m.insert(combo(Right, KeyModifiers::CONTROL), Action::WordRight);
        m.insert(
            combo(Left, KeyModifiers::CONTROL | KeyModifiers::SHIFT),
            Action::SelectWordLeft,
        );
        m.insert(
            combo(Right, KeyModifiers::CONTROL | KeyModifiers::SHIFT),
            Action::SelectWordRight,
        );
        Self { bindings: m }
    }

    /// Defaults overlaid with `[keymap]` overrides. Malformed entries are
    /// skipped (with a warning) so a typo can't brick the editor — the default
    /// for that key survives.
    pub(super) fn from_config(keymap: &KeymapConfig) -> Self {
        let mut km = Self::defaults();
        km.apply_overrides(keymap);
        km
    }

    pub(super) fn apply_overrides(&mut self, keymap: &KeymapConfig) {
        for (spec, action_name) in &keymap.bindings {
            let Some(c) = parse_key_spec(spec) else {
                tracing::warn!(key = %spec, "keymap: skipping unparseable key spec");
                continue;
            };
            let Some(a) = parse_action_name(action_name) else {
                tracing::warn!(
                    key = %spec,
                    action = %action_name,
                    "keymap: skipping entry with unknown action"
                );
                continue;
            };
            self.bindings.insert(c, a);
        }
    }

    /// Resolve a key event to an action: exact-match the combo, else fall back
    /// to `InsertChar(c)` for a printable char with no Ctrl/Alt (the universal
    /// text-entry default), else `None` (unhandled).
    pub(super) fn action_for(&self, key: &KeyEvent) -> Option<Action> {
        if let Some(a) = self.bindings.get(&KeyCombo::from(key)).copied() {
            return Some(a);
        }
        if let KeyCode::Char(c) = key.code {
            if !c.is_control()
                && !key.modifiers.contains(KeyModifiers::CONTROL)
                && !key.modifiers.contains(KeyModifiers::ALT)
            {
                return Some(Action::InsertChar(c));
            }
        }
        None
    }
}

/// Parse a key-spec like `"ctrl+shift+c"` or `"left"` into a [`KeyCombo`].
/// Modifiers: `ctrl`/`control`, `alt`/`option`/`meta`, `shift`, joined by `+`.
pub(super) fn parse_key_spec(spec: &str) -> Option<KeyCombo> {
    let mut mods = KeyModifiers::NONE;
    let mut key_name: Option<&str> = None;
    for part in spec.split('+').map(str::trim) {
        if part.is_empty() {
            continue;
        }
        match part.to_ascii_lowercase().as_str() {
            "ctrl" | "control" => mods |= KeyModifiers::CONTROL,
            "alt" | "option" | "meta" => mods |= KeyModifiers::ALT,
            "shift" => mods |= KeyModifiers::SHIFT,
            _ => key_name = Some(part),
        }
    }
    let code = parse_key_code(key_name?)?;
    Some(combo(code, mods))
}

/// Parse the trailing key name of a spec (`"c"`, `"enter"`, `"f5"`, …).
fn parse_key_code(name: &str) -> Option<KeyCode> {
    let lower = name.to_ascii_lowercase();
    match lower.as_str() {
        "enter" | "return" => Some(KeyCode::Enter),
        "tab" => Some(KeyCode::Tab),
        "backspace" | "bs" => Some(KeyCode::Backspace),
        "esc" | "escape" => Some(KeyCode::Esc),
        "delete" | "del" => Some(KeyCode::Delete),
        "insert" | "ins" => Some(KeyCode::Insert),
        "home" => Some(KeyCode::Home),
        "end" => Some(KeyCode::End),
        "pageup" | "pgup" => Some(KeyCode::PageUp),
        "pagedown" | "pgdn" => Some(KeyCode::PageDown),
        "left" => Some(KeyCode::Left),
        "right" => Some(KeyCode::Right),
        "up" => Some(KeyCode::Up),
        "down" => Some(KeyCode::Down),
        "space" | "spacebar" => Some(KeyCode::Char(' ')),
        _ => {
            // F1–F12.
            if let Some(rest) = lower.strip_prefix('f') {
                if let Ok(n) = rest.parse::<u8>() {
                    if (1..=12).contains(&n) {
                        return Some(KeyCode::F(n));
                    }
                }
            }
            // A single literal character (letter/digit/punct). Keep original
            // case — `combo()` lower-cases letters for matching.
            let mut chars = name.chars();
            match (chars.next(), chars.next()) {
                (Some(c), None) => Some(KeyCode::Char(c)),
                _ => None,
            }
        }
    }
}

/// Parse an action name (`"save"`, `"select_all"`, …) into an [`Action`].
/// Accepts `snake_case` and `kebab-case`; unknown names → `None`.
pub(super) fn parse_action_name(name: &str) -> Option<Action> {
    let s = name.trim().to_ascii_lowercase().replace('-', "_");
    Some(match s.as_str() {
        "quit" => Action::Quit,
        "save" => Action::Save,
        "reload" => Action::Reload,
        "open_fuzzy" | "open" => Action::OpenFuzzy,
        "paste_image" | "paste" => Action::PasteImage,
        "delete_image_token" | "delete_image" => Action::DeleteImageToken,
        "conflict_copy" => Action::ConflictCopy,
        "enter" | "newline" => Action::Enter,
        "backspace" => Action::Backspace,
        "tab" => Action::Tab,
        "move_left" => Action::MoveLeft,
        "move_right" => Action::MoveRight,
        "move_up" => Action::MoveUp,
        "move_down" => Action::MoveDown,
        "move_home" | "home" => Action::MoveHome,
        "move_end" | "end" => Action::MoveEnd,
        "select_left" => Action::SelectLeft,
        "select_right" => Action::SelectRight,
        "select_up" => Action::SelectUp,
        "select_down" => Action::SelectDown,
        "select_home" => Action::SelectHome,
        "select_end" => Action::SelectEnd,
        "select_all" => Action::SelectAll,
        "clear_selection" | "deselect" => Action::ClearSelection,
        "copy" => Action::Copy,
        "cut" => Action::Cut,
        "delete_forward" | "delete" => Action::DeleteForward,
        "word_left" => Action::WordLeft,
        "word_right" => Action::WordRight,
        "select_word_left" => Action::SelectWordLeft,
        "select_word_right" => Action::SelectWordRight,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Parity: every binding that was HARDCODED before the keymap refactor now
    /// resolves (via the registry) to the action whose dispatch body is the
    /// identical old code. Locks the default map against regressions.
    #[test]
    fn keymap_defaults_match_old_hardcoded_bindings() {
        let km = KeymapRegistry::defaults();
        let k = |code: KeyCode, mods: KeyModifiers| km.action_for(&KeyEvent::new(code, mods));
        // Globals — old code quit on both ^Q and ^C.
        assert_eq!(
            k(KeyCode::Char('q'), KeyModifiers::CONTROL),
            Some(Action::Quit)
        );
        assert_eq!(
            k(KeyCode::Char('c'), KeyModifiers::CONTROL),
            Some(Action::Quit)
        );
        // Old ctrl commands.
        assert_eq!(
            k(KeyCode::Char('s'), KeyModifiers::CONTROL),
            Some(Action::Save)
        );
        assert_eq!(
            k(KeyCode::Char('o'), KeyModifiers::CONTROL),
            Some(Action::OpenFuzzy)
        );
        assert_eq!(
            k(KeyCode::Char('p'), KeyModifiers::CONTROL),
            Some(Action::PasteImage)
        );
        assert_eq!(
            k(KeyCode::Char('d'), KeyModifiers::CONTROL),
            Some(Action::DeleteImageToken)
        );
        assert_eq!(
            k(KeyCode::Char('r'), KeyModifiers::CONTROL),
            Some(Action::Reload)
        );
        assert_eq!(
            k(KeyCode::Char('k'), KeyModifiers::CONTROL),
            Some(Action::ConflictCopy)
        );
        // Plain keys.
        assert_eq!(k(KeyCode::Enter, KeyModifiers::NONE), Some(Action::Enter));
        assert_eq!(
            k(KeyCode::Backspace, KeyModifiers::NONE),
            Some(Action::Backspace)
        );
        assert_eq!(k(KeyCode::Tab, KeyModifiers::NONE), Some(Action::Tab));
        // Motion (old move_cursor arms).
        assert_eq!(k(KeyCode::Left, KeyModifiers::NONE), Some(Action::MoveLeft));
        assert_eq!(
            k(KeyCode::Right, KeyModifiers::NONE),
            Some(Action::MoveRight)
        );
        assert_eq!(k(KeyCode::Up, KeyModifiers::NONE), Some(Action::MoveUp));
        assert_eq!(k(KeyCode::Down, KeyModifiers::NONE), Some(Action::MoveDown));
        assert_eq!(k(KeyCode::Home, KeyModifiers::NONE), Some(Action::MoveHome));
        assert_eq!(k(KeyCode::End, KeyModifiers::NONE), Some(Action::MoveEnd));
        // Printable-char fallback (old `Char(c) => insert_char`).
        assert_eq!(
            k(KeyCode::Char('a'), KeyModifiers::NONE),
            Some(Action::InsertChar('a'))
        );
    }

    /// InsertChar fallback must NOT fire under Ctrl/Alt (those are commands,
    /// not literals), and an unbound ctrl combo resolves to `None`.
    #[test]
    fn keymap_insertchar_fallback_rejects_modifiers() {
        let km = KeymapRegistry::defaults();
        // Ctrl+a is bound (SelectAll), not InsertChar.
        assert_eq!(
            km.action_for(&KeyEvent::new(KeyCode::Char('a'), KeyModifiers::CONTROL)),
            Some(Action::SelectAll)
        );
        // An unbound ctrl combo (Ctrl+z) → None, never InsertChar.
        assert_eq!(
            km.action_for(&KeyEvent::new(KeyCode::Char('z'), KeyModifiers::CONTROL)),
            None
        );
        // Alt+a → None (no fallback under alt).
        assert_eq!(
            km.action_for(&KeyEvent::new(KeyCode::Char('a'), KeyModifiers::ALT)),
            None
        );
    }

    /// Letter-case normalization: Ctrl+Shift+C resolves to Copy whether the
    /// terminal reports `'c'` or `'C'`, while Ctrl+C (no shift) stays Quit.
    #[test]
    fn keymap_letter_case_normalized_for_ctrl_shift() {
        let km = KeymapRegistry::defaults();
        let mods = KeyModifiers::CONTROL | KeyModifiers::SHIFT;
        assert_eq!(
            km.action_for(&KeyEvent::new(KeyCode::Char('c'), mods)),
            Some(Action::Copy)
        );
        assert_eq!(
            km.action_for(&KeyEvent::new(KeyCode::Char('C'), mods)),
            Some(Action::Copy)
        );
        // No shift → quit, not copy.
        assert_eq!(
            km.action_for(&KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL)),
            Some(Action::Quit)
        );
    }

    /// `[keymap]` overrides rebind known keys and add new ones; malformed
    /// specs/actions are skipped so the default survives.
    #[test]
    fn keymap_overrides_rebind_and_skip_invalid() {
        let mut km = KeymapRegistry::defaults();
        let mut cfg = KeymapConfig::default();
        cfg.bindings.insert("ctrl+s".into(), "reload".into()); // rebind save→reload
        cfg.bindings.insert("ctrl+d".into(), "save".into()); // rebind delete-img→save
        cfg.bindings.insert("ctrl+z".into(), "bogus_action".into()); // unknown action → skip
        cfg.bindings.insert("not+a+real+key".into(), "save".into()); // unparsable → skip
        cfg.bindings.insert("f4".into(), "copy".into()); // add a brand-new binding
        km.apply_overrides(&cfg);
        let ctrl = |c: char| KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL);
        assert_eq!(km.action_for(&ctrl('s')), Some(Action::Reload));
        assert_eq!(km.action_for(&ctrl('d')), Some(Action::Save));
        // Bogus/invalid entries skipped — Ctrl+z still unbound.
        assert_eq!(km.action_for(&ctrl('z')), None);
        // New binding took effect.
        assert_eq!(
            km.action_for(&KeyEvent::new(KeyCode::F(4), KeyModifiers::NONE)),
            Some(Action::Copy)
        );
    }

    /// Every action name a user might write in `[keymap]` parses, and nonsense
    /// doesn't. Guards the documented config vocabulary.
    #[test]
    fn parse_action_name_covers_documented_vocabulary() {
        let names = [
            "quit",
            "save",
            "reload",
            "open_fuzzy",
            "paste_image",
            "delete_image_token",
            "conflict_copy",
            "enter",
            "backspace",
            "tab",
            "move_left",
            "move_right",
            "move_up",
            "move_down",
            "move_home",
            "move_end",
            "select_left",
            "select_right",
            "select_up",
            "select_down",
            "select_home",
            "select_end",
            "select_all",
            "clear_selection",
            "copy",
            "cut",
            "delete_forward",
            "word_left",
            "word_right",
            "select_word_left",
            "select_word_right",
        ];
        for n in names {
            assert!(
                parse_action_name(n).is_some(),
                "action name {n:?} should parse"
            );
        }
        assert!(parse_action_name("totally_bogus").is_none());
        // kebab-case alias also works.
        assert_eq!(parse_action_name("select-all"), Some(Action::SelectAll));
    }

    /// Cross-module config→keymap wire (architect #2): the live line in `run()`
    /// is `state.keymap = KeymapRegistry::from_config(&app.config().keymap)`.
    /// Pin the transform run() depends on end-to-end: a TOML `[keymap]` parses
    /// (config.rs) into `Config.keymap`, which `from_config` (tui) turns into
    /// typed overrides layered on the baked defaults. A regression that drops
    /// the wire, renames the field, or stops honoring overrides breaks this.
    #[test]
    fn keymap_override_flows_from_config_to_registry() {
        use crate::config::Config;
        use std::io::Write;
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let mut f = tmp.reopen().unwrap();
        // Cross-platform absolute vault (config.rs rejects relative/non-absolute
        // on the host): backslash-doubling keeps it a valid TOML basic string.
        let vault = std::env::temp_dir()
            .join("onote-keymap-wire-test")
            .to_string_lossy()
            .replace('\\', "\\\\");
        writeln!(
            f,
            r#"
vault = "{vault}"
[keymap]
"ctrl+s" = "reload"
"ctrl+x" = "cut"
"#
        )
        .unwrap();
        let cfg = Config::load_from(Some(tmp.path())).unwrap();
        let km = KeymapRegistry::from_config(&cfg.keymap);
        let ctrl = |c: char| KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL);
        // User override wins over the baked default (Ctrl+S was `save`).
        assert_eq!(km.action_for(&ctrl('s')), Some(Action::Reload));
        // A new binding (Ctrl+X = cut, not in the default map) takes effect.
        assert_eq!(km.action_for(&ctrl('x')), Some(Action::Cut));
        // Untouched defaults survive (overrides layer, not replace).
        assert_eq!(km.action_for(&ctrl('q')), Some(Action::Quit));
    }
}

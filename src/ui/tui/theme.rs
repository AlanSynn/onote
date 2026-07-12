//! Catppuccin theming (`CLAUDE.md` §1.3 — pure UI layer; domain never sees it).
//!
//! Provides the four Catppuccin flavors as `Color::Rgb` palettes and maps them
//! to a small set of semantic UI roles the renderers consume. The default flavor
//! is **Latte (light)** — a fixed, predictable light theme — with Frappé,
//! Macchiato, and Mocha (dark) selectable via `theme = "…"` in config.toml.
//!
//! Per the product brief: terminal colors are normally managed by the terminal
//! itself, so large surfaces (the editor body) are left unpainted — only the
//! bars (path / status) and accents (titles, borders, selection, status label)
//! are themed. That keeps the app cohesive in both light and dark terminals
//! without fighting the user's chosen terminal background.
//!
//! Palette values are the canonical Catppuccin v0.3 hexes
//! (<https://catppuccin.com/palette>).

use ratatui::style::Color;

/// The 26 named Catppuccin colors for one flavor. Private — callers consume the
/// semantic roles on [`Theme`], not raw palette slots, so a role can be remapped
/// (e.g. accent → Mauve instead of Lavender) in one place.
#[derive(Debug, Clone, Copy)]
#[allow(dead_code)] // full canonical Catppuccin palette; roles read a subset,
                   // the rest are kept as the auditable reference + reserved for
                   // future roles (CLAUDE.md §5 single source of truth).
struct Palette {
    rosewater: Color,
    flamingo: Color,
    pink: Color,
    mauve: Color,
    red: Color,
    maroon: Color,
    peach: Color,
    yellow: Color,
    green: Color,
    teal: Color,
    sky: Color,
    sapphire: Color,
    blue: Color,
    lavender: Color,
    text: Color,
    subtext1: Color,
    subtext0: Color,
    overlay2: Color,
    overlay1: Color,
    overlay0: Color,
    surface2: Color,
    surface1: Color,
    surface0: Color,
    base: Color,
    mantle: Color,
    crust: Color,
}

/// `Color::Rgb` shorthand to keep the palette literals terse + auditable.
const fn rgb(r: u8, g: u8, b: u8) -> Color {
    Color::Rgb(r, g, b)
}

const LATTE: Palette = Palette {
    rosewater: rgb(220, 138, 120),
    flamingo: rgb(221, 120, 120),
    pink: rgb(234, 118, 203),
    mauve: rgb(136, 57, 239),
    red: rgb(210, 15, 57),
    maroon: rgb(230, 69, 83),
    peach: rgb(254, 100, 11),
    yellow: rgb(223, 142, 29),
    green: rgb(64, 160, 43),
    teal: rgb(23, 146, 153),
    sky: rgb(4, 165, 229),
    sapphire: rgb(32, 159, 181),
    blue: rgb(30, 102, 245),
    lavender: rgb(114, 135, 253),
    text: rgb(76, 79, 105),
    subtext1: rgb(92, 95, 119),
    subtext0: rgb(108, 111, 133),
    overlay2: rgb(124, 127, 147),
    overlay1: rgb(140, 143, 161),
    overlay0: rgb(156, 160, 176),
    surface2: rgb(172, 176, 190),
    surface1: rgb(188, 192, 204),
    surface0: rgb(204, 208, 218),
    base: rgb(239, 241, 245),
    mantle: rgb(230, 233, 239),
    crust: rgb(220, 224, 232),
};

const FRAPPE: Palette = Palette {
    rosewater: rgb(242, 213, 207),
    flamingo: rgb(238, 190, 190),
    pink: rgb(244, 184, 228),
    mauve: rgb(202, 158, 230),
    red: rgb(231, 130, 132),
    maroon: rgb(234, 153, 156),
    peach: rgb(239, 159, 118),
    yellow: rgb(229, 200, 144),
    green: rgb(166, 209, 137),
    teal: rgb(129, 200, 190),
    sky: rgb(153, 209, 219),
    sapphire: rgb(133, 193, 220),
    blue: rgb(140, 170, 238),
    lavender: rgb(186, 187, 241),
    text: rgb(198, 208, 245),
    subtext1: rgb(181, 191, 226),
    subtext0: rgb(165, 173, 206),
    overlay2: rgb(148, 156, 187),
    overlay1: rgb(131, 139, 167),
    overlay0: rgb(115, 121, 148),
    surface2: rgb(98, 104, 128),
    surface1: rgb(81, 87, 109),
    surface0: rgb(65, 69, 89),
    base: rgb(48, 52, 70),
    mantle: rgb(41, 44, 60),
    crust: rgb(35, 38, 52),
};

const MACCHIATO: Palette = Palette {
    rosewater: rgb(240, 219, 211),
    flamingo: rgb(236, 205, 207),
    pink: rgb(240, 182, 211),
    mauve: rgb(198, 160, 246),
    red: rgb(237, 135, 150),
    maroon: rgb(238, 153, 160),
    peach: rgb(245, 169, 127),
    yellow: rgb(238, 212, 159),
    green: rgb(166, 218, 149),
    teal: rgb(139, 213, 202),
    sky: rgb(145, 215, 227),
    sapphire: rgb(125, 196, 228),
    blue: rgb(138, 173, 244),
    lavender: rgb(183, 189, 248),
    text: rgb(202, 211, 245),
    subtext1: rgb(184, 192, 224),
    subtext0: rgb(165, 173, 203),
    overlay2: rgb(147, 154, 183),
    overlay1: rgb(128, 135, 162),
    overlay0: rgb(110, 115, 141),
    surface2: rgb(91, 96, 120),
    surface1: rgb(73, 77, 100),
    surface0: rgb(54, 58, 79),
    base: rgb(36, 39, 58),
    mantle: rgb(30, 32, 48),
    crust: rgb(24, 25, 38),
};

const MOCHA: Palette = Palette {
    rosewater: rgb(245, 224, 220),
    flamingo: rgb(242, 205, 205),
    pink: rgb(245, 194, 231),
    mauve: rgb(203, 166, 247),
    red: rgb(243, 139, 168),
    maroon: rgb(235, 160, 172),
    peach: rgb(250, 179, 135),
    yellow: rgb(249, 226, 175),
    green: rgb(166, 227, 161),
    teal: rgb(148, 226, 213),
    sky: rgb(137, 220, 235),
    sapphire: rgb(116, 199, 236),
    blue: rgb(137, 180, 250),
    lavender: rgb(180, 190, 254),
    text: rgb(205, 214, 244),
    subtext1: rgb(186, 194, 222),
    subtext0: rgb(166, 173, 200),
    overlay2: rgb(147, 153, 178),
    overlay1: rgb(127, 132, 156),
    overlay0: rgb(108, 112, 134),
    surface2: rgb(88, 91, 112),
    surface1: rgb(69, 71, 90),
    surface0: rgb(49, 50, 68),
    base: rgb(30, 30, 46),
    mantle: rgb(24, 24, 37),
    crust: rgb(17, 17, 27),
};

/// A Catppuccin flavor. `Default` is `Latte` (the fixed light default).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(super) enum Flavor {
    #[default]
    Latte,
    Frappe,
    Macchiato,
    Mocha,
}

impl Flavor {
    /// Parse a config string (`latte` / `frappe` / `macchiato` / `mocha`),
    /// case-insensitive. An unknown or empty value falls back to Latte (the
    /// fixed default) rather than erroring — a theme typo must never block
    /// startup.
    fn parse(s: &str) -> Self {
        match s.trim().to_ascii_lowercase().as_str() {
            "frappe" => Self::Frappe,
            "macchiato" => Self::Macchiato,
            "mocha" => Self::Mocha,
            // Includes "" and any unrecognized value → light default.
            _ => Self::Latte,
        }
    }

    fn palette(self) -> Palette {
        match self {
            Self::Latte => LATTE,
            Self::Frappe => FRAPPE,
            Self::Macchiato => MACCHIATO,
            Self::Mocha => MOCHA,
        }
    }
}

/// A resolved theme: a flavor's palette projected onto semantic UI roles.
/// Construct once at startup from config and thread through render. Cheap to
/// copy (all `Color`).
#[derive(Debug, Clone, Copy)]
pub(super) struct Theme {
    p: Palette,
}

impl Default for Theme {
    fn default() -> Self {
        Self::from_flavor(Flavor::Latte)
    }
}

impl Theme {
    pub(super) fn from_flavor(flavor: Flavor) -> Self {
        Self {
            p: flavor.palette(),
        }
    }

    /// Parse a config string into a theme (Latte on unknown).
    pub(super) fn from_config_str(s: &str) -> Self {
        Self::from_flavor(Flavor::parse(s))
    }

    // ── Surfaces (bars only; editor body is left to the terminal) ───────────

    /// Background for the path + status bars. `Surface1` reads as a distinct
    /// bar on both Latte (light gray) and the dark flavors, without darkening
    /// the editor surface itself.
    pub(super) fn bar_bg(&self) -> Color {
        self.p.surface1
    }

    // ── Text ────────────────────────────────────────────────────────────────

    /// Default body / name text.
    pub(super) fn text(&self) -> Color {
        self.p.text
    }

    /// Dimmed text: hints, snippets, empty-vault/empty-result lines.
    pub(super) fn muted(&self) -> Color {
        self.p.subtext0
    }

    // ── Accents (Catppuccin style guide) ────────────────────────────────────

    /// Primary accent: titles, headers, prompt cursor, folder-leaf emphasis.
    /// Lavender across all four flavors per the style guide.
    pub(super) fn accent(&self) -> Color {
        self.p.lavender
    }

    /// Pane + widget borders. `Overlay1` is visible against every flavor's base.
    pub(super) fn border(&self) -> Color {
        self.p.overlay1
    }

    /// Links, tags, attachment paths, folder names. Blue per the style guide.
    pub(super) fn link(&self) -> Color {
        self.p.blue
    }

    /// Inline image-embed glyphs. Magenta isn't a Catppuccin color; Mauve is
    /// the closest accent and stays readable on both light and dark.
    pub(super) fn glyph(&self) -> Color {
        self.p.mauve
    }

    // ── Semantic status colors ──────────────────────────────────────────────

    pub(super) fn success(&self) -> Color {
        self.p.green
    }
    pub(super) fn warning(&self) -> Color {
        self.p.yellow
    }
    pub(super) fn error(&self) -> Color {
        self.p.red
    }
    /// Info accent for the body-search picker (distinct from the fuzzy picker's
    /// `warning`). Sky/Sapphire is Catppuccin's cool-info range.
    pub(super) fn info(&self) -> Color {
        self.p.sky
    }

    // ── Selection ───────────────────────────────────────────────────────────

    /// List-item highlight background (Explorer rows). `Surface2` is clearly
    /// distinct from both the editor surface and the bar bg.
    pub(super) fn selection_bg(&self) -> Color {
        self.p.surface2
    }

    /// List-item highlight foreground.
    pub(super) fn selection_fg(&self) -> Color {
        self.p.text
    }

    /// Darkest contrast foreground for an inverted popup-selected row (text on
    /// an accent / warning / info bg). `Crust` gives high contrast on both
    /// Latte's bright accents and the dark flavors' lighter accents.
    pub(super) fn crust(&self) -> Color {
        self.p.crust
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Latte is the default and parses from the empty/unknown config strings a
    /// fresh or misconfigured install would produce.
    #[test]
    fn default_and_unknown_flavor_is_latte() {
        assert_eq!(Flavor::default(), Flavor::Latte);
        assert_eq!(Flavor::parse(""), Flavor::Latte);
        assert_eq!(Flavor::parse("nonsense"), Flavor::Latte);
    }

    /// The four flavors parse case-insensitively — a config value is a plain
    /// user string, not a case-sensitive token.
    #[test]
    fn flavors_parse_case_insensitive() {
        assert_eq!(Flavor::parse("latte"), Flavor::Latte);
        assert_eq!(Flavor::parse("FRAPPE"), Flavor::Frappe);
        assert_eq!(Flavor::parse("Macchiato"), Flavor::Macchiato);
        assert_eq!(Flavor::parse(" mocha "), Flavor::Mocha);
    }

    /// Latte is genuinely LIGHT and Mocha genuinely DARK: Latte's base is
    /// brighter than Mocha's, and Latte's text is darker than its base (dark
    /// text on a light surface) while Mocha's text is lighter than its base.
    /// This guards the headline user complaint ("doesn't look light") — a bug
    /// that swapped palettes would flip these relationships.
    fn luminance(c: Color) -> u32 {
        match c {
            Color::Rgb(r, g, b) => r as u32 * 299 + g as u32 * 587 + b as u32 * 114,
            _ => 0,
        }
    }

    #[test]
    fn latte_is_light_and_mocha_is_dark() {
        let latte = Theme::from_flavor(Flavor::Latte);
        let mocha = Theme::from_flavor(Flavor::Mocha);
        // Latte base is brighter than Mocha base.
        assert!(
            luminance(latte.bar_bg()) < luminance(Color::Rgb(239, 241, 245)) + 1
                && luminance(mocha.bar_bg()) < luminance(latte.bar_bg()),
            "Latte should be the light flavor, Mocha the dark one"
        );
        // Latte: dark text on a light bar (text luminance < bar luminance).
        assert!(
            luminance(latte.text()) < luminance(latte.bar_bg()),
            "Latte text must be darker than its bar (dark-on-light)"
        );
        // Mocha: light text on a dark bar.
        assert!(
            luminance(mocha.text()) > luminance(mocha.bar_bg()),
            "Mocha text must be lighter than its bar (light-on-dark)"
        );
    }

    /// Every accessor returns an `Rgb` (no `Reset`/named fallbacks leaking into
    /// the render path). A `Color::Indexed`/`Named` would ignore the flavor.
    #[test]
    fn all_roles_are_rgb() {
        let t = Theme::default();
        for c in [
            t.bar_bg(),
            t.text(),
            t.muted(),
            t.accent(),
            t.border(),
            t.link(),
            t.glyph(),
            t.success(),
            t.warning(),
            t.error(),
            t.info(),
            t.selection_bg(),
            t.selection_fg(),
            t.crust(),
        ] {
            assert!(
                matches!(c, Color::Rgb(_, _, _)),
                "role must be Rgb, got {c:?}"
            );
        }
    }

    /// Distinct flavors yield distinct accent colors — selecting a flavor in
    /// config actually changes the rendered palette.
    #[test]
    fn flavors_have_distinct_accents() {
        let accents = [
            Theme::from_flavor(Flavor::Latte).accent(),
            Theme::from_flavor(Flavor::Frappe).accent(),
            Theme::from_flavor(Flavor::Macchiato).accent(),
            Theme::from_flavor(Flavor::Mocha).accent(),
        ];
        for i in 0..accents.len() {
            for j in (i + 1)..accents.len() {
                assert_ne!(accents[i], accents[j], "flavors {i}/{j} share an accent");
            }
        }
    }
}

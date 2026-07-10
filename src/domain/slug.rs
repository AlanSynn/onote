//! Note-filename slug policy (`CLAUDE.md` §5 centralized filename rule).
//!
//! A pure string transform — no IO, no filesystem — so it belongs in the domain
//! layer. Both the infra `create_note` adapter and the application rename use
//! case call THIS function, so a note renamed in the Explorer slugifies
//! identically to one created there (DRY §5) and the application layer never
//! reaches across into an infra adapter (§4 dependency inversion — the prior
//! `application → crate::infra::filesystem_vault::slugify` arrow is gone, which
//! also removes the only `crate::infra` reference from `application/`).

/// Slugify a note title into a filename stem: lowercase, runs of non-`[a-z0-9]`
/// collapse to a single `-`, then trim leading/trailing `-`.
pub fn slugify(s: &str) -> String {
    let lower = s.to_lowercase();
    let mut out = String::with_capacity(lower.len());
    let mut prev_dash = false;
    for c in lower.chars() {
        if c.is_ascii_lowercase() || c.is_ascii_digit() {
            out.push(c);
            prev_dash = false;
        } else if !prev_dash {
            out.push('-');
            prev_dash = true;
        }
    }
    out.trim_matches('-').to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lowercases_and_dashes_non_alnum() {
        assert_eq!(slugify("Robot Idea!"), "robot-idea");
        assert_eq!(slugify("  spaced   out  "), "spaced-out");
    }

    #[test]
    fn collapses_runs_and_trims_dashes() {
        assert_eq!(slugify("a---b"), "a-b");
        assert_eq!(slugify("--- leading & trailing ---"), "leading-trailing");
    }

    #[test]
    fn non_ascii_becomes_a_separator() {
        // Non-ASCII letters are not `[a-z0-9]`, so they turn into separators.
        assert_eq!(slugify("café au lait"), "caf-au-lait");
    }

    #[test]
    fn empty_or_all_symbols_is_empty() {
        // The caller (`compose_rename_target`) falls back to "untitled" here.
        assert_eq!(slugify(""), "");
        assert_eq!(slugify("!!!"), "");
    }
}

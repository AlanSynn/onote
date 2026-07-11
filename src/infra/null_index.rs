//! No-op `NoteIndex` for indexless mode (`CLAUDE.md` §6.1).
//!
//! When no writable location exists for the derived cache (read-only vault AND
//! no writable cache dir — see [`super::index_location`]), onote still runs but
//! with search disabled. Every query returns empty/`Ok(())` so the app degrades
//! gracefully instead of crashing. Note reads/edits of writable paths are
//! unaffected: the index is pure derived cache, never a source of truth.

use crate::domain::errors::IndexError;
use crate::domain::note::{NoteDocument, NoteSummary, SearchHit};
use crate::domain::vault::RelativeNotePath;
use crate::ports::NoteIndex;

/// A `NoteIndex` that stores nothing and finds nothing.
#[derive(Debug, Default, Clone, Copy)]
pub struct NullNoteIndex;

impl NoteIndex for NullNoteIndex {
    fn refresh_note(&self, _note: &NoteDocument) -> Result<(), IndexError> {
        Ok(())
    }
    fn remove_note(&self, _path: &RelativeNotePath) -> Result<(), IndexError> {
        Ok(())
    }
    fn fuzzy_titles(&self, _query: &str) -> Result<Vec<NoteSummary>, IndexError> {
        Ok(Vec::new())
    }
    fn full_text_search(&self, _query: &str) -> Result<Vec<SearchHit>, IndexError> {
        Ok(Vec::new())
    }
    fn touch_recent(&self, _path: &RelativeNotePath, _now: i64) -> Result<(), IndexError> {
        Ok(())
    }
    fn rebuild(&self, _notes: &[NoteDocument]) -> Result<(), IndexError> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every operation is a no-op that succeeds and yields no results — the
    /// contract indexless mode relies on to keep reads working with search off.
    #[test]
    fn all_ops_are_noops_returning_empty() {
        let idx = NullNoteIndex;
        let path = RelativeNotePath::from_user("Notes/a.md").unwrap();
        let doc = NoteDocument::from_raw(
            RelativeNotePath::from_user("Notes/a.md").unwrap(),
            "body",
            0,
        );
        assert!(idx.refresh_note(&doc).is_ok());
        assert!(idx.remove_note(&path).is_ok());
        assert!(idx.touch_recent(&path, 0).is_ok());
        assert!(idx.rebuild(&[]).is_ok());
        assert!(idx.fuzzy_titles("anything").unwrap().is_empty());
        assert!(idx.full_text_search("anything").unwrap().is_empty());
    }
}

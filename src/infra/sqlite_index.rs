//! `NoteIndex` over SQLite + FTS5 + nucleo fuzzy matching (`CLAUDE.md` §2.6, §6.2).
//!
//! `notes` holds metadata; `notes_fts` (FTS5) holds the searchable body. Title
//! fuzzy search loads candidates from `notes` and ranks with nucleo (FTS is
//! tokenizing, not fuzzy). The remaining §6.2 tables are created for forward use.

use std::path::Path;
use std::sync::Mutex;

use nucleo_matcher::pattern::{CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Config, Matcher, Utf32Str};
use rusqlite::{params, Connection};

use crate::domain::errors::IndexError;
use crate::domain::note::{NoteDocument, NoteSummary, SearchHit};
use crate::domain::vault::RelativeNotePath;
use crate::ports::NoteIndex;

pub struct SqliteNoteIndex {
    conn: Mutex<Connection>,
}

fn sq<T>(r: rusqlite::Result<T>) -> Result<T, IndexError> {
    r.map_err(|e| IndexError::Sqlite(e.to_string()))
}

/// Upsert one note's metadata row (DRY: shared by `refresh_note` and `rebuild`).
/// `indexed_at` is passed in so callers control the timestamp in one place.
fn upsert_note_row(
    tx: &rusqlite::Transaction,
    note: &NoteDocument,
    now: i64,
) -> Result<(), IndexError> {
    sq(tx.execute(
        "INSERT INTO notes (path, title, content_hash, modified_at, indexed_at, pinned)
         VALUES (?1, ?2, ?3, ?4, ?5, 0)
         ON CONFLICT(path) DO UPDATE SET
           title=excluded.title,
           content_hash=excluded.content_hash,
           modified_at=excluded.modified_at,
           indexed_at=excluded.indexed_at",
        params![
            note.path.as_str(),
            note.title.as_str(),
            note.content_hash.as_str(),
            note.modified_at,
            now,
        ],
    ))?;
    Ok(())
}

/// Upsert one note's FTS row via delete-then-insert (DRY: shared by
/// `refresh_note` and `rebuild`). The per-row delete is a harmless no-op during
/// a full `rebuild` (which clears `notes_fts` first) but is required by
/// `refresh_note` to avoid accumulating duplicate FTS rows on re-index.
fn upsert_fts_row(tx: &rusqlite::Transaction, note: &NoteDocument) -> Result<(), IndexError> {
    sq(tx.execute(
        "DELETE FROM notes_fts WHERE path = ?1",
        params![note.path.as_str()],
    ))?;
    sq(tx.execute(
        "INSERT INTO notes_fts (path, title, body) VALUES (?1, ?2, ?3)",
        params![note.path.as_str(), note.title.as_str(), note.body.as_str()],
    ))?;
    Ok(())
}

impl SqliteNoteIndex {
    /// Open (or create) the index at `db_path`, initializing the schema.
    ///
    /// A corrupt `index.sqlite` (a half-written file from a crash, a hand-placed
    /// non-DB file, bit-rot) is RECOVERED, not fatal: the corrupt bytes are
    /// moved aside and a fresh DB is initialized. The index is pure derived
    /// cache (`CLAUDE.md` §6.1 — fully rebuildable from vault files via
    /// `App::reindex_all`), so discarding it loses no note data. Without this,
    /// a corrupt cache locked out EVERY `onote` command at startup — including
    /// `backup`/`copy`/`img`, which never touch the index.
    pub fn new(db_path: &Path) -> Result<Self, IndexError> {
        if let Some(parent) = db_path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let conn = Self::open_with_recovery(db_path)?;
        conn.execute_batch("PRAGMA journal_mode=WAL;").ok();
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS notes (
               path TEXT PRIMARY KEY,
               title TEXT NOT NULL,
               content_hash TEXT NOT NULL,
               modified_at INTEGER NOT NULL,
               indexed_at INTEGER NOT NULL,
               pinned INTEGER NOT NULL DEFAULT 0
             );
             CREATE VIRTUAL TABLE IF NOT EXISTS notes_fts USING fts5(
               path, title, body, tokenize='unicode61'
             );
             CREATE TABLE IF NOT EXISTS attachments (
               path TEXT PRIMARY KEY, mime TEXT, width INTEGER, height INTEGER,
               size_bytes INTEGER, created_at INTEGER
             );
             CREATE TABLE IF NOT EXISTS note_attachments (
               note_path TEXT NOT NULL, attachment_path TEXT NOT NULL,
               PRIMARY KEY (note_path, attachment_path)
             );
             CREATE TABLE IF NOT EXISTS sessions (
               session_id TEXT PRIMARY KEY, note_path TEXT NOT NULL, pid INTEGER NOT NULL,
               mode TEXT NOT NULL, opened_at INTEGER NOT NULL, last_seen_at INTEGER NOT NULL
             );
             CREATE TABLE IF NOT EXISTS recent_notes (
               path TEXT PRIMARY KEY, opened_at INTEGER NOT NULL
             );",
        )
        .map_err(|e| IndexError::Sqlite(e.to_string()))?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    /// Open the connection, probing for a corrupt/non-DB file and recovering.
    ///
    /// `Connection::open` is lazy and succeeds on a file whose bytes aren't a
    /// valid SQLite DB, so validity is forced by reading the schema catalog
    /// (`sqlite_master`). A real corrupt file surfaces SQLITE_NOTADB there; a
    /// brand-new empty (0-byte) DB reports zero rows and is treated as valid.
    fn open_with_recovery(db_path: &Path) -> Result<Connection, IndexError> {
        let conn = Connection::open(db_path).map_err(|e| IndexError::Sqlite(e.to_string()))?;
        let probe =
            conn.query_row::<i64, _, _>("SELECT COUNT(*) FROM sqlite_master", [], |row| row.get(0));
        if probe.is_ok() {
            return Ok(conn);
        }
        // Probe failed → the file is not a usable DB. Drop this connection and
        // recover (move aside, reopen fresh).
        drop(conn);
        Self::recover_corrupt_db(db_path)
    }

    /// Move a corrupt `index.sqlite` aside (forensics) and reopen fresh.
    ///
    /// The `-wal`/`-shm` siblings are transient journal artifacts and are
    /// removed so the reopened DB starts clean. If the aside-rename itself
    /// fails (e.g. permissions), fall back to deletion — recovery must not be
    /// blocked, since the cache is fully derived and rebuildable.
    fn recover_corrupt_db(db_path: &Path) -> Result<Connection, IndexError> {
        tracing::warn!(
            path = %db_path.display(),
            "index.sqlite is corrupt (not a database); moving it aside and \
             reinitializing. The index is derived cache rebuilt from vault \
             files — no note data is lost."
        );
        let parent = db_path.parent().unwrap_or_else(|| Path::new("."));
        let stem = db_path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("index.sqlite");
        let aside = parent.join(format!("{stem}.corrupt"));
        let rename_ok = std::fs::rename(db_path, &aside).is_ok();
        let _ = std::fs::remove_file(parent.join(format!("{stem}-wal")));
        let _ = std::fs::remove_file(parent.join(format!("{stem}-shm")));
        // Rename failed or the file still present → delete so a fresh open can
        // create a valid DB rather than reopening the same corrupt bytes.
        if !rename_ok && db_path.exists() {
            let _ = std::fs::remove_file(db_path);
        }
        Connection::open(db_path).map_err(|e| IndexError::Sqlite(e.to_string()))
    }

    /// In-memory index for tests.
    pub fn in_memory() -> Result<Self, IndexError> {
        let conn = Connection::open_in_memory().map_err(|e| IndexError::Sqlite(e.to_string()))?;
        conn.execute_batch(
            "CREATE TABLE notes (path TEXT PRIMARY KEY, title TEXT NOT NULL,
               content_hash TEXT NOT NULL, modified_at INTEGER NOT NULL,
               indexed_at INTEGER NOT NULL, pinned INTEGER NOT NULL DEFAULT 0);
             CREATE VIRTUAL TABLE notes_fts USING fts5(path, title, body);",
        )
        .map_err(|e| IndexError::Sqlite(e.to_string()))?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }
}

impl NoteIndex for SqliteNoteIndex {
    fn refresh_note(&self, note: &NoteDocument) -> Result<(), IndexError> {
        let mut conn = self
            .conn
            .lock()
            .map_err(|e| IndexError::Sqlite(e.to_string()))?;
        let now = chrono::Utc::now().timestamp();
        // Wrap all writes in one transaction so `notes` and `notes_fts` commit
        // atomically — a failure partway through cannot leave the tables
        // desynchronized (e.g. `notes` row without a matching `notes_fts` row,
        // which would make the note permanently unsearchable).
        let tx = sq(conn.transaction())?;
        upsert_note_row(&tx, note, now)?;
        upsert_fts_row(&tx, note)?;
        sq(tx.commit())?;
        Ok(())
    }

    fn remove_note(&self, path: &RelativeNotePath) -> Result<(), IndexError> {
        let mut conn = self
            .conn
            .lock()
            .map_err(|e| IndexError::Sqlite(e.to_string()))?;
        // Single transaction: deleting from `notes` and `notes_fts` must commit
        // together so a failure cannot leave an orphan FTS row that makes search
        // return hits for a deleted file.
        let tx = sq(conn.transaction())?;
        sq(tx.execute("DELETE FROM notes WHERE path = ?1", params![path.as_str()]))?;
        sq(tx.execute(
            "DELETE FROM notes_fts WHERE path = ?1",
            params![path.as_str()],
        ))?;
        sq(tx.commit())?;
        Ok(())
    }

    fn rebuild(&self, notes: &[NoteDocument]) -> Result<(), IndexError> {
        let mut conn = self
            .conn
            .lock()
            .map_err(|e| IndexError::Sqlite(e.to_string()))?;
        let now = chrono::Utc::now().timestamp();
        // One transaction for the whole rebuild: clearing + bulk insert commit
        // atomically, so a crash mid-rebuild leaves the previous index intact
        // (never a half-cleared, half-populated index). The clear-evict semantics
        // also drop rows for notes deleted externally since the last run.
        let tx = sq(conn.transaction())?;
        sq(tx.execute("DELETE FROM notes", []))?;
        sq(tx.execute("DELETE FROM notes_fts", []))?;
        for note in notes {
            upsert_note_row(&tx, note, now)?;
            // `notes_fts` was cleared above, so upsert_fts_row's per-row delete is
            // a no-op here — kept to share one code path with refresh_note (DRY).
            upsert_fts_row(&tx, note)?;
        }
        sq(tx.commit())?;
        Ok(())
    }

    fn fuzzy_titles(&self, query: &str) -> Result<Vec<NoteSummary>, IndexError> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| IndexError::Sqlite(e.to_string()))?;
        // Cap is a perf guard (nucleo scores thousands of rows well within a
        // frame), NOT a hard ceiling on searchable notes. The previous 500-row
        // limit made older notes permanently unreachable by fuzzy open once a
        // vault grew past it; 5000 keeps even large vaults fully reachable while
        // bounding the worst-case row scan.
        let mut stmt = sq(conn.prepare(
            "SELECT path, title, modified_at FROM notes ORDER BY modified_at DESC LIMIT 5000",
        ))?;
        let rows: Vec<(String, String, i64)> = sq(stmt.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
            ))
        }))?
        .filter_map(Result::ok)
        .collect();
        drop(stmt);

        // Empty query → most recent notes (recency surface).
        if query.trim().is_empty() {
            return Ok(rows
                .into_iter()
                .filter_map(|(p, t, m)| {
                    Some(NoteSummary {
                        path: RelativeNotePath::from_user(&p).ok()?,
                        title: t,
                        modified_at: m,
                    })
                })
                .collect());
        }

        let mut matcher = Matcher::new(Config::DEFAULT);
        let pattern = Pattern::parse(query, CaseMatching::Smart, Normalization::Smart);
        let mut char_buf: Vec<char> = Vec::new();
        let mut scored: Vec<(u32, NoteSummary)> = Vec::new();
        for (p, t, m) in rows {
            char_buf.clear();
            char_buf.extend(t.chars());
            let haystack = Utf32Str::Unicode(&char_buf);
            if let Some(score) = pattern.score(haystack, &mut matcher) {
                if let Ok(path) = RelativeNotePath::from_user(&p) {
                    scored.push((
                        score,
                        NoteSummary {
                            path,
                            title: t,
                            modified_at: m,
                        },
                    ));
                }
            }
        }
        // nucleo scores: higher is better.
        scored.sort_by_key(|s| std::cmp::Reverse(s.0));
        Ok(scored.into_iter().map(|(_, s)| s).collect())
    }

    fn full_text_search(&self, query: &str) -> Result<Vec<SearchHit>, IndexError> {
        let trimmed = query.trim();
        if trimmed.is_empty() {
            return Ok(Vec::new());
        }
        // Phrase-quote the query so user input with FTS metacharacters is literal.
        let escaped = format!("\"{}\"", trimmed.replace('"', "\"\""));
        let conn = self
            .conn
            .lock()
            .map_err(|e| IndexError::Sqlite(e.to_string()))?;
        let mut stmt = sq(conn.prepare(
            "SELECT path, title,
                    snippet(notes_fts, 2, '«', '»', '…', 16) AS snip,
                    rank
             FROM notes_fts WHERE notes_fts MATCH ?1
             ORDER BY rank LIMIT 50",
        ))?;
        let rows = sq(stmt.query_map(params![escaped], |row| {
            let path: String = row.get(0)?;
            let title: String = row.get(1)?;
            let snip: String = row.get(2)?;
            let rank: f64 = row.get(3)?;
            Ok((path, title, snip, rank))
        }))?
        .filter_map(Result::ok)
        .collect::<Vec<_>>();
        Ok(rows
            .into_iter()
            .filter_map(|(p, t, snip, rank)| {
                let path = RelativeNotePath::from_user(&p).ok()?;
                // FTS5 BM25 rank is negative (lower = better). Flip & scale.
                let score = (-rank * 1000.0).max(0.0) as i64;
                Some(SearchHit {
                    path,
                    title: t,
                    snippet: snip,
                    score,
                })
            })
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::note::{ContentHash, MarkdownBody, NoteDocument, NoteTitle};
    use crate::domain::vault::RelativeNotePath;

    fn doc(path: &str, title: &str, body: &str) -> NoteDocument {
        NoteDocument {
            path: RelativeNotePath::from_user(path).unwrap(),
            title: NoteTitle(title.into()),
            body: MarkdownBody(body.into()),
            frontmatter: Default::default(),
            content_hash: ContentHash::of_str(body),
            modified_at: 1000,
        }
    }

    #[test]
    fn refresh_then_search_and_fuzzy() {
        let idx = SqliteNoteIndex::in_memory().unwrap();
        idx.refresh_note(&doc("Notes/robot.md", "Robot Idea", "build a robot arm"))
            .unwrap();
        idx.refresh_note(&doc(
            "Notes/cook.md",
            "Cooking Notes",
            "pasta recipe tonight",
        ))
        .unwrap();

        let fts = idx.full_text_search("robot").unwrap();
        assert_eq!(fts.len(), 1);
        assert_eq!(fts[0].path.as_str(), "Notes/robot.md");

        let fz = idx.fuzzy_titles("rob").unwrap();
        assert!(fz.iter().any(|s| s.title == "Robot Idea"));
    }

    #[test]
    fn remove_drops_from_index() {
        let idx = SqliteNoteIndex::in_memory().unwrap();
        idx.refresh_note(&doc("Notes/a.md", "A", "alpha body"))
            .unwrap();
        assert!(!idx.full_text_search("alpha").unwrap().is_empty());
        idx.remove_note(&RelativeNotePath::from_user("Notes/a.md").unwrap())
            .unwrap();
        assert!(idx.full_text_search("alpha").unwrap().is_empty());
    }

    /// Regression guard for the round-7 atomicity defect: `refresh_note` and
    /// `remove_note` must keep `notes` and `notes_fts` consistent. After a
    /// refresh both tables must hold the path; after a remove neither may
    /// retain it — in particular `notes_fts` must not keep an orphan row that
    /// would make `full_text_search` return hits for a deleted file.
    #[test]
    fn refresh_then_remove_leaves_both_tables_empty() {
        let idx = SqliteNoteIndex::in_memory().unwrap();
        idx.refresh_note(&doc("Notes/orphan.md", "Orphan", "orphan body"))
            .unwrap();

        let count = |table: &str| -> i64 {
            let conn = idx.conn.lock().unwrap();
            let sql = format!("SELECT COUNT(*) FROM {} WHERE path = ?1", table);
            conn.query_row(&sql, params!["Notes/orphan.md"], |row| row.get(0))
                .unwrap()
        };

        // After refresh: both tables carry the path — refresh is itself
        // all-or-nothing, so a partial refresh would already show here.
        assert_eq!(count("notes"), 1);
        assert_eq!(count("notes_fts"), 1);

        // After remove: path must be gone from BOTH tables atomically.
        idx.remove_note(&RelativeNotePath::from_user("Notes/orphan.md").unwrap())
            .unwrap();
        assert_eq!(count("notes"), 0, "notes must not retain a removed path");
        assert_eq!(
            count("notes_fts"),
            0,
            "notes_fts must not retain an orphan row for a removed path"
        );
    }

    /// Round-8 #3 regression guard: `rebuild` atomically replaces the entire
    /// index — a note present before but absent from the rebuild set must be
    /// evicted (not retained as a stale ghost), and every note in the set must
    /// land in both FTS and fuzzy. This is the primitive `App::reindex_all`
    /// relies on to bootstrap search for an existing vault.
    #[test]
    fn rebuild_replaces_full_index_evicting_stale_rows() {
        let idx = SqliteNoteIndex::in_memory().unwrap();
        idx.refresh_note(&doc("Notes/stale.md", "Stale", "stale body"))
            .unwrap();
        assert!(!idx.full_text_search("stale").unwrap().is_empty());

        idx.rebuild(&[
            doc("Notes/a.md", "Alpha", "alpha note"),
            doc("Notes/b.md", "Beta", "beta note"),
        ])
        .unwrap();

        // Stale note (on disk-deleted externally, or just not in the rebuild set)
        // must be gone from both surfaces.
        assert!(
            idx.full_text_search("stale").unwrap().is_empty(),
            "rebuild must evict notes absent from the new set"
        );
        // New set is fully searchable + fuzzable.
        assert_eq!(idx.full_text_search("alpha").unwrap().len(), 1);
        assert!(idx
            .fuzzy_titles("Beta")
            .unwrap()
            .iter()
            .any(|s| s.title == "Beta"));
    }

    /// Round-10 MINOR regression guard: a corrupt `.onote/index.sqlite` (a
    /// half-written crash file, a hand-placed non-DB file, bit-rot) must be
    /// RECOVERED, not fatal. The index is derived cache (§6.1), so moving the
    /// corrupt bytes aside and reinitializing loses no note data. Before this
    /// fix, opening a corrupt cache surfaced "file is not a database" and
    /// locked out EVERY `onote` command at startup — including `backup`/`copy`
    /// /`img`, which never query the index.
    #[test]
    fn new_recovers_from_corrupt_index_file_instead_of_locking_out() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db = dir.path().join(".onote").join("index.sqlite");
        std::fs::create_dir_all(db.parent().unwrap()).unwrap();
        // Garbage bytes that are NOT a valid SQLite DB (invalid header magic).
        std::fs::write(&db, b"definitely not a sqlite database").unwrap();

        let idx = SqliteNoteIndex::new(&db).expect("corrupt index must be recovered, not fatal");

        // The recovered index is fully usable immediately.
        idx.refresh_note(&doc("Notes/a.md", "A", "alpha body"))
            .unwrap();
        assert_eq!(idx.full_text_search("alpha").unwrap().len(), 1);

        // The corrupt bytes were moved aside (not silently deleted) for forensics.
        let aside = dir.path().join(".onote/index.sqlite.corrupt");
        assert!(
            aside.exists(),
            "corrupt file should be moved aside for forensics"
        );
        assert_eq!(
            std::fs::read(&aside).unwrap(),
            b"definitely not a sqlite database",
        );
    }

    /// A brand-new (0-byte) DB is valid, not "corrupt": `Connection::open` of a
    /// non-existent path creates an empty file, which must initialize normally
    /// rather than trigger the recovery path. Guards against the recovery probe
    /// (`SELECT COUNT(*) FROM sqlite_master`) mis-classifying a fresh empty DB.
    #[test]
    fn new_initializes_brand_new_empty_db_without_recovery() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db = dir.path().join(".onote").join("index.sqlite");
        // Path does not exist yet — open must create + initialize it.
        let idx = SqliteNoteIndex::new(&db).expect("fresh index must initialize");
        idx.refresh_note(&doc("Notes/a.md", "A", "alpha body"))
            .unwrap();
        assert_eq!(idx.full_text_search("alpha").unwrap().len(), 1);
        // No recovery artifact should be created for a clean init.
        assert!(
            !dir.path().join(".onote/index.sqlite.corrupt").exists(),
            "no .corrupt aside should exist for a clean new DB"
        );
    }
}

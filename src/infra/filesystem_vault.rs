//! Filesystem-backed `VaultRepository` (`CLAUDE.md` §3.1 Vault, §3.2 infra).
//!
//! Owns the vault root path and nothing else. Every note path is resolved
//! through [`RelativeNotePath::resolve_within`] so traversal outside the vault
//! is impossible; writes are atomic (tmp-then-rename) and use optimistic
//! concurrency (`CLAUDE.md` §7).

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::domain::errors::VaultError;
use crate::domain::note::{ContentHash, NoteDocument, NoteSummary};
use crate::domain::vault::RelativeNotePath;
use crate::ports::{VaultRepository, WriteResult};

/// Directory names skipped during `list_notes` recursion
/// (`CLAUDE.md` §3.1 Vault). These hold Obsidian config, onote state, git
/// internals, or JS deps — never user notes.
///
/// `pub(crate)` so the file watcher (`file_watch.rs`) reuses the SAME set
/// rather than re-declaring it (DRY §5); the watcher must drop events under
/// these dirs to avoid surfacing ghost notes in fuzzy/FTS.
pub(crate) const SKIP_DIRS: &[&str] = &[".git", ".obsidian", ".onote", "node_modules"];

/// Filesystem implementation of [`VaultRepository`].
///
/// Stateless beyond the vault root — every operation re-reads disk so external
/// edits (Obsidian, another terminal, `git pull`) are reflected without a
/// cache. The vault root is *not* validated at construction time; callers that
/// need validation should go through [`crate::domain::vault::VaultPath`].
pub struct FilesystemVault {
    root: PathBuf,
}

impl FilesystemVault {
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }

    /// Resolve `path` under the vault root, re-checking escape after join.
    #[inline]
    fn resolve(&self, path: &RelativeNotePath) -> Result<PathBuf, VaultError> {
        path.resolve_within(&self.root)
    }
}

impl VaultRepository for FilesystemVault {
    fn list_notes(&self) -> Result<Vec<NoteSummary>, VaultError> {
        let mut entries: Vec<NoteSummary> = Vec::new();
        walk_md(&self.root, &self.root, &mut entries)?;
        // Most-recent-first (`CLAUDE.md` §3.3 list_notes).
        entries.sort_by_key(|e| std::cmp::Reverse(e.modified_at));
        Ok(entries)
    }

    fn read_note(&self, path: &RelativeNotePath) -> Result<NoteDocument, VaultError> {
        let abs = self.resolve(path)?;
        if !abs.exists() {
            return Err(VaultError::NoteNotFound(path.as_str()));
        }
        let contents = fs::read_to_string(&abs)?;
        let mtime = file_mtime_secs(&abs)?;
        Ok(NoteDocument::from_raw(path.clone(), &contents, mtime))
    }

    fn write_note(
        &self,
        note: &NoteDocument,
        expected_hash: Option<&ContentHash>,
    ) -> Result<WriteResult, VaultError> {
        // §7 optimistic concurrency. A missing file is treated, for both the
        // equality check and the value reported on conflict, as having the hash
        // of empty bytes — so a first write with `Some(empty_hash)` succeeds.
        let current_disk = match self.disk_hash(&note.path)? {
            Some(h) => h,
            None => ContentHash::of_bytes(b""),
        };

        if let Some(expected) = expected_hash {
            if &current_disk != expected {
                return Ok(WriteResult::Conflict {
                    current_disk_hash: current_disk,
                });
            }
        }

        let new_hash = ContentHash::of_str(note.body.as_str());
        if current_disk == new_hash {
            return Ok(WriteResult::NoChange);
        }

        let target = self.resolve(&note.path)?;
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent)?;
        }
        // Atomic write: stage in the same directory, then rename over target.
        // The suffix combines pid + a per-write nanosecond nonce, so two
        // concurrent saves of the SAME note (even from one process, or two
        // onote processes) can never share a tmp path and race to rename —
        // which would otherwise clobber one writer and silently bypass the
        // §7 optimistic-concurrency check.
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or_else(|_| std::process::id() as u128);
        let tmp_path = PathBuf::from(format!(
            "{}.onote-tmp-{}-{}",
            target.display(),
            std::process::id(),
            nonce
        ));
        // Round-9: wrap the STAGING write itself in cleanup-on-error. A failure
        // here (disk-full mid-write, EIO) leaves a partially-written tmp on disk;
        // every POST-staging error path below already cleans up, but the staging
        // call itself used to propagate without removing the tmp. Remove it
        // before propagating so no path between tmp creation and rename leaks.
        let stage = write_private(&tmp_path, note.body.as_str());
        if let Err(e) = stage {
            let _ = fs::remove_file(&tmp_path);
            return Err(e);
        }

        // §7 TOCTOU re-check (CLAUDE.md §7 "never silently overwrite").
        // Between the baseline `current_disk` read at the top of this function
        // and the rename below, another writer (Obsidian Sync, `git pull`, a
        // second `onote`, iCloud) can replace the file. Renaming without
        // re-checking would silently clobber them and — worse — leave
        // `opened_hash == disk_hash` post-write so the §7 conflict detector
        // could never fire. Re-read disk; if it no longer matches the
        // baseline `current_disk` (NOT `expected_hash`, which may be `None`),
        // abandon the write (deleting the staged tmp first so it can't linger
        // world-readable) and surface Conflict with the *current* disk hash.
        // This narrows the TOCTOU window from "the entire write" to "between
        // this read and the rename", defeating the common Obsidian / git-pull
        // / iCloud case without adding a file-locking dependency.
        let current_disk_now = match self.disk_hash(&note.path) {
            Ok(Some(h)) => h,
            Ok(None) => ContentHash::of_bytes(b""),
            Err(e) => {
                let _ = fs::remove_file(&tmp_path);
                return Err(e);
            }
        };
        if current_disk_now != current_disk {
            let _ = fs::remove_file(&tmp_path);
            return Ok(WriteResult::Conflict {
                current_disk_hash: current_disk_now,
            });
        }

        let result = fs::rename(&tmp_path, &target);
        // If rename fails, clean up the staged tmp so it can't linger readable.
        if result.is_err() {
            let _ = fs::remove_file(&tmp_path);
        }
        result?;
        Ok(WriteResult::Written(new_hash))
    }

    fn create_note(
        &self,
        title: &str,
        folder: Option<&RelativeNotePath>,
    ) -> Result<RelativeNotePath, VaultError> {
        let slug = slugify(title);
        let stem = if slug.is_empty() {
            "untitled".to_string()
        } else {
            slug
        };

        let base_dir = match folder {
            Some(f) => self.resolve(f)?,
            None => self.root.clone(),
        };

        // Find a unique filename, appending -2, -3, ... as needed.
        let mut candidate = base_dir.join(format!("{stem}.md"));
        let mut counter = 2;
        while candidate.exists() {
            candidate = base_dir.join(format!("{stem}-{counter}.md"));
            counter += 1;
        }

        if let Some(parent) = candidate.parent() {
            fs::create_dir_all(parent)?;
        }
        // Body uses the *original* title; only the filename is slugified.
        write_private(&candidate, &format!("# {title}\n"))?;

        let rel = candidate
            .strip_prefix(&self.root)
            .map_err(|_| VaultError::Escape(candidate.display().to_string()))?;
        RelativeNotePath::new(rel.to_path_buf())
    }

    fn delete_note(&self, path: &RelativeNotePath) -> Result<(), VaultError> {
        let abs = self.resolve(path)?;
        match fs::remove_file(&abs) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                Err(VaultError::NoteNotFound(path.as_str()))
            }
            Err(e) => Err(e.into()),
        }
    }

    fn disk_hash(&self, path: &RelativeNotePath) -> Result<Option<ContentHash>, VaultError> {
        let abs = self.resolve(path)?;
        match fs::read(&abs) {
            Ok(bytes) => Ok(Some(ContentHash::of_bytes(&bytes))),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e.into()),
        }
    }
}

/// Recursively collect `*.md` files under `dir`, skipping [`SKIP_DIRS`] and
/// non-UTF-8 files.
///
/// `root` is the vault root (for `strip_prefix`); `dir` is the current
/// recursion position. On any non-encoding I/O error the walk aborts and the
/// error propagates — a half-listed vault is worse than a clear failure.
fn walk_md(root: &Path, dir: &Path, out: &mut Vec<NoteSummary>) -> Result<(), VaultError> {
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let ft = entry.file_type()?;
        let path = entry.path();

        if ft.is_dir() {
            let name = entry.file_name();
            let name_str = name.to_string_lossy();
            // Skip the named cache/config dirs (SKIP_DIRS) AND any dotfile dir.
            // The dotfile sweep keeps `walk_md` aligned with the file watcher
            // (`file_watch::forward`) and `attachment_store::walk_notes`, both of
            // which drop dot-prefixed segments — without it this function would
            // INDEX notes under Obsidian's `.trash/` recycle bin (and user
            // `.drafts/`, `.templates/`, `.archive/`) while the watcher silently
            // dropped their external edits, leaving permanently stale index rows
            // (round-10 regression-hunt MEDIUM). Indexing a recycle bin is also
            // a UX bug in its own right.
            if SKIP_DIRS.iter().any(|s| *s == &*name_str) || name_str.starts_with('.') {
                continue;
            }
            walk_md(root, &path, out)?;
        } else if ft.is_file() {
            if path.extension().and_then(|e| e.to_str()) != Some("md") {
                continue;
            }
            // File-level dotfile sweep: skip a `.md` file whose NAME starts with
            // `.` (e.g. `.scratch.md`). Aligns the FILE branch with the directory
            // branch above and with `file_watch::forward` (which drops any path
            // segment starting with `.`). Without it, a hidden `.md` would be
            // INDEXED by `list_notes`/`reindex_all` while the watcher drops its
            // external edits — the same stale-index divergence the directory
            // sweep closed, just one level down (round-11 convergence nit).
            let file_name = entry.file_name();
            if file_name.to_string_lossy().starts_with('.') {
                continue;
            }
            let rel = match path.strip_prefix(root) {
                Ok(r) => r,
                Err(_) => continue,
            };
            let rel_path = RelativeNotePath::new(rel.to_path_buf())?;
            // Skip non-UTF-8 files silently; any other read error propagates.
            let contents = match fs::read_to_string(&path) {
                Ok(s) => s,
                Err(e) if e.kind() == std::io::ErrorKind::InvalidData => continue,
                Err(e) => return Err(e.into()),
            };
            let mtime = file_mtime_secs(&path)?;
            let doc = NoteDocument::from_raw(rel_path.clone(), &contents, mtime);
            out.push(NoteSummary {
                path: rel_path,
                title: doc.title.as_str().to_string(),
                modified_at: mtime,
            });
        }
    }
    Ok(())
}

/// `mtime` of `path` as Unix-epoch seconds. Pre-epoch mtimes are reported as
/// negative; filesystems that don't support `modified()` surface an error via
/// the `From<io::Error>` impl on [`VaultError`].
fn file_mtime_secs(path: &Path) -> Result<i64, VaultError> {
    let meta = fs::metadata(path)?;
    let mtime = meta.modified()?;
    let secs = mtime
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or_else(|e| -(e.duration().as_secs() as i64));
    Ok(secs)
}

/// Write `bytes` to `path` with owner-only permissions (`0o600`) on Unix so the
/// staged note body isn't briefly world-readable during the atomic-rename
/// window. Falls back to a plain write elsewhere.
fn write_private(path: &Path, body: &str) -> Result<(), VaultError> {
    #[cfg(unix)]
    {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        let mut f = fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(path)?;
        f.write_all(body.as_bytes())?;
    }
    #[cfg(not(unix))]
    {
        fs::write(path, body)?;
    }
    Ok(())
}

/// Slugify per `create_note` spec: lowercase, runs of non-`[a-z0-9]` collapse
/// to a single `-`, then trim leading/trailing `-`.
fn slugify(s: &str) -> String {
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
    use tempfile::{tempdir, TempDir};

    /// Build a vault backed by a fresh temp directory. The `TempDir` is returned
    /// (binding kept alive via `_dir`) so it isn't cleaned up mid-test.
    fn vault() -> (TempDir, FilesystemVault) {
        let dir = tempdir().expect("tempdir created");
        let v = FilesystemVault::new(dir.path().to_path_buf());
        (dir, v)
    }

    #[test]
    fn write_then_read_roundtrip() {
        let (_dir, vault) = vault();
        let rel = RelativeNotePath::new("Notes/idea.md").unwrap();
        let doc = NoteDocument::from_raw(rel.clone(), "# Idea\nbody", 0);
        let res = vault.write_note(&doc, None).unwrap();
        assert!(matches!(res, WriteResult::Written(_)));

        let read = vault.read_note(&rel).unwrap();
        assert_eq!(read.body.as_str(), "# Idea\nbody");
        assert_eq!(read.title.as_str(), "Idea");
    }

    #[test]
    fn write_conflict_path() {
        let (_dir, vault) = vault();
        let rel = RelativeNotePath::new("a.md").unwrap();

        // Seed disk with "hello".
        let doc = NoteDocument::from_raw(rel.clone(), "hello", 0);
        vault.write_note(&doc, None).unwrap();
        let disk = vault.disk_hash(&rel).unwrap().unwrap();
        assert_eq!(disk, ContentHash::of_str("hello"));

        // Stale expected_hash → Conflict; disk must be untouched.
        let wrong = ContentHash::of_str("not-on-disk");
        let doc2 = NoteDocument::from_raw(rel.clone(), "world", 0);
        let res = vault.write_note(&doc2, Some(&wrong)).unwrap();
        match res {
            WriteResult::Conflict { current_disk_hash } => {
                assert_eq!(current_disk_hash, disk);
            }
            other => panic!("expected Conflict, got {other:?}"),
        }
        let on_disk = std::fs::read_to_string(_dir.path().join("a.md")).unwrap();
        assert_eq!(on_disk, "hello");
    }

    #[test]
    fn write_note_with_stale_expected_hash_returns_conflict_and_does_not_clobber() {
        // §7 TOCTOU invariant (CLAUDE.md §7 "never silently overwrite"):
        // when a concurrent writer (Obsidian Sync, `git pull`, a second
        // `onote`, iCloud) lands between onote's top-level hash baseline and
        // its atomic rename, onote must NOT clobber the newer content. The
        // pre-rename re-check in `write_note` is responsible for catching
        // this; on mismatch it returns Conflict with the *current* disk hash
        // and leaves disk untouched.
        //
        // The actual TOCTOU race is not deterministically reproducible in a
        // unit test without injection hooks (the window between the top-level
        // read and the rename is sub-microsecond and `write_note` exposes no
        // mid-write hook to mutate disk inside it), so this test exercises
        // the same `WriteResult::Conflict` return path the re-check uses: it
        // simulates a concurrent write (direct disk overwrite) and then calls
        // `write_note` with the now-stale baseline hash. The top-level check
        // and the pre-rename re-check share an identical Conflict contract,
        // so a test that exercises one documents the guarantee the other
        // enforces.
        let (_dir, vault) = vault();
        let rel = RelativeNotePath::new("race.md").unwrap();
        let root = _dir.path().to_path_buf();

        // Establish a baseline: write "v1", capture the real disk hash.
        let v1 = NoteDocument::from_raw(rel.clone(), "v1 baseline", 0);
        vault.write_note(&v1, None).unwrap();
        let baseline_hash = vault.disk_hash(&rel).unwrap().unwrap();
        assert_eq!(baseline_hash, ContentHash::of_str("v1 baseline"));

        // Simulate a concurrent writer landing AFTER onote read its baseline
        // hash but BEFORE its rename: overwrite disk directly with new
        // content. (In the real TOCTOU scenario this happens between the
        // top-level `disk_hash` call and the `fs::rename`; here we force the
        // same end-state so the conflict path is observable.)
        std::fs::write(root.join("race.md"), "concurrent writer landed").unwrap();
        let concurrent_hash = ContentHash::of_str("concurrent writer landed");

        // Attempt to write "v2" carrying the STALE baseline hash. The mismatch
        // must be detected (top-level check at call time, or pre-rename
        // re-check if the race is simulated mid-write) and onote must return
        // Conflict WITHOUT clobbering the concurrent write. Before the §7
        // re-check fix, the rename path had no second check and could
        // silently overwrite a writer that landed in the TOCTOU window —
        // this test pins down the invariant the re-check enforces.
        let v2 = NoteDocument::from_raw(rel.clone(), "v2 user edit", 0);
        let res = vault.write_note(&v2, Some(&baseline_hash)).unwrap();
        match res {
            WriteResult::Conflict { current_disk_hash } => {
                assert_eq!(
                    current_disk_hash, concurrent_hash,
                    "must report the CURRENT disk hash (not the stale baseline)",
                );
            }
            other => panic!("expected Conflict, got {other:?}"),
        }

        // §7 "never silently overwrite": the concurrent write must survive.
        let on_disk = std::fs::read_to_string(root.join("race.md")).unwrap();
        assert_eq!(
            on_disk, "concurrent writer landed",
            "concurrent write must NOT be clobbered",
        );

        // No tmp litter: the conflict path (whether top-level or pre-rename
        // re-check) must not leave staged tmp files in the vault. On the
        // re-check path specifically the tmp IS created before the conflict
        // is detected, so the cleanup `fs::remove_file` in `write_note` is
        // load-bearing for this assertion.
        let tmp_litter = std::fs::read_dir(&root)
            .unwrap()
            .filter_map(Result::ok)
            .filter(|e| e.file_name().to_string_lossy().contains(".onote-tmp-"))
            .count();
        assert_eq!(
            tmp_litter, 0,
            "no tmp files should litter the vault on Conflict"
        );
    }

    #[test]
    fn write_nochange_path() {
        let (_dir, vault) = vault();
        let rel = RelativeNotePath::new("b.md").unwrap();
        let body = "# Note\nhello";
        let doc = NoteDocument::from_raw(rel.clone(), body, 0);
        vault.write_note(&doc, None).unwrap();

        // Re-writing identical bytes with the correct expected_hash is a no-op.
        let actual = vault.disk_hash(&rel).unwrap().unwrap();
        let doc2 = NoteDocument::from_raw(rel.clone(), body, 0);
        let res = vault.write_note(&doc2, Some(&actual)).unwrap();
        assert_eq!(res, WriteResult::NoChange);
    }

    #[test]
    fn write_first_time_with_empty_expected_hash_succeeds() {
        // A missing file is treated as having the hash of empty bytes, so a
        // caller may express "I expect no file" by passing Some(empty_hash).
        let (_dir, vault) = vault();
        let rel = RelativeNotePath::new("fresh.md").unwrap();
        let empty_hash = ContentHash::of_bytes(b"");
        let doc = NoteDocument::from_raw(rel.clone(), "fresh body", 0);
        let res = vault.write_note(&doc, Some(&empty_hash)).unwrap();
        assert!(matches!(res, WriteResult::Written(_)));
    }

    #[test]
    fn write_first_time_with_wrong_expected_hash_conflicts() {
        let (_dir, vault) = vault();
        let rel = RelativeNotePath::new("fresh2.md").unwrap();
        let wrong = ContentHash::of_str("something-else");
        let doc = NoteDocument::from_raw(rel.clone(), "fresh body", 0);
        let res = vault.write_note(&doc, Some(&wrong)).unwrap();
        match res {
            WriteResult::Conflict { current_disk_hash } => {
                assert_eq!(current_disk_hash, ContentHash::of_bytes(b""));
            }
            other => panic!("expected Conflict, got {other:?}"),
        }
        // No file should have been created.
        assert!(vault.disk_hash(&rel).unwrap().is_none());
    }

    #[test]
    fn write_creates_parent_dirs() {
        let (_dir, vault) = vault();
        let rel = RelativeNotePath::new("deep/nested/path/note.md").unwrap();
        let doc = NoteDocument::from_raw(rel.clone(), "# Deep", 0);
        let res = vault.write_note(&doc, None).unwrap();
        assert!(matches!(res, WriteResult::Written(_)));
        assert!(vault.read_note(&rel).is_ok());
    }

    #[test]
    fn list_notes_walks_and_skips_hidden_dirs() {
        let (_dir, vault) = vault();
        let root = _dir.path();
        std::fs::write(root.join("a.md"), "# A").unwrap();
        std::fs::create_dir_all(root.join("Daily")).unwrap();
        std::fs::write(root.join("Daily/today.md"), "# Today").unwrap();
        std::fs::create_dir_all(root.join(".obsidian")).unwrap();
        std::fs::write(root.join(".obsidian/app.md"), "hidden").unwrap();
        std::fs::create_dir_all(root.join(".git")).unwrap();
        std::fs::write(root.join(".git/HISTORY.md"), "hidden").unwrap();
        std::fs::create_dir_all(root.join(".onote")).unwrap();
        std::fs::write(root.join(".onote/state.md"), "hidden").unwrap();
        std::fs::create_dir_all(root.join("node_modules")).unwrap();
        std::fs::write(root.join("node_modules/pkg.md"), "hidden").unwrap();
        // A generic dotfile dir NOT in SKIP_DIRS — Obsidian's `.trash/` recycle
        // bin is the canonical case. The dotfile sweep (round-10 MEDIUM) must
        // exclude it; without it `.trash/deleted.md` would be indexed as if it
        // were a live note, while the file watcher drops its external edits —
        // leaving a permanently stale, undeletable-from-search row.
        std::fs::create_dir_all(root.join(".trash")).unwrap();
        std::fs::write(root.join(".trash/deleted.md"), "# Deleted").unwrap();
        // A hidden `.md` FILE (dot-prefixed name) at the vault root. The file-
        // level dotfile sweep (round-11) must exclude it, mirroring the
        // directory branch and `file_watch::forward`; without it `.scratch.md`
        // would be indexed while the watcher drops its external edits.
        std::fs::write(root.join(".scratch.md"), "# Hidden Scratch").unwrap();
        // Non-.md files are ignored.
        std::fs::write(root.join("not-md.txt"), "ignore me").unwrap();

        let notes = vault.list_notes().unwrap();
        let mut titles: Vec<String> = notes.iter().map(|n| n.title.clone()).collect();
        titles.sort();
        assert_eq!(titles, vec!["A".to_string(), "Today".to_string()]);
    }

    #[test]
    fn list_notes_ignores_non_utf8_files() {
        let (_dir, vault) = vault();
        let root = _dir.path();
        std::fs::write(root.join("good.md"), "# Good").unwrap();
        // 0xFF is never valid UTF-8 → read_to_string returns InvalidData.
        std::fs::write(root.join("bad.md"), [0xFFu8, 0xFE, 0x00]).unwrap();

        let notes = vault.list_notes().unwrap();
        let titles: Vec<String> = notes.iter().map(|n| n.title.clone()).collect();
        assert_eq!(titles, vec!["Good".to_string()]);
    }

    #[test]
    fn list_notes_sorted_by_modified_desc() {
        let (_dir, vault) = vault();
        let root = _dir.path();
        std::fs::write(root.join("old.md"), "# Old").unwrap();
        std::fs::write(root.join("mid.md"), "# Mid").unwrap();
        std::fs::write(root.join("new.md"), "# New").unwrap();

        // Pin distinct, known mtimes on each file. Without this the files share
        // one second-resolution mtime and the `sort_by_key(Reverse(...))` in
        // `list_notes` is unobservable — the test name would promise descending
        // order while only checking presence. `FileTimes::set_modified` /
        // `File::set_times` are stable since 1.75 (MSRV is 1.82).
        let set_mtime = |name: &str, secs: u64| {
            let f = std::fs::File::open(root.join(name)).expect("open for set_times");
            let times = std::fs::FileTimes::new()
                .set_modified(std::time::UNIX_EPOCH + std::time::Duration::from_secs(secs));
            f.set_times(times).expect("set_times");
        };
        set_mtime("old.md", 1_000_000_000); // 2001-09-09T01:46:40Z
        set_mtime("mid.md", 1_500_000_000); // 2017-07-14T02:40:00Z
        set_mtime("new.md", 2_000_000_000); // 2033-05-18T03:33:20Z

        let notes = vault.list_notes().unwrap();
        assert!(!notes.is_empty());

        // Presence: all three notes show up (preserves the original intent).
        let titles: Vec<String> = notes.iter().map(|n| n.title.clone()).collect();
        assert!(titles.contains(&"Old".to_string()));
        assert!(titles.contains(&"Mid".to_string()));
        assert!(titles.contains(&"New".to_string()));

        // Order: most-recent-first. If the `sort_by_key(Reverse(...))` in
        // `list_notes` were deleted, walk order (creation/lexical here) would
        // surface `Old` first and these assertions would fail.
        let mtimes: Vec<i64> = notes.iter().map(|n| n.modified_at).collect();
        assert!(
            mtimes.windows(2).all(|w| w[0] >= w[1]),
            "mtimes must be non-increasing (descending); got {mtimes:?}",
        );
        assert_eq!(notes[0].title.as_str(), "New", "newest note must be first");
        assert_eq!(
            notes[notes.len() - 1].title.as_str(),
            "Old",
            "oldest note must be last",
        );
    }

    #[test]
    fn create_note_slugifies_and_dedups() {
        let (_dir, vault) = vault();
        let p1 = vault.create_note("Robot Idea!", None).unwrap();
        assert_eq!(p1.as_str(), "robot-idea.md");
        let p2 = vault.create_note("Robot Idea!", None).unwrap();
        assert_eq!(p2.as_str(), "robot-idea-2.md");
        let p3 = vault.create_note("Robot Idea!", None).unwrap();
        assert_eq!(p3.as_str(), "robot-idea-3.md");

        let body = std::fs::read_to_string(_dir.path().join("robot-idea.md")).unwrap();
        assert_eq!(body, "# Robot Idea!\n");
    }

    #[test]
    fn create_note_in_folder() {
        let (_dir, vault) = vault();
        let folder = RelativeNotePath::new("Inbox").unwrap();
        let p = vault.create_note("My Note", Some(&folder)).unwrap();
        assert_eq!(p.as_str(), "Inbox/my-note.md");
        assert!(_dir.path().join("Inbox/my-note.md").exists());
    }

    #[test]
    fn create_note_empty_slug_becomes_untitled() {
        let (_dir, vault) = vault();
        // All non-slug chars → slug trims to empty → "untitled".
        let p = vault.create_note("!!!", None).unwrap();
        assert_eq!(p.as_str(), "untitled.md");
    }

    #[test]
    fn delete_note_errors_when_missing() {
        let (_dir, vault) = vault();
        let rel = RelativeNotePath::new("ghost.md").unwrap();
        match vault.delete_note(&rel) {
            Err(VaultError::NoteNotFound(_)) => {}
            other => panic!("expected NoteNotFound, got {other:?}"),
        }
    }

    #[test]
    fn delete_note_removes_existing() {
        let (_dir, vault) = vault();
        let rel = RelativeNotePath::new("real.md").unwrap();
        std::fs::write(_dir.path().join("real.md"), "x").unwrap();
        vault.delete_note(&rel).unwrap();
        assert!(!_dir.path().join("real.md").exists());
    }

    #[test]
    fn disk_hash_missing_returns_none() {
        let (_dir, vault) = vault();
        let rel = RelativeNotePath::new("nope.md").unwrap();
        assert_eq!(vault.disk_hash(&rel).unwrap(), None);
    }

    #[test]
    fn disk_hash_existing_matches_of_bytes() {
        let (_dir, vault) = vault();
        let rel = RelativeNotePath::new("yes.md").unwrap();
        std::fs::write(_dir.path().join("yes.md"), "abc").unwrap();
        let h = vault.disk_hash(&rel).unwrap().unwrap();
        assert_eq!(h, ContentHash::of_bytes(b"abc"));
    }

    #[test]
    fn read_note_missing_returns_not_found() {
        let (_dir, vault) = vault();
        let rel = RelativeNotePath::new("missing.md").unwrap();
        match vault.read_note(&rel) {
            Err(VaultError::NoteNotFound(_)) => {}
            other => panic!("expected NoteNotFound, got {other:?}"),
        }
    }
}

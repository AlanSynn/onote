//! `BackupService` adapter that shells out to system `git` (`CLAUDE.md` §2.7
//! MVP). Stays out of note bytes; only stages/commits/pushes/pulls. `git2` is a
//! later, optional backend (Open/Closed via the `BackupService` port).

use crate::domain::backup::*;
use crate::domain::errors::BackupError;
use crate::ports::BackupService;
use std::path::PathBuf;

/// Concrete `BackupService` backed by the system `git` CLI.
///
/// `remote` is the conventional remote name (e.g. `"origin"`); callers pick the
/// default before construction.
pub struct GitCliBackup {
    root: PathBuf,
    remote: String,
}

impl GitCliBackup {
    pub fn new(root: PathBuf, remote: String) -> Self {
        Self { root, remote }
    }

    /// Run `git -C <root> <args..>`, returning trimmed stdout on success.
    ///
    /// Non-zero exit is mapped to `BackupError::Git(stderr)` except the
    /// "not a git repository" / missing-path cases which map to `NotARepo`.
    fn run(&self, args: &[&str]) -> Result<String, BackupError> {
        let out = std::process::Command::new("git")
            .arg("-C")
            .arg(&self.root)
            .args(args)
            .output()
            .map_err(|e| BackupError::Git(format!("spawn git: {e}")))?;
        if !out.status.success() {
            let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
            let lower = stderr.to_lowercase();
            if lower.contains("not a git repository") || lower.contains("no such file") {
                return Err(BackupError::NotARepo);
            }
            return Err(BackupError::Git(stderr));
        }
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
    }
}

impl BackupService for GitCliBackup {
    fn status(&self) -> Result<BackupState, BackupError> {
        let mut state = BackupState::default();

        // Are we inside a work tree at all?
        match self.run(&["rev-parse", "--is-inside-work-tree"]) {
            Ok(_) => {}
            Err(BackupError::NotARepo) => {
                state.status = GitStatus::NoRepo;
                return Ok(state);
            }
            Err(_) => {
                state.status = GitStatus::Error;
                return Ok(state);
            }
        }

        // Current branch name (HEAD may be detached → None).
        state.branch = self
            .run(&["rev-parse", "--abbrev-ref", "HEAD"])
            .ok()
            .filter(|s| !s.is_empty() && s != "HEAD");

        // Dirty file count via porcelain status.
        let porcelain = self.run(&["status", "--porcelain"]).unwrap_or_default();
        let dirty = porcelain.lines().filter(|l| !l.trim().is_empty()).count() as u32;
        state.dirty_files = dirty;
        state.status = if dirty == 0 {
            GitStatus::Clean
        } else {
            GitStatus::Dirty
        };

        // Ahead/behind vs upstream. With no upstream configured, git errors and
        // we leave both counters at 0.
        let upstream_arg = format!("{}/HEAD", self.remote);
        if let Ok(s) = self.run(&["rev-list", "--left-right", "--count", &upstream_arg]) {
            let parts: Vec<&str> = s.split_whitespace().collect();
            if parts.len() == 2 {
                if let (Ok(a), Ok(b)) = (parts[0].parse::<u32>(), parts[1].parse::<u32>()) {
                    state.ahead = a;
                    state.behind = b;
                }
            }
        }

        Ok(state)
    }

    fn commit(&self, message: BackupMessage) -> Result<BackupReport, BackupError> {
        // §6.1 / §2.7: `.onote/` holds derived SQLite cache (`index.sqlite`,
        // `-wal`, `-shm`), not user notes — generated metadata is excluded
        // from backup BY DEFAULT. Staging it would (a) churn cache state into
        // git history and (b) leave `git status` dirty after WAL/SHM files
        // are deleted on connection close. Use a non-mutating pathspec
        // exclude (`:(exclude)` magic) so no `.gitignore` is written into the
        // user's vault. With `-A` plus a sole exclude pathspec, git implicitly
        // starts from "all files" then removes `.onote/` matches.
        self.run(&["add", "-A", "--", ":(exclude).onote"])?;

        // Detect "nothing staged" via EXIT CODE (locale- and version-
        // independent) rather than parsing git's human-facing stdout. The prior
        // pattern-match (`"nothing to commit"` / `"no changes added"` /
        // `"no changes to commit"`) missed the canonical no-op case: when
        // `.onote/` is the only untracked artifact, git prints
        // "nothing added to commit but untracked files present" — matching NONE
        // of the patterns — so `commit()` fell through to a confusing raw-git
        // error on the exact "ran `onote backup`, nothing changed" workflow.
        // `git diff --cached --quiet` exits 0 when the index matches HEAD
        // (nothing to commit) and 1 when staged differences exist, regardless
        // of locale, git version, or leftover untracked files.
        let staged = std::process::Command::new("git")
            .arg("-C")
            .arg(&self.root)
            .args(["diff", "--cached", "--quiet"])
            .status()
            .map_err(|e| BackupError::Git(format!("spawn git diff: {e}")))?;
        if staged.success() {
            // Index == HEAD: clean tree (modulo untracked `.onote/`). Report
            // committed=false so the caller prints "nothing to commit", not an
            // error (round-10 MAJOR; the canonical backup no-op must succeed).
            return Ok(BackupReport {
                committed: false,
                message: String::new(),
                ..BackupReport::default()
            });
        }

        // Staged differences exist — commit them, capturing both streams so the
        // report message isn't empty regardless of which stream git used.
        let out = std::process::Command::new("git")
            .arg("-C")
            .arg(&self.root)
            .args(["commit", "-m", message.as_str()])
            .output()
            .map_err(|e| BackupError::Git(format!("spawn git: {e}")))?;
        let stdout = String::from_utf8_lossy(&out.stdout).trim().to_string();
        let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
        let message_out = if stdout.is_empty() {
            stderr.clone()
        } else {
            stdout.clone()
        };

        if out.status.success() {
            return Ok(BackupReport {
                committed: true,
                message: message_out,
                ..BackupReport::default()
            });
        }

        // Non-zero exit after a confirmed-staged tree is unusual; keep the
        // "nothing to commit" pattern match as a defensive fallback (diff and
        // commit could disagree across git edge cases), then surface stderr.
        let combined = format!("{stdout}\n{stderr}").to_lowercase();
        if combined.contains("nothing to commit")
            || combined.contains("no changes added")
            || combined.contains("no changes to commit")
            || combined.contains("nothing added to commit")
        {
            return Ok(BackupReport {
                committed: false,
                message: message_out,
                ..BackupReport::default()
            });
        }
        Err(BackupError::Git(if stderr.is_empty() {
            stdout
        } else {
            stderr
        }))
    }

    fn push(&self) -> Result<BackupReport, BackupError> {
        let mut report = BackupReport::default();
        match self.run(&["push", &self.remote]) {
            Ok(stdout) => {
                report.pushed = true;
                report.message = stdout;
            }
            Err(BackupError::Git(stderr)) => {
                let lower = stderr.to_lowercase();
                if lower.contains("non-fast-forward") {
                    report.pushed = false;
                    report.message = stderr;
                } else {
                    return Err(BackupError::Git(stderr));
                }
            }
            Err(e) => return Err(e),
        }
        Ok(report)
    }

    fn pull_ff_only(&self) -> Result<BackupReport, BackupError> {
        let mut report = BackupReport::default();
        match self.run(&["pull", "--ff-only", &self.remote]) {
            Ok(stdout) => {
                report.pulled = true;
                for line in stdout.lines() {
                    if line.contains("CONFLICT") {
                        report.conflicts.push(line.to_string());
                    }
                }
                report.message = stdout;
            }
            Err(e) => return Err(e),
        }
        Ok(report)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::process::Command;
    use tempfile::TempDir;

    fn git_init(dir: &std::path::Path) {
        Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(["init"])
            .status()
            .expect("git init");
        Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(["config", "user.email", "test@example.com"])
            .status()
            .expect("git config user.email");
        Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(["config", "user.name", "onote-test"])
            .status()
            .expect("git config user.name");
    }

    fn git(dir: &std::path::Path, args: &[&str]) {
        let status = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .status()
            .expect("run git");
        assert!(status.success(), "git {:?} failed", args);
    }

    #[test]
    fn status_clean_then_dirty_then_commit() {
        let tmp = TempDir::new().expect("tempdir");
        let root = tmp.path().to_path_buf();
        git_init(&root);

        // Seed an initial commit so HEAD exists and the tree is clean.
        fs::write(root.join("Scratch.md"), "# scratch\n").expect("write seed");
        git(&root, &["add", "-A"]);
        git(&root, &["commit", "-m", "seed"]);

        let backup = GitCliBackup::new(root.clone(), "origin".to_string());

        let clean = backup.status().expect("status clean");
        assert_eq!(clean.status, GitStatus::Clean);
        assert_eq!(clean.dirty_files, 0);
        assert!(clean.branch.is_some(), "branch should be detected");
        // No upstream configured → counters stay at 0.
        assert_eq!(clean.ahead, 0);
        assert_eq!(clean.behind, 0);

        // Add a new untracked file → dirty.
        fs::write(root.join("Inbox.md"), "# inbox\n").expect("write inbox");
        let dirty = backup.status().expect("status dirty");
        assert_eq!(dirty.status, GitStatus::Dirty);
        assert!(
            dirty.dirty_files >= 1,
            "expected at least one dirty file, got {}",
            dirty.dirty_files
        );

        // Commit through the adapter.
        let report = backup
            .commit(BackupMessage("onote backup: test".to_string()))
            .expect("commit");
        assert!(report.committed, "commit should report committed=true");

        // Tree is clean again; a follow-up no-op commit reports committed=false.
        let after = backup.status().expect("status after");
        assert_eq!(after.status, GitStatus::Clean);
        assert_eq!(after.dirty_files, 0);

        let noop = backup
            .commit(BackupMessage("onote backup: empty".to_string()))
            .expect("commit noop");
        assert!(
            !noop.committed,
            "empty commit should report committed=false"
        );
    }

    #[test]
    fn commit_excludes_onote_cache_from_staging() {
        // CLAUDE.md §6.1 ("SQLite is cache/index") + §2.7 ("Backup can commit
        // generated metadata only if configured" — i.e. generated metadata is
        // excluded BY DEFAULT). `commit()` must NOT stage the `.onote/` SQLite
        // cache; doing so would (a) churn cache state into git history and
        // (b) leave `git status` dirty after WAL/SHM files are deleted on
        // connection close. The fix is a non-mutating `:(exclude).onote`
        // pathspec on the `git add -A`, so no `.gitignore` is written into
        // the user's vault.
        let tmp = TempDir::new().expect("tempdir");
        let root = tmp.path().to_path_buf();
        git_init(&root);

        // Seed an initial commit so HEAD exists and the tree is clean.
        fs::write(root.join("Scratch.md"), "# scratch\n").expect("write seed");
        git(&root, &["add", "-A"]);
        git(&root, &["commit", "-m", "seed"]);

        // A new note AND a `.onote/` cache file appear in the working tree.
        fs::create_dir_all(root.join(".onote")).expect("mkdir .onote");
        fs::write(root.join(".onote/index.sqlite"), b"sqlite-bytes").expect("write cache");
        fs::create_dir_all(root.join("Notes")).expect("mkdir Notes");
        fs::write(root.join("Notes/idea.md"), "# idea\n").expect("write note");

        let backup = GitCliBackup::new(root.clone(), "origin".to_string());
        let report = backup
            .commit(BackupMessage("onote backup: exclude cache".to_string()))
            .expect("commit");
        assert!(report.committed, "commit should report committed=true");

        // Enumerate the set of files tracked at HEAD after the backup commit.
        let out = Command::new("git")
            .arg("-C")
            .arg(&root)
            .args(["ls-tree", "-r", "--name-only", "HEAD"])
            .output()
            .expect("git ls-tree");
        assert!(out.status.success(), "git ls-tree failed");
        let tracked = String::from_utf8_lossy(&out.stdout);
        let tracked_lines: Vec<&str> = tracked.lines().collect();

        // The `.onote/` cache must NOT be committed (§6.1 — derived cache).
        assert!(
            !tracked_lines.iter().any(|l| l.starts_with(".onote/")),
            ".onote/ must not be staged by commit(); tracked: {tracked_lines:?}",
        );
        // The user note must still be committed.
        assert!(
            tracked_lines.contains(&"Notes/idea.md"),
            "Notes/idea.md must be staged by commit(); tracked: {tracked_lines:?}",
        );
    }

    /// Round-10 MAJOR regression guard: the canonical "ran `onote backup`,
    /// nothing changed" no-op must succeed, not error. When `.onote/` is the
    /// ONLY untracked artifact (the normal steady state between edits — every
    /// session materializes the SQLite cache), `git commit` prints
    /// "nothing added to commit but untracked files present", which the prior
    /// pattern-match (`"nothing to commit"` / `"no changes added"` /
    /// `"no changes to commit"`) matched NONE of — so `commit()` fell through
    /// to `Err(BackupError::Git(...))` and `onote backup` surfaced a confusing
    /// raw-git error instead of "nothing to commit (working tree clean)". The
    /// fix detects an empty stage via `git diff --cached --quiet` (exit code,
    /// locale-independent) before attempting the commit.
    #[test]
    fn commit_noop_with_only_untracked_onote_cache_reports_committed_false() {
        let tmp = TempDir::new().expect("tempdir");
        let root = tmp.path().to_path_buf();
        git_init(&root);

        // Seed an initial commit so HEAD exists and the tree is clean.
        fs::write(root.join("Scratch.md"), "# scratch\n").expect("write seed");
        git(&root, &["add", "-A"]);
        git(&root, &["commit", "-m", "seed"]);

        // Materialize the derived cache exactly as a real session does, leaving
        // the tree otherwise clean. `.onote/` is untracked (excluded from
        // staging by the pathspec), so `git status` is non-empty but there is
        // NOTHING to commit.
        fs::create_dir_all(root.join(".onote")).expect("mkdir .onote");
        fs::write(root.join(".onote/index.sqlite"), b"sqlite-bytes").expect("write cache");

        let backup = GitCliBackup::new(root.clone(), "origin".to_string());
        let report = backup
            .commit(BackupMessage("onote backup: empty".to_string()))
            .expect("clean-tree no-op with untracked .onote/ must NOT error");
        assert!(
            !report.committed,
            "clean tree (only untracked .onote/) must report committed=false"
        );
    }

    #[test]
    fn status_not_a_repo_reports_no_repo() {
        let tmp = TempDir::new().expect("tempdir");
        let backup = GitCliBackup::new(tmp.path().to_path_buf(), "origin".to_string());
        let state = backup
            .status()
            .expect("status should not error on non-repo");
        assert_eq!(state.status, GitStatus::NoRepo);
        assert_eq!(state.dirty_files, 0);
        assert_eq!(state.branch, None);
    }
}

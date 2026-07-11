//! Resolves where the derived note index (`index.sqlite`) lives
//! (`CLAUDE.md` §6.1 — SQLite is derived cache, fully rebuildable from files).
//!
//! Preference order:
//!   1. `vault/.onote/index.sqlite` — co-located with the vault (writable vault)
//!   2. `$XDG_CACHE_HOME/onote/<vault-dir>-<hash>/index.sqlite` — fallback when
//!      the vault is read-only but the user's cache dir is writable
//!   3. indexless — neither is writable; the app runs with search disabled
//!
//! The fallback keeps a read-only vault (a checked-out docs tree, a mounted
//! share, an Obsidian vault on a read-only volume) fully searchable instead of
//! aborting at startup with an opaque permission error. Indexless mode is
//! honest degradation: reading notes still works; fuzzy open + full-text search
//! are disabled with a clear message (no fake results).
//!
//! `CLAUDE.md` §1.2 — Obsidian-compatible, not Obsidian-dependent: a plain
//! writable `.md` folder takes path 1 like any vault; no `.obsidian/` is ever
//! required.

use std::path::{Path, PathBuf};

use crate::config::Config;

/// Where the derived index will be opened from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IndexLocation {
    /// `vault/.onote/index.sqlite` — the vault dir is writable.
    Vault(PathBuf),
    /// A cache dir fallback — the vault is read-only but the cache is writable.
    Cache(PathBuf),
    /// Neither writable: run without an index (search disabled, reads work).
    Indexless,
}

impl IndexLocation {
    /// The DB path when there IS one (`None` for indexless).
    pub fn db_path(&self) -> Option<&Path> {
        match self {
            IndexLocation::Vault(p) | IndexLocation::Cache(p) => Some(p),
            IndexLocation::Indexless => None,
        }
    }

    /// True when no index could be opened (search disabled).
    pub fn is_indexless(&self) -> bool {
        matches!(self, IndexLocation::Indexless)
    }

    /// True when the index lives in the cache fallback (not the vault).
    pub fn is_cache(&self) -> bool {
        matches!(self, IndexLocation::Cache(_))
    }
}

/// Resolve the index location for `config`'s vault per the preference order.
///
/// The vault root must already exist (callers ensure it via `ensure_vault`).
/// Probes each candidate by attempting to create its parent dir + a probe write,
/// so a read-only vault reliably falls through to the cache, then indexless.
pub fn resolve_index_location(config: &Config) -> IndexLocation {
    let vault_db = config.vault.join(".onote").join("index.sqlite");
    let cache_db = cache_index_path(&config.vault);
    resolve_at(&vault_db, cache_db.as_deref())
}

/// Pure resolver over concrete paths — extracted from
/// [`resolve_index_location`] so it can be
/// exercised without touching the real user cache dir. The vault candidate is
/// tried first; on any write failure, the optional cache candidate; else
/// indexless.
fn resolve_at(vault_db: &Path, cache_db: Option<&Path>) -> IndexLocation {
    if is_db_writable(vault_db) {
        return IndexLocation::Vault(vault_db.to_path_buf());
    }
    if let Some(cache) = cache_db {
        if is_db_writable(cache) {
            return IndexLocation::Cache(cache.to_path_buf());
        }
    }
    IndexLocation::Indexless
}

/// Probe whether a SQLite DB at `db_path` can be opened for writing.
///
/// `create_dir_all` succeeding on an existing read-only dir is NOT sufficient
/// proof (it no-ops), so an authoritative probe write is performed: create the
/// parent dir, write+remove a uniquely-named probe file. A concurrent onote
/// instance would at worst race on the probe file, which is harmless (the
/// `SqliteNoteIndex` open still owns actual DB locking).
fn is_db_writable(db_path: &Path) -> bool {
    let Some(parent) = db_path.parent() else {
        return false;
    };
    if std::fs::create_dir_all(parent).is_err() {
        return false;
    }
    // Unique probe name so two near-simultaneous onote starts (e.g. a TUI + a
    // `onote backup`) don't remove each other's probe file mid-check.
    let probe = parent.join(format!(".onote-write-probe-{}", std::process::id()));
    let wrote = std::fs::write(&probe, b"").is_ok();
    let removed = std::fs::remove_file(&probe).is_ok();
    wrote && removed
}

/// The cache-fallback DB path for a vault, or `None` if the user's cache base
/// dir can't be resolved. Includes a short path hash so two different read-only
/// vaults keep separate indices (no cross-contaminated search results); the
/// vault's dir name prefix keeps it human-identifiable under the cache root.
fn cache_index_path(vault_root: &Path) -> Option<PathBuf> {
    let proj = directories::ProjectDirs::from("", "", "onote")?;
    let cache_dir = proj.cache_dir();
    let name = vault_root
        .file_name()
        .and_then(|n| n.to_str())
        .filter(|s| !s.is_empty())
        .unwrap_or("vault");
    let short_hash = &vault_hash(vault_root)[..8];
    Some(
        cache_dir
            .join(format!("{name}-{short_hash}"))
            .join("index.sqlite"),
    )
}

/// Stable, non-crypto hash of the absolute vault path — disambiguates two
/// read-only vaults that happen to share a dir name.
fn vault_hash(vault_root: &Path) -> String {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut h = DefaultHasher::new();
    vault_root.hash(&mut h);
    format!("{:016x}", h.finish())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Writable vault candidate → `Vault`. No cache candidate is needed.
    #[test]
    fn writable_vault_takes_precedence() {
        let dir = tempfile::tempdir().expect("tempdir");
        let vault_db = dir.path().join(".onote").join("index.sqlite");
        assert_eq!(
            resolve_at(&vault_db, None),
            IndexLocation::Vault(vault_db.clone()),
        );
    }

    /// Read-only vault + writable cache → `Cache`.
    ///
    /// `chmod 0555` on the vault root makes `vault/.onote/` un-creatable, so the
    /// probe fails and the resolver falls through to the cache candidate.
    #[cfg(unix)]
    #[test]
    fn readonly_vault_falls_back_to_cache() {
        use std::os::unix::fs::PermissionsExt;
        let vault = tempfile::tempdir().expect("tempdir");
        let cache = tempfile::tempdir().expect("tempdir");

        // Lock the vault root: no writes (including subdir creation) under it.
        let perms = std::fs::metadata(vault.path())
            .expect("vault metadata")
            .permissions();
        std::fs::set_permissions(vault.path(), std::fs::Permissions::from_mode(0o555))
            .expect("chmod vault read-only");

        let vault_db = vault.path().join(".onote").join("index.sqlite");
        let cache_db = cache.path().join("index.sqlite");
        let got = resolve_at(&vault_db, Some(&cache_db));

        // Restore perms so tempdir cleanup (rm -rf) succeeds on test teardown.
        std::fs::set_permissions(vault.path(), perms).expect("restore vault perms");

        assert_eq!(got, IndexLocation::Cache(cache_db));
    }

    /// Neither writable → `Indexless`. Honest degradation, not a crash.
    #[cfg(unix)]
    #[test]
    fn readonly_vault_and_cache_yields_indexless() {
        use std::os::unix::fs::PermissionsExt;
        let vault = tempfile::tempdir().expect("tempdir");
        let cache = tempfile::tempdir().expect("tempdir");

        let vault_perms = std::fs::metadata(vault.path())
            .expect("vault metadata")
            .permissions();
        let cache_perms = std::fs::metadata(cache.path())
            .expect("cache metadata")
            .permissions();
        std::fs::set_permissions(vault.path(), std::fs::Permissions::from_mode(0o555))
            .expect("chmod vault");
        std::fs::set_permissions(cache.path(), std::fs::Permissions::from_mode(0o555))
            .expect("chmod cache");

        let vault_db = vault.path().join(".onote").join("index.sqlite");
        let cache_db = cache.path().join("index.sqlite");
        let got = resolve_at(&vault_db, Some(&cache_db));

        std::fs::set_permissions(vault.path(), vault_perms).expect("restore vault perms");
        std::fs::set_permissions(cache.path(), cache_perms).expect("restore cache perms");

        assert_eq!(got, IndexLocation::Indexless);
    }

    /// `cache_index_path` is stable (same vault → same path) and unique per
    /// distinct vault (different names → different paths). Guards against two
    /// read-only vaults silently sharing one cache index.
    #[test]
    fn cache_path_stable_and_distinct() {
        let a = Path::new("/tmp/vaultA");
        let b = Path::new("/tmp/vaultB");
        let pa = cache_index_path(a).expect("cache path for a");
        let pb = cache_index_path(b).expect("cache path for b");
        assert_eq!(pa, cache_index_path(a).unwrap(), "stable for same vault");
        assert_ne!(pa, pb, "distinct for different vaults");
        assert!(pa.ends_with("index.sqlite"));
    }
}

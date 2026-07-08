//! Logging setup (`CLAUDE.md` §2.11).
//!
//! Initializes `tracing` with both a stderr layer and a daily-rotating file
//! appender under `$ONOTE_LOG_DIR` (validated) or `<home>/.local/state/onote/`.
//! Returns a non-blocking writer guard that must outlive the program so buffered
//! file writes flush on exit. File-logging setup failures are non-fatal: we fall
//! back to stderr-only and emit a notice.

use std::io;
use std::path::{Component, Path, PathBuf};

use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt, EnvFilter, Registry};

/// Initialize tracing with both stderr and a daily-rotating file appender at
/// `$ONOTE_LOG_DIR` or `<home>/.local/state/onote/onote.log` (`CLAUDE.md` §2.11).
///
/// Returns a non-blocking writer guard that must outlive the program so buffered
/// file writes flush on exit. File-logging setup failures are non-fatal: we fall
/// back to stderr-only and emit a notice.
pub fn init_logging() -> Option<tracing_appender::non_blocking::WorkerGuard> {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("warn"));

    // Shared env+home resolution (`log_dir`); kept DRY so `onote log` and
    // `init_logging` can never drift on where logs live.
    let resolved = log_dir();

    match resolved {
        Ok(dir) => {
            let file_appender = tracing_appender::rolling::daily(&dir, "onote.log");
            let (file_writer, guard) = tracing_appender::non_blocking(file_appender);

            // Two `fmt` layers sharing the same `EnvFilter` subscriber-level
            // filter: one to stderr, one to the rolling file. Both receive the
            // same filtered events.
            let stderr_layer = tracing_subscriber::fmt::layer().with_writer(io::stderr);
            let file_layer = tracing_subscriber::fmt::layer().with_writer(file_writer);

            if let Err(e) = Registry::default()
                .with(filter)
                .with(stderr_layer)
                .with(file_layer)
                .try_init()
            {
                eprintln!("(onote: tracing subscriber already installed; logging disabled: {e})");
            }

            Some(guard)
        }
        Err(e) => {
            eprintln!("(onote: log dir unavailable, falling back to stderr-only: {e})");
            if let Err(e) = tracing_subscriber::fmt()
                .with_env_filter(filter)
                .with_writer(io::stderr)
                .try_init()
            {
                eprintln!("(onote: tracing subscriber already installed; logging disabled: {e})");
            }
            None
        }
    }
}

/// Resolve (but do NOT initialize) the log directory onote writes to — honoring
/// `$ONOTE_LOG_DIR` (absolute, no `..`) else `<home>/.local/state/onote/`.
///
/// Pure w.r.t. the tracing subscriber: `onote log` calls this to print the path
/// without installing a subscriber, and `init_logging` shares it so the two can
/// never disagree on where logs live. Reads `ONOTE_LOG_DIR` + the home dir here
/// (the only global reads); delegates the traversal/absolute validation + dir
/// creation to [`resolve_log_dir`].
pub fn log_dir() -> io::Result<PathBuf> {
    match std::env::var("ONOTE_LOG_DIR").ok().as_deref() {
        Some(env) => resolve_log_dir(Some(env), Path::new("")),
        None => {
            let home = directories::BaseDirs::new()
                .map(|b| b.home_dir().to_path_buf())
                .ok_or_else(|| {
                    io::Error::new(io::ErrorKind::NotFound, "could not resolve home directory")
                })?;
            resolve_log_dir(None, &home)
        }
    }
}

/// Resolve the log directory: an `$ONOTE_LOG_DIR` override (must be absolute and
/// must not contain `..`), else `<home>/.local/state/onote/`. Creates the
/// directory tree if missing.
///
/// Pure (no `std::env`/global reads) so it can be unit-tested without env-var
/// races; `init_logging` feeds it the env value + resolved home. When
/// `log_dir_env` is `Some`, `home` is ignored.
///
/// Rejecting non-absolute or `..`-bearing overrides neutralizes the
/// path-traversal / symlink-via-env risk on the env override path.
pub fn resolve_log_dir(log_dir_env: Option<&str>, home: &Path) -> io::Result<PathBuf> {
    if let Some(dir) = log_dir_env {
        if dir.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "ONOTE_LOG_DIR must be an absolute path without '..'",
            ));
        }
        let p = PathBuf::from(dir);
        if !p.is_absolute() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "ONOTE_LOG_DIR must be an absolute path without '..'",
            ));
        }
        if p.components().any(|c| matches!(c, Component::ParentDir)) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "ONOTE_LOG_DIR must be an absolute path without '..'",
            ));
        }
        std::fs::create_dir_all(&p)?;
        return Ok(p);
    }
    let dir = home.join(".local").join("state").join("onote");
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_log_dir_env_override_absolute() {
        let tmp = tempfile::tempdir().unwrap();
        let env = tmp.path().to_str().unwrap();
        // `home` is ignored when the env override is set.
        let dir = resolve_log_dir(Some(env), Path::new("/unused/home"))
            .expect("absolute env override should resolve");
        assert!(dir.is_absolute());
        assert!(dir.exists());
        assert_eq!(dir, tmp.path());
    }

    #[test]
    fn resolve_log_dir_env_override_relative_rejected() {
        let home = tempfile::tempdir().unwrap();
        let err = resolve_log_dir(Some("relative/logs"), home.path())
            .expect_err("relative ONOTE_LOG_DIR must be rejected");
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
    }

    #[test]
    fn resolve_log_dir_env_override_traversal_rejected() {
        let home = tempfile::tempdir().unwrap();
        let traversal = home.path().join("..").join("evil");
        let err = resolve_log_dir(Some(traversal.to_str().unwrap()), home.path())
            .expect_err("ONOTE_LOG_DIR with '..' must be rejected");
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
    }

    #[test]
    fn resolve_log_dir_default_under_home() {
        let home = tempfile::tempdir().unwrap();
        let dir =
            resolve_log_dir(None, home.path()).expect("default path should resolve under home");
        assert!(dir.starts_with(home.path()));
        assert!(dir.ends_with(".local/state/onote"));
        assert!(dir.exists());
    }
}

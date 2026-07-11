//! `onote` entry point — load config, wire adapters, dispatch CLI/TUI.

use std::io::{self, IsTerminal as _, Write as _};
use std::process::ExitCode;

use anyhow::{anyhow, Context, Result};
use clap::{CommandFactory as _, Parser};

use onote::application::ops::CopyFormat;
use onote::application::App;
use onote::cli::{Cli, Command, ImgCmd};
use onote::config::Config;
use onote::domain::vault::RelativeNotePath;
use onote::infra::{build_deps, logging, resolve_index_location};
use onote::ui::tui;

fn main() -> ExitCode {
    let _log_guard = logging::init_logging();

    match try_main() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("onote: {e:#}");
            ExitCode::FAILURE
        }
    }
}

fn try_main() -> Result<()> {
    let cli = Cli::parse();
    let command = cli.command_or_default();

    // CLI-only commands that need no vault/config/index — handle before the
    // heavy adapter wiring so `onote completions` / `onote log` work even with
    // no configured vault and don't pay `build_deps` / `reindex_all` cost.
    // Borrowed (`&command`) so `command` stays owned for the dispatch below.
    if let Command::Completions { shell } = &command {
        print_completions(*shell);
        return Ok(());
    }
    if let Command::Log = &command {
        print_log()?;
        return Ok(());
    }

    let config = Config::load().context("loading config")?;
    ensure_vault(&config)?;

    // §6.1: the index is derived cache. Resolve where it lives by preference —
    // `vault/.onote/` (writable vault), then a user-cache fallback (read-only
    // vault but writable home), then indexless. A read-only vault no longer
    // aborts startup: the cache path keeps full-text search working, and
    // indexless mode runs the app with search disabled + a clear message.
    let index_location = resolve_index_location(&config);
    if index_location.is_cache() {
        eprintln!(
            "onote: vault {} is read-only; using a cache-backed index (full-text search retained).",
            config.vault.display()
        );
    } else if index_location.is_indexless() {
        eprintln!(
            "onote: vault {} is read-only and no writable cache dir was found; \
             running without a local index (fuzzy open + full-text search disabled).",
            config.vault.display()
        );
    }
    let deps = build_deps(&config, &index_location)?;
    let app = App::new(config, deps);

    // Bootstrap the derived search index (§6 cache) from the source-of-truth
    // files so an existing vault's notes are queryable immediately — without
    // this, `open`/`gui`/Ctrl+O/FTS find nothing until each note is opened once.
    // Skip it for commands that never query the index (`backup`/`img`/`copy`/
    // `share`/`tags`) so they don't pay a full-vault walk+read proportional to
    // vault size (round-9; `share` joined the skip list in round-10, `tags` in
    // the tags-surface spike — both read note bodies directly via
    // `vault.read_note`, never touching the index). Also skip indexless mode:
    // a NullNoteIndex rebuild is a no-op, so a full-vault read+walk would be
    // pure waste. Non-fatal: a failed rebuild only degrades search, never
    // note editing.
    if !index_location.is_indexless()
        && !matches!(
            command,
            Command::Backup { .. }
                | Command::Img { .. }
                | Command::Copy { .. }
                | Command::Share
                | Command::Tags
        )
    {
        if let Err(e) = app.reindex_all() {
            tracing::warn!(error = %e, "startup reindex failed; search may be incomplete");
        }
    }

    match command {
        Command::Run | Command::Scratch => {
            let doc = app.open_default()?;
            tui::run(&app, doc)?;
        }
        Command::Today => {
            let doc = app.open_daily()?;
            tui::run(&app, doc)?;
        }
        Command::New { title } => {
            let path = app.create_note(&title, None)?;
            let doc = app.open_note(&path)?;
            tui::run(&app, doc)?;
        }
        Command::Open { query } => {
            // Disambiguate: a short query often matches many notes. Silently
            // opening the top fuzzy hit (or, for `gui`, launching Obsidian at it)
            // risks editing/opening the wrong note, so list the matches and ask
            // the user to refine instead (round-9).
            let path = resolve_disambiguated(&app, Some(&query))?;
            let doc = app.open_note(&path)?;
            tui::run(&app, doc)?;
        }
        Command::Share => share(&app)?,
        Command::Backup { push, pull } => backup(&app, push, pull)?,
        Command::Gui { query } => {
            let path = resolve_disambiguated(&app, query.as_deref())?;
            println!("opening {} in Obsidian…", path.as_str());
            app.open_in_gui(&path)?;
        }
        Command::Img { cmd } => match cmd {
            ImgCmd::Paste => match app.paste_image()? {
                Some(p) => println!("{}", p.token),
                None => return Err(anyhow!("no image on clipboard")),
            },
        },
        Command::Copy { md, html, rich } => {
            // `--md` / `--html` / `--rich` are mutually exclusive — passing two
            // silently picks one (first-wins), so reject it explicitly rather
            // than let a typo silently do the wrong thing.
            let set = [md, html, rich].iter().filter(|b| **b).count();
            if set > 1 {
                return Err(anyhow!("--md, --html, and --rich are mutually exclusive"));
            }
            // No flag (or `--md`) copies Markdown — the default per §8.
            let fmt = if html {
                CopyFormat::Html
            } else if rich {
                CopyFormat::Rich
            } else {
                CopyFormat::Markdown
            };
            // A fresh CLI invocation has no note open; copy the default note so
            // `onote copy` is usable standalone (mirrors `onote share`).
            if app.current_note().is_none() {
                let _ = app.open_default()?;
            }
            app.copy_note(fmt)?;
            println!("copied as {}", fmt.label());
        }
        Command::Tags => {
            let tags = app.all_tags()?;
            if tags.is_empty() {
                println!("(no tags found)");
            } else {
                for t in &tags {
                    println!("{:>4}  #{}", t.count, t.tag);
                }
            }
        }
        // Handled by the early-return short-circuit above; arm present only so
        // the match stays exhaustive (the compiler can't see the prior returns).
        Command::Completions { .. } | Command::Log => {}
    }
    Ok(())
}

/// Print a shell completion script to stdout. Idempotent and side-effect-free
/// beyond stdout; users redirect to install, e.g.
/// `onote completions zsh > "${fpath[1]}/_onote"`.
fn print_completions(shell: clap_complete::Shell) {
    let mut cmd = Cli::command();
    clap_complete::generate(shell, &mut cmd, "onote", &mut io::stdout());
}

/// Print the most recent onote log file to stdout (§2.11 diagnostics). The
/// path is reported on stderr so stdout stays clean for piping
/// (`onote log | grep ERROR`). Reuses `log_dir`, honoring `$ONOTE_LOG_DIR`.
/// Does NOT install a tracing subscriber — it only reads what prior runs wrote.
///
/// `tracing_appender::rolling::daily` writes `onote.log.YYYY-MM-DD`; the
/// lexically-greatest such name is the latest day's log. If no log file exists
/// yet (fresh install, nothing logged), prints the resolved directory + a hint
/// to stderr so the user still learns where logs will appear.
fn print_log() -> Result<()> {
    let dir = logging::log_dir().context("could not resolve log directory")?;

    // Pick the newest log file: dated `onote.log.YYYY-MM-DD` entries sort
    // chronologically under a lexical max; a bare `onote.log` (if a non-rolling
    // appender ever wrote one) is treated as older than any dated file.
    let newest = std::fs::read_dir(&dir)
        .context("reading log directory")?
        .filter_map(Result::ok)
        .map(|e| e.file_name())
        .filter(|n| {
            let s = n.to_string_lossy();
            s == "onote.log" || s.starts_with("onote.log.")
        })
        .max()
        .map(|name| dir.join(name));

    match newest {
        Some(path) => {
            eprintln!("onote log: {}", path.display());
            // Stream the file to stdout so `onote log | grep` / `| less` work.
            // A warn-level daily log is small; for a huge file the user pipes
            // through `tail`/`less` anyway.
            std::io::copy(
                &mut std::fs::File::open(&path)
                    .with_context(|| format!("opening {}", path.display()))?,
                &mut std::io::stdout(),
            )?;
            Ok(())
        }
        None => {
            eprintln!(
                "no log file yet. diagnostics will be written to: {}",
                dir.display()
            );
            Ok(())
        }
    }
}

/// Ensure the vault root exists, and `.onote/` if it can be created.
///
/// On first run (vault root absent), announce where the vault was created so
/// the user knows where their files live — a local-first, vault-as-source-of-
/// truth tool must not silently materialize `~/Notes/Vault/` with zero output.
///
/// The vault root itself is required (no root = no app), so failing to create
/// it is fatal. But `.onote/` only holds derived index cache (§6.1) — a
/// read-only vault can't host it, and that must NOT block startup. A missing
/// `.onote/` is benign: the index-location resolver falls back to a
/// cache-backed or indexless index instead.
fn ensure_vault(cfg: &Config) -> Result<()> {
    if !cfg.vault.exists() {
        std::fs::create_dir_all(&cfg.vault)
            .with_context(|| format!("creating vault at {}", cfg.vault.display()))?;
        eprintln!("onote: initialized vault at {}", cfg.vault.display());
    }
    if let Err(e) = std::fs::create_dir_all(cfg.vault.join(".onote")) {
        tracing::debug!(
            error = %e,
            "could not create vault/.onote (read-only vault?); index will use fallback or indexless mode"
        );
    }
    Ok(())
}

/// Resolve a query (or `None` → default note) to exactly one note path,
/// disambiguating fuzzy matches. A single match returns its path; zero matches
/// errors clearly; multiple matches print numbered candidates to stderr and
/// error so the user refines the query rather than silently landing on the
/// top-ranked note (round-9: `open`/`gui` previously opened rank-1 with no
/// confirmation, risking the wrong note — especially bad for `gui`, which
/// launches Obsidian).
fn resolve_disambiguated(app: &App, query: Option<&str>) -> Result<RelativeNotePath> {
    let Some(q) = query else {
        return Ok(app.config().vault_layout().default_note_relative()?);
    };
    let matches = app.fuzzy(q)?;
    match matches.len() {
        0 => Err(anyhow!("no note matched {q:?}")),
        // `next()` is provably `Some` here (len == 1), but `ok_or_else` + `?`
        // keeps this panic-free per CONTRIBUTING (no expect/panic outside tests):
        // a broken invariant surfaces as a clean error rather than a crash.
        1 => Ok(matches
            .into_iter()
            .next()
            .ok_or_else(|| anyhow!("internal: fuzzy match lost its sole element"))?
            .path),
        _ => {
            eprintln!("multiple notes match {q:?}:");
            let shown = matches.iter().take(20);
            for (i, m) in shown.enumerate() {
                eprintln!("  {:>2}. {}", i + 1, m.title);
            }
            if matches.len() > 20 {
                eprintln!("  … ({} more)", matches.len() - 20);
            }
            Err(anyhow!(
                "{} notes match {q:?}; refine the query",
                matches.len()
            ))
        }
    }
}

fn share(app: &App) -> Result<()> {
    // `onote share` from a fresh CLI has no note open; share the default note
    // so the command is usable standalone (matches `onote`/`onote scratch`).
    if app.current_note().is_none() {
        let _ = app.open_default()?;
    }
    let sess = app.share_current()?;
    println!("{}", sess.local_url);
    if let Some(lan) = &sess.lan_url {
        println!("{}", lan);
    }
    // §2.8 share mode: copy the local URL to the clipboard alongside the QR.
    if let Err(e) = app.copy_text(&sess.local_url) {
        eprintln!("(clipboard unavailable: {e})");
    }
    // Render the QR only on a real terminal: `qr2term` emits raw ANSI escapes
    // that would garble piped output (`URL=$(onote share)`). The URL + LAN URL
    // on stdout stay clean for scripts regardless.
    if std::io::stdout().is_terminal() {
        if let Err(e) = qr2term::print_qr(&sess.local_url) {
            eprintln!("(qr code unavailable: {e})");
        }
    }
    // Prompt on stderr so stdout (the URL/LAN URL) stays clean for scripting
    // (`URL=$(onote share)` sees only the URL, not the prompt). The server runs
    // until Enter on a TTY, or until stdin hits EOF (a closed pipe / `/dev/null`).
    eprintln!("sharing read-only. press Enter to stop…");
    let _ = io::stderr().flush();
    let mut buf = String::new();
    let _ = io::stdin().read_line(&mut buf);
    app.stop_share()?;
    Ok(())
}

fn backup(app: &App, push: bool, pull: bool) -> Result<()> {
    let vault_display = app.config().vault.display().to_string();
    if pull {
        let r = app.backup_pull().with_context(|| {
            format!("pulling at vault {vault_display} (is it a git repo? run `git init` there)")
        })?;
        println!("pulled ({} conflicts)", r.conflicts.len());
    }
    let r = app.backup_commit(None).with_context(|| {
        format!("committing at vault {vault_display} (is it a git repo? run `git init` there)")
    })?;
    // A clean tree maps to `committed: false` — print a clear "nothing to do"
    // message rather than the literal `committed: false`, which reads as failure
    // (round-9 H1).
    if r.committed {
        println!("committed");
    } else {
        println!("nothing to commit (working tree clean)");
    }
    if push {
        // Report the real outcome: `push` returns `pushed: false` on a
        // non-fast-forward (caught gracefully in `git_cli`) — don't claim
        // success when nothing was pushed.
        let r = app.backup_push()
            .with_context(|| format!("pushing at vault {vault_display} (configure a remote with `git remote add origin <url>`)"))?;
        if r.pushed {
            println!("pushed");
        } else {
            println!("not pushed (non-fast-forward; pull first)");
        }
    }
    Ok(())
}

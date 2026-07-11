//! The terminal runtime: the event loop that turns the pure Slice 8 [`App`] core
//! into a running TUI. A crossterm event thread and a refresh worker thread both
//! feed one `mpsc` channel of [`Msg`]; the UI thread `recv`s each message, calls
//! [`App::reduce`], executes the returned [`Effect`]s, and redraws via
//! [`crate::app::view::draw`]. Terminal setup/teardown installs a panic hook that
//! restores the terminal (the session-tui pattern). See `plans/slices/slice-9.md`.

use std::io::{self, Stdout};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Sender};
use std::thread;
use std::time::{Duration, SystemTime};

use crossterm::event::{self, Event};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;

use crate::app::{App, Effect, Msg, keys, view};
use crate::bd::{BdCli, BdClient};
use crate::cli::{CliError, sanitize, version_gate};
use crate::config::{Config, Paths};
use crate::hub::{ensure_hub, hub_dir};
use crate::refresh::{self, PrefixMap, RefreshError};
use crate::snapshot::{self, Snapshot};

/// How long the event thread blocks on `event::poll` before re-checking the stop
/// flag, so a quit is observed promptly without a busy loop.
const INPUT_POLL: Duration = Duration::from_millis(100);

type Tui = Terminal<CrosstermBackend<Stdout>>;

/// Launch the interactive TUI (bare `fbd`). Sets up the terminal, runs the event
/// loop against `roster`, and always restores the terminal before returning —
/// even on error, so a failure never leaves the user's terminal wedged.
pub fn run(paths: &Paths, roster: Config) -> Result<(), CliError> {
    let mut terminal = setup_terminal().map_err(CliError::Io)?;
    let loop_result = event_loop(&mut terminal, paths, &roster);
    let restore_result = restore_terminal(&mut terminal);
    // Surface a loop failure first; a restore failure only if the loop was fine.
    loop_result?;
    restore_result.map_err(CliError::Io)?;
    Ok(())
}

/// The UI thread: spawn the input + initial-refresh producers, then consume
/// messages, reduce, execute effects, and redraw until the app is done.
fn event_loop(terminal: &mut Tui, paths: &Paths, roster: &Config) -> Result<(), CliError> {
    let (tx, rx) = mpsc::channel::<Msg>();
    let stop = Arc::new(AtomicBool::new(false));

    let input_handle = {
        let tx = tx.clone();
        let stop = Arc::clone(&stop);
        thread::spawn(move || input_thread(&tx, &stop))
    };

    let mut app = App::new();
    // The App is born stale; launch immediately kicks off the first refresh.
    spawn_refresh(&tx, paths, roster);
    draw(terminal, &app)?;

    while let Ok(msg) = rx.recv() {
        for effect in app.reduce(msg) {
            match effect {
                Effect::Refresh => spawn_refresh(&tx, paths, roster),
            }
        }
        if app.is_done() {
            break;
        }
        draw(terminal, &app)?;
    }

    // Stop the input thread and join it; detached refresh workers (if any are
    // still running a bd subprocess) are harmless — their sends fail once `rx`
    // drops, and the process exits after this returns.
    stop.store(true, Ordering::SeqCst);
    let _ = input_handle.join();
    Ok(())
}

/// Render the current state with a fresh `now` for the staleness age.
fn draw(terminal: &mut Tui, app: &App) -> Result<(), CliError> {
    terminal
        .draw(|frame| view::draw(frame, app, SystemTime::now()))
        .map_err(CliError::Io)?;
    Ok(())
}

/// Spawn a background refresh worker that reports over `tx`. Clones the roster
/// and paths into the thread and builds a fresh [`BdCli`] (stateless).
fn spawn_refresh(tx: &Sender<Msg>, paths: &Paths, roster: &Config) {
    let tx = tx.clone();
    let paths = paths.clone();
    let roster = roster.clone();
    thread::spawn(move || refresh_worker(BdCli::new(), roster, paths, tx));
}

/// The refresh worker body: announce the start, run the pipeline, then send
/// exactly one atomic completion. Owned args so it moves cleanly into a thread;
/// unit-tested directly with a [`crate::bd::FakeBdClient`] and a channel.
pub(crate) fn refresh_worker(bd: impl BdClient, roster: Config, paths: Paths, tx: Sender<Msg>) {
    let _ = tx.send(Msg::RefreshStarted);
    let (snapshot, warnings) = gather_snapshot(&bd, &roster, &paths);
    let _ = tx.send(Msg::RefreshCompleted { snapshot, warnings });
}

/// Run `ensure_hub → refresh → fetch` and return the fresh snapshot (or `None`
/// on any fatal failure, keeping the caller's last-good rows) plus the warnings
/// to surface. Deliberately fatal-tolerant, unlike the fail-fast CLI
/// [`crate::cli::run_snapshot`]: the TUI degrades and stays interactive. All
/// warnings are [`sanitize`]d (they embed bd stderr / paths and reach a
/// terminal).
pub(crate) fn gather_snapshot(
    bd: &impl BdClient,
    roster: &Config,
    paths: &Paths,
) -> (Option<Snapshot>, Vec<String>) {
    let mut warnings = Vec::new();

    // Version gate: a bd whose schema fbd cannot vouch for yields no snapshot.
    match bd.version() {
        Ok(v) => {
            if let Err(msg) = version_gate(&v) {
                warnings.push(sanitize(&msg));
                return (None, warnings);
            }
        }
        Err(e) => {
            warnings.push(sanitize(&format!("bd version check failed: {e}")));
            return (None, warnings);
        }
    }

    match ensure_hub(bd, paths, roster) {
        Ok(status) => warnings.extend(status.warnings.iter().map(|w| sanitize(w))),
        Err(e) => {
            warnings.push(sanitize(&format!("hub error: {e}")));
            return (None, warnings);
        }
    }

    let hub = hub_dir(paths);
    let (prefix_map, fetched_at) = match refresh::run(bd, roster, paths) {
        Ok(outcome) => {
            for repo_error in &outcome.errors {
                warnings.push(sanitize(&repo_error.to_string()));
            }
            for collision in outcome.prefix_map.collisions() {
                warnings.push(sanitize(&format!(
                    "id prefix `{}` is claimed by {} repos; its issues show as `{}`",
                    collision.prefix,
                    collision.repos.len(),
                    snapshot::UNKNOWN_REPO,
                )));
            }
            (outcome.prefix_map, outcome.synced_at)
        }
        // Another fbd holds the lock: show the last synced data (unattributed,
        // since we did not build a fresh prefix map this cycle).
        Err(RefreshError::AlreadyRefreshing) => {
            warnings
                .push("another fbd is refreshing this hub; showing the last synced data".into());
            (PrefixMap::default(), SystemTime::now())
        }
        Err(fatal) => {
            warnings.push(sanitize(&format!("refresh failed: {fatal}")));
            return (None, warnings);
        }
    };

    match snapshot::fetch(bd, &hub, &prefix_map, fetched_at) {
        Ok(snapshot) => (Some(snapshot), warnings),
        Err(e) => {
            warnings.push(sanitize(&format!("reading ready list failed: {e}")));
            (None, warnings)
        }
    }
}

/// The crossterm event producer: map key presses to [`Msg`]s until told to stop.
/// Polls with a timeout so the stop flag is observed even while idle.
fn input_thread(tx: &Sender<Msg>, stop: &AtomicBool) {
    while !stop.load(Ordering::SeqCst) {
        match event::poll(INPUT_POLL) {
            Ok(true) => match event::read() {
                Ok(Event::Key(key)) => {
                    if let Some(msg) = keys::map_key(key)
                        && tx.send(msg).is_err()
                    {
                        return; // UI thread gone.
                    }
                }
                Ok(_) => {}       // non-key event (resize, mouse): ignored
                Err(_) => return, // terminal read failure: stop producing
            },
            Ok(false) => {} // timeout: loop and re-check the stop flag
            Err(_) => return,
        }
    }
}

/// Enter raw mode + the alternate screen and install the restoring panic hook.
fn setup_terminal() -> io::Result<Tui> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    set_panic_hook();
    Terminal::new(CrosstermBackend::new(stdout))
}

/// Leave the alternate screen, disable raw mode, and show the cursor.
fn restore_terminal(terminal: &mut Tui) -> io::Result<()> {
    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;
    Ok(())
}

/// Chain a terminal-restoring step before the default panic hook, so a panic
/// mid-render leaves the user with a usable terminal instead of a wedged one.
fn set_panic_hook() {
    let original = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = disable_raw_mode();
        let _ = execute!(io::stdout(), LeaveAlternateScreen);
        original(info);
    }));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bd::{BdError, BdErrorKind, FakeBdClient, Issue};
    use crate::config::RepoEntry;
    use std::fs;
    use std::path::{Path, PathBuf};

    fn issue(id: &str, priority: i64, title: &str) -> Issue {
        Issue {
            id: id.to_string(),
            title: title.to_string(),
            status: "open".into(),
            priority,
            description: None,
            issue_type: None,
            owner: None,
            created_at: None,
            created_by: None,
            updated_at: Some("2026-07-11T00:00:00Z".into()),
            dependency_count: None,
            dependent_count: None,
            comment_count: None,
        }
    }

    /// A repo dir under `base` with a seeded `.beads/metadata.json` prefix.
    fn seed_repo(base: &Path, name: &str, prefix: &str) -> PathBuf {
        let repo = base.join(name);
        let beads = repo.join(".beads");
        fs::create_dir_all(&beads).unwrap();
        fs::write(
            beads.join("metadata.json"),
            format!(r#"{{"database":"dolt","dolt_database":"{prefix}"}}"#),
        )
        .unwrap();
        repo
    }

    fn roster(paths: &[&Path]) -> Config {
        Config {
            repos: paths
                .iter()
                .map(|p| RepoEntry {
                    path: p.to_path_buf(),
                })
                .collect(),
        }
    }

    fn bd_err() -> BdError {
        BdError {
            command: "bd repo sync".into(),
            stderr: "boom".into(),
            kind: BdErrorKind::NonZeroExit { code: Some(1) },
        }
    }

    #[test]
    fn refresh_task_sends_started_then_completed() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = Paths::with_base(tmp.path());
        let ra = seed_repo(tmp.path(), "ra", "ra");
        let bd = FakeBdClient::new().with_ready(vec![issue("ra-1", 1, "Ready one")]);
        let (tx, rx) = mpsc::channel();

        let handle = thread::spawn(move || refresh_worker(bd, roster(&[&ra]), paths, tx));

        // Exactly: RefreshStarted, then one RefreshCompleted carrying the rows.
        assert_eq!(rx.recv().unwrap(), Msg::RefreshStarted);
        match rx.recv().unwrap() {
            Msg::RefreshCompleted { snapshot, .. } => {
                let snap = snapshot.expect("a snapshot on success");
                assert!(
                    snap.rows.iter().any(|r| r.issue.id == "ra-1"),
                    "the ready row flows through: {:?}",
                    snap.rows
                );
            }
            other => panic!("expected RefreshCompleted, got {other:?}"),
        }
        // The worker's `tx` drops when it returns, closing the channel: no third
        // message, so the two-message lifecycle is exact (no sleeps needed).
        assert!(
            rx.recv().is_err(),
            "exactly one completion, then the channel closes"
        );
        handle.join().unwrap();
    }

    #[test]
    fn gather_snapshot_collects_repo_warnings() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = Paths::with_base(tmp.path());
        let ra = seed_repo(tmp.path(), "ra", "ra");
        let missing = tmp.path().join("gone");
        let bd = FakeBdClient::new().with_ready(vec![issue("ra-1", 1, "t")]);

        let (snapshot, warnings) = gather_snapshot(&bd, &roster(&[&ra, &missing]), &paths);

        let snap = snapshot.expect("healthy repo still yields a snapshot");
        assert!(
            snap.rows.iter().any(|r| r.issue.id == "ra-1"),
            "the healthy repo's rows appear"
        );
        assert!(
            warnings.iter().any(|w| w.contains("gone")),
            "the missing roster path is warned about: {warnings:?}"
        );
    }

    #[test]
    fn gather_snapshot_none_on_fatal_sync() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = Paths::with_base(tmp.path());
        let ra = seed_repo(tmp.path(), "ra", "ra");
        let bd = FakeBdClient::new().with_repo_sync_err(bd_err());

        let (snapshot, warnings) = gather_snapshot(&bd, &roster(&[&ra]), &paths);

        assert!(
            snapshot.is_none(),
            "a fatal sync failure yields no snapshot"
        );
        assert!(
            warnings.iter().any(|w| w.contains("refresh failed")),
            "the fatal refresh is surfaced: {warnings:?}"
        );
    }
}

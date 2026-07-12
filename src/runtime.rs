//! The terminal runtime: the event loop that turns the pure Slice 8 [`App`] core
//! into a running TUI. A crossterm event thread and a refresh worker thread both
//! feed one `mpsc` channel of [`Msg`]; the UI thread `recv`s each message, calls
//! [`App::reduce`], executes the returned [`Effect`]s, and redraws via
//! [`crate::app::view::draw`]. Terminal setup/teardown installs a panic hook that
//! restores the terminal (the session-tui pattern). See `plans/slices/slice-9.md`.

use std::io::{self, Stdout, Write};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::thread;
use std::time::{Duration, SystemTime};

use crossterm::event::{self, Event, KeyEvent};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;

use crate::app::{App, Effect, Msg, context, keys, view};
use crate::bd::{BdCli, BdClient, IssueDetail};
use crate::cache;
use crate::cli::{CliError, sanitize, version_gate};
use crate::config::{Config, Paths};
use crate::hub::{ensure_hub, hub_dir};
use crate::refresh::{self, RefreshError};
use crate::snapshot::{self, Row, Snapshot};

/// How long the event thread blocks on `event::poll` before re-checking the stop
/// flag, so a quit is observed promptly without a busy loop.
const INPUT_POLL: Duration = Duration::from_millis(100);

/// How long the UI thread waits for a message before redrawing anyway, so the
/// status bar's last-refreshed age advances while the user is idle.
const TICK: Duration = Duration::from_secs(1);

/// The most characters of a copied command/block shown in the status-bar
/// confirmation before it is truncated with an ellipsis.
const COPY_SUMMARY_MAX: usize = 72;

type Tui = Terminal<CrosstermBackend<Stdout>>;

/// What the UI thread consumes from its one channel: a **raw** key event from the
/// input thread, or an app [`Msg`] from a background worker.
///
/// Keys arrive raw (not pre-decoded) so [`keys::map_key`] runs on the UI thread
/// against the app's *live* search-input focus. Because the channel preserves
/// order, a `/` that opens the search editor is reduced before the next key is
/// decoded — so a pasted `/query` burst can never decode `query`'s characters as
/// commands (e.g. `q` quitting) the way a producer-thread decoder racing an
/// asynchronously-published mode flag could.
#[derive(Debug)]
pub(crate) enum Incoming {
    /// A raw key press/repeat, decoded on the UI thread against live app state.
    Key(KeyEvent),
    /// An app message from a background worker (refresh / detail / search).
    Msg(Msg),
}

impl From<Msg> for Incoming {
    fn from(msg: Msg) -> Self {
        Incoming::Msg(msg)
    }
}

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
    let (tx, rx) = mpsc::channel::<Incoming>();
    let stop = Arc::new(AtomicBool::new(false));

    let input_handle = {
        let tx = tx.clone();
        let stop = Arc::clone(&stop);
        thread::spawn(move || input_thread(&tx, &stop))
    };

    let mut app = App::new();
    // A fresh (<12h) on-disk cache paints instantly, before the real refresh
    // below has a chance to land, so launch never sits in `Loading` behind a
    // slow `bd ready` when yesterday's rows would do. `hydrate_from_cache`
    // (unlike `reduce(Msg::RefreshCompleted { .. })`) leaves `stale` alone,
    // so the born-stale in-flight guard `App::new` reserves for the launch
    // refresh below stays armed the whole time. A stale/missing/corrupt
    // cache is a silent no-op: the app stays `Loading` exactly as before
    // this existed.
    if let Some(snapshot) = cache::load(paths.cache_file(), SystemTime::now()) {
        app.hydrate_from_cache(snapshot);
    }
    // In-flight background workers (refresh *and* detail), tracked so shutdown can
    // wait for the running bd subprocess to finish and release the hub lock —
    // never orphaning a child that would keep mutating the hub after fbd's lock
    // has dropped. Finished handles are pruned on each new spawn so the vec cannot
    // grow across a long session (the Slice 8 guard bounds live refresh workers to
    // one; detail fetches are short and pruned likewise).
    let mut worker_handles: Vec<thread::JoinHandle<()>> = Vec::new();
    // The App is born stale; launch immediately kicks off the first refresh.
    worker_handles.push(spawn_refresh(&tx, paths, roster));

    // Run the render/reduce loop, then join threads *unconditionally* — for a
    // clean quit and for every error return alike — so a terminal write failure
    // can never detach the input thread or an in-flight worker (which would
    // orphan its bd subprocess while our process exits and drops the hub lock).
    let result = ui_loop(
        terminal,
        &rx,
        &tx,
        &mut app,
        &mut worker_handles,
        paths,
        roster,
    );
    stop.store(true, Ordering::SeqCst);
    let _ = input_handle.join();
    for handle in worker_handles {
        let _ = handle.join();
    }
    result
}

/// The render/reduce loop, factored out so [`event_loop`] can join its threads
/// whether this returns `Ok` (a `q` quit) or `Err` (a terminal draw failure).
fn ui_loop(
    terminal: &mut Tui,
    rx: &Receiver<Incoming>,
    tx: &Sender<Incoming>,
    app: &mut App,
    worker_handles: &mut Vec<thread::JoinHandle<()>>,
    paths: &Paths,
    roster: &Config,
) -> Result<(), CliError> {
    draw(terminal, app)?;
    // Redraw on every message and on every idle tick, so the staleness age keeps
    // advancing even while no messages arrive. `Disconnected` cannot occur while
    // the caller still holds `tx`, but is handled defensively as a clean exit.
    loop {
        match rx.recv_timeout(TICK) {
            Ok(incoming) => {
                // Decode a raw key against the app's *current* search focus (so a
                // pasted `/query` burst can't run `query` as commands); worker
                // messages pass through. An unmapped key yields no message.
                let msg = match incoming {
                    Incoming::Key(key) => keys::map_key(key, app.search_editing()),
                    Incoming::Msg(msg) => Some(msg),
                };
                if let Some(msg) = msg {
                    for effect in app.reduce(msg) {
                        execute_effect(effect, tx, worker_handles, paths, roster);
                    }
                    if app.is_done() {
                        return Ok(());
                    }
                }
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => return Ok(()),
        }
        draw(terminal, app)?;
    }
}

/// Perform one [`Effect`] by spawning the matching background worker, tracking
/// its handle for shutdown. The single dispatch point for every effect `reduce`
/// returns — Slice 11's `Effect::Search` slots in as one more arm with no change
/// to [`ui_loop`]. Finished handles are pruned first so the vec stays bounded.
fn execute_effect(
    effect: Effect,
    tx: &Sender<Incoming>,
    worker_handles: &mut Vec<thread::JoinHandle<()>>,
    paths: &Paths,
    roster: &Config,
) {
    worker_handles.retain(|h| !h.is_finished());
    let handle = match effect {
        Effect::Refresh => spawn_refresh(tx, paths, roster),
        Effect::FetchDetail { id, token } => spawn_detail(tx, paths, id, token),
        Effect::Search { query, token } => spawn_search(tx, paths, roster, query, token),
        Effect::Copy {
            row,
            markdown,
            token,
        } => spawn_copy(tx, paths, roster, *row, markdown, token),
        // Not a worker: write the OSC 52 escape here, on the UI thread that owns
        // the tty, so it can never interleave with a ratatui draw. Returns without
        // a handle to track.
        Effect::WriteClipboard(payload) => {
            write_clipboard(&payload);
            return;
        }
    };
    worker_handles.push(handle);
}

/// Render the current state with a fresh `now` for the staleness age.
fn draw(terminal: &mut Tui, app: &App) -> Result<(), CliError> {
    terminal
        .draw(|frame| view::draw(frame, app, SystemTime::now()))
        .map_err(CliError::Io)?;
    Ok(())
}

/// Spawn a background refresh worker that reports over `tx`, returning its join
/// handle so the event loop can wait for it on shutdown. Clones the roster and
/// paths into the thread and builds a fresh [`BdCli`] (stateless).
fn spawn_refresh(tx: &Sender<Incoming>, paths: &Paths, roster: &Config) -> thread::JoinHandle<()> {
    let tx = tx.clone();
    let paths = paths.clone();
    let roster = roster.clone();
    thread::spawn(move || refresh_worker(BdCli::new(), roster, paths, tx))
}

/// The refresh worker body: announce the start, run the pipeline, cache a
/// successful snapshot to disk (best-effort — a write failure never blocks
/// delivery), then send exactly one atomic completion. Owned args so it moves
/// cleanly into a thread; unit-tested directly with a [`crate::bd::FakeBdClient`]
/// and a channel.
pub(crate) fn refresh_worker(
    bd: impl BdClient,
    roster: Config,
    paths: Paths,
    tx: Sender<Incoming>,
) {
    let _ = tx.send(Msg::RefreshStarted.into());
    let (snapshot, warnings) = gather_snapshot(&bd, &roster, &paths);
    if let Some(snapshot) = &snapshot {
        let _ = cache::save(paths.cache_file(), snapshot);
    }
    let _ = tx.send(Msg::RefreshCompleted { snapshot, warnings }.into());
}

/// Spawn a background detail worker that reports over `tx`, returning its join
/// handle so the event loop can wait for it on shutdown. Clones the paths into
/// the thread and builds a fresh [`BdCli`] (stateless).
fn spawn_detail(
    tx: &Sender<Incoming>,
    paths: &Paths,
    id: String,
    token: u64,
) -> thread::JoinHandle<()> {
    let tx = tx.clone();
    let paths = paths.clone();
    thread::spawn(move || detail_worker(BdCli::new(), paths, id, token, tx))
}

/// The detail worker body: fetch one issue's detail and send exactly one
/// [`Msg::DetailReady`] echoing `token` (so a superseded response can be dropped).
/// Owned args so it moves cleanly into a thread; unit-tested directly with a
/// [`crate::bd::FakeBdClient`] and a channel.
pub(crate) fn detail_worker(
    bd: impl BdClient,
    paths: Paths,
    id: String,
    token: u64,
    tx: Sender<Incoming>,
) {
    let detail = gather_detail(&bd, &paths, &id).map(Box::new);
    let _ = tx.send(Msg::DetailReady { token, detail }.into());
}

/// Run `bd show <id> --json` against the hub, mapping a [`BdError`] to a
/// pre-formatted, [`sanitize`]d message for the pane. No version gate or
/// `ensure_hub`: the detail pane is reachable only from the list, i.e. after a
/// snapshot already hydrated the hub.
pub(crate) fn gather_detail(
    bd: &impl BdClient,
    paths: &Paths,
    id: &str,
) -> Result<IssueDetail, String> {
    bd.show(&hub_dir(paths), id)
        .map_err(|e| sanitize(&format!("couldn't load {id}: {e}")))
}

/// Spawn a background search worker that reports over `tx`, returning its join
/// handle so the event loop can wait for it on shutdown. Clones the roster and
/// paths into the thread and builds a fresh [`BdCli`] (stateless).
fn spawn_search(
    tx: &Sender<Incoming>,
    paths: &Paths,
    roster: &Config,
    query: String,
    token: u64,
) -> thread::JoinHandle<()> {
    let tx = tx.clone();
    let paths = paths.clone();
    let roster = roster.clone();
    thread::spawn(move || search_worker(BdCli::new(), roster, paths, query, token, tx))
}

/// The search worker body: run the query, attribute the results, and send exactly
/// one [`Msg::SearchResults`] echoing `token` (so a superseded response can be
/// dropped). Owned args so it moves cleanly into a thread; unit-tested directly
/// with a [`crate::bd::FakeBdClient`] and a channel.
pub(crate) fn search_worker(
    bd: impl BdClient,
    roster: Config,
    paths: Paths,
    query: String,
    token: u64,
    tx: Sender<Incoming>,
) {
    let rows = gather_search(&bd, &roster, &paths, &query);
    let _ = tx.send(Msg::SearchResults { token, rows }.into());
}

/// Run `bd search <query> --json` against the hub and attribute the results
/// through the **same** [`snapshot::attribute`] path as ready rows, so search rows
/// carry `repo_name` identically. The prefix map is rebuilt from the roster via
/// [`refresh::attribution_map`] (its per-repo prefix-read failures are non-fatal —
/// those ids fall to the `unknown` bucket). A `bd search` failure maps to a
/// [`sanitize`]d message. No version gate / `ensure_hub`: search is reachable only
/// from the list, i.e. after a snapshot already hydrated the hub.
pub(crate) fn gather_search(
    bd: &impl BdClient,
    roster: &Config,
    paths: &Paths,
    query: &str,
) -> Result<Vec<Row>, String> {
    let hub = hub_dir(paths);
    let issues = bd
        .search(&hub, query)
        .map_err(|e| sanitize(&format!("search failed: {e}")))?;
    let (prefix_map, _errors) = refresh::attribution_map(bd, roster);
    Ok(snapshot::attribute(issues, &prefix_map, SystemTime::now()).rows)
}

/// Spawn a background copy worker that reports over `tx`, returning its join
/// handle so the event loop can wait for it on shutdown. Clones the roster and
/// paths into the thread and builds a fresh [`BdCli`] (stateless).
fn spawn_copy(
    tx: &Sender<Incoming>,
    paths: &Paths,
    roster: &Config,
    row: Row,
    markdown: bool,
    token: u64,
) -> thread::JoinHandle<()> {
    let tx = tx.clone();
    let paths = paths.clone();
    let roster = roster.clone();
    thread::spawn(move || copy_worker(BdCli::new(), roster, paths, row, markdown, token, tx))
}

/// The copy worker body: build the clipboard payload + status summary off the UI
/// thread (the id→repo-path resolution runs `bd`), then send exactly one
/// [`Msg::Copied`]. `reduce` turns that into the UI-thread [`Effect::WriteClipboard`]
/// so the escape write never races a draw. Owned args so it moves cleanly into a
/// thread; unit-tested directly with a [`crate::bd::FakeBdClient`] and a channel.
pub(crate) fn copy_worker(
    bd: impl BdClient,
    roster: Config,
    paths: Paths,
    row: Row,
    markdown: bool,
    token: u64,
    tx: Sender<Incoming>,
) {
    let (payload, summary) = build_copy(&bd, &roster, &paths, &row, markdown);
    let _ = tx.send(
        Msg::Copied {
            token,
            payload,
            summary,
        }
        .into(),
    );
}

/// Build the clipboard payload and its status-bar summary for `row`.
///
/// The command form (`markdown == false`) resolves the row's source-repo path
/// from its issue id via [`refresh::attribution_map`] — the **same** prefix map
/// search uses — and falls back to the hub (`bd -C <hub> show <id>`) for an
/// unattributed id. The markdown form needs no path, so it skips the (subprocess)
/// prefix read entirely. All bd-sourced text is sanitized inside [`context`].
fn build_copy(
    bd: &impl BdClient,
    roster: &Config,
    paths: &Paths,
    row: &Row,
    markdown: bool,
) -> (String, String) {
    let payload = if markdown {
        context::markdown_block(&row.issue, &row.repo_name)
    } else {
        let (prefix_map, _errors) = refresh::attribution_map(bd, roster);
        let repo = prefix_map.repo_for(&row.issue.id).map(|e| e.path.clone());
        context::shell_command(repo.as_deref(), &hub_dir(paths), &row.issue.id)
    };
    let summary = context::summarize(&payload, COPY_SUMMARY_MAX);
    (payload, summary)
}

/// Write `payload` to the terminal clipboard via an OSC 52 escape. Called only on
/// the UI thread (which owns the tty), so the sequence can never interleave with
/// a ratatui draw. Best-effort: a terminal that ignores OSC 52 simply drops it,
/// and a write failure is non-fatal (the status bar still confirms the attempt).
fn write_clipboard(payload: &str) {
    let seq = context::osc52(payload);
    let mut out = io::stdout();
    let _ = out.write_all(seq.as_bytes());
    let _ = out.flush();
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
        // Another fbd holds the lock: keep the current view intact rather than
        // fetching a snapshot with no prefix map (which would re-attribute every
        // row to `unknown`, reset the age, and empty an active repo filter).
        // Returning `None` makes `reduce` retain the last-good rows.
        Err(RefreshError::AlreadyRefreshing) => {
            warnings.push("another fbd is refreshing this hub; keeping the current view".into());
            return (None, warnings);
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

/// The crossterm event producer: forward **raw** key presses until told to stop
/// (the UI thread decodes them against live app state). Polls with a timeout so
/// the stop flag is observed even while idle. On a terminal read/poll failure it
/// sends `Quit` before exiting, so the UI thread — which holds its own sender and
/// would otherwise block on `recv` forever with no producer left — always has a
/// path to a clean shutdown.
fn input_thread(tx: &Sender<Incoming>, stop: &AtomicBool) {
    while !stop.load(Ordering::SeqCst) {
        match event::poll(INPUT_POLL) {
            Ok(true) => match event::read() {
                Ok(Event::Key(key)) => {
                    if tx.send(Incoming::Key(key)).is_err() {
                        return; // UI thread gone.
                    }
                }
                Ok(_) => {} // non-key event (resize, mouse): ignored
                Err(_) => {
                    let _ = tx.send(Msg::Quit.into()); // can't read input: quit cleanly
                    return;
                }
            },
            Ok(false) => {} // timeout: loop and re-check the stop flag
            Err(_) => {
                let _ = tx.send(Msg::Quit.into());
                return;
            }
        }
    }
}

/// Enter raw mode + the alternate screen and install the restoring panic hook.
///
/// Rolls back each step if a later one fails, so a partial setup never returns
/// `Err` while leaving the terminal in raw mode or the alternate screen (the
/// caller has no `Tui` to restore in that case).
fn setup_terminal() -> io::Result<Tui> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    if let Err(e) = execute!(stdout, EnterAlternateScreen) {
        let _ = disable_raw_mode();
        return Err(e);
    }
    set_panic_hook();
    match Terminal::new(CrosstermBackend::new(stdout)) {
        Ok(terminal) => Ok(terminal),
        Err(e) => {
            let _ = execute!(io::stdout(), LeaveAlternateScreen);
            let _ = disable_raw_mode();
            Err(e)
        }
    }
}

/// Leave the alternate screen, disable raw mode, and show the cursor.
///
/// Best-effort: every step is attempted even if an earlier one fails (cleanup
/// matters most precisely when a terminal op is failing), and the first error is
/// returned once all three have run.
fn restore_terminal(terminal: &mut Tui) -> io::Result<()> {
    let raw = disable_raw_mode();
    let screen = execute!(terminal.backend_mut(), LeaveAlternateScreen);
    let cursor = terminal.show_cursor();
    raw.and(screen).and(cursor)
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
    use crate::bd::{BdError, BdErrorKind, Dependency, FakeBdClient, Issue};
    use crate::config::RepoEntry;
    use crate::refresh::HubLock;
    use std::fs;
    use std::path::{Path, PathBuf};

    /// Receive the next app message a worker sent, unwrapping the [`Incoming`]
    /// channel envelope (workers only ever send `Incoming::Msg`).
    fn recv_msg(rx: &Receiver<Incoming>) -> Msg {
        match rx.recv().expect("a worker message") {
            Incoming::Msg(msg) => msg,
            Incoming::Key(key) => panic!("workers never send keys, got {key:?}"),
        }
    }

    fn issue(id: &str, priority: i64, title: &str) -> Issue {
        Issue {
            id: id.to_string(),
            title: title.to_string(),
            status: "open".into(),
            priority,
            description: None,
            issue_type: None,
            owner: None,
            labels: Vec::new(),
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
        assert_eq!(recv_msg(&rx), Msg::RefreshStarted);
        match recv_msg(&rx) {
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
    fn refresh_task_caches_a_successful_snapshot_to_disk() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = Paths::with_base(tmp.path());
        let cache_file = paths.cache_file().to_path_buf();
        let ra = seed_repo(tmp.path(), "ra", "ra");
        let bd = FakeBdClient::new().with_ready(vec![issue("ra-1", 1, "Ready one")]);
        let (tx, rx) = mpsc::channel();

        let handle = thread::spawn(move || refresh_worker(bd, roster(&[&ra]), paths, tx));
        assert_eq!(recv_msg(&rx), Msg::RefreshStarted);
        let snapshot = match recv_msg(&rx) {
            Msg::RefreshCompleted { snapshot, .. } => snapshot.expect("a snapshot on success"),
            other => panic!("expected RefreshCompleted, got {other:?}"),
        };
        handle.join().unwrap();

        let cached = crate::cache::load(&cache_file, SystemTime::now()).expect("a fresh cache hit");
        assert_eq!(
            cached, snapshot,
            "the cached snapshot matches what shipped over the channel"
        );
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

    fn detail() -> IssueDetail {
        IssueDetail {
            issue: issue("ra-1", 2, "Blocked task"),
            dependencies: vec![Dependency {
                id: "ra-z70".into(),
                title: Some("Blocker task".into()),
                status: Some("open".into()),
                dependency_type: Some("blocks".into()),
            }],
        }
    }

    #[test]
    fn detail_worker_sends_ready_for_id() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = Paths::with_base(tmp.path());
        let bd = FakeBdClient::new().with_show(detail());
        let (tx, rx) = mpsc::channel();

        let handle = thread::spawn(move || detail_worker(bd, paths, "ra-1".into(), 7, tx));

        match recv_msg(&rx) {
            Msg::DetailReady { token, detail } => {
                assert_eq!(token, 7, "the request token is echoed back");
                let d = detail.expect("a detail on success");
                assert_eq!(d.issue.id, "ra-1");
                assert_eq!(d.dependencies.len(), 1);
            }
            other => panic!("expected DetailReady, got {other:?}"),
        }
        // The worker's tx drops on return: exactly one message, then closed.
        assert!(rx.recv().is_err(), "exactly one DetailReady, then closed");
        handle.join().unwrap();
    }

    #[test]
    fn detail_worker_maps_error() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = Paths::with_base(tmp.path());
        let bd = FakeBdClient::new().with_show_err(bd_err());
        let (tx, rx) = mpsc::channel();

        let handle = thread::spawn(move || detail_worker(bd, paths, "ra-1".into(), 1, tx));

        match recv_msg(&rx) {
            Msg::DetailReady { token, detail } => {
                assert_eq!(token, 1);
                let msg = detail.expect_err("a message on failure");
                assert!(
                    msg.contains("boom") || msg.to_lowercase().contains("fail"),
                    "the failure is surfaced: {msg}"
                );
            }
            other => panic!("expected DetailReady, got {other:?}"),
        }
        handle.join().unwrap();
    }

    #[test]
    fn search_worker_sends_results_for_token() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = Paths::with_base(tmp.path());
        let ra = seed_repo(tmp.path(), "ra", "ra");
        let bd = FakeBdClient::new().with_search(vec![issue("ra-1", 1, "Found one")]);
        let (tx, rx) = mpsc::channel();

        let handle =
            thread::spawn(move || search_worker(bd, roster(&[&ra]), paths, "foo".into(), 7, tx));

        match recv_msg(&rx) {
            Msg::SearchResults { token, rows } => {
                assert_eq!(token, 7, "the request token is echoed back");
                let rows = rows.expect("results on success");
                let found = rows
                    .iter()
                    .find(|r| r.issue.id == "ra-1")
                    .expect("row present");
                assert_eq!(
                    found.repo_name, "ra",
                    "results are attributed via the roster prefix map"
                );
            }
            other => panic!("expected SearchResults, got {other:?}"),
        }
        // The worker's tx drops on return: exactly one message, then closed.
        assert!(rx.recv().is_err(), "exactly one SearchResults, then closed");
        handle.join().unwrap();
    }

    #[test]
    fn search_worker_maps_error() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = Paths::with_base(tmp.path());
        let ra = seed_repo(tmp.path(), "ra", "ra");
        let bd = FakeBdClient::new().with_search_err(bd_err());
        let (tx, rx) = mpsc::channel();

        let handle =
            thread::spawn(move || search_worker(bd, roster(&[&ra]), paths, "foo".into(), 3, tx));

        match recv_msg(&rx) {
            Msg::SearchResults { token, rows } => {
                assert_eq!(token, 3);
                let msg = rows.expect_err("a message on failure");
                assert!(
                    msg.to_lowercase().contains("search failed") || msg.contains("boom"),
                    "the failure is surfaced: {msg}"
                );
            }
            other => panic!("expected SearchResults, got {other:?}"),
        }
        handle.join().unwrap();
    }

    /// The single `Msg::Copied` a copy worker sends: (token, payload, summary).
    fn recv_copied(rx: &Receiver<Incoming>) -> (u64, String, String) {
        match recv_msg(rx) {
            Msg::Copied {
                token,
                payload,
                summary,
            } => (token, payload, summary),
            other => panic!("expected Copied, got {other:?}"),
        }
    }

    fn copy_row(repo_name: &str, id: &str) -> Row {
        Row {
            issue: issue(id, 1, "Ready one"),
            repo_name: repo_name.to_string(),
        }
    }

    #[test]
    fn copy_worker_builds_cd_for_attributed() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = Paths::with_base(tmp.path());
        let ra = seed_repo(tmp.path(), "ra", "ra");
        let bd = FakeBdClient::new();
        let (tx, rx) = mpsc::channel();

        let row = copy_row("ra", "ra-1");
        let paths2 = paths.clone();
        let ra2 = ra.clone();
        let handle =
            thread::spawn(move || copy_worker(bd, roster(&[&ra2]), paths2, row, false, 7, tx));

        let (token, payload, summary) = recv_copied(&rx);
        assert_eq!(token, 7, "the request token is echoed back");
        assert_eq!(
            payload,
            format!("cd {} && bd show ra-1", ra.display()),
            "attributed id resolves to its repo path"
        );
        assert!(
            summary.starts_with("cd ") && summary.chars().count() <= COPY_SUMMARY_MAX,
            "summary is the truncated command form: {summary}"
        );
        handle.join().unwrap();
    }

    #[test]
    fn copy_worker_falls_back_to_hub_for_unattributed() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = Paths::with_base(tmp.path());
        let ra = seed_repo(tmp.path(), "ra", "ra");
        let bd = FakeBdClient::new();
        let (tx, rx) = mpsc::channel();

        // An id whose prefix (`zz`) matches no roster repo → hub fallback.
        let row = copy_row("unknown", "zz-9");
        let paths2 = paths.clone();
        let handle =
            thread::spawn(move || copy_worker(bd, roster(&[&ra]), paths2, row, false, 1, tx));

        let (_, payload, _) = recv_copied(&rx);
        assert_eq!(
            payload,
            format!("bd -C {} show zz-9", hub_dir(&paths).display()),
            "an unattributed id uses the always-correct hub form"
        );
        handle.join().unwrap();
    }

    #[test]
    fn copy_worker_markdown_block() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = Paths::with_base(tmp.path());
        let ra = seed_repo(tmp.path(), "ra", "ra");
        let bd = FakeBdClient::new();
        let (tx, rx) = mpsc::channel();

        let row = copy_row("session-tui", "ra-1");
        let handle =
            thread::spawn(move || copy_worker(bd, roster(&[&ra]), paths, row, true, 1, tx));

        let (_, payload, _) = recv_copied(&rx);
        assert!(payload.contains("Ready one"), "markdown title: {payload:?}");
        assert!(payload.contains("ra-1"), "markdown id: {payload:?}");
        assert!(
            payload.contains("session-tui"),
            "markdown repo: {payload:?}"
        );
        handle.join().unwrap();
    }

    #[test]
    fn pasted_query_keys_never_run_commands() {
        // Regression for the autoreview finding: a pasted `/qk` burst must open
        // search and type "qk" — never quit on `q` or move the list on `k`.
        // Decoding each raw key against the app's *live* focus (as `ui_loop`
        // does) guarantees this; a producer-side decoder reading an
        // asynchronously-published mode flag could map `q` to Quit before the
        // `/` was reduced. Here we drive the exact decode-then-reduce seam.
        use crossterm::event::{KeyCode, KeyModifiers};

        let mut app = App::new();
        app.reduce(Msg::RefreshCompleted {
            snapshot: Some(Snapshot {
                rows: Vec::new(),
                fetched_at: SystemTime::now(),
            }),
            warnings: Vec::new(),
        });

        for code in [KeyCode::Char('/'), KeyCode::Char('q'), KeyCode::Char('k')] {
            let key = KeyEvent::new(code, KeyModifiers::NONE);
            // Mirror the UI loop: decode against the current focus, then reduce.
            if let Some(msg) = keys::map_key(key, app.search_editing()) {
                app.reduce(msg);
            }
        }

        assert!(
            !app.is_done(),
            "the pasted 'q' typed into the query, not quit"
        );
        assert_eq!(
            app.search_query(),
            Some("qk"),
            "the whole burst after '/' became the query text"
        );
    }

    #[test]
    fn gather_snapshot_none_when_refresh_declined() {
        // Another fbd holds the lock: gather must NOT fetch a mis-attributed
        // snapshot; it returns None so the caller keeps its last-good rows.
        let tmp = tempfile::tempdir().unwrap();
        let paths = Paths::with_base(tmp.path());
        let ra = seed_repo(tmp.path(), "ra", "ra");
        let hub = hub_dir(&paths);
        fs::create_dir_all(&hub).unwrap();
        let _held = HubLock::try_acquire(&hub)
            .unwrap()
            .expect("acquired the lock");
        let bd = FakeBdClient::new().with_ready(vec![issue("ra-1", 1, "t")]);

        let (snapshot, warnings) = gather_snapshot(&bd, &roster(&[&ra]), &paths);

        assert!(
            snapshot.is_none(),
            "a declined refresh yields no snapshot, so last-good rows are kept"
        );
        assert!(
            warnings
                .iter()
                .any(|w| w.to_lowercase().contains("refreshing")),
            "the lock contention is surfaced: {warnings:?}"
        );
    }
}

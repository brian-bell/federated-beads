//! The pure app state core: an [`App`] value, a [`Msg`] enum covering keypresses
//! and the refresh lifecycle, and [`App::reduce`] mapping a message to a new
//! state plus a list of [`Effect`]s the runtime performs.
//!
//! No terminal, no threads, no `bd` calls, and no clock read inside `reduce`
//! (the shown snapshot's `fetched_at` is supplied by the caller; Slice 9 derives
//! staleness *age* from an injected `now`). Crossterm types appear only in
//! [`keys`], so this core stays backend-agnostic and exhaustively unit-testable.
//! See `plans/slices/slice-8.md`.

pub mod keys;
pub mod view;

use std::time::SystemTime;

use crate::bd::IssueDetail;
use crate::snapshot::{Row, Snapshot};

/// A message driving a state transition: either a decoded keypress (see
/// [`keys::map_key`]) or a refresh-lifecycle event fed by the Slice 9 runtime's
/// worker thread.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Msg {
    // ---- Refresh lifecycle (runtime worker → app) ----
    /// A refresh began. Current rows stay visible and are marked [`App::is_stale`].
    RefreshStarted,
    /// A refresh cycle concluded, atomically: the fresh snapshot (`Some` on
    /// success, `None` when the refresh failed and the stale view is kept) plus
    /// the warnings/errors to surface (per-repo export failures, prefix
    /// collisions, missing roster paths, or a fatal sync error the runtime chose
    /// to show rather than abort on). One terminal message per cycle — the single
    /// point that clears [`App::is_stale`] — so a success-with-warnings cannot
    /// split into two `stale`-clearing messages and let an overlapping refresh
    /// slip through the dedup guard. Warnings are pre-formatted so this core
    /// stays free of `refresh`/`hub` error types.
    RefreshCompleted {
        snapshot: Option<Snapshot>,
        warnings: Vec<String>,
    },
    /// A `bd show` detail fetch concluded (runtime detail worker → app). The
    /// `token` echoes the request's generation (see [`Effect::FetchDetail`]) so a
    /// stale/out-of-order response is dropped — even when the *same* issue is
    /// reopened, whose two fetches would share an id but not a token. `detail` is
    /// the fetched [`IssueDetail`] on success or a pre-formatted, sanitized message
    /// on failure (keeping this core free of `bd` error types).
    DetailReady {
        token: u64,
        detail: Result<Box<IssueDetail>, String>,
    },

    // ---- Navigation ----
    /// Move the selection one row down (`j` / `Down`). Clamps at the last row.
    SelectNext,
    /// Move the selection one row up (`k` / `Up`). Clamps at the first row.
    SelectPrev,

    // ---- Filters ----
    /// Cycle the repo filter `All → repo₀ → … → All` (`f`).
    CycleRepoFilter,
    /// Toggle the priority filter `All ↔ P0/P1-only` (`p`).
    TogglePriorityFilter,

    // ---- Commands / modes ----
    /// Request a refresh (`r`); `reduce` emits [`Effect::Refresh`].
    Refresh,
    /// Open the detail pane for the selected row (`Enter`). Placeholder in Slice
    /// 8; Slice 10 makes it emit `Effect::FetchDetail` and enter `Detail`.
    OpenDetail,
    /// Open cross-repo search (`/`). Placeholder in Slice 8; Slice 11 owns it.
    OpenSearch,
    /// Copy an actionable context string for the selected row (`y`). Placeholder
    /// in Slice 8; Slice 12 owns it.
    CopyContext,
    /// Leave the current sub-mode back to the list (`Esc`). No-op in `List`;
    /// Slices 10/11 return from `Detail`/`Search`.
    Back,
    /// Quit the app (`q`); sets [`App::is_done`].
    Quit,
}

/// A side effect the runtime must perform after a transition. `reduce` stays pure
/// by *describing* I/O rather than performing it. Slice 8 emits only
/// [`Effect::Refresh`]; Slice 10 adds `FetchDetail`, Slice 11 `Search(String)` —
/// additive, without changing `reduce`'s signature.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Effect {
    /// Spawn a refresh worker (the `r` keypress → `Msg::Refresh`).
    Refresh,
    /// Fetch one issue's detail via `bd show <id> --json` (the `Enter` keypress →
    /// `Msg::OpenDetail`). `token` is the request's generation; the runtime runs
    /// the fetch on a worker thread and echoes `token` back in [`Msg::DetailReady`]
    /// so a superseded request's late response is dropped.
    FetchDetail { id: String, token: u64 },
}

/// Which screen the app is showing. Slice 11 adds `Search`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ViewMode {
    /// Before the first snapshot arrives.
    Loading,
    /// The cross-repo ready list.
    List,
    /// One issue's detail pane (opened with `Enter`, left with `Esc`).
    Detail,
}

/// The detail pane's state for one issue id. `Loaded` is boxed so the enum stays
/// small (the [`IssueDetail`] payload dwarfs the other variants).
#[derive(Debug, Clone)]
pub enum DetailState {
    /// The `bd show` fetch is in flight for this id.
    Loading { id: String },
    /// The fetched detail (its id is `issue.id`).
    Loaded(Box<IssueDetail>),
    /// The fetch failed; `message` is a pre-formatted, sanitized reason.
    Error { id: String, message: String },
}

impl DetailState {
    /// The issue id the pane is showing, across every variant (for the header /
    /// error line; stale-response matching is by request token, not id).
    pub fn id(&self) -> &str {
        match self {
            DetailState::Loading { id } => id,
            DetailState::Loaded(detail) => &detail.issue.id,
            DetailState::Error { id, .. } => id,
        }
    }
}

/// The repo-attribution axis of the filter, matched against [`Row::repo_name`].
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum RepoFilter {
    /// Show every repo.
    #[default]
    All,
    /// Show only rows whose `repo_name` equals this.
    Only(String),
}

/// The priority axis of the filter.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PriorityFilter {
    /// Show every priority.
    #[default]
    All,
    /// Show only P0/P1 (`priority <= 1`).
    HighOnly,
}

/// The active filter: an independent repo axis and priority axis, applied
/// together by [`FilterSet::matches`].
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct FilterSet {
    repo: RepoFilter,
    priority: PriorityFilter,
}

impl FilterSet {
    /// Whether a row passes both filter axes.
    pub fn matches(&self, row: &Row) -> bool {
        let repo_ok = match &self.repo {
            RepoFilter::All => true,
            RepoFilter::Only(name) => &row.repo_name == name,
        };
        let priority_ok = match self.priority {
            PriorityFilter::All => true,
            PriorityFilter::HighOnly => row.issue.priority <= 1,
        };
        repo_ok && priority_ok
    }

    /// The active repo filter.
    pub fn repo(&self) -> &RepoFilter {
        &self.repo
    }

    /// The active priority filter.
    pub fn priority(&self) -> PriorityFilter {
        self.priority
    }
}

/// The whole TUI state as a pure value. Fields are private to protect the
/// selection invariant (see [`App::reduce`]); read through the accessors.
#[derive(Debug, Clone)]
pub struct App {
    /// Every row from the latest snapshot, in display (sorted) order.
    rows: Vec<Row>,
    /// Indices into `rows` passing `filter`, in display order.
    filtered_ix: Vec<usize>,
    /// Offset into `filtered_ix` (never into `rows`). Invariant: `filtered_ix`
    /// empty ⇒ 0; else `< filtered_ix.len()`.
    selection: usize,
    /// The active filter across both axes.
    filter: FilterSet,
    /// Which screen is shown.
    view_mode: ViewMode,
    /// A refresh is in flight over the shown rows (they may be about to change).
    stale: bool,
    /// Non-fatal warnings for the status bar, replaced each refresh cycle.
    status_warnings: Vec<String>,
    /// When the shown snapshot was fetched (injected upstream; Slice 9 renders
    /// its age against a `now`). `None` before the first snapshot.
    fetched_at: Option<SystemTime>,
    /// The detail pane, `Some` exactly when `view_mode == Detail`.
    detail: Option<DetailState>,
    /// A monotonic generation stamped on each detail request. The current pane's
    /// token is this value; a `DetailReady` is accepted only when its token still
    /// matches, so a superseded fetch (including a reopen of the same issue) is
    /// dropped.
    detail_seq: u64,
    /// The detail pane's vertical scroll offset (rows). Reset on open/close; the
    /// view clamps it to the wrapped content so all of a long detail is reachable.
    detail_scroll: u16,
    /// The user asked to quit; the runtime loop should exit.
    done: bool,
}

impl Default for App {
    fn default() -> Self {
        App::new()
    }
}

impl App {
    /// A fresh app: `Loading`, no rows, no selection, not done — and already
    /// **in-flight**. Construction is always part of launch, which immediately
    /// initiates the first refresh (the Slice 9 runtime spawns it), so the app is
    /// born `stale`: this reserves the refresh in-flight slot from the very first
    /// event, deduping an `r` keypress that races the initial worker's
    /// `RefreshStarted`. The flag clears when that first refresh concludes with a
    /// `RefreshCompleted`, like any other cycle.
    pub fn new() -> App {
        App {
            rows: Vec::new(),
            filtered_ix: Vec::new(),
            selection: 0,
            filter: FilterSet::default(),
            view_mode: ViewMode::Loading,
            stale: true,
            status_warnings: Vec::new(),
            fetched_at: None,
            detail: None,
            detail_seq: 0,
            detail_scroll: 0,
            done: false,
        }
    }

    /// Apply a message, returning the effects the runtime must perform.
    ///
    /// Pure: given the same starting state and message, the resulting state and
    /// effects are identical and nothing outside `self` is touched. Every branch
    /// that can change the row set or filter re-establishes the selection
    /// invariant via [`App::recompute`].
    pub fn reduce(&mut self, msg: Msg) -> Vec<Effect> {
        match msg {
            Msg::RefreshStarted => {
                // Keep the current rows on screen; mark them stale while the
                // refresh runs. First load (no rows) stays in `Loading`.
                self.stale = true;
            }
            Msg::RefreshCompleted { snapshot, warnings } => {
                if let Some(snapshot) = snapshot {
                    self.rows = snapshot.rows;
                    self.fetched_at = Some(snapshot.fetched_at);
                    // Only promote the first-snapshot transition; a refresh
                    // landing under an open `Detail` pane must not slam it shut
                    // (the 1s cadence would otherwise eject the reader).
                    if self.view_mode == ViewMode::Loading {
                        self.view_mode = ViewMode::List;
                    }
                    // The active filter persists across refreshes; recompute it
                    // against the new rows and re-clamp the selection. (`None`
                    // keeps the last-good rows, so no recompute is needed.)
                    self.recompute();
                }
                // The runtime sends the full warning set per cycle, so replace.
                self.status_warnings = warnings;
                // The single, atomic point that ends the in-flight cycle.
                self.stale = false;
            }
            // `j`/`k` move the list selection in `List`, but scroll the pane in
            // `Detail` — so while the pane is open the underlying selection never
            // moves (`Esc` returns to the row it was opened from) yet a long detail
            // stays fully reachable. (In `Loading` both are no-ops.)
            Msg::SelectNext => match self.view_mode {
                ViewMode::List => {
                    if !self.filtered_ix.is_empty() {
                        self.selection = (self.selection + 1).min(self.filtered_ix.len() - 1);
                    }
                }
                // The view clamps to the wrapped content height, so an over-scroll
                // past the end shows no blank; keep `reduce` dimension-free.
                ViewMode::Detail => self.detail_scroll = self.detail_scroll.saturating_add(1),
                ViewMode::Loading => {}
            },
            Msg::SelectPrev => match self.view_mode {
                // Saturating: safe when already at 0 or the list is empty.
                ViewMode::List => self.selection = self.selection.saturating_sub(1),
                ViewMode::Detail => self.detail_scroll = self.detail_scroll.saturating_sub(1),
                ViewMode::Loading => {}
            },
            // Filters act only on the list; they are inert while the pane is open.
            Msg::CycleRepoFilter => {
                if self.view_mode == ViewMode::List {
                    self.filter.repo = self.next_repo_filter();
                    self.recompute();
                }
            }
            Msg::TogglePriorityFilter => {
                if self.view_mode == ViewMode::List {
                    self.filter.priority = match self.filter.priority {
                        PriorityFilter::All => PriorityFilter::HighOnly,
                        PriorityFilter::HighOnly => PriorityFilter::All,
                    };
                    self.recompute();
                }
            }
            Msg::Refresh => {
                // Dedup: a refresh is already pending/in-flight (`stale`), so
                // requesting another would spawn an overlapping worker whose
                // out-of-order completion could clobber a newer snapshot. Mark
                // in-flight synchronously here — before any `RefreshStarted`
                // arrives — so a mashed or key-repeated `r` yields one effect.
                // The guard clears only when the single terminal
                // `RefreshCompleted` arrives, so there is no window between two
                // completion messages for an overlapping request to slip through.
                if self.stale {
                    return Vec::new();
                }
                self.stale = true;
                return vec![Effect::Refresh];
            }
            Msg::OpenDetail => {
                // Open only from the list, and only with a selected row: this
                // makes it exactly one `bd show` per Enter (a second Enter while
                // already in `Detail` is a no-op, an empty list has no row), and
                // cursor movement never fetches.
                if self.view_mode == ViewMode::List
                    && let Some(row) = self.selected_row()
                {
                    let id = row.issue.id.clone();
                    self.detail_seq += 1;
                    let token = self.detail_seq;
                    self.view_mode = ViewMode::Detail;
                    self.detail = Some(DetailState::Loading { id: id.clone() });
                    self.detail_scroll = 0;
                    return vec![Effect::FetchDetail { id, token }];
                }
            }
            Msg::DetailReady { token, detail } => {
                // Accept only the response for the pane's current request; a
                // superseded one (the user moved on, or reopened the same issue)
                // carries an older token and is dropped.
                if self.detail.is_some() && token == self.detail_seq {
                    self.detail = Some(match detail {
                        Ok(loaded) => DetailState::Loaded(loaded),
                        // Reuse the id the pane is already bound to (the Loading
                        // state) so the error names the right issue.
                        Err(message) => DetailState::Error {
                            id: self
                                .detail
                                .as_ref()
                                .map(DetailState::id)
                                .unwrap_or("")
                                .to_string(),
                            message,
                        },
                    });
                }
            }
            Msg::Back => {
                // Return from the detail pane to the list; the selection is
                // untouched, so it is preserved across an open/close.
                if self.view_mode == ViewMode::Detail {
                    self.view_mode = ViewMode::List;
                    self.detail = None;
                    self.detail_scroll = 0;
                }
            }
            // Placeholders: the pipeline accepts these now; the slice that owns
            // each (11 search, 12 copy) gives it behavior.
            Msg::OpenSearch | Msg::CopyContext => {}
            Msg::Quit => self.done = true,
        }
        Vec::new()
    }

    /// Rebuild `filtered_ix` from `rows` under the current filter, then re-clamp
    /// `selection` into bounds — the one place the selection invariant is
    /// re-established after the row set or filter changes.
    fn recompute(&mut self) {
        self.filtered_ix = (0..self.rows.len())
            .filter(|&i| self.filter.matches(&self.rows[i]))
            .collect();
        if self.filtered_ix.is_empty() {
            self.selection = 0;
        } else if self.selection >= self.filtered_ix.len() {
            self.selection = self.filtered_ix.len() - 1;
        }
    }

    /// The next repo filter when cycling with `f`: the sequence is
    /// `All → repo₀ → … → repoₙ₋₁ → All`, where the repos are the distinct
    /// `repo_name`s in first-appearance (display) order of the current rows. A
    /// `Only(name)` whose repo is absent from the current rows falls through to
    /// `All`.
    fn next_repo_filter(&self) -> RepoFilter {
        let mut names: Vec<&str> = Vec::new();
        for row in &self.rows {
            if !names.contains(&row.repo_name.as_str()) {
                names.push(&row.repo_name);
            }
        }
        match &self.filter.repo {
            RepoFilter::All => match names.first() {
                Some(name) => RepoFilter::Only((*name).to_string()),
                None => RepoFilter::All,
            },
            RepoFilter::Only(current) => match names.iter().position(|&n| n == current.as_str()) {
                Some(i) if i + 1 < names.len() => RepoFilter::Only(names[i + 1].to_string()),
                _ => RepoFilter::All,
            },
        }
    }

    // ---- Accessors (the Slice 9 view's read API) ----

    /// The current screen.
    pub fn view_mode(&self) -> ViewMode {
        self.view_mode
    }

    /// Every row (unfiltered), in display order.
    pub fn rows(&self) -> &[Row] {
        &self.rows
    }

    /// The rows passing the current filter, in display order.
    pub fn filtered_rows(&self) -> Vec<&Row> {
        self.filtered_ix.iter().map(|&i| &self.rows[i]).collect()
    }

    /// The selection offset into [`App::filtered_rows`], or `None` when nothing
    /// is visible.
    pub fn selection(&self) -> Option<usize> {
        if self.filtered_ix.is_empty() {
            None
        } else {
            Some(self.selection)
        }
    }

    /// The selected row, or `None` when nothing is visible.
    pub fn selected_row(&self) -> Option<&Row> {
        self.filtered_ix.get(self.selection).map(|&i| &self.rows[i])
    }

    /// Whether a refresh is in flight over the shown rows.
    pub fn is_stale(&self) -> bool {
        self.stale
    }

    /// The status-bar warnings from the last refresh cycle.
    pub fn status_warnings(&self) -> &[String] {
        &self.status_warnings
    }

    /// Whether the user asked to quit.
    pub fn is_done(&self) -> bool {
        self.done
    }

    /// The active filter.
    pub fn filter(&self) -> &FilterSet {
        &self.filter
    }

    /// When the shown snapshot was fetched, if any.
    pub fn fetched_at(&self) -> Option<SystemTime> {
        self.fetched_at
    }

    /// The detail pane state, `Some` exactly when [`ViewMode::Detail`] is shown.
    pub fn detail(&self) -> Option<&DetailState> {
        self.detail.as_ref()
    }

    /// The detail pane's requested vertical scroll offset (rows). The view clamps
    /// this to the wrapped content so an over-scroll never shows blank space.
    pub fn detail_scroll(&self) -> u16 {
        self.detail_scroll
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bd::{Dependency, Issue, IssueDetail};
    use std::time::{Duration, UNIX_EPOCH};

    fn row(repo: &str, id: &str, priority: i64) -> Row {
        Row {
            issue: Issue {
                id: id.to_string(),
                title: format!("title {id}"),
                status: "open".into(),
                priority,
                description: None,
                issue_type: None,
                owner: None,
                labels: Vec::new(),
                created_at: None,
                created_by: None,
                updated_at: None,
                dependency_count: None,
                dependent_count: None,
                comment_count: None,
            },
            repo_name: repo.to_string(),
        }
    }

    fn snapshot(rows: Vec<Row>) -> Snapshot {
        Snapshot {
            rows,
            fetched_at: UNIX_EPOCH + Duration::from_secs(1_700_000_000),
        }
    }

    /// A successful refresh completion carrying `rows` and no warnings.
    fn completed(rows: Vec<Row>) -> Msg {
        Msg::RefreshCompleted {
            snapshot: Some(snapshot(rows)),
            warnings: Vec::new(),
        }
    }

    /// An app advanced to `List` with the given rows via a `RefreshCompleted`.
    fn app_with(rows: Vec<Row>) -> App {
        let mut app = App::new();
        app.reduce(completed(rows));
        app
    }

    fn ids(rows: &[&Row]) -> Vec<String> {
        rows.iter().map(|r| r.issue.id.clone()).collect()
    }

    /// A boxed [`IssueDetail`] for `id` carrying `deps` blockers (boxed to match
    /// [`Msg::DetailReady`]'s payload).
    fn detail(id: &str, deps: Vec<Dependency>) -> Box<IssueDetail> {
        Box::new(IssueDetail {
            issue: Issue {
                id: id.to_string(),
                title: format!("title {id}"),
                status: "open".into(),
                priority: 2,
                description: Some("a description".into()),
                issue_type: Some("task".into()),
                owner: None,
                labels: Vec::new(),
                created_at: None,
                created_by: None,
                updated_at: None,
                dependency_count: Some(deps.len() as i64),
                dependent_count: None,
                comment_count: Some(0),
            },
            dependencies: deps,
        })
    }

    fn blocker(id: &str) -> Dependency {
        Dependency {
            id: id.to_string(),
            title: Some(format!("blocker {id}")),
            status: Some("open".into()),
            dependency_type: Some("blocks".into()),
        }
    }

    /// Open the detail for the current selection, returning the request token from
    /// the emitted `FetchDetail` (so tests echo the right token in `DetailReady`).
    fn open(app: &mut App) -> u64 {
        match app.reduce(Msg::OpenDetail).as_slice() {
            [Effect::FetchDetail { token, .. }] => *token,
            other => panic!("expected one FetchDetail, got {other:?}"),
        }
    }

    #[test]
    fn enter_requests_detail() {
        let mut app = app_with(vec![row("ra", "ra-1", 1)]);
        assert_eq!(app.view_mode(), ViewMode::List);

        let effects = app.reduce(Msg::OpenDetail);
        assert_eq!(
            effects,
            vec![Effect::FetchDetail {
                id: "ra-1".into(),
                token: 1
            }]
        );
        assert_eq!(app.view_mode(), ViewMode::Detail);
        assert!(
            matches!(app.detail(), Some(DetailState::Loading { id }) if id == "ra-1"),
            "the pane is loading the selected id: {:?}",
            app.detail()
        );
    }

    #[test]
    fn cursor_movement_does_not_fetch() {
        // Browsing the list must make zero bd calls (no FetchDetail effect).
        let mut app = app_with(vec![row("ra", "ra-1", 1), row("ra", "ra-2", 1)]);
        assert_eq!(app.reduce(Msg::SelectNext), Vec::new());
        assert_eq!(app.reduce(Msg::SelectPrev), Vec::new());
        assert_eq!(app.view_mode(), ViewMode::List, "still browsing the list");
    }

    #[test]
    fn open_detail_noop_on_empty_list() {
        // No selected row: Enter cannot open a detail and emits no effect.
        let mut app = app_with(vec![]);
        assert_eq!(app.selected_row(), None);
        assert_eq!(app.reduce(Msg::OpenDetail), Vec::new());
        assert_eq!(app.view_mode(), ViewMode::List);
        assert!(app.detail().is_none());
    }

    #[test]
    fn detail_ready_stores_for_matching_id() {
        let mut app = app_with(vec![row("ra", "ra-1", 1)]);
        let token = open(&mut app);

        app.reduce(Msg::DetailReady {
            token,
            detail: Ok(detail("ra-1", vec![blocker("ra-z70")])),
        });
        match app.detail() {
            Some(DetailState::Loaded(d)) => {
                assert_eq!(d.issue.id, "ra-1");
                assert_eq!(d.dependencies.len(), 1);
                assert_eq!(d.dependencies[0].id, "ra-z70");
            }
            other => panic!("expected Loaded, got {other:?}"),
        }
    }

    #[test]
    fn stale_detail_response_is_dropped() {
        let mut app = app_with(vec![row("ra", "ra-1", 1), row("ra", "ra-2", 1)]);
        let token = open(&mut app); // bound to ra-1

        // A response carrying a token that is not the current request is dropped.
        app.reduce(Msg::DetailReady {
            token: token + 99,
            detail: Ok(detail("ra-1", vec![])),
        });
        assert!(
            matches!(app.detail(), Some(DetailState::Loading { id }) if id == "ra-1"),
            "a mismatched-token response is dropped: {:?}",
            app.detail()
        );

        // The pane's own request still completes.
        app.reduce(Msg::DetailReady {
            token,
            detail: Ok(detail("ra-1", vec![])),
        });
        assert!(matches!(app.detail(), Some(DetailState::Loaded(_))));
    }

    #[test]
    fn same_id_reopen_drops_earlier_response() {
        // Open ra-1, Esc, reopen ra-1: the two fetches share an id but not a
        // token, so the first (slower) worker's late response must not overwrite
        // the pane the second request owns.
        let mut app = app_with(vec![row("ra", "ra-1", 1)]);
        let first = open(&mut app);
        app.reduce(Msg::Back);
        let second = open(&mut app);
        assert_ne!(first, second, "each open gets a fresh token");

        // The first request answers late (after the reopen): dropped.
        app.reduce(Msg::DetailReady {
            token: first,
            detail: Err("stale error".into()),
        });
        assert!(
            matches!(app.detail(), Some(DetailState::Loading { .. })),
            "the earlier request's late response is dropped: {:?}",
            app.detail()
        );

        // The second (current) request lands and is shown.
        app.reduce(Msg::DetailReady {
            token: second,
            detail: Ok(detail("ra-1", vec![])),
        });
        assert!(matches!(app.detail(), Some(DetailState::Loaded(_))));
    }

    #[test]
    fn detail_fetch_error_shows_message() {
        let mut app = app_with(vec![row("ra", "ra-1", 1)]);
        let token = open(&mut app);

        app.reduce(Msg::DetailReady {
            token,
            detail: Err("bd show failed: boom".into()),
        });
        match app.detail() {
            Some(DetailState::Error { id, message }) => {
                assert_eq!(id, "ra-1");
                assert!(message.contains("boom"), "message surfaced: {message}");
            }
            other => panic!("expected Error, got {other:?}"),
        }
        assert_eq!(app.view_mode(), ViewMode::Detail, "pane still open");
        assert_eq!(app.rows().len(), 1, "the list is intact behind the pane");
    }

    #[test]
    fn esc_returns_to_list() {
        let mut app = app_with(vec![row("ra", "ra-1", 1), row("ra", "ra-2", 1)]);
        app.reduce(Msg::SelectNext); // selection = 1 -> ra-2
        assert_eq!(app.selection(), Some(1));
        let token = open(&mut app);
        app.reduce(Msg::DetailReady {
            token,
            detail: Ok(detail("ra-2", vec![])),
        });

        app.reduce(Msg::Back);
        assert_eq!(app.view_mode(), ViewMode::List);
        assert!(app.detail().is_none());
        assert_eq!(
            app.selection(),
            Some(1),
            "selection preserved across detail"
        );
    }

    #[test]
    fn navigation_inert_while_detail_open() {
        // With the pane open, j/k must not move the underlying list selection (they
        // scroll the pane instead) and f/p are inert — otherwise Esc would return
        // to a different row than it was opened from.
        let mut app = app_with(vec![row("ra", "ra-1", 1), row("ra", "ra-2", 1)]);
        app.reduce(Msg::OpenDetail); // Detail, bound to ra-1, selection 0

        app.reduce(Msg::SelectNext);
        app.reduce(Msg::CycleRepoFilter);
        app.reduce(Msg::TogglePriorityFilter);
        assert_eq!(app.selection(), Some(0), "selection frozen under the pane");
        assert_eq!(app.filter().repo(), &RepoFilter::All, "filters frozen too");

        app.reduce(Msg::Back);
        assert_eq!(app.selection(), Some(0), "returns to the original row");
    }

    #[test]
    fn detail_scroll_moves_and_resets() {
        // In Detail mode j/k scroll the pane; the offset resets on open and close.
        let mut app = app_with(vec![row("ra", "ra-1", 1)]);
        open(&mut app);
        assert_eq!(app.detail_scroll(), 0, "opens at the top");

        app.reduce(Msg::SelectNext);
        app.reduce(Msg::SelectNext);
        assert_eq!(app.detail_scroll(), 2, "j scrolls down");
        app.reduce(Msg::SelectPrev);
        assert_eq!(app.detail_scroll(), 1, "k scrolls up");

        app.reduce(Msg::Back);
        assert_eq!(app.detail_scroll(), 0, "reset on close");
        open(&mut app);
        assert_eq!(app.detail_scroll(), 0, "reset on reopen");
    }

    #[test]
    fn refresh_under_detail_keeps_pane() {
        // A background refresh must not slam the open detail pane shut.
        let mut app = app_with(vec![row("ra", "ra-1", 1)]);
        let token = open(&mut app);
        app.reduce(Msg::DetailReady {
            token,
            detail: Ok(detail("ra-1", vec![])),
        });
        assert_eq!(app.view_mode(), ViewMode::Detail);

        app.reduce(completed(vec![row("ra", "ra-1", 1), row("ra", "ra-9", 2)]));
        assert_eq!(
            app.view_mode(),
            ViewMode::Detail,
            "the pane stays open across a refresh"
        );
        assert!(app.detail().is_some());
        assert_eq!(app.rows().len(), 2, "rows updated underneath the pane");
    }

    #[test]
    fn starts_in_loading_then_shows_rows() {
        let mut app = App::new();
        assert_eq!(app.view_mode(), ViewMode::Loading);
        assert!(app.rows().is_empty());
        assert_eq!(app.selection(), None);
        assert!(!app.is_done());

        // A refresh begins before any data: still loading.
        app.reduce(Msg::RefreshStarted);
        assert_eq!(app.view_mode(), ViewMode::Loading, "no rows yet -> Loading");

        // First snapshot: rows appear, list shown, selection at the top.
        app.reduce(completed(vec![row("ra", "ra-1", 1), row("ra", "ra-2", 2)]));
        assert_eq!(app.view_mode(), ViewMode::List);
        assert_eq!(app.rows().len(), 2);
        assert_eq!(app.selection(), Some(0));
        assert!(!app.is_stale());
    }

    #[test]
    fn selection_moves_and_clamps() {
        let mut app = app_with(vec![
            row("ra", "ra-1", 1),
            row("ra", "ra-2", 1),
            row("ra", "ra-3", 1),
        ]);

        assert_eq!(app.selection(), Some(0));
        app.reduce(Msg::SelectNext);
        assert_eq!(app.selection(), Some(1));
        app.reduce(Msg::SelectNext);
        assert_eq!(app.selection(), Some(2));
        // Clamps at the last row, never out of bounds.
        app.reduce(Msg::SelectNext);
        assert_eq!(app.selection(), Some(2));

        app.reduce(Msg::SelectPrev);
        assert_eq!(app.selection(), Some(1));
        app.reduce(Msg::SelectPrev);
        app.reduce(Msg::SelectPrev);
        assert_eq!(app.selection(), Some(0), "clamps at the first row");

        // Empty list: navigation is a safe no-op and nothing is selected.
        let mut empty = app_with(vec![]);
        assert_eq!(empty.selection(), None);
        empty.reduce(Msg::SelectNext);
        empty.reduce(Msg::SelectPrev);
        assert_eq!(empty.selection(), None);
        assert!(empty.selected_row().is_none());
    }

    #[test]
    fn repo_filter_cycles() {
        // repo-a appears first, then repo-b.
        let mut app = app_with(vec![
            row("repo-a", "ra-1", 1),
            row("repo-a", "ra-2", 1),
            row("repo-b", "rb-1", 1),
        ]);
        assert_eq!(app.filtered_rows().len(), 3, "All: every row visible");

        app.reduce(Msg::CycleRepoFilter);
        assert_eq!(app.filter().repo(), &RepoFilter::Only("repo-a".into()));
        assert_eq!(ids(&app.filtered_rows()), vec!["ra-1", "ra-2"]);
        assert_eq!(app.selection(), Some(0), "selection stays valid");

        app.reduce(Msg::CycleRepoFilter);
        assert_eq!(app.filter().repo(), &RepoFilter::Only("repo-b".into()));
        assert_eq!(ids(&app.filtered_rows()), vec!["rb-1"]);

        app.reduce(Msg::CycleRepoFilter);
        assert_eq!(app.filter().repo(), &RepoFilter::All, "cycles back to All");
        assert_eq!(app.filtered_rows().len(), 3);
    }

    #[test]
    fn priority_filter_toggles() {
        let mut app = app_with(vec![
            row("ra", "ra-0", 0),
            row("ra", "ra-1", 1),
            row("ra", "ra-2", 2),
            row("ra", "ra-3", 3),
        ]);
        assert_eq!(app.filtered_rows().len(), 4);

        app.reduce(Msg::TogglePriorityFilter);
        assert_eq!(app.filter().priority(), PriorityFilter::HighOnly);
        assert_eq!(
            ids(&app.filtered_rows()),
            vec!["ra-0", "ra-1"],
            "only P0/P1 visible"
        );

        app.reduce(Msg::TogglePriorityFilter);
        assert_eq!(app.filter().priority(), PriorityFilter::All);
        assert_eq!(app.filtered_rows().len(), 4, "toggles back to all");
    }

    #[test]
    fn refresh_while_stale_keeps_rows() {
        let mut app = app_with(vec![row("ra", "ra-1", 1), row("ra", "ra-2", 1)]);
        app.reduce(Msg::SelectNext);
        assert_eq!(app.selection(), Some(1));

        app.reduce(Msg::RefreshStarted);
        assert_eq!(app.rows().len(), 2, "old rows stay visible during refresh");
        assert_eq!(app.selection(), Some(1), "selection preserved");
        assert_eq!(app.view_mode(), ViewMode::List);
        assert!(app.is_stale(), "stale flag set while refreshing");
    }

    #[test]
    fn refresh_error_surfaces_in_status() {
        let mut app = app_with(vec![row("ra", "ra-1", 1)]);
        app.reduce(Msg::RefreshStarted);
        assert!(app.is_stale());

        // A refresh that succeeded but had per-repo trouble: the snapshot and its
        // warnings arrive together in one completion message.
        app.reduce(Msg::RefreshCompleted {
            snapshot: Some(snapshot(vec![row("ra", "ra-1", 1)])),
            warnings: vec![
                "export failed for repo-b".into(),
                "id prefix `dup` claimed by 2 repos".into(),
            ],
        });
        assert!(
            app.status_warnings()
                .iter()
                .any(|w| w.contains("export failed for repo-b")),
            "per-repo error surfaced: {:?}",
            app.status_warnings()
        );
        assert!(!app.is_stale(), "the refresh cycle concluded");
    }

    #[test]
    fn fatal_refresh_keeps_rows_and_surfaces_warning() {
        // A refresh that failed outright: no snapshot, but the stale view is kept
        // and the error is surfaced.
        let mut app = app_with(vec![row("ra", "ra-1", 1), row("ra", "ra-2", 1)]);
        app.reduce(Msg::RefreshStarted);
        app.reduce(Msg::RefreshCompleted {
            snapshot: None,
            warnings: vec!["hub sync failed".into()],
        });
        assert_eq!(
            app.rows().len(),
            2,
            "last-good rows kept on a failed refresh"
        );
        assert_eq!(app.view_mode(), ViewMode::List);
        assert!(!app.is_stale(), "the failed cycle still concludes");
        assert!(app.status_warnings().iter().any(|w| w.contains("hub sync")));
    }

    #[test]
    fn refresh_key_requests_refresh_effect() {
        let mut app = app_with(vec![row("ra", "ra-1", 1)]);
        let before = app.clone();

        let effects = app.reduce(Msg::Refresh);
        assert_eq!(effects, vec![Effect::Refresh]);
        // Marks the shown rows stale/in-flight, but touches nothing else: the
        // runtime spawns the worker.
        assert!(app.is_stale());
        assert_eq!(app.rows().len(), before.rows().len());
        assert_eq!(app.selection(), before.selection());
        assert_eq!(app.view_mode(), before.view_mode());
        assert!(!app.is_done());
    }

    #[test]
    fn refresh_is_deduped_while_in_flight() {
        // A second `r` (or a key-repeat) while a refresh is pending must not spawn
        // an overlapping worker whose out-of-order completion could clobber a
        // newer snapshot.
        let mut app = app_with(vec![row("ra", "ra-1", 1)]);

        assert_eq!(app.reduce(Msg::Refresh), vec![Effect::Refresh]);
        assert_eq!(
            app.reduce(Msg::Refresh),
            Vec::new(),
            "no second effect while a refresh is in flight"
        );

        // Once the cycle concludes, a fresh request is honored again.
        app.reduce(completed(vec![row("ra", "ra-1", 1)]));
        assert!(!app.is_stale());
        assert_eq!(app.reduce(Msg::Refresh), vec![Effect::Refresh]);
    }

    #[test]
    fn success_with_warnings_completes_atomically() {
        // Regression: a successful-with-warnings refresh must conclude in ONE
        // message. If it split into snapshot-then-warnings, an `r` in the gap
        // would slip past the dedup guard and the trailing warnings message would
        // then clear the *new* refresh's in-flight flag. Here the single
        // completion clears `stale` exactly once, and the interleaved second
        // refresh is a distinct, still-guarded cycle.
        let mut app = app_with(vec![row("ra", "ra-1", 1)]);

        assert_eq!(app.reduce(Msg::Refresh), vec![Effect::Refresh]);
        assert!(app.is_stale());
        // First cycle concludes atomically with a snapshot and warnings.
        app.reduce(Msg::RefreshCompleted {
            snapshot: Some(snapshot(vec![row("ra", "ra-2", 1)])),
            warnings: vec!["export failed for repo-b".into()],
        });
        assert!(!app.is_stale());
        assert_eq!(app.status_warnings().len(), 1);

        // A new refresh starts its own guarded cycle; no leftover completion
        // message from the first cycle exists to clear it.
        assert_eq!(app.reduce(Msg::Refresh), vec![Effect::Refresh]);
        assert!(app.is_stale());
        assert_eq!(
            app.reduce(Msg::Refresh),
            Vec::new(),
            "the second cycle is still deduped"
        );
    }

    #[test]
    fn startup_refresh_holds_the_in_flight_slot() {
        // The runtime spawns an initial refresh at launch without going through
        // `Msg::Refresh`, so a brand-new app must already be in-flight: an `r`
        // that races the initial worker's `RefreshStarted` is deduped, not a
        // second worker.
        let mut app = App::new();
        assert!(app.is_stale(), "a fresh app is born in-flight");
        assert_eq!(
            app.reduce(Msg::Refresh),
            Vec::new(),
            "an immediate r is deduped against the startup refresh"
        );

        // When the initial refresh concludes, the slot frees and r works again.
        app.reduce(completed(vec![row("ra", "ra-1", 1)]));
        assert!(!app.is_stale());
        assert_eq!(app.reduce(Msg::Refresh), vec![Effect::Refresh]);
    }

    #[test]
    fn quit_msg_sets_done() {
        let mut app = app_with(vec![row("ra", "ra-1", 1)]);
        assert!(!app.is_done());
        app.reduce(Msg::Quit);
        assert!(app.is_done());
    }

    #[test]
    fn filters_persist_and_recompute_across_refresh() {
        let mut app = app_with(vec![row("repo-a", "ra-1", 1), row("repo-b", "rb-1", 1)]);
        app.reduce(Msg::CycleRepoFilter); // Only("repo-a")
        assert_eq!(app.filter().repo(), &RepoFilter::Only("repo-a".into()));

        // A new snapshot (different rows, still has repo-a) keeps the filter.
        app.reduce(completed(vec![
            row("repo-a", "ra-9", 1),
            row("repo-a", "ra-8", 2),
            row("repo-b", "rb-9", 1),
        ]));
        assert_eq!(
            app.filter().repo(),
            &RepoFilter::Only("repo-a".into()),
            "the active filter survives a refresh"
        );
        assert_eq!(ids(&app.filtered_rows()), vec!["ra-9", "ra-8"]);
        assert_eq!(app.selection(), Some(0), "selection valid after recompute");
    }

    #[test]
    fn selection_invariant_holds_under_random_messages() {
        // A deterministic LCG (no rand dep) drives a long message sequence; after
        // every step the selection invariant must hold.
        let mut seed: u64 = 0x1234_5678_9abc_def0;
        let mut next = || {
            seed = seed
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            (seed >> 33) as u32
        };

        let sample_sets: Vec<Vec<Row>> = vec![
            vec![],
            vec![row("repo-a", "ra-1", 0)],
            vec![
                row("repo-a", "ra-1", 0),
                row("repo-a", "ra-2", 2),
                row("repo-b", "rb-1", 1),
                row("repo-b", "rb-2", 3),
            ],
            vec![row("repo-b", "rb-9", 1), row("repo-c", "rc-9", 2)],
        ];

        let mut app = App::new();
        for _ in 0..5_000 {
            let msg = match next() % 6 {
                0 => Msg::SelectNext,
                1 => Msg::SelectPrev,
                2 => Msg::CycleRepoFilter,
                3 => Msg::TogglePriorityFilter,
                4 => Msg::RefreshStarted,
                _ => {
                    let set = &sample_sets[(next() as usize) % sample_sets.len()];
                    completed(set.clone())
                }
            };
            app.reduce(msg);

            let visible = app.filtered_rows();
            match app.selection() {
                None => {
                    assert!(visible.is_empty(), "no selection only when nothing visible");
                    assert!(app.selected_row().is_none());
                }
                Some(i) => {
                    assert!(
                        i < visible.len(),
                        "selection {i} within {} rows",
                        visible.len()
                    );
                    assert_eq!(
                        app.selected_row().map(|r| &r.issue.id),
                        Some(&visible[i].issue.id),
                        "selected_row agrees with the selection index"
                    );
                }
            }
        }
    }
}

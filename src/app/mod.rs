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
    /// A cross-repo search concluded (runtime search worker → app). `token` echoes
    /// the request's generation (see [`Effect::Search`]) so a superseded query's
    /// late reply is dropped. `rows` are **already attributed** `Row`s (the worker
    /// ran them through the same `PrefixMap` path as ready rows) on success, or a
    /// pre-formatted, sanitized message on failure — keeping this core free of
    /// `bd`/`PrefixMap` types and preserving `Msg`'s `Eq` derive.
    SearchResults {
        token: u64,
        rows: Result<Vec<Row>, String>,
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
    /// Open cross-repo search (`/`): enter [`ViewMode::Search`] editing the query.
    OpenSearch,
    /// Append a character to the search query (a key typed while the search input
    /// is focused).
    SearchInput(char),
    /// Delete the last character of the search query (`Backspace` while editing).
    SearchBackspace,
    /// Run the current search query (`Enter` while editing); `reduce` emits
    /// [`Effect::Search`] unless the query is empty/whitespace.
    SubmitSearch,
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
    /// Run `bd search <query> --json` against the hub (the `Enter` keypress while
    /// editing → `Msg::SubmitSearch`). `token` is the request's generation; the
    /// runtime runs the search on a worker thread, attributes the results, and
    /// echoes `token` back in [`Msg::SearchResults`] so a superseded query's late
    /// response is dropped.
    Search { query: String, token: u64 },
}

/// Which screen the app is showing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ViewMode {
    /// Before the first snapshot arrives.
    Loading,
    /// The cross-repo ready list.
    List,
    /// One issue's detail pane (opened with `Enter`, left with `Esc`).
    Detail,
    /// Cross-repo search: a query input and its results (opened with `/`).
    Search,
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

/// A navigable, filterable list of rows with a clamped selection — the shared
/// "row list" state the ready list and the search results both use, so selection,
/// filtering, and navigation live in **one** place (Slice 11's required refactor).
/// The Slice 8 selection invariant (`filtered_ix` empty ⇒ `selection == 0` and
/// `selected_row() == None`; else `selection < filtered_ix.len()`) is enforced
/// here after every mutation.
#[derive(Debug, Clone, Default)]
struct RowList {
    /// Every row (unfiltered), in display (sorted) order.
    rows: Vec<Row>,
    /// Indices into `rows` passing `filter`, in display order.
    filtered_ix: Vec<usize>,
    /// Offset into `filtered_ix` (never into `rows`).
    selection: usize,
    /// The active filter across both axes.
    filter: FilterSet,
}

impl RowList {
    /// Replace the rows, keeping the active filter, then recompute + re-clamp.
    fn set_rows(&mut self, rows: Vec<Row>) {
        self.rows = rows;
        self.recompute();
    }

    /// Rebuild `filtered_ix` under the current filter and re-clamp `selection` —
    /// the one place the selection invariant is re-established.
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

    /// Move the selection one row down, clamping at the last row (safe when empty).
    fn select_next(&mut self) {
        if !self.filtered_ix.is_empty() {
            self.selection = (self.selection + 1).min(self.filtered_ix.len() - 1);
        }
    }

    /// Move the selection one row up, clamping at the first row (safe when empty).
    fn select_prev(&mut self) {
        self.selection = self.selection.saturating_sub(1);
    }

    /// Cycle the repo filter `All → repo₀ → … → All`, then recompute.
    fn cycle_repo_filter(&mut self) {
        self.filter.repo = self.next_repo_filter();
        self.recompute();
    }

    /// Toggle the priority filter `All ↔ P0/P1-only`, then recompute.
    fn toggle_priority_filter(&mut self) {
        self.filter.priority = match self.filter.priority {
            PriorityFilter::All => PriorityFilter::HighOnly,
            PriorityFilter::HighOnly => PriorityFilter::All,
        };
        self.recompute();
    }

    /// The next repo filter when cycling with `f`: `All → repo₀ → … → repoₙ₋₁ →
    /// All`, over the distinct `repo_name`s in first-appearance (display) order.
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

    /// Relocate the selection onto the visible row whose issue id is `id`,
    /// returning whether it was found (else the selection is left as-is).
    fn select_id(&mut self, id: &str) -> bool {
        if let Some(pos) = self
            .filtered_ix
            .iter()
            .position(|&i| self.rows[i].issue.id == id)
        {
            self.selection = pos;
            true
        } else {
            false
        }
    }

    /// The selection offset into the filtered rows, or `None` when nothing shows.
    fn selection(&self) -> Option<usize> {
        if self.filtered_ix.is_empty() {
            None
        } else {
            Some(self.selection)
        }
    }

    /// The selected row, or `None` when nothing is visible.
    fn selected_row(&self) -> Option<&Row> {
        self.filtered_ix.get(self.selection).map(|&i| &self.rows[i])
    }

    /// The rows passing the current filter, in display order.
    fn filtered_rows(&self) -> Vec<&Row> {
        self.filtered_ix.iter().map(|&i| &self.rows[i]).collect()
    }
}

/// A phase of the cross-repo search flow (see [`ViewMode::Search`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SearchPhase {
    /// The query input is focused; keys edit the query.
    Editing,
    /// A query was submitted; awaiting results.
    Loading,
    /// Attributed results are shown and browsable (the list may be empty).
    Results,
    /// The `bd search` call failed; the pre-formatted message is shown instead of
    /// a misleading empty result.
    Error(String),
}

/// The cross-repo search state, `Some` exactly while the search flow is live (in
/// [`ViewMode::Search`], or in [`ViewMode::Detail`] opened from a search result).
#[derive(Debug, Clone)]
struct SearchState {
    /// The query being edited / that produced the current results.
    query: String,
    /// Which phase of the flow.
    phase: SearchPhase,
    /// The generation of the in-flight/last request, matched against
    /// [`Msg::SearchResults`] to drop a superseded query's late reply.
    token: u64,
    /// The attributed results, sharing all navigation/filter code with the ready
    /// list.
    list: RowList,
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
    /// The cross-repo ready list (rows, filter, selection), always maintained by
    /// refresh even while a search overlays it.
    ready: RowList,
    /// The cross-repo search flow, `Some` while it is live (see [`SearchState`]).
    /// The ready list stays untouched behind it, so `Esc` restores it exactly.
    search: Option<SearchState>,
    /// A monotonic generation stamped on each search request, echoed by the worker
    /// so a superseded query's late results are dropped (mirrors `detail_seq`).
    search_seq: u64,
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
            ready: RowList::default(),
            search: None,
            search_seq: 0,
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
                    // With a detail opened *from the ready list*, remember the
                    // opened issue so the refresh's re-sort does not move the
                    // selection to a *different* row: the pane pins one issue, and
                    // `Esc` must return to it. (Slice 8 decision 5 otherwise
                    // preserves only the selection index; this narrower rule applies
                    // just while a ready-list detail is open.) A detail opened from
                    // a *search* result must NOT relocate the hidden ready selection
                    // — its id may also be a ready row, and moving to it would
                    // corrupt the ready selection `Esc` restores. So relocate only
                    // when no search is active (`search.is_none()`).
                    let opened_id = if self.search.is_none() {
                        self.detail.as_ref().map(|d| d.id().to_string())
                    } else {
                        None
                    };
                    // A refresh always updates the ready list, even under a search
                    // or detail overlay; `set_rows` keeps the active filter and
                    // re-clamps the selection. (`None` keeps the last-good rows.)
                    self.ready.set_rows(snapshot.rows);
                    self.fetched_at = Some(snapshot.fetched_at);
                    // Only promote the first-snapshot transition; a refresh landing
                    // under an open `Detail`/`Search` overlay must not slam it shut
                    // (the 1s cadence would otherwise eject the reader).
                    if self.view_mode == ViewMode::Loading {
                        self.view_mode = ViewMode::List;
                    }
                    // Relocate the ready selection onto the opened ready issue if it
                    // survived the refresh; otherwise the clamped index stands.
                    if let Some(id) = opened_id {
                        self.ready.select_id(&id);
                    }
                }
                // The runtime sends the full warning set per cycle, so replace.
                self.status_warnings = warnings;
                // The single, atomic point that ends the in-flight cycle.
                self.stale = false;
            }
            // `j`/`k` move the selection of the active browsing list (ready in
            // `List`, the results in `Search`+`Results`), but scroll the pane in
            // `Detail`. While a detail or the search editor is up, the ready
            // selection never moves. The view clamps the scroll to the wrapped
            // content height, so keep `reduce` dimension-free.
            Msg::SelectNext => {
                if self.view_mode == ViewMode::Detail {
                    self.detail_scroll = self.detail_scroll.saturating_add(1);
                } else if let Some(list) = self.browsing_list_mut() {
                    list.select_next();
                }
            }
            Msg::SelectPrev => {
                if self.view_mode == ViewMode::Detail {
                    self.detail_scroll = self.detail_scroll.saturating_sub(1);
                } else if let Some(list) = self.browsing_list_mut() {
                    list.select_prev();
                }
            }
            // Filters act on the active browsing list; inert while a pane/editor is up.
            Msg::CycleRepoFilter => {
                if let Some(list) = self.browsing_list_mut() {
                    list.cycle_repo_filter();
                }
            }
            Msg::TogglePriorityFilter => {
                if let Some(list) = self.browsing_list_mut() {
                    list.toggle_priority_filter();
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
                // Open only from a browsing list (the ready list, or search
                // results), and only with a selected row: exactly one `bd show`
                // per Enter (a second Enter in `Detail` is a no-op, an empty list
                // has no row), and cursor movement never fetches. The search state
                // is left intact, so `Back` returns to the results behind the pane.
                if self.is_browsing()
                    && let Some(row) = self.active().selected_row()
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
            Msg::Back => match self.view_mode {
                // Leave the detail pane, returning to the search results it was
                // opened from (if any) or the ready list; the selection is
                // untouched, so it is preserved across an open/close.
                ViewMode::Detail => {
                    self.view_mode = if self.search.is_some() {
                        ViewMode::Search
                    } else {
                        ViewMode::List
                    };
                    self.detail = None;
                    self.detail_scroll = 0;
                }
                // From the query editor, `Esc` exits search and restores the ready
                // list (never touched, so this is exact). From the results/loading/
                // error phases it returns to editing so the query can be refined.
                ViewMode::Search => {
                    if let Some(s) = &mut self.search {
                        if matches!(s.phase, SearchPhase::Editing) {
                            self.view_mode = ViewMode::List;
                            self.search = None;
                        } else {
                            s.phase = SearchPhase::Editing;
                        }
                    }
                }
                ViewMode::List | ViewMode::Loading => {}
            },
            Msg::OpenSearch => match self.view_mode {
                // `/` from the ready list opens an empty query editor; the ready
                // list is preserved behind it.
                ViewMode::List => {
                    self.view_mode = ViewMode::Search;
                    self.search = Some(SearchState {
                        query: String::new(),
                        phase: SearchPhase::Editing,
                        token: 0,
                        list: RowList::default(),
                    });
                }
                // `/` while viewing results (not editing — a typed `/` is input)
                // restarts a fresh query.
                ViewMode::Search => {
                    if let Some(s) = &mut self.search {
                        s.query.clear();
                        s.phase = SearchPhase::Editing;
                    }
                }
                ViewMode::Detail | ViewMode::Loading => {}
            },
            Msg::SearchInput(c) => {
                if let Some(s) = &mut self.search
                    && matches!(s.phase, SearchPhase::Editing)
                {
                    s.query.push(c);
                }
            }
            Msg::SearchBackspace => {
                if let Some(s) = &mut self.search
                    && matches!(s.phase, SearchPhase::Editing)
                {
                    s.query.pop();
                }
            }
            Msg::SubmitSearch => {
                // Only a non-empty query submits; bump the request generation,
                // enter `Loading`, and ask the runtime to run the search.
                let ready = matches!(
                    self.search.as_ref().map(|s| &s.phase),
                    Some(SearchPhase::Editing)
                ) && self
                    .search
                    .as_ref()
                    .is_some_and(|s| !s.query.trim().is_empty());
                if ready {
                    self.search_seq += 1;
                    let token = self.search_seq;
                    let s = self.search.as_mut().expect("checked Some above");
                    s.token = token;
                    s.phase = SearchPhase::Loading;
                    let query = s.query.clone();
                    return vec![Effect::Search { query, token }];
                }
            }
            Msg::SearchResults { token, rows } => {
                // Accept only the response for the current, still-pending query; a
                // superseded one (re-submitted, or `Esc`'d back to editing) is
                // dropped by the token + `Loading`-phase guard.
                if let Some(s) = &mut self.search
                    && token == s.token
                    && matches!(s.phase, SearchPhase::Loading)
                {
                    match rows {
                        Ok(rows) => {
                            s.list.set_rows(rows);
                            s.phase = SearchPhase::Results;
                        }
                        Err(message) => {
                            s.list.set_rows(Vec::new());
                            s.phase = SearchPhase::Error(message);
                        }
                    }
                }
            }
            // Placeholder: Slice 12 owns copy-context.
            Msg::CopyContext => {}
            Msg::Quit => self.done = true,
        }
        Vec::new()
    }

    /// The list the read accessors and the view reflect: the search results while
    /// the search flow is live (including a detail opened from a result), else the
    /// ready list. So one read API renders whichever list is active.
    fn active(&self) -> &RowList {
        match &self.search {
            Some(s) => &s.list,
            None => &self.ready,
        }
    }

    /// The list a navigation/filter key should act on right now, or `None` when
    /// keys don't move a selection (the detail pane scrolls; the search editor and
    /// its loading phase take no navigation). `List` acts on the ready list;
    /// `Search`+`Results` on the results.
    fn browsing_list_mut(&mut self) -> Option<&mut RowList> {
        match self.view_mode {
            ViewMode::List => Some(&mut self.ready),
            ViewMode::Search => match &mut self.search {
                Some(s) if matches!(s.phase, SearchPhase::Results) => Some(&mut s.list),
                _ => None,
            },
            ViewMode::Detail | ViewMode::Loading => None,
        }
    }

    /// Whether a navigation list is currently browsable (so `Enter` may open a
    /// detail): the ready list, or search results.
    fn is_browsing(&self) -> bool {
        matches!(self.view_mode, ViewMode::List)
            || (self.view_mode == ViewMode::Search
                && matches!(
                    self.search.as_ref().map(|s| &s.phase),
                    Some(SearchPhase::Results)
                ))
    }

    // ---- Accessors (the Slice 9 view's read API) ----

    /// The current screen.
    pub fn view_mode(&self) -> ViewMode {
        self.view_mode
    }

    /// Every row of the active list (unfiltered), in display order.
    pub fn rows(&self) -> &[Row] {
        &self.active().rows
    }

    /// The active list's rows passing its filter, in display order.
    pub fn filtered_rows(&self) -> Vec<&Row> {
        self.active().filtered_rows()
    }

    /// The selection offset into [`App::filtered_rows`], or `None` when nothing
    /// is visible.
    pub fn selection(&self) -> Option<usize> {
        self.active().selection()
    }

    /// The selected row, or `None` when nothing is visible.
    pub fn selected_row(&self) -> Option<&Row> {
        self.active().selected_row()
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

    /// The active list's filter.
    pub fn filter(&self) -> &FilterSet {
        &self.active().filter
    }

    /// The current search query, if the search flow is live.
    pub fn search_query(&self) -> Option<&str> {
        self.search.as_ref().map(|s| s.query.as_str())
    }

    /// The current search phase, if the search flow is live.
    pub fn search_phase(&self) -> Option<&SearchPhase> {
        self.search.as_ref().map(|s| &s.phase)
    }

    /// Whether the search query input is focused (so a key edits the query rather
    /// than acting as a command). Read by the runtime to route key mapping.
    pub fn search_editing(&self) -> bool {
        matches!(self.search_phase(), Some(SearchPhase::Editing))
    }

    /// The number of rows the current search returned (0 when not in results).
    pub fn search_result_count(&self) -> usize {
        self.search.as_ref().map(|s| s.list.rows.len()).unwrap_or(0)
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

    /// Drive `OpenSearch → type each char → SubmitSearch` from the list, returning
    /// the search request token from the emitted `Effect::Search`.
    fn submit(app: &mut App, query: &str) -> u64 {
        app.reduce(Msg::OpenSearch);
        for c in query.chars() {
            app.reduce(Msg::SearchInput(c));
        }
        match app.reduce(Msg::SubmitSearch).as_slice() {
            [Effect::Search { token, query: q }] => {
                assert_eq!(q, query, "the effect carries the typed query");
                *token
            }
            other => panic!("expected one Search effect, got {other:?}"),
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
    fn refresh_under_detail_preserves_opened_row() {
        // Open the detail on ra-1 (selection 0), then a refresh reorders the rows
        // so ra-1 moves. The selection must follow the opened issue, so Esc
        // returns to ra-1 rather than whatever now sits at index 0.
        let mut app = app_with(vec![row("ra", "ra-1", 1), row("ra", "ra-2", 2)]);
        let token = open(&mut app);
        app.reduce(Msg::DetailReady {
            token,
            detail: Ok(detail("ra-1", vec![])),
        });

        app.reduce(completed(vec![
            row("ra", "ra-0", 0), // new row jumps to the front
            row("ra", "ra-2", 2),
            row("ra", "ra-1", 1), // ra-1 now at index 2
        ]));

        app.reduce(Msg::Back);
        assert_eq!(
            app.selected_row().map(|r| r.issue.id.as_str()),
            Some("ra-1"),
            "selection follows the opened issue across the refresh re-sort"
        );
    }

    #[test]
    fn refresh_under_detail_falls_back_when_opened_row_gone() {
        // If the opened issue vanishes from the refreshed rows, the selection
        // falls back to the clamped index (no panic, still a valid selection).
        let mut app = app_with(vec![row("ra", "ra-1", 1), row("ra", "ra-2", 2)]);
        let token = open(&mut app);
        app.reduce(Msg::DetailReady {
            token,
            detail: Ok(detail("ra-1", vec![])),
        });

        app.reduce(completed(vec![row("ra", "ra-2", 2), row("ra", "ra-3", 2)]));
        app.reduce(Msg::Back);
        assert!(app.selected_row().is_some(), "a valid selection remains");
    }

    // ---- Cross-repo search (Slice 11) ----

    #[test]
    fn slash_opens_search_input() {
        let mut app = app_with(vec![row("ra", "ra-1", 1), row("ra", "ra-2", 1)]);
        app.reduce(Msg::OpenSearch);
        assert_eq!(app.view_mode(), ViewMode::Search);
        assert!(app.search_editing(), "the query input is focused");
        assert_eq!(app.search_query(), Some(""));
        // While editing, navigation keys don't move a selection (they are text
        // now): the active (empty) search list has no selection.
        app.reduce(Msg::SelectNext);
        assert_eq!(app.selection(), None);
    }

    #[test]
    fn typing_edits_query() {
        let mut app = app_with(vec![row("ra", "ra-1", 1)]);
        app.reduce(Msg::OpenSearch);
        app.reduce(Msg::SearchInput('f'));
        app.reduce(Msg::SearchInput('o'));
        app.reduce(Msg::SearchInput('o'));
        assert_eq!(app.search_query(), Some("foo"));
        app.reduce(Msg::SearchBackspace);
        assert_eq!(app.search_query(), Some("fo"));
        // Backspace past empty is a safe no-op.
        app.reduce(Msg::SearchBackspace);
        app.reduce(Msg::SearchBackspace);
        app.reduce(Msg::SearchBackspace);
        assert_eq!(app.search_query(), Some(""));
    }

    #[test]
    fn enter_submits_search_effect() {
        let mut app = app_with(vec![row("ra", "ra-1", 1)]);
        app.reduce(Msg::OpenSearch);
        for c in "foo".chars() {
            app.reduce(Msg::SearchInput(c));
        }
        let effects = app.reduce(Msg::SubmitSearch);
        assert_eq!(
            effects,
            vec![Effect::Search {
                query: "foo".into(),
                token: 1
            }]
        );
        assert_eq!(app.search_phase(), Some(&SearchPhase::Loading));
    }

    #[test]
    fn empty_query_no_ops() {
        let mut app = app_with(vec![row("ra", "ra-1", 1)]);
        app.reduce(Msg::OpenSearch);
        assert_eq!(
            app.reduce(Msg::SubmitSearch),
            Vec::new(),
            "an empty query submits nothing"
        );
        assert!(app.search_editing(), "and stays in the editor");
        // Whitespace-only is also treated as empty.
        app.reduce(Msg::SearchInput(' '));
        assert_eq!(app.reduce(Msg::SubmitSearch), Vec::new());
        assert!(app.search_editing());
    }

    #[test]
    fn results_replace_rows_with_attribution() {
        let mut app = app_with(vec![row("ra", "ra-1", 1)]);
        let token = submit(&mut app, "foo");

        // The worker delivers already-attributed rows (its repo_name carried).
        app.reduce(Msg::SearchResults {
            token,
            rows: Ok(vec![
                row("megaclock", "mc-1", 0),
                row("session-tui", "ra-9", 2),
            ]),
        });
        assert_eq!(app.view_mode(), ViewMode::Search);
        assert_eq!(app.search_phase(), Some(&SearchPhase::Results));
        assert_eq!(ids(&app.filtered_rows()), vec!["mc-1", "ra-9"]);
        assert_eq!(
            app.filtered_rows()[0].repo_name,
            "megaclock",
            "attribution flows through the same PrefixMap path as ready rows"
        );
        assert_eq!(app.selection(), Some(0));
    }

    #[test]
    fn stale_search_results_dropped() {
        let mut app = app_with(vec![row("ra", "ra-1", 1)]);
        let first = submit(&mut app, "foo");

        // A non-current token is dropped: the phase stays Loading.
        app.reduce(Msg::SearchResults {
            token: first + 99,
            rows: Ok(vec![row("ra", "ra-1", 1)]),
        });
        assert_eq!(app.search_phase(), Some(&SearchPhase::Loading));

        // Re-submit a new query: a fresh token supersedes the first.
        app.reduce(Msg::Back); // Loading -> Editing (query preserved)
        for c in "bar".chars() {
            app.reduce(Msg::SearchInput(c));
        }
        let second = match app.reduce(Msg::SubmitSearch).as_slice() {
            [Effect::Search { token, .. }] => *token,
            other => panic!("expected Search, got {other:?}"),
        };
        assert_ne!(first, second, "each submit gets a fresh token");

        // The first request answers late (after the re-submit): dropped.
        app.reduce(Msg::SearchResults {
            token: first,
            rows: Ok(vec![row("ra", "ra-1", 1)]),
        });
        assert_eq!(
            app.search_phase(),
            Some(&SearchPhase::Loading),
            "the superseded query's late reply is dropped"
        );
        // The current request lands.
        app.reduce(Msg::SearchResults {
            token: second,
            rows: Ok(vec![row("ra", "ra-2", 2)]),
        });
        assert_eq!(app.search_phase(), Some(&SearchPhase::Results));
        assert_eq!(ids(&app.filtered_rows()), vec!["ra-2"]);
    }

    #[test]
    fn esc_restores_ready_list() {
        // A ready list with an active filter and a non-zero selection.
        let mut app = app_with(vec![
            row("repo-a", "ra-1", 1),
            row("repo-a", "ra-2", 1),
            row("repo-b", "rb-1", 1),
        ]);
        app.reduce(Msg::CycleRepoFilter); // Only("repo-a"): shows ra-1, ra-2
        app.reduce(Msg::SelectNext); // selection 1 -> ra-2
        let ready_filter = app.filter().clone();
        let ready_selection = app.selection();
        let ready_ids = ids(&app.filtered_rows());
        assert_eq!(ready_ids, vec!["ra-1", "ra-2"]);
        assert_eq!(ready_selection, Some(1));

        let token = submit(&mut app, "foo");
        app.reduce(Msg::SearchResults {
            token,
            rows: Ok(vec![row("megaclock", "mc-1", 0)]),
        });

        // Esc from the results returns to editing, query preserved (refine path).
        app.reduce(Msg::Back);
        assert_eq!(app.view_mode(), ViewMode::Search);
        assert_eq!(app.search_phase(), Some(&SearchPhase::Editing));
        assert_eq!(
            app.search_query(),
            Some("foo"),
            "query preserved for refining"
        );

        // Esc from the editor exits search and restores the ready list exactly.
        app.reduce(Msg::Back);
        assert_eq!(app.view_mode(), ViewMode::List);
        assert!(app.search_query().is_none(), "search state cleared");
        assert_eq!(app.filter(), &ready_filter, "ready filter restored");
        assert_eq!(app.selection(), ready_selection, "ready selection restored");
        assert_eq!(ids(&app.filtered_rows()), ready_ids, "ready rows restored");
    }

    #[test]
    fn search_detail_and_back() {
        let mut app = app_with(vec![row("ra", "ra-1", 1)]);
        let token = submit(&mut app, "foo");
        app.reduce(Msg::SearchResults {
            token,
            rows: Ok(vec![
                row("megaclock", "mc-1", 0),
                row("megaclock", "mc-2", 1),
            ]),
        });
        app.reduce(Msg::SelectNext); // select mc-2 in the results
        assert_eq!(
            app.selected_row().map(|r| r.issue.id.as_str()),
            Some("mc-2")
        );

        // Enter opens the detail on the selected search result.
        let effects = app.reduce(Msg::OpenDetail);
        let dtoken = match effects.as_slice() {
            [Effect::FetchDetail { id, token }] => {
                assert_eq!(id, "mc-2");
                *token
            }
            other => panic!("expected one FetchDetail, got {other:?}"),
        };
        assert_eq!(app.view_mode(), ViewMode::Detail);

        app.reduce(Msg::DetailReady {
            token: dtoken,
            detail: Ok(detail("mc-2", vec![])),
        });
        // Back returns to the search results (not the ready list), selection intact.
        app.reduce(Msg::Back);
        assert_eq!(app.view_mode(), ViewMode::Search);
        assert_eq!(app.search_phase(), Some(&SearchPhase::Results));
        assert_eq!(
            app.selected_row().map(|r| r.issue.id.as_str()),
            Some("mc-2"),
            "the results selection survives the detail round-trip"
        );
    }

    #[test]
    fn search_error_shows_message() {
        let mut app = app_with(vec![row("ra", "ra-1", 1)]);
        let token = submit(&mut app, "foo");
        app.reduce(Msg::SearchResults {
            token,
            rows: Err("bd search failed: boom".into()),
        });
        match app.search_phase() {
            Some(SearchPhase::Error(msg)) => assert!(msg.contains("boom"), "message: {msg}"),
            other => panic!("expected Error, got {other:?}"),
        }
        // The ready list is intact behind the search.
        app.reduce(Msg::Back); // Error -> Editing
        app.reduce(Msg::Back); // Editing -> List
        assert_eq!(ids(&app.filtered_rows()), vec!["ra-1"]);
    }

    #[test]
    fn refresh_under_search_updates_ready_not_view() {
        let mut app = app_with(vec![row("ra", "ra-1", 1)]);
        let token = submit(&mut app, "foo");
        app.reduce(Msg::SearchResults {
            token,
            rows: Ok(vec![row("megaclock", "mc-1", 0)]),
        });

        // A background refresh lands under the search: it updates the ready list
        // but must not change the shown results or eject the user from search.
        app.reduce(completed(vec![row("ra", "ra-1", 1), row("ra", "ra-9", 2)]));
        assert_eq!(app.view_mode(), ViewMode::Search);
        assert_eq!(
            ids(&app.filtered_rows()),
            vec!["mc-1"],
            "the visible search results are unchanged by the refresh"
        );

        // Leaving search reveals the refreshed ready list.
        app.reduce(Msg::Back); // Results -> Editing
        app.reduce(Msg::Back); // Editing -> List
        assert_eq!(
            ids(&app.filtered_rows()),
            vec!["ra-1", "ra-9"],
            "the ready list reflects the refresh that landed during search"
        );
    }

    #[test]
    fn refresh_under_search_detail_preserves_ready_selection() {
        // A detail opened from a search result must not hijack the hidden ready
        // selection when a refresh lands — even when the searched id is ALSO a
        // ready row. Otherwise backing all the way out lands on the searched
        // issue instead of the ready row selected before search.
        let mut app = app_with(vec![row("ra", "ra-1", 1), row("ra", "ra-2", 1)]);
        assert_eq!(
            app.selected_row().map(|r| r.issue.id.as_str()),
            Some("ra-1"),
            "ready starts selected on ra-1"
        );

        // Search returns ra-2 (which is also a ready row); open its detail.
        let token = submit(&mut app, "foo");
        app.reduce(Msg::SearchResults {
            token,
            rows: Ok(vec![row("ra", "ra-2", 1)]),
        });
        let dtoken = match app.reduce(Msg::OpenDetail).as_slice() {
            [Effect::FetchDetail { id, token }] => {
                assert_eq!(id, "ra-2");
                *token
            }
            other => panic!("expected one FetchDetail, got {other:?}"),
        };
        app.reduce(Msg::DetailReady {
            token: dtoken,
            detail: Ok(detail("ra-2", vec![])),
        });

        // A refresh lands under the search-opened detail (same ready rows).
        app.reduce(completed(vec![row("ra", "ra-1", 1), row("ra", "ra-2", 1)]));

        // Back out fully: detail -> results -> editing -> list.
        app.reduce(Msg::Back);
        app.reduce(Msg::Back);
        app.reduce(Msg::Back);
        assert_eq!(app.view_mode(), ViewMode::List);
        assert_eq!(
            app.selected_row().map(|r| r.issue.id.as_str()),
            Some("ra-1"),
            "the ready selection is untouched by the search-opened detail"
        );
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

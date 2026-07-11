# Slice 11 — Cross-repo search

Bead: `federated-beads-dxh.12` (child of epic `federated-beads-dxh`).
Mol workflow root: `federated-beads-mol-j15`.
Master plan: `plans/fbd-v1-implementation-plan.md` (Slice 11 + global sections).
Depends on: Slices 0–10 + 9b (merged). Uses `app::{App, Msg, Effect, ViewMode,
DetailState, RepoFilter, PriorityFilter, FilterSet, keys::map_key}`,
`app::view::draw`, `snapshot::{Snapshot, Row, fetch, attribute}`,
`refresh::{PrefixMap, attribution_map}`, `runtime::{execute_effect,
spawn_search, search_worker, gather_search}`, `bd::{BdClient, Issue}` (the
`search` method already exists on the trait + `BdCli` + `FakeBdClient`),
`hub::hub_dir`, `cli::{format_row_body, sanitize}`.

## Goal

`/` opens a search input; typing edits a query; Enter runs `bd search <q>
--json` against the hub on a worker thread; the results **replace** the ready
list — attributed to their source repos through the **same `PrefixMap` path as
ready rows** — until `Esc`. The ready list's rows, selection, and filters are
preserved untouched behind the search and restored exactly on exit.

The bd layer is already complete (`BdClient::search`, `BdCli::search` with the
`--query=<q> --limit 0` argv from Slice 2, and `FakeBdClient::with_search`), and
the Slice 10 runtime already generalized effect dispatch into `execute_effect`
so `Effect::Search` slots in as one more arm. This slice adds:

1. a **`RowList`** value type extracted from `App`'s ready-list fields (rows +
   filtered index + selection + filter, with all navigation/filter/recompute
   logic) — the refactor the master plan calls out, so the ready list and the
   search results share one code path;
2. a **search sub-mode** (`ViewMode::Search`) with a small editing/loading/
   results phase machine, driven by new `Msg`/`Effect` variants;
3. a **search worker** in the runtime that attributes results via a rebuilt
   prefix map and sends them back over the channel;
4. a **search renderer** (input line + result count + the shared row list).

## Design decisions (recorded so downstream slices and autoreview don't re-litigate)

1. **`RowList` is the shared "row list" state (the required refactor).** A
   private struct
   ```
   struct RowList { rows: Vec<Row>, filtered_ix: Vec<usize>, selection: usize, filter: FilterSet }
   ```
   owns every operation the ready list had inline in Slice 8: `set_rows`
   (replace rows, recompute the filter, re-clamp selection), `recompute`,
   `select_next`/`select_prev` (clamp, no wrap), `cycle_repo_filter`/
   `toggle_priority_filter`, `next_repo_filter`, `select_id` (relocate the
   selection onto a row id, for the refresh-under-detail case), and the read
   accessors (`selection`, `selected_row`, `filtered_rows`). `App` now holds
   `ready: RowList` plus an `Option` search list, and the Slice 8 selection
   invariant lives **once**, inside `RowList`. The App's public accessors
   (`rows`, `filtered_rows`, `selection`, `selected_row`, `filter`) delegate to
   the **active** list (search list while the search flow is live, else ready),
   so the view renders whichever list is active through one API and every Slice
   8/9/10 List-mode test — which runs with no search active — is unchanged.

2. **`ViewMode::Search` with a three-phase `SearchState`.** One field
   `search: Option<SearchState>`, `Some` exactly while the search flow is live
   (in `Search` mode, or in `Detail` opened *from* a search result):
   ```
   struct SearchState { query: String, phase: SearchPhase, token: u64, list: RowList }
   enum SearchPhase { Editing, Loading, Results, Error(String) }
   ```
   - **Editing** — the query input is focused; printable keys edit the query.
   - **Loading** — a query was submitted; awaiting results.
   - **Results** — attributed rows are shown and browsable (`list` holds them,
     possibly empty = "no results").
   - **Error(msg)** — the `bd search` call failed; the message is shown instead
     of a misleading "0 results".
   Invariant: `view_mode == Search ⇒ search.is_some()`; `search.is_some()` iff
   the search flow is live, so `active()` keys off `search.is_some()` and `Back`
   from `Detail` returns to `Search` exactly when the detail was opened from a
   search result.

3. **The prefix map is rebuilt in the search worker, not cached.** Attribution
   needs a `PrefixMap` (id-prefix → repo). Rather than thread the last refresh's
   map through the pure `App` (which would force `PrefixMap: Eq` onto every
   `Msg`/`Snapshot` clone and add shared mutable state — both against this
   codebase's "stateless workers, everything through the channel" grain), the
   search worker rebuilds it from the roster via a new
   `refresh::attribution_map(bd, roster) -> (PrefixMap, Vec<RepoError>)` that
   reads each repo's authoritative prefix with `bd.issue_prefix` (the same call
   `refresh::run` uses, so the hyphen-preserving attribution is identical). This
   costs one `bd config get` per repo per search (~1 s for 5 repos), paid on the
   worker while the UI stays responsive with a `searching…` indicator. A v2
   optimization (cache the map from the last refresh) is filed as a bead.
   `refresh::run` is **left untouched** so Slice 4's `exports_all_then_syncs_once`
   call-order test does not move; `attribution_map` is a small standalone
   (dedupe + `issue_prefix` loop) documented as the shared attribution reader.

4. **Attribution + sort are shared with the ready snapshot.** `snapshot::fetch`
   is split: a new pure `snapshot::attribute(issues, &prefix_map, fetched_at) ->
   Snapshot` (the existing attribution + basename-disambiguation + priority/
   updated/id sort) becomes the core, and `fetch` is `attribute(bd.ready(hub)?,
   …)`. The search worker calls `bd.search(hub, q)` then the *same* `attribute`,
   so search rows carry `repo_name` exactly as ready rows do (the master plan's
   "same `PrefixMap` attribution path"), sort identically, and render through the
   same grouped row renderer.

5. **`Msg::SearchResults { token, rows: Result<Vec<Row>, String> }` — token-
   tagged, carrying pre-attributed rows or a message.** Mirrors Slice 10's
   `DetailReady`: a monotonic `search_seq` (bumped per submit) tags each request;
   `reduce` accepts a completion only when its token still matches the current
   search *and* the phase is still `Loading`, so a superseded query's late reply
   (the user re-submitted, or `Esc`'d back to editing) is dropped. Rows are
   pre-attributed `Row`s (not raw `Issue`s) so the pure core needs no
   `PrefixMap`, and `Vec<Row>` / `String` keep `Msg`'s `Eq` derive (relied on by
   runtime + reduce tests) intact. The error arm surfaces a failed `bd search`
   as `Error(msg)` rather than an empty result.

6. **`Effect::Search { query, token }` is the third reserved I/O variant.**
   `reduce` emits it on `Msg::SubmitSearch` with a non-empty query; the runtime's
   `execute_effect` gains a `Search` arm that spawns a search worker (tracked in
   `worker_handles` for shutdown-join, like refresh/detail). Empty/whitespace
   query ⇒ no effect (stays in `Editing`).

7. **Key routing is phase-aware via one editing flag.** `map_key` becomes
   `map_key(event, editing: bool)`: when `editing` (search input focused), a
   `Char(c)` ⇒ `Msg::SearchInput(c)`, `Backspace` ⇒ `Msg::SearchBackspace`,
   `Enter` ⇒ `Msg::SubmitSearch`, `Esc` ⇒ `Msg::Back`, everything else `None`;
   otherwise the existing mapping (so in the **Results** phase `j`/`k` navigate,
   `Enter` opens detail, `/` restarts the query). The input thread has no `&App`,
   so it reads the flag from an `Arc<AtomicBool>` the UI thread refreshes each
   loop (mirroring the existing `stop` flag). This trails `reduce` by one message,
   but **every mode-changing key is itself mapped under the pre-change flag** and
   the flag is stored before the next key can arrive (human cadence), so no
   keystroke is mismapped; a mis-timed key would at worst take the other reading
   once, non-destructive in a read-only TUI.

8. **`Esc` semantics (documented as required).** `Back` in `Search`:
   - from **Editing** ⇒ **exit search**, restoring the ready list (rows,
     selection, filters) exactly — it was never touched, so this is just
     `view_mode = List; search = None`.
   - from **Loading / Results / Error** ⇒ return to **Editing** with the query
     preserved, so the user can refine and re-submit (a second `Esc` then exits).
   `Back` in `Detail` ⇒ return to `Search` (if the detail was opened from a
   search result, i.e. `search.is_some()`) else `List`; the search phase it
   returns to is whatever it was (`Results`), never disturbed.

9. **Detail works from search results.** `OpenDetail` is allowed from the
   *browsing* contexts — `List` **or** `Search`+`Results` — using `active()
   .selected_row()`; it enters `Detail` without clearing `search`, so `active()`
   stays the search list behind the pane and `Back` returns to the results. This
   reuses Slice 10's detail machinery unchanged (one `FetchDetail` per Enter,
   token-dropped stale replies). A background refresh under a search-opened detail
   updates `ready` underneath (harmless; the opened id may not be in `ready`, so
   the relocate falls back to the clamped index, already covered by Slice 10's
   fallback test) and `Esc` returns to the intact search results.

10. **The search worker degrades, never aborts** (mirrors `gather_snapshot`).
    `gather_search` returns `Result<Vec<Row>, String>`: a `bd search` failure maps
    to a `sanitize`d message (surfaced as `Error`); prefix-read failures during
    `attribution_map` are non-fatal (those repos' ids fall to the `unknown`
    bucket, exactly as a ready-list attribution miss would). No version gate /
    `ensure_hub`: search is reachable only from `List`, i.e. after a snapshot
    already hydrated the hub.

## Module layout

- **`src/app/mod.rs`** (edit): extract `RowList` + its methods; add
  `ViewMode::Search`, `SearchState`/`SearchPhase`, `search`/`search_seq` fields;
  new `Msg` variants `SearchInput(char)`, `SearchBackspace`, `SubmitSearch`,
  `SearchResults { token, rows }`; `Effect::Search { query, token }`; make
  `OpenSearch`/`Back` real for search; route nav/filter/`OpenDetail` through the
  active/ browsing list; accessors delegate to `active()`; add `search_query`,
  `search_phase`, `search_result_count`, `search_editing`, `is_searching`
  accessors for the view + runtime. New unit tests.
- **`src/app/keys.rs`** (edit): `map_key(event, editing)`; editing-mode arm. Update
  existing tests to pass `false`; add editing-mode tests.
- **`src/app/view.rs`** (edit): `draw` dispatch adds a `Search` arm; extract the
  grouped-rows renderer into `draw_rows` (shared by `draw_list` and the search
  results); add `draw_search` (input line + count/status line + `draw_rows`);
  `SEARCH_HINTS`. New render test.
- **`src/snapshot.rs`** (edit): extract pure `attribute(issues, &prefix_map,
  fetched_at) -> Snapshot`; `fetch` calls it. (Existing tests unchanged.)
- **`src/refresh.rs`** (edit): add `attribution_map(bd, roster) -> (PrefixMap,
  Vec<RepoError>)`. New unit test.
- **`src/runtime.rs`** (edit): `execute_effect` gains the `Effect::Search` arm;
  `spawn_search`, `search_worker`, `gather_search`; the UI loop maintains an
  `Arc<AtomicBool> editing` and passes it to `input_thread` → `map_key`. New unit
  test.

No new files; no changes to `bd` (search already implemented), `hub`, `config`,
`main.rs`, or `lib.rs`.

## Ordered TDD test list (red → green)

### `app/mod.rs` (unit, drive `reduce`, assert via accessors)

Helpers reuse Slice 8/10's `row`, `snapshot`, `app_with`, plus new `search_row`
(a pre-attributed `Row`) and `submit(app, query) -> token` (OpenSearch → type
each char → SubmitSearch, returning the `Effect::Search` token).

1. **`slash_opens_search_input`**
   - Red: `ViewMode::Search`/`SearchState`/`Msg::SearchInput` don't exist.
   - Green: `app_with([row…])`, `reduce(OpenSearch)` ⇒ `view_mode() == Search`,
     `search_editing()` true, `search_query() == ""`. In `Editing`, `reduce(
     SelectNext)` is a no-op on the ready selection (keys are text now) — the
     ready selection is frozen (asserts routing).

2. **`typing_edits_query`**
   - Green: after `OpenSearch`, `SearchInput('f')`, `SearchInput('o')`,
     `SearchInput('o')` ⇒ `search_query() == "foo"`; `SearchBackspace` ⇒
     `"fo"`; backspace past empty is a safe no-op.

3. **`enter_submits_search_effect`**
   - Green: with query `"foo"`, `reduce(SubmitSearch)` returns
     `vec![Effect::Search { query: "foo", token: 1 }]` and `search_phase()` is
     `Loading`. Edge `empty_query_no_ops`: on an empty (or whitespace) query,
     `SubmitSearch` returns `vec![]` and stays `Editing`.

4. **`results_replace_rows_with_attribution`**
   - Green: submit `"foo"` (token t), then `reduce(SearchResults { token: t,
     rows: Ok(vec![search_row("megaclock", "mc-1"), …]) })` ⇒ `view_mode()`
     still `Search`, phase `Results`, `filtered_rows()` are the search rows with
     their `repo_name` carried through (attribution path), `selection() ==
     Some(0)`, and the **ready** rows are unchanged behind it (assert a private
     `ready` accessor or restore-on-Esc below). Companion
     `stale_search_results_dropped`: a `SearchResults` with a non-current token
     is a no-op (phase stays `Loading`); and after a second submit (token t+1) a
     late `t` reply is dropped.

5. **`esc_restores_ready_list`** (Esc-from-input and Esc-from-results both
   defined)
   - Green: `app_with([ra, rb])`, `SelectNext` (ready selection 1),
     `CycleRepoFilter` (a ready filter active); submit `"foo"`, get results.
     `reduce(Back)` from `Results` ⇒ `view_mode() == Search`, phase `Editing`,
     query preserved (`"foo"`). `reduce(Back)` from `Editing` ⇒ `view_mode() ==
     List`, `search()` cleared, and the ready list restored **exactly**: same
     rows, `selection() == Some(1)`, same active filter.

6. **`search_detail_and_back`** (decision 9)
   - Green: submit, results shown; `reduce(OpenDetail)` on a search row returns
     one `Effect::FetchDetail`, `view_mode() == Detail`; `DetailReady(Ok)` loads;
     `reduce(Back)` ⇒ `view_mode() == Search`, phase `Results`, selection intact.

7. **`search_error_shows_message`** (decision 5/10)
   - Green: submit, then `SearchResults { token, rows: Err("bd search failed:
     boom") }` ⇒ phase `Error` whose message contains `boom`; ready rows intact.

8. **`refresh_under_search_updates_ready_not_view`** (edge: refresh during
   search)
   - Green: in `Search`+`Results`, a `RefreshCompleted { snapshot: Some(new
     ready rows) }` leaves `view_mode() == Search` and the shown (search) rows
     unchanged, while `Esc`→`Esc` then reveals the **refreshed** ready rows.

### `keys.rs` (unit)

9. **`maps_search_input_keys`**
   - Green: `map_key(Char('f'), true) == Some(SearchInput('f'))`;
     `Backspace,true == SearchBackspace`; `Enter,true == SubmitSearch`;
     `Esc,true == Back`. And `map_key(Char('j'), false) == SelectNext` (existing
     behavior when not editing). Existing tests updated to pass `false`.

### `snapshot.rs` / `refresh.rs` (unit)

10. **`attribute_is_shared_by_fetch`** (guards decision 4)
    - Green: `attribute(vec![issue("ra-1"…)], &map, when)` yields the same rows/
      order/attribution `fetch` would (a direct call, no `bd`); an existing `fetch`
      test still passes (proving `fetch` delegates).

11. **`attribution_map_reads_prefixes`** (guards decision 3)
    - Green: two seeded repos ⇒ `attribution_map` returns a `PrefixMap`
      attributing each repo's ids, and a metadata-read failure is a non-fatal
      `RepoError` (not a panic), the other repo still attributed.

### `runtime.rs` (unit)

12. **`search_worker_sends_results_for_token`**
    - Red: `search_worker`/`gather_search` don't exist.
    - Green: spawn `search_worker(FakeBdClient::with_search([issue("ra-1"…)]),
      roster, paths, "foo", 7, tx)`; `rx.recv()` yields exactly one
      `Msg::SearchResults { token: 7, rows: Ok(rows) }` whose rows carry
      `repo_name` (attributed via the seeded roster); a second `recv` errors.

13. **`search_worker_maps_error`**
    - Green: with `FakeBdClient::with_search_err(..)`, the worker sends
      `SearchResults { token, rows: Err(msg) }` naming the failure.

### `view.rs` (TestBackend)

14. **`renders_search_input_and_result_count`**
    - Red: no search renderer.
    - Green: an app in `Search`+`Results` with query `"foo"` and 12 rows ⇒ the
      buffer contains the query text on the input line and a count line
      `12 results for "foo"`; a search row's title/id renders through the shared
      row renderer. Companion `renders_search_editing_and_empty`: `Editing`
      shows the input with a cursor and a "type a query" hint; `Results` with 0
      rows shows `0 results for "…"` (not the ready-list "no repos" hint).

## Edge cases

- **Empty / whitespace query**: `SubmitSearch` is a no-op, no effect, stays
  `Editing` (test 3).
- **No results**: `Results` with an empty `list` renders `0 results for "…"`,
  not the ready-list empty hint (test 14).
- **Stale / out-of-order results**: dropped by token + `Loading`-phase guard
  (test 4).
- **Search error**: surfaced as `Error(msg)` in the pane (tests 7, 13).
- **Refresh during search**: updates `ready` underneath; the visible search
  results and mode are untouched; `Esc` later reveals the refreshed ready list
  (test 8).
- **Detail from a search result + refresh**: `Esc` returns to the intact search
  results; the ready relocate falls back safely (decision 9; Slice 10 fallback
  test still covers the relocate miss).
- **bd-sourced text in results** (title/id/repo_name): `sanitize`d at the render
  boundary by the shared `draw_rows` / `format_row_body`, same as ready rows.
- **Key-mapping race at a mode edge**: benign (decision 7).
- **Quit mid-`bd search`**: the search worker's handle is joined on shutdown like
  refresh/detail, so its subprocess is not orphaned.

## Out of scope (later slices / filed as beads)

- Caching the prefix map from the last refresh to avoid the per-search
  `issue_prefix` reads (v2 optimization — filed).
- Incremental / as-you-type search (submit-on-Enter only for v1).
- Copy-context (`y`) from a search result (Slice 12 owns `y`; it will use the
  active row like detail does).
- Highlighting the matched term within result rows.
- Searching within a repo filter (search queries the whole hub; the repo/priority
  filters still apply to the *results* list via the shared `RowList`).

## Verification (all four must be green)

```
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
cargo test --test bd_integration
```

## Autoreview outcomes (codex gpt-5.6-sol, high; branch vs main)

Ran to convergence over eight rounds; all seven findings accepted and fixed
(none rejected or filed as out-of-scope beads).

- **Round 1 — key-decode race (P2).** The input thread decoded state-dependent
  keys against an asynchronously-published `editing` `AtomicBool`, so a pasted or
  rapid `/query` burst could decode `query`'s characters in command mode before
  `OpenSearch` was reduced — e.g. `q` quitting. **Fixed:** the input thread now
  forwards raw key events (`Incoming::Key`); the UI thread decodes each via
  `map_key` against the app's live search focus, and channel ordering makes the
  race structurally impossible (the shared flag is removed). Test
  `pasted_query_keys_never_run_commands`.
- **Round 2 — search-opened detail moved the ready selection (P2).** A refresh
  landing while a detail opened *from a search result* was shown relocated the
  hidden ready selection onto that id when it was also a ready row, corrupting the
  restore-on-exit. **Fixed:** relocate the ready selection only when the detail
  came from the ready list (`search.is_none()`; decision revised). Test
  `refresh_under_search_detail_preserves_ready_selection`.
- **Round 3 — two view defects (P3).** The search title hint was static and
  advertised editing bindings on the results screen; and the `Error` status line
  re-prefixed the worker's already-`search failed:`-prefixed message. **Fixed:**
  phase-aware title hints; render the error message directly. Test
  `search_hints_are_phase_aware_and_error_renders_once`.
- **Round 4 — loading/error advertised inert keys (P3).** `Loading`/`Error`
  reused the results hint (`j/k move`, `enter open`), which are inert there.
  **Fixed:** a `SEARCH_WAIT_HINTS` listing only the keys that act (`esc edit`,
  `q quit`).
- **Round 5 — search undiscoverable (P2).** `LIST_HINTS` omitted `/` and its
  comment stale-claimed search was inert. **Fixed:** added `/ search` to the list
  hint (the README keybindings table remains a Slice 12 deliverable).
- **Round 6 — stale results under a new query (P2).** `draw_search` rendered the
  retained results in every phase, so `Esc`→edit left the prior query's hits
  showing beneath the new one. **Fixed:** render rows only in `Results` (the list
  is still retained so returning from a detail re-shows it). Test
  `editing_after_results_hides_stale_rows`.
- **Round 7 — filter-hidden results were a blank pane (P2).** `f`/`p` filter the
  results, but a filter hiding all of them rendered nothing, with a stale count
  and no filter-key hints. **Fixed:** show the ready list's `NO_MATCH_HINT` when
  results exist but are all filtered out (distinguished from a genuinely empty
  query), and advertise `f`/`p` in the results hint. Test
  `filtered_empty_search_shows_filter_hint`.
- **Round 8: clean** — helper exited 0 with no accepted/actionable findings.

Final counts on the branch: **175 unit + 7 integration** tests green;
`fmt`/`clippy` clean.

# Slice 8 — App state machine (pure `reduce`, no I/O)

Bead: `federated-beads-dxh.9` (child of epic `federated-beads-dxh`).
Mol workflow root: `federated-beads-mol-3vj`.
Master plan: `plans/fbd-v1-implementation-plan.md` (Slice 8 + global sections).
Depends on: Slices 0–7 (merged). Uses `snapshot::{Snapshot, Row}`,
`bd::Issue`, and (in tests) `bd::FakeBdClient` shapes.

## Goal

Model the entire TUI as a **pure state core**: an `App` value, a `Msg` enum
covering keypresses + the refresh lifecycle, and a `reduce` that maps
`(App, Msg)` to a new state plus a list of `Effect`s the runtime performs. No
terminal, no threads, no `bd` calls, and — critically — **no clock read inside
`reduce`** (staleness age is derived later from an injected `now`; the shown
snapshot's `fetched_at` is supplied by the caller). A separate `keys` module
maps a crossterm `KeyEvent` to a `Msg`; crossterm types appear **only** in
`keys.rs`, so `reduce` stays backend-agnostic and exhaustively unit-testable.

This slice writes logic only. Rendering (`view.rs`) and the terminal runtime
(`runtime.rs`) are Slice 9; the detail pane is Slice 10; search is Slice 11;
copy-context is Slice 12. The `Msg`/`Effect`/`ViewMode` shapes are designed to
*anticipate* those slices (documented extension points) without implementing
them.

## Design decisions (recorded so downstream slices and autoreview don't re-litigate)

1. **`reduce` signature: `fn reduce(&mut self, msg: Msg) -> Vec<Effect>`.** The
   master plan's shorthand is `reduce(App, Msg) -> (App, Vec<Effect>)`. The
   `&mut self` form is the *equivalent pure* realization the plan permits: a
   deterministic transition over the app's **own** fields with no I/O and no
   clock — same testability (construct an `App`, call `reduce`, assert on
   accessors), but it does not clone the whole `Vec<Row>` on every keystroke the
   owning-and-returning form would. "Pure" here means: given the same starting
   `App` and `Msg`, the resulting `App` and returned `Effect`s are identical, and
   nothing outside `self` is touched.

2. **`Effect` is the reserved I/O extension point; Slice 8 emits exactly one
   variant.** `reduce` describes side effects instead of performing them; the
   Slice 9 runtime executes them. Slice 8 defines `Effect::Refresh` (emitted for
   `Msg::Refresh`, i.e. the `r` key: "the user wants a refresh; runtime, spawn
   the worker"), which gives the `Vec<Effect>` return a concrete, tested purpose
   now and models the reduce→effect split end-to-end. Slice 10 adds
   `Effect::FetchDetail(String)`, Slice 11 `Effect::Search(String)` — additive,
   no change to `reduce`'s signature or the runtime's call site.

3. **`Msg` names follow the Slice 8 test vocabulary.** Refresh lifecycle is three
   messages the Slice 9 runtime feeds from its worker thread:
   - `RefreshStarted` — a refresh began; keep current rows visible, set `stale`.
   - `SnapshotReady(Snapshot)` — fresh data; replace rows, recompute the filter,
     clamp selection, clear `stale`, record `fetched_at`, enter `List`.
   - `RefreshWarnings(Vec<String>)` — the refresh cycle's warnings/errors to
     surface (per-repo export failures, prefix collisions, `ensure_hub` missing
     paths, or a fatal sync error mapped by the runtime to a status line rather
     than aborting the whole TUI). Sets `status_warnings`, clears `stale`. A
     clean refresh with per-repo warnings sends `SnapshotReady` **and**
     `RefreshWarnings`; a fatal refresh sends only `RefreshWarnings` (stale view
     stays browsable). The pre-formatted `String`s keep `reduce` free of
     `refresh`/`hub` error types (the runtime owns formatting + sanitization).

4. **`stale` = "a refresh is in flight over the shown rows."** Set by
   `RefreshStarted`, cleared by whichever message concludes the cycle
   (`SnapshotReady` or `RefreshWarnings`). Distinct from *age* (Slice 9 computes
   "refreshed 3m ago" from `fetched_at` and an injected `now`); this flag is the
   "refreshing…" indicator and the reason old rows are kept on screen.

5. **`selection` indexes `filtered_ix`, not `rows`.** `filtered_ix: Vec<usize>`
   holds the indices of rows passing the current `FilterSet`, in display order.
   `selection: usize` is an offset into `filtered_ix`. **Invariant (enforced
   after every mutation): `filtered_ix` empty ⇒ `selection == 0` and
   `selected_row() == None`; otherwise `selection < filtered_ix.len()`.** Movement
   clamps (no wrap): `SelectNext` saturates at `len-1`, `SelectPrev` at `0`;
   both are safe no-ops on an empty list. Any rebuild of `filtered_ix` (new
   snapshot or filter change) re-clamps `selection` into bounds. Row-identity
   preservation across refresh/filter (keeping the *same* issue selected) is a
   nice-to-have left out of scope; only in-bounds validity is guaranteed.

6. **`FilterSet` is applied uniformly** via `FilterSet::matches(&Row) -> bool`,
   the single predicate that builds `filtered_ix`. Two independent axes:
   - `RepoFilter`: `All` or `Only(String)` (matched against `Row::repo_name`).
     `f` cycles `All → repo₀ → repo₁ → … → repoₙ₋₁ → All`, where the repo list is
     the **distinct `repo_name`s in first-appearance (display) order** of the
     current `rows`. `Only(name)` for a name absent from the current rows yields
     an empty view (valid per the selection invariant) and cycles back to `All`.
   - `PriorityFilter`: `All` or `HighOnly` (P0/P1, i.e. `priority <= 1`). `p`
     toggles `All ↔ HighOnly`.
   Filters persist across refreshes (a refresh must not silently drop the user's
   active filter) and recompute against the new rows.

7. **`ViewMode` is `Loading` | `List` in Slice 8.** `App::new()` starts in
   `Loading`; the first `SnapshotReady` moves to `List`. `Detail` and `Search`
   are added by Slices 10/11 (the `reduce`/view `match` on `view_mode` is the
   extension point). Placeholder key messages that need I/O or later modes
   (`OpenDetail`, `OpenSearch`, `CopyContext`, `Back`) are accepted by `reduce`
   now as pure no-ops (no state change, no effect) so the key pipeline is
   complete; their real behavior lands in the slice that owns it.

8. **Encapsulation via accessors.** `App`'s fields are private; the state is read
   through methods (`view_mode`, `rows`, `filtered_rows`, `selection`,
   `selected_row`, `is_stale`, `status_warnings`, `is_done`, `filter`,
   `fetched_at`). This protects the selection invariant and gives Slice 9's view
   a stable read API. Tests drive `reduce` and assert through these accessors.

## Module layout

- **`src/app/mod.rs`** (new; `pub mod app;` in `lib.rs`): `App`, `Msg`, `Effect`,
  `ViewMode`, `FilterSet`, `RepoFilter`, `PriorityFilter`, `App::new`,
  `reduce`, the accessors, and the private `recompute`/`clamp`/`cycle` helpers.
  `#[cfg(test)] mod tests`.
- **`src/app/keys.rs`** (new; `pub mod keys;` in `app/mod.rs`): `map_key(KeyEvent)
  -> Option<Msg>`. The only file importing `crossterm`. `#[cfg(test)] mod tests`.

## Ordered TDD test list (red → green)

`reduce` unit tests in `src/app/mod.rs`. Helpers: `row(repo, id, prio)` builds a
`Row`; `snapshot(rows)` wraps them in a `Snapshot { rows, fetched_at: fixed }`;
`app_with(rows)` returns an `App` already advanced to `List` via `SnapshotReady`.

1. **`starts_in_loading_then_shows_rows`**
   - Red: `App`/`Msg`/`reduce` don't exist (compile error).
   - Green: `App::new()` is `ViewMode::Loading`, no rows, `selection() == None`,
     `!is_done()`. `reduce(RefreshStarted)` stays `Loading` (no rows yet).
     `reduce(SnapshotReady(snapshot(2 rows)))` ⇒ `List`, `rows().len() == 2`,
     `selection() == Some(0)`, `!is_stale()`.

2. **`selection_moves_and_clamps`**
   - Red: navigation/clamp not implemented.
   - Green: with 3 rows, `SelectNext` walks `0→1→2` then **stays** at `2`;
     `SelectPrev` walks back to `0` then stays. On an empty `App`
     (`SnapshotReady` of 0 rows) `SelectNext`/`SelectPrev` are no-ops and
     `selected_row() == None`.

3. **`repo_filter_cycles`**
   - Red: repo filter/cycle absent.
   - Green: rows spanning `repo-a` (appears first) then `repo-b`.
     `CycleRepoFilter`: `All → Only("repo-a")` (only `repo-a` rows visible) →
     `Only("repo-b")` → back to `All`. `filtered_rows()` recomputes each step and
     `selection()` remains `Some(0)` (valid).

4. **`priority_filter_toggles`**
   - Red: priority filter absent.
   - Green: rows mixing P0/P1 and P2. `TogglePriorityFilter` ⇒ only `priority <=
     1` rows visible; again ⇒ all rows visible.

5. **`refresh_while_stale_keeps_rows`**
   - Red: `RefreshStarted` wipes rows / no stale flag.
   - Green: from a loaded `List` app, `reduce(RefreshStarted)` keeps the same
     rows and selection, sets `is_stale()`, view stays `List`.

6. **`refresh_error_surfaces_in_status`**
   - Red: warnings not stored.
   - Green: `reduce(RefreshWarnings(vec!["export failed for repo-b".into()]))`
     ⇒ `status_warnings()` contains that string; `!is_stale()`.

7. **`refresh_key_requests_refresh_effect`**
   - Red: `Effect`/effect return absent.
   - Green: `reduce(Refresh)` returns `vec![Effect::Refresh]` and changes no
     observable state (the runtime spawns the worker).

8. **`quit_msg_sets_done`**
   - Red: `done`/`is_done` absent.
   - Green: `reduce(Quit)` ⇒ `is_done()`.

9. **`filters_persist_and_recompute_across_refresh`** (guards decision 6)
   - Green: with `Only("repo-a")` active, a new `SnapshotReady` (different row
     set still containing `repo-a`) keeps the filter and shows only `repo-a`
     rows, selection valid.

10. **`selection_invariant_holds_under_random_messages`** (refactor / property)
    - Green: a deterministic LCG (no `rand` dep) generates a long sequence of
      messages drawn from {`SelectNext`, `SelectPrev`, `CycleRepoFilter`,
      `TogglePriorityFilter`, `SnapshotReady(varied row sets incl. empty)`,
      `RefreshStarted`}. After **every** step assert the invariant: either
      `filtered_ix` empty with `selection() == None` and `selected_row() ==
      None`, or `selection()` is `Some(i)` with `i < filtered_rows().len()` and
      `selected_row() == filtered_rows()[i]`.

`map_key` unit tests in `src/app/keys.rs` (build events with
`KeyEvent::new(code, KeyModifiers::NONE)`):

11. **`maps_command_keys`** — `q→Quit`, `/→OpenSearch`, `r→Refresh`,
    `y→CopyContext`, `Enter→OpenDetail`, `Esc→Back`.
12. **`maps_navigation_keys`** — `j`/`Down→SelectNext`, `k`/`Up→SelectPrev`.
13. **`maps_filter_keys`** — `f→CycleRepoFilter`, `p→TogglePriorityFilter`.
14. **`ignores_unmapped_and_release`** — an unmapped char (`z`) ⇒ `None`; a
    `KeyEventKind::Release` event ⇒ `None` (so a key press+release fires once).

## Edge cases

- **Empty ready list**: `SnapshotReady` of 0 rows ⇒ `filtered_ix` empty,
  `selection() == None`, navigation safe (tests 2, 10).
- **Filter names a now-absent repo**: `Only(name)` with no matching rows ⇒ empty
  view (valid), and `f` cycles it back toward `All` (decision 6).
- **Repeat vs Press key kinds**: `Release` is ignored; `Press`/`Repeat` map
  (holding `j` repeats). Handled in `keys.rs` (test 14).
- **Warnings replace, not append**: each `RefreshWarnings` sets the current
  status list (the runtime sends the full set per cycle), so a healed repo's
  warning disappears on the next clean refresh.

## Out of scope (later slices)

- `view.rs` rendering, group headers, status bar, staleness *age* text (Slice 9).
- `runtime.rs` event loop, threads, effect execution, `now` injection (Slice 9).
- Detail pane + `Effect::FetchDetail` + `Msg::DetailReady`/`ViewMode::Detail`
  (Slice 10).
- Search input/mode + `Effect::Search` (Slice 11).
- Copy-context string building + `Effect`/status confirmation (Slice 12).
- Row-identity preservation of selection across refresh/filter (decision 5).

## Verification (all four must be green)

```
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
cargo test --test bd_integration
```

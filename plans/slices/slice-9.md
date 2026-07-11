# Slice 9 — Ready view rendering + terminal runtime (TUI launches)

Bead: `federated-beads-dxh.10` (child of epic `federated-beads-dxh`).
Mol workflow root: `federated-beads-mol-2pz`.
Master plan: `plans/fbd-v1-implementation-plan.md` (Slice 9 + global sections).
Depends on: Slices 0–8 (merged). Uses `app::{App, Msg, Effect, ViewMode,
RepoFilter, PriorityFilter, keys::map_key}`, `snapshot::{Snapshot, Row, fetch}`,
`hub::{ensure_hub, hub_dir}`, `refresh::{run, RefreshOutcome, RefreshError,
PrefixMap}`, `cli::{version_gate, format_row, sanitize}`, `config::{Config,
Paths}`, `bd::{BdClient, BdCli, FakeBdClient}`.

## Goal

Make a human see the cross-repo ready list. Two new modules turn the pure Slice 8
state core into a running terminal app:

- **`src/app/view.rs`** — `draw(frame, &app, now)`: a **pure** render of `&App`
  into a ratatui `Frame`. Repo group headers, ready rows, selection highlight, a
  status bar (last-refreshed *age* from an injected `now`, plus a warning
  summary), and an empty-state hint. Tested with `TestBackend(80×24)` by
  asserting on specific buffer cells/rows — never whole-screen golden files.
- **`src/runtime.rs`** — the event loop: a crossterm event thread and a refresh
  worker thread, both feeding one `mpsc` channel of `Msg`; terminal
  setup/teardown with a panic hook that restores the terminal; `reduce`'s
  `Effect::Refresh` spawns a worker. Bare `fbd` (no subcommand) launches it:
  ensure_hub warnings → status, an initial refresh spawned at launch (the App is
  born stale), `q` quits cleanly.

This slice adds rendering + runtime only. Detail pane (Slice 10), search (11),
and copy-context (12) stay out; their `Msg`/`ViewMode` extension points already
exist in the Slice 8 core and this slice does not implement them.

## Design decisions (recorded so downstream slices and autoreview don't re-litigate)

1. **`draw(frame: &mut Frame, app: &App, now: SystemTime)` is pure over `App`.**
   No clock read inside (the age is `now - app.fetched_at()`, `now` injected by
   the runtime as `SystemTime::now()` and by tests as a fixed instant), no I/O,
   no mutation of anything but the frame's buffer. This keeps every rendered
   pixel a deterministic function of `(App, now)` and unit-testable via
   `TestBackend`.

2. **Grouping is done in the view; navigation follows the App's flat order.** The
   Slice 8 `App` exposes `filtered_rows()` in the snapshot's order (Slice 5:
   priority ascending, then `updated_at` descending), and `selection()` indexes
   that flat list. The view **buckets** those rows by `repo_name` in
   first-appearance order and draws a `▸ <repo>` header above each bucket, with
   each row rendered as `P<pri> <id> <title>` (no repo — the header carries it).
   The selection highlight marks whichever *displayed* row equals
   `app.selected_row()`. Because the App's flat order is global-priority and the
   display is bucketed by repo, `j`/`k` walk priority order while the eye sees
   repo sections, so the highlight can jump between sections when a repo's rows
   are non-contiguous in priority order. This is an accepted v1 limitation:
   making navigation follow the grouped display order would require the Slice
   8/Slice 5 row order itself to be grouped, which is out of scope for a
   render-only slice and would perturb merged, reviewed modules. Filed as a v2
   refinement bead. (The render tests construct contiguous-by-repo `App` states —
   a legitimate state — so they read one header per repo.)

3. **The empty-state hint is derived from `App` alone.** `draw` is pure over
   `App`, which carries no roster, so the view shows the hint
   `no repos configured — run: fbd repos discover ~/dev` whenever the ready list
   is empty in `List` mode. For a brand-new user (no repos) this is exactly
   right; for "repos configured but nothing ready" it is a mild imprecision
   accepted for v1 (distinguishing the two needs roster context the pure view
   deliberately does not take). `Loading` mode (before the first snapshot) shows
   `Loading…` instead.

4. **The status bar summarizes, it does not dump.** `App::status_warnings()` holds
   full pre-formatted strings (per-repo export failures, prefix collisions,
   missing paths) that cannot fit one line. The bar shows
   `refreshed <age>` (or `refreshing…` while `is_stale()` with no prior fetch) and,
   when warnings are present, `<n> repo<s> failed (see doctor)` pointing at the
   `fbd doctor` command that prints them in full. `<age>` is humanized
   (`just now` / `Ns` / `Nm` / `Nh` / `Nd ago`) from `now - fetched_at`, saturating
   to `just now` on clock skew.

5. **One `mpsc` channel, two producer threads, one consumer (the UI thread).** A
   crossterm **event thread** polls `event::read()`, maps each `KeyEvent` via
   `keys::map_key`, and sends the `Msg`; it exits when a shared `AtomicBool` stop
   flag is set (checked around a short `event::poll` timeout) so shutdown is
   clean. A **refresh worker thread** sends `RefreshStarted`, runs
   `ensure_hub → refresh::run → snapshot::fetch`, then sends exactly one
   `RefreshCompleted { snapshot, warnings }`. The UI thread `recv()`s a `Msg`,
   calls `app.reduce`, executes returned `Effect`s (`Effect::Refresh` spawns a new
   worker — the Slice 8 in-flight guard prevents overlap), redraws, and breaks
   when `app.is_done()`.

6. **The refresh worker is fatal-tolerant (the TUI degrades, never aborts).** A
   separate `gather_snapshot(bd, roster, paths) -> (Option<Snapshot>,
   Vec<String>)` mirrors `run_snapshot`'s `ensure_hub → refresh → fetch` pipeline
   but returns rather than prints, and turns *every* failure into a warning with a
   `None` snapshot (so the last-good rows stay browsable) instead of a fatal
   `CliError`. It is deliberately parallel to (not shared with) `run_snapshot`,
   whose CLI contract is fail-fast with typed `CliError`s asserted by merged
   tests; unifying the two failure policies would break those tests for no real
   gain. Warnings are `sanitize`d (they embed bd stderr / paths and are rendered
   to a terminal).

7. **Row formatting is shared with `fbd snapshot` (the required refactor).**
   `cli::format_row` is split: a new `cli::format_row_body(row) -> "P<pri> <id>
   <title>"` (sanitized) is the shared core; `format_row` prepends `[<repo>] `.
   The view uses `format_row_body` for rows and `cli::sanitize` (made
   `pub(crate)`) for the `▸ <repo>` header, so the headless and TUI renderings of
   a row can never drift. `format_row`'s existing output/tests are unchanged.

8. **Terminal safety via a panic hook (session-tui pattern).** Setup enables raw
   mode + alternate screen; a panic hook restores them before the default hook
   runs, so a panic mid-render never leaves the user's terminal wedged. Normal
   exit restores via the same teardown.

## Module layout

- **`src/app/view.rs`** (new; `pub mod view;` in `app/mod.rs`): `pub fn draw`,
  private `format_age`, group-bucketing + line-building helpers, `#[cfg(test)]
  mod tests` (TestBackend render assertions).
- **`src/runtime.rs`** (new; `pub mod runtime;` in `lib.rs`): `pub fn run(paths,
  roster) -> Result<(), CliError>` (bare-`fbd` entry), private terminal
  setup/teardown/panic-hook, `pub(crate) fn refresh_worker`, `pub(crate) fn
  gather_snapshot`, `#[cfg(test)] mod tests` (channel + FakeBdClient).
- **`src/cli.rs`** (edit): extract `format_row_body`, expose `sanitize` as
  `pub(crate)`.
- **`src/main.rs`** (edit): bare `fbd` (`None` arm) calls `runtime::run`.
- **`src/lib.rs`** (edit): `pub mod runtime;`.

## Ordered TDD test list (red → green)

### `view.rs` (TestBackend 80×24; assert on specific cells/rows)

Helper: `app_with(rows)` builds an `App` advanced to `List` via
`reduce(RefreshCompleted{ snapshot: Some(Snapshot{rows, fetched_at}), warnings })`;
`render(&app, now) -> Buffer` draws to a `TestBackend` and clones its buffer;
`row_text(buf, y) -> String` concatenates a row's cell symbols.

1. **`renders_group_headers_and_rows`**
   - Red: `view`/`draw` don't exist (compile error).
   - Green: given rows `[session-tui/ra-2hc P1, session-tui/ra-9 P2,
     megaclock/mc-1 P0]` (contiguous by repo), some buffer row contains
     `▸ session-tui`, another `▸ megaclock`, and a row line contains
     `P1 ra-2hc` and its title. Assert header + row appear on distinct lines.

2. **`renders_selection_highlight`**
   - Red: no highlight styling.
   - Green: with selection at row 1 (after one `SelectNext`), the cells of the
     displayed line holding `selected_row()` carry `Modifier::REVERSED`, and a
     non-selected row's cells do not.

3. **`renders_status_bar_with_age_and_warnings`**
   - Red: no status bar.
   - Green: `fetched_at = t`, `now = t + 180s`, one warning
     `"export failed for reading-lite"` ⇒ the bottom line contains
     `refreshed 3m ago` and `1 repo failed (see doctor)`.

4. **`renders_empty_state_hint`**
   - Red: empty list renders nothing useful.
   - Green: an `App` with zero rows in `List` mode ⇒ some buffer row contains
     `no repos configured — run: fbd repos discover ~/dev`.

5. **`renders_loading_before_first_snapshot`** (edge)
   - Green: a fresh `App::new()` (`Loading`) ⇒ buffer contains `Loading…` and no
     empty-state hint.

6. **`format_age_humanizes`** (unit, pure)
   - Green: `just now` (<5s), `42s ago`, `3m ago`, `2h ago`, `5d ago`, and
     `just now` when `now < fetched_at` (skew).

### `runtime.rs`

7. **`refresh_task_sends_started_then_completed`**
   - Red: `refresh_worker`/channel don't exist.
   - Green: spawn `refresh_worker(FakeBdClient::new().with_ready([...]), roster,
     paths, tx)` on a thread; `rx.recv()` yields `Msg::RefreshStarted`, the next
     `rx.recv()` yields exactly one `Msg::RefreshCompleted { snapshot: Some, .. }`
     carrying the ready rows; a third `recv()` errors (channel closed → worker
     done). Deterministic via blocking `recv` — no sleeps. Join the thread before
     the tempdir drops.

8. **`gather_snapshot_collects_repo_warnings`** (unit)
   - Green: a roster with one healthy seeded repo and one missing path ⇒ the
     returned `Some(snapshot)` has the healthy repo's rows and `warnings`
     mentions the missing path (from `ensure_hub`).

9. **`gather_snapshot_none_on_fatal_sync`** (unit)
   - Green: `FakeBdClient::with_repo_sync_err(..)` ⇒ `(None, warnings)` with a
     warning mentioning the sync failure (last-good rows kept by the caller).

## Edge cases

- **Empty ready list vs no repos**: both render the discover hint (decision 3).
- **Clock skew** (`now < fetched_at`): age saturates to `just now` (decision 4).
- **Narrow/short terminal**: layout uses `Min(0)` for the list and fixed-height
  title/status; rows longer than the width are truncated by ratatui, never wrap
  into forged rows (bd text is `sanitize`d of control chars regardless).
- **Refresh in flight**: `is_stale()` shows `refreshing…`; old rows stay drawn
  (Slice 8 keeps them). A second `r` is deduped by the Slice 8 guard, so at most
  one worker thread exists per cycle.
- **Panic mid-render**: the panic hook restores the terminal before propagating.

## Out of scope (later slices)

- Detail pane / `Effect::FetchDetail` (Slice 10); search input (Slice 11);
  copy-context (Slice 12).
- Navigation following grouped display order (v2 refinement bead).
- Distinguishing "no repos configured" from "nothing ready" in the empty state
  (needs roster context the pure view omits).
- Automated interactive-TUI smoke (no tty in the agent harness — documented
  manual step, pending the user).

## Verification (all four must be green)

```
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
cargo test --test bd_integration
```

Plus manual smoke on the real machine (recorded in a bead comment):
`cargo run -- repos discover /Users/brian/dev` then `cargo run -- snapshot | head`
against the 5 real repos. Interactive `fbd` launch is left for the user (no tty
in the harness).

# Slice 10 — Detail pane

Bead: `federated-beads-dxh.11` (child of epic `federated-beads-dxh`).
Mol workflow root: `federated-beads-mol-1tm`.
Master plan: `plans/fbd-v1-implementation-plan.md` (Slice 10 + global sections).
Depends on: Slices 0–9 + 9b (merged). Uses `app::{App, Msg, Effect, ViewMode,
reduce}`, `app::view::draw`, `runtime::{ui_loop, spawn_refresh, gather_snapshot}`,
`bd::{BdClient, IssueDetail, Dependency, Issue}`, `hub::hub_dir`,
`cli::{sanitize, format_row_body}`.

## Goal

Enter on a ready row opens a **detail pane**: the issue's title, description, and
its dependencies with each blocker's status. The detail is fetched **lazily on
Enter only** (one `bd show <id> --json` per confirm, ~0.2 s) on a worker thread so
the UI never blocks, mirroring the Slice 9 refresh-worker pattern. `Esc` returns
to the list with the selection preserved.

This extends the pure Slice 8 core (`Msg`/`Effect`/`ViewMode` already reserve the
extension points), adds a detail renderer to the Slice 9 view, and teaches the
Slice 9 runtime to execute a second effect — via a generalized effect executor
that Slice 11's `Search` will reuse.

## Design decisions (recorded so downstream slices and autoreview don't re-litigate)

1. **`Msg::DetailReady { id, detail: Result<IssueDetail, String> }` — tagged with
   the requested id, carrying success *or* a message.** The master plan's
   shorthand is `Msg::DetailReady(IssueDetail)`. Two additions are load-bearing:
   - **The `id`** lets `reduce` drop a **stale/mismatched** response. A user can
     Enter issue X, `Esc`, then Enter issue Y before X's ~0.2 s `bd show` returns;
     without the tag, X's late response would overwrite Y's pane. `reduce` accepts
     a completion only when its `id` equals the id the pane is currently bound to.
   - **The `Result`** carries a fetch **error message** so `detail_fetch_error_
     shows_message` can render "couldn't load" in the pane while the list stays
     intact — the runtime pre-formats and `sanitize`s the `bd` error into the
     `Err` string, keeping this core free of `BdError` types (same policy as
     `RefreshCompleted`'s pre-formatted warnings).

2. **`Effect::FetchDetail(String)` is the second reserved I/O variant.** `reduce`
   emits it on `Msg::OpenDetail`; the runtime spawns a `bd show` worker. Additive:
   no change to `reduce`'s signature or the runtime's effect-dispatch call site
   (which this slice generalizes).

3. **`ViewMode::Detail` + a single `detail: Option<DetailState>` field.** One
   source of truth for the pane. `DetailState` is:
   ```
   enum DetailState {
       Loading { id: String },
       Loaded(Box<IssueDetail>),          // id lives in issue.id
       Error { id: String, message: String },
   }
   ```
   `Box` on the large `Loaded` variant keeps the enum small (clippy
   `large_enum_variant`). The **currently-bound id** is read out of `detail` via
   `DetailState::id()` (returns the id for all three variants), so stale-matching
   has no second field to keep in sync. `view_mode == Detail ⇔ detail.is_some()`;
   both are set/cleared together. `App` gains one accessor `detail(&self) ->
   Option<&DetailState>` for the view (fields stay private per Slice 8 decision 8).

4. **`OpenDetail` fires only from `List`, and only with a selected row.** This
   guarantees "exactly one `bd` call per Enter": a second Enter while already in
   `Detail` is a no-op (no re-fetch), and Enter on an empty list is a no-op (no
   selected row → no effect). Cursor movement (`SelectNext`/`SelectPrev`) and
   filters never emit `FetchDetail` — only `OpenDetail` does — so browsing the
   list makes zero `bd` calls (acceptance criterion).

5. **A background refresh must not eject the user from the detail pane.** The
   Slice 8 `RefreshCompleted` set `view_mode = List` unconditionally on a
   successful snapshot; that would slam a detail pane shut mid-read when the 1-second
   refresh cadence lands. Fixed: `RefreshCompleted` promotes `Loading → List` only
   (the first-snapshot transition) and otherwise leaves `view_mode` untouched, so a
   refresh under an open `Detail` keeps it open. Rows/selection still recompute
   underneath (the detail is a snapshot of what the user opened); `Esc` returns to
   the (possibly re-clamped) list. Guarded by `refresh_under_detail_keeps_pane`.

6. **`Back` returns from `Detail` to `List` and is otherwise inert.** It clears
   `detail` and sets `view_mode = List`; it never touches `selection`, so the
   selection is preserved across an open/close. In `List`/`Loading` it stays a
   no-op (nothing to return from).

7. **Split layout by *frame* width: side-by-side ≥ 100 cols, stacked below else.**
   The vertical title/content/status split does not change width, so the content
   area width equals the frame width. `draw_detail_split` picks `Horizontal`
   (list left, detail right, `Percentage(50)/(50)`) when `area.width >= 100`, else
   `Vertical` (list top, detail bottom). Tested at 120×24 (side-by-side) and 80×24
   (stacked). The list keeps its full Slice 9 rendering (headers, selection,
   scroll) inside its half.

8. **The detail renderer sanitizes all bd-sourced text and wraps to the pane.**
   Title, id, description, and every dependency field are `sanitize`d (they are
   attacker-influenceable federated-repo text written straight to the terminal —
   same posture as `format_row_body`). Content is a `Vec<Line>` rendered through a
   wrapping `Paragraph`, so a long description wraps to the pane width instead of
   truncating. `sanitize` still neutralizes embedded newlines to `U+FFFD` (row-
   forging defense); wrapping handles width. A dependency renders as
   `⛔ {type}: {id} {title} ({status})`, e.g. `⛔ blocks: ra-z70 Blocker task
   (open)`, with `dependency_type`/`title`/`status` defaulting when bd omits them.
   The `⛔` glyph is our literal (not bd text). `Loading` shows `Loading <id>…`;
   `Error` shows the id and the message.

9. **Key hints become mode-aware.** Slice 9 omitted Enter/Esc from the title hint
   because they were inert. Now `List` advertises `enter detail` and `Detail`
   advertises `esc back`, so the UI never promises an inert key nor hides a live
   one.

## Runtime refactor (the required refactor)

The Slice 9 `ui_loop` matched a single `Effect::Refresh` inline. Generalize to an
**effect executor** so Slice 11's `Search` slots in with no call-site change:

- One `worker_handles: Vec<JoinHandle<()>>` (renamed from `refresh_handles`) holds
  **every** background worker (refresh *and* detail), pruned of finished handles
  on each spawn and joined unconditionally on shutdown — so a quit mid-`bd show`
  waits for that subprocess just as it already does for a mid-refresh one, never
  orphaning a child that touches the hub after fbd drops its lock.
- `fn execute_effect(effect, tx, worker_handles, paths, roster)` prunes finished
  handles, then spawns the matching worker: `Effect::Refresh → spawn_refresh`,
  `Effect::FetchDetail(id) → spawn_detail`. `ui_loop` calls it for each effect
  `reduce` returns.
- `spawn_detail(tx, paths, id)` spawns `detail_worker(BdCli::new(), paths, id, tx)`,
  which calls `gather_detail` and sends exactly one `Msg::DetailReady { id, detail }`.
- `gather_detail(bd, paths, id) -> Result<IssueDetail, String>` runs
  `bd.show(hub_dir(paths), id)` and maps a `BdError` to a `sanitize`d message. No
  version gate / ensure_hub: detail is reachable only from `List`, i.e. after a
  snapshot already hydrated the hub.

## Module layout

- **`src/app/mod.rs`** (edit): add `Effect::FetchDetail(String)`,
  `ViewMode::Detail`, `Msg::DetailReady { id, detail }` (and split `OpenDetail`/
  `Back` out of the no-op arm), `DetailState` enum + `id()`, the private `detail`
  field, the `detail()` accessor, reduce arms, and the `RefreshCompleted`
  `Loading→List` guard. New unit tests.
- **`src/app/view.rs`** (edit): mode-aware `draw` dispatch, `draw_detail_split`,
  `draw_detail`, mode-aware key hints. New render tests.
- **`src/runtime.rs`** (edit): `execute_effect`, `spawn_detail`, `detail_worker`,
  `gather_detail`; rename `refresh_handles → worker_handles`. New unit test.

No new files; no changes to `bd`, `snapshot`, `hub`, `refresh`, `config`,
`main.rs`, or `lib.rs`.

## Ordered TDD test list (red → green)

### `app/mod.rs` (unit, drive `reduce`, assert via accessors)

Helper additions: `detail(id, title, desc, deps)` builds an `IssueDetail`;
`open_on(app)` selects and opens the current row.

1. **`enter_requests_detail`**
   - Red: `Effect::FetchDetail`/`ViewMode::Detail`/`DetailState` don't exist.
   - Green: `app_with([row(ra, ra-1, 1)])`, `reduce(OpenDetail)` returns
     `vec![Effect::FetchDetail("ra-1")]`, `view_mode() == Detail`, `detail()` is
     `Some(Loading { id: "ra-1" })`.

2. **`cursor_movement_does_not_fetch`**
   - Green: on a loaded list, `reduce(SelectNext)` and `reduce(SelectPrev)` each
     return `vec![]` and leave `view_mode() == List` (browsing makes no `bd` call).

3. **`open_detail_noop_on_empty_list`** (edge)
   - Green: `app_with([])` (no selected row), `reduce(OpenDetail)` returns `vec![]`
     and stays in `List`.

4. **`detail_ready_stores_for_matching_id`**
   - Green: after `OpenDetail` on `ra-1`, `reduce(DetailReady { id: "ra-1",
     detail: Ok(detail("ra-1", …, deps)) })` ⇒ `detail()` is `Loaded` whose
     `issue.id == "ra-1"` and dependencies carried through.

5. **`stale_detail_response_is_dropped`**
   - Green: after `OpenDetail` on `ra-1`, a `DetailReady { id: "other", … }` is a
     no-op — `detail()` stays `Loading { id: "ra-1" }` (the mismatched response is
     dropped). Companion: Enter X → `Esc` → Enter Y, then X's late `DetailReady`
     is dropped (bound id is Y).

6. **`detail_fetch_error_shows_message`**
   - Green: after `OpenDetail`, `reduce(DetailReady { id: "ra-1", detail:
     Err("bd show failed: boom") })` ⇒ `detail()` is `Error` whose message
     contains `boom`, `view_mode() == Detail`, and `rows().len()` is unchanged
     (the list is intact behind the pane).

7. **`esc_returns_to_list`**
   - Green: `app_with([row, row])`, `SelectNext` (selection 1), `OpenDetail`,
     `DetailReady(Ok)`, then `reduce(Back)` ⇒ `view_mode() == List`, `detail() ==
     None`, `selection() == Some(1)` (preserved).

8. **`refresh_under_detail_keeps_pane`** (guards decision 5)
   - Green: while in `Detail`, a `RefreshCompleted { snapshot: Some(new rows) }`
     leaves `view_mode() == Detail` and `detail()` still `Some`, while `rows()`
     updates.

### `view.rs` (TestBackend; assert on specific cells/rows)

Helper: `app_in_detail(rows, id, detail_or_none)` drives `app_with(rows) →
OpenDetail → [DetailReady]` so the pane reaches the desired state via the real
reduce path (the selected row's id must equal the detail's id). `col_of(buf, y,
needle)` returns the starting column of `needle` on row `y`.

9. **`renders_detail_pane`** (wide, 120×24, Loaded)
   - Red: no detail renderer.
   - Green: buffer contains the issue title (`Blocked task`), a word of the
     description (`blocked`), and a dependency row containing `blocks:`,
     `ra-z70`, `Blocker task`, `(open)`, plus a `⛔` glyph on that row.

10. **`detail_pane_splits_right_when_wide`** (120×24)
    - Green: the dependency line (`blocks:`, detail-only) starts at column `>= 60`
      (right half), and a list-only marker (`▸ ` repo header) appears at column
      `< 60` (left half) — the panes are side by side.

11. **`detail_pane_stacks_below_when_narrow`** (80×24)
    - Green: the dependency line starts at column `< 40` (full-width) and on a row
      *below* the list's header row — stacked, not beside.

12. **`renders_detail_loading`** (80×24)
    - Green: after `OpenDetail` but before any `DetailReady`, the pane contains
      `Loading` and the id.

13. **`renders_detail_error_message`** (80×24)
    - Green: an `Error` detail renders the message text; the list rows are still
      present in the buffer.

14. **`wraps_long_description`** (80×24, stacked)
    - Green: a description far wider than the pane occupies two or more rows (its
      trailing words appear on a later row than its leading words).

15. **`key_hints_are_mode_aware`**
    - Green: in `List` the title row contains `enter`; in `Detail` it contains
      `esc`.

### `runtime.rs`

16. **`detail_worker_sends_ready_for_id`**
    - Red: `detail_worker`/`gather_detail` don't exist.
    - Green: spawn `detail_worker(FakeBdClient::with_show(detail("ra-1", …)),
      paths, "ra-1", tx)`; `rx.recv()` yields exactly one `Msg::DetailReady { id:
      "ra-1", detail: Ok(d) }` whose `d.issue.id == "ra-1"`; a second `recv`
      errors (worker done, channel closed).

17. **`detail_worker_maps_error`**
    - Green: with `FakeBdClient::with_show_err(..)`, the worker sends
      `DetailReady { id: "ra-1", detail: Err(msg) }` whose `msg` names the failure
      (surfaced, not swallowed).

## Edge cases

- **Stale / out-of-order `bd show`**: dropped by the id tag (decision 1, tests 5).
- **Fetch error**: rendered in the pane; the list is untouched (tests 6, 13).
- **Empty list**: `OpenDetail` is a no-op (no selected row, test 3).
- **Refresh under an open pane**: the pane stays open (decision 5, test 8).
- **Narrow terminal**: the pane stacks below the list; a long description wraps
  rather than truncating (tests 11, 14).
- **bd-sourced control chars** in title/description/deps: `sanitize`d at the
  render boundary, so no forged rows or terminal-control injection (decision 8).
- **Dependency with omitted `title`/`status`/`type`**: each defaults (empty /
  `?` / `depends on`) so a partial `bd show` payload still renders a line.
- **Quit mid-`bd show`**: the detail worker's handle is joined on shutdown like a
  refresh worker, so its subprocess is not orphaned.

## Out of scope (later slices)

- Cross-repo search input/mode + `Effect::Search` (Slice 11) — the effect executor
  is generalized here so Search only adds an arm.
- Copy-context (`y`) string building (Slice 12).
- Deduping repeat `bd show` calls in `reduce` (each Enter fetches; guarded to one
  call per Enter by the `List`-only + no-op-in-`Detail` rule, so no dedup needed).
- Rich dependency styling beyond the `⛔` prefix / status suffix; comment threads.
- Interactive-TUI smoke (no tty in the agent harness — the documented manual step).

## Verification (all four must be green)

```
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
cargo test --test bd_integration
```

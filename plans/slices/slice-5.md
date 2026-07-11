# Slice 5 — Snapshot: the read model the UI consumes

Bead: `federated-beads-dxh.6` (child of epic `federated-beads-dxh`).
Mol workflow root: `federated-beads-mol-85l`.
Master plan: `plans/fbd-v1-implementation-plan.md` (Slice 5 + global sections).
Depends on: Slice 4 (`federated-beads-dxh.5`, merged — `refresh::run`,
`PrefixMap`, `RepoError`, `RefreshOutcome { prefix_map, errors, synced_at }`).

## Goal

One call produces everything the ready screen needs, UI-agnostic and
serializable. Introduce `src/snapshot.rs`:

- `snapshot::fetch(bd, hub, prefix_map, fetched_at) -> Result<Snapshot, BdError>`
  — call `bd ready --json` on the hub, attribute each issue to its source repo
  via the `PrefixMap`, sort for display, and stamp a caller-supplied fetch time.
- `Snapshot { rows: Vec<Row>, fetched_at: SystemTime }`, `Row { issue: Issue,
  repo_name: String }`. Both derive `Serialize` so Slice 6 can emit
  `fbd snapshot --json` verbatim.

**This slice writes only the read model.** No CLI subcommand, no TUI, no
grouping structure — grouping is a view concern (Slice 9); rows merely *carry*
`repo_name` so a view can group by it. `ensure_hub`/`refresh` are assumed to have
run; `fetch` does not call them (Slice 6 composes `ensure_hub → refresh →
fetch`).

## Design decisions (recorded so downstream slices and autoreview don't re-litigate)

1. **`fetch` is fallible and takes an injected clock.** The master plan's
   shorthand `fetch(bd, hub, prefix_map) -> Snapshot` is realized as
   `fetch(bd, hub, prefix_map, fetched_at: SystemTime) -> Result<Snapshot,
   BdError>`:
   - `bd.ready(hub)` is a fallible subprocess call, so the return is `Result`;
     a `ready` failure propagates as `BdError` (the caller keeps the stale view).
   - `fetched_at` is a **parameter**, not `SystemTime::now()` buried in the
     function — the master plan and orchestrator require no hidden clock reads in
     logic (test determinism). Slice 6 passes `outcome.synced_at` (or a real
     `now`); tests pass a fixed instant. `SystemTime` matches `RefreshOutcome::
     synced_at`'s type so the two compose without conversion.

2. **`repo_name` is the source repo's directory basename.** `prefix_map.repo_for(
   &issue.id)` yields `Option<&RepoEntry>`; the row's `repo_name` is that repo
   path's final component (`/Users/brian/dev/session-tui` → `session-tui`),
   matching the Slice 9 group-header render (`▸ session-tui`). A path with no
   final component falls back to its full string form (robustness; not expected
   for real repos).

3. **Unattributed → explicit "unknown" bucket.** An id under no configured prefix
   (or under a *collided* prefix, where `repo_for` already returns `None`) gets
   `repo_name = UNKNOWN_REPO` (`pub const UNKNOWN_REPO: &str = "unknown"`), a
   documented sentinel Slice 9 renders as its own group. (A real repo literally
   named `unknown` would share the bucket — an accepted v1 edge, noted
   out-of-scope.)

4. **Sort: priority asc (0 first), then `updated_at` desc, then id asc.**
   - `priority` is `i64`, 0 = highest, so ascending puts P0 first.
   - `updated_at` is an `Option<String>` ISO-8601/RFC-3339 UTC timestamp
     (`"2026-07-11T12:41:26Z"`); fixed-width UTC `Z` timestamps sort
     lexicographically in chronological order, so a plain string compare on the
     reversed operands gives newest-first without pulling in `chrono`. `None`
     (omitted `updated_at`) sorts **last** within a priority tier.
   - A final `id` ascending tiebreak makes the order **total and deterministic**
     regardless of `bd ready`'s emission order — important because the sorted
     `rows` are serialized for `--json`.
   Implemented with a stable `sort_by` comparator chain.

5. **Row carries `repo_name` only, not the repo path.** The master plan pins
   `Row { issue, repo_name }`. Slice 12's copy-context (which needs the full
   `cd <path>`) reconstructs the path by re-consulting the `PrefixMap` (available
   in app state) via the issue id — not by widening `Row` here. Keeps the read
   model minimal and the serialized shape stable.

6. **Enabling change in `src/refresh.rs`: `PrefixMap::from_pairs` made public.**
   Slice 4's map constructor was the private `PrefixMap::build`. Snapshot's unit
   tests (and any direct consumer) need to construct a populated map without
   running a whole refresh, so it is renamed `from_pairs` and exposed `pub`
   (collision detection unchanged; `run`'s single call site updated). This is the
   only edit to prior-slice code.

## Scope (in)

- `src/snapshot.rs` — new module: `Snapshot`, `Row`, `UNKNOWN_REPO`, `fetch`,
  and the private `repo_name` helper. `#[cfg(test)]` unit tests.
- `src/lib.rs` — add `pub mod snapshot;`.
- `src/refresh.rs` — rename private `PrefixMap::build` → `pub fn from_pairs`;
  update the one call site.

## Scope (out)

- No `fbd snapshot` CLI subcommand or `--json` printing (Slice 6).
- No integration test (Slice 6 adds `snapshot_command_end_to_end`); this slice's
  integration suite is unchanged and still skips cleanly without bd.
- No grouped/`GroupedSnapshot` structure — grouping stays a Slice 9 view concern.
- No TUI, no staleness/age computation (Slice 9 injects a render clock).
- No widening of `Row` with the repo path (see decision 5).

## Ordered TDD test list (red → green)

Unit tests in `src/snapshot.rs` `#[cfg(test)]`. A helper parses the checked-in
`tests/fixtures/ready.json` (`include_str!`) into `Vec<Issue>`; another builds a
`PrefixMap` from `(prefix, RepoEntry)` pairs via the newly-public `from_pairs`.
A fixed `SystemTime` (`UNIX_EPOCH + Duration`) is passed as `fetched_at`.

1. **`merges_ready_with_attribution`**
   - Red: `snapshot::fetch` / `Snapshot` / `Row` do not exist (compile error).
   - Green: `FakeBdClient::with_ready(ready_fixture())` (ids `ra-z70`, `ra-shr`)
     + a `PrefixMap` mapping prefix `ra` → repo `/dev/session-tui`. `fetch`
     returns a `Snapshot` whose two rows each carry `repo_name == "session-tui"`,
     and whose `issue.id`s are exactly the fixture ids. `fetched_at` equals the
     injected instant.

2. **`sorts_by_priority_then_updated`**
   - Red: naive impl preserves `bd`'s order / mis-sorts ties.
   - Green: program the fake with a **scrambled** hand-built `Vec<Issue>` the
     fixture can't express: e.g. `[P1@t=…:25, P0@t=…:26, P1@t=…:27]`. Assert the
     row order is `P0` first, then the two `P1`s newest-`updated_at`-first
     (`:27` before `:25`). Confirms priority-asc then updated-desc.

3. **`groups_by_repo`**
   - Red: rows don't carry per-repo attribution usable for grouping.
   - Green: `PrefixMap` maps `ra` → `/dev/repo-a`, `rb` → `/dev/repo-b`; ready
     issues span both prefixes. Assert grouping rows by `repo_name` yields
     exactly `{repo-a: [ra ids], repo-b: [rb ids]}` — grouping data lives on the
     row, no grouped struct required.

4. **`unattributed_goes_to_unknown_bucket`**
   - Red: an id with no matching prefix panics / is dropped / gets an empty name.
   - Green: `PrefixMap` maps only `ra`; a ready issue `zz-999` yields a row with
     `repo_name == UNKNOWN_REPO` (`"unknown"`); attributed rows are unaffected.

5. **`serializes_to_json`** (acceptance criterion: "Snapshot serializes to JSON")
   - Red: `Snapshot`/`Row` don't derive `Serialize`.
   - Green: `serde_json::to_value(&snapshot)` succeeds; the JSON has a `rows`
     array whose first element exposes the issue `id` and a top-level `repo_name`
     on the row, plus a `fetched_at` key. Guards the `--json` contract Slice 6
     depends on.

6. **`ready_error_propagates`**
   - Red: a `bd ready` failure is swallowed / panics.
   - Green: `FakeBdClient::with_ready_err(..)` ⇒ `fetch` returns `Err(BdError)`
     (the caller keeps the last good snapshot).

## Edge cases

- **Empty ready list** → `Snapshot { rows: [], fetched_at }` (implicit; covered
  by the shape of #1/#5 with zero rows — not a separate test unless red reveals a
  gap).
- **`updated_at` absent** on some rows → those sort last within their priority
  tier (decision 4); not separately tested unless the sort red exposes a gap.
- **Collided prefix** → `repo_for` already returns `None` (Slice 4), so the row
  lands in the `unknown` bucket via the same path as #4 (no new code).

## Verification (all four must be green)

```
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
cargo test --test bd_integration
```

## Autoreview outcomes (post-implementation)

Codex (gpt-5.6-sol, high) review of the branch vs main, three passes; final
pass clean ("patch is correct", 0.93).

1. **P2 — duplicate directory basenames collapse distinct repos.** Two roster
   repos sharing a basename (`/work/a/api`, `/work/b/api`) both produced
   `repo_name == "api"`, conflating them in downstream grouping/filtering. Fixed:
   `fetch` makes `repo_name` roster-unique.
2. **P2 — full filesystem paths leaked into the serialized `repo_name`.** The
   first fix disambiguated by falling back to the full path, which the serialized
   snapshot would leak (local usernames/layout) and which violated the
   basename-only display contract. Fixed properly: added `PrefixMap::attribution`
   to expose the matched id-prefix (a unique, short, non-sensitive repo id) and
   disambiguate a collided basename as `basename (prefix)` — e.g. `api (ra)` /
   `api (rb)` — never a path. Regression test:
   `disambiguates_duplicate_basenames`.
3. **P2 — lexical `updated_at` sort not universally chronological (RFC3339).**
   *Rejected as speculative for bd's contract.* bd 1.1.0 emits homogeneous
   whole-second UTC-`Z` timestamps, and every row in one `bd ready` call shares
   that format, so a lexical compare is chronological; the final `id`-ascending
   tiebreak keeps the order total and deterministic regardless. Adding a datetime
   crate is a deliberate project-wide dependency decision, out of scope for this
   read-model slice. The assumption is documented inline at the comparator; the
   contingency is tracked in `federated-beads-23v` (parse timestamps if a future
   bd emits fractional seconds/offsets).

# Slice 4 — Refresh pipeline + prefix map + collision detection

Bead: `federated-beads-dxh.5` (child of epic `federated-beads-dxh`).
Mol workflow root: `federated-beads-mol-cfh`.
Master plan: `plans/fbd-v1-implementation-plan.md` (Slice 4 + global sections).
Depends on: Slice 3 (`federated-beads-dxh.4`, merged — `ensure_hub`, `reset`,
`hub_dir`, `read_hub_roster`).

## Goal

One refresh turns N source repos into a fresh hub plus a repo-attribution map,
never failing wholesale on one bad repo. Introduce `src/refresh.rs`:

- `refresh::run(bd, roster, paths) -> Result<RefreshOutcome, RefreshError>` —
  export every roster repo (sequentially), then `bd repo sync` the hub once,
  build a prefix→repo map, and collect per-repo failures instead of aborting.
- `PrefixMap::repo_for(id) -> Option<&RepoEntry>` — longest-configured-prefix
  followed by `-` match; ambiguous (collided) prefixes resolve to `None`.
- A process-level advisory file lock on `<hub>/.fbd.lock` held across the whole
  refresh, so two concurrent fbd instances cannot run `repo sync` against the
  same embedded-Dolt hub at once.

**This slice writes only the refresh pipeline.** No snapshot read model, CLI
subcommands, or TUI — those are Slices 5–12. `ensure_hub` (Slice 3) is assumed
to have run already; `run` does not call it (Slice 6 wires `ensure_hub → refresh
→ fetch`).

## Resolved facts (verified against real bd 1.1.0, build 8e4e59d39)

- **Prefix source.** `<repo>/.beads/metadata.json` is JSON with a
  `"dolt_database": "<prefix>"` key (confirmed: a `bd init --prefix ra` repo
  writes `{"database":"dolt","backend":"dolt","dolt_mode":"embedded",
  "dolt_database":"ra","project_id":"…"}`). fbd reads only `dolt_database`; a
  tolerant serde struct (no `deny_unknown_fields`, everything else ignored)
  parses it.
- Issue ids are `<prefix>-<hash>` (e.g. `ra-2hc`), per master-plan observation
  §6. Attribution is by id prefix; there is no CLI-exposed `source_repo`.

## Design decisions (recorded so downstream slices and autoreview don't re-litigate)

1. **`run`'s result models three outcomes.** `Ok(RefreshOutcome)` on a completed
   refresh (individual repos may still be listed in `errors`); `Err(RefreshError::
   AlreadyRefreshing)` when the hub lock is already held (a *distinguishable,
   matchable* decline — not a crash, the status bar renders "another fbd is
   refreshing" and retries next manual refresh); `Err(RefreshError::Sync(..))`
   when the single `repo sync` fails (the hub was not updated at all — a
   whole-refresh failure, distinct from one repo's export failing); `Err(
   RefreshError::Lock(..))` for a genuine lock-file IO error. Keeping "already
   refreshing" as a dedicated `Err` variant preserves the master plan's success
   struct `RefreshOutcome { prefix_map, errors, synced_at }` verbatim.
2. **`RefreshOutcome { prefix_map: PrefixMap, errors: Vec<RepoError>, synced_at:
   SystemTime }`** — exactly the master-plan fields.
   - `RepoError` = a per-repo *operational* failure surfaced but not fatal:
     `Export { repo, source: BdError }` (its `bd export` failed; sync still ran,
     other repos still hydrate) and `Metadata { repo, detail }` (its
     `.beads/metadata.json` prefix was unreadable, so it cannot be attributed).
   - **Prefix collisions are a property of the built map, not a `RepoError`.** A
     refresh with two repos declaring the same prefix still *succeeds*;
     attribution for that prefix is merely ambiguous. `PrefixMap` records
     `Collision { prefix, repos }` and `repo_for` returns `None` for ids under a
     collided prefix (the "ambiguous → unknown bucket" behavior from the master
     plan's risk table). `PrefixMap::collisions() -> &[Collision]` exposes them
     for the status bar. This separates operational errors from attribution
     ambiguity while keeping the plan's three-field struct.
3. **Longest-prefix-then-dash match.** `repo_for(id)` selects every configured
   prefix `P` such that `id.starts_with(&format!("{P}-"))`, then picks the
   longest. The trailing `-` guard already disambiguates `app` vs `app2` for an
   id `app2-xyz` (it does not start with `app-`); "longest wins" additionally
   covers a configured prefix that is itself a dash-prefix of another (e.g.
   `app` and `app-foo`).
4. **Lock mechanism: `fs2` crate advisory flock.** `run` opens/creates
   `<hub>/.fbd.lock` and calls `FileExt::try_lock_exclusive`. Held-elsewhere ⇒
   `io::ErrorKind::WouldBlock` ⇒ `RefreshError::AlreadyRefreshing`. The lock is
   released when the guard's `File` drops (closing the fd releases the OS lock).
   `fs2` is the simplest option that clippy accepts and is exercised in-process:
   `flock` treats two separate `open()`s of the same file independently, so a
   second `try_lock_exclusive` from the same process is denied — making the
   "already refreshing" path unit-testable without spawning a subprocess.
   A `HubLock::try_acquire(hub)` helper is `pub` so the test can hold the lock
   while calling `run`.
5. **Sequential exports (v1).** Exports run one repo at a time; parallelism is a
   documented v2 optimization (master-plan refactor note; 5 × ~0.3 s is fine).
6. **`synced_at` is `SystemTime::now()`** stamped at successful completion; later
   slices inject a render clock for staleness, but the refresh event's own time
   is a real wall-clock reading, not injected.

## Order of operations in `run`

1. `hub = hub_dir(paths)`; `create_dir_all(&hub)` (robustness — ensure_hub
   normally made it; refresh must be able to open the lock file regardless).
2. Acquire the advisory lock on `<hub>/.fbd.lock`. If `WouldBlock` ⇒ return
   `Err(AlreadyRefreshing)` immediately (no exports, no sync).
3. For each roster entry, in order:
   - `bd.export(&repo)`: on `Err` push `RepoError::Export { repo, source }` and
     continue (do **not** skip the metadata read — attribution of already-synced
     ids should still work).
   - Read `<repo>/.beads/metadata.json` `dolt_database`: on failure push
     `RepoError::Metadata { repo, detail }`; on success collect `(prefix, repo)`.
   - An absent roster path (`!repo.path.exists()`): push `RepoError::Metadata`
     (its metadata is unreadable) and skip export. (ensure_hub already warned;
     refresh records it too so a repo deleted between ensure and refresh still
     surfaces.)
4. `bd.repo_sync(&hub)` exactly once. On `Err` ⇒ return `Err(RefreshError::
   Sync(..))` (the hub was not updated; the stale view stays browsable and the
   prior refresh's attribution still applies).
5. Build `PrefixMap` from the collected `(prefix, repo)` pairs, detecting
   collisions (group by prefix; any prefix with ≥2 repos becomes a `Collision`
   and is excluded from the lookup table).
6. Return `Ok(RefreshOutcome { prefix_map, errors, synced_at: now })`. The lock
   guard drops here, releasing `<hub>/.fbd.lock`.

## Scope (in)

- `src/refresh.rs` — new module: `RefreshOutcome`, `RepoError`, `RefreshError`,
  `PrefixMap`, `Collision`, `HubLock` (+ `try_acquire`), `run`, and the
  `metadata.json` prefix reader.
- `src/lib.rs` — add `pub mod refresh;`.
- `Cargo.toml` — add `fs2`.
- `tests/bd_integration.rs` — add gated `refresh_two_repos`.
- (Helpers from Slice 3 — `build_ready_fixture_repo_with_prefix` — are reused
  as-is; no helper change expected.)

## Scope (out)

- No snapshot / read model (`snapshot.rs`), CLI subcommands, or TUI.
- No parallel exports (v2).
- No call to `ensure_hub` from `run` (Slice 6 composes them).
- No stale-hub-entry pruning (tracked separately for Slice 7).
- No retry/backoff on a held lock — the caller retries on the next manual
  refresh, per the master plan.

## Ordered TDD test list (red → green)

Unit tests live in `src/refresh.rs` `#[cfg(test)]`, driving `FakeBdClient` +
tempdirs (a repo's `.beads/metadata.json` is seeded directly).

1. **`exports_all_then_syncs_once`**
   - Red: `refresh::run` does not exist (compile error).
   - Green: roster of two seeded repos ⇒ recorded calls are `Export(a)`,
     `Export(b)`, then exactly one `RepoSync(hub)`, in that order.

2. **`collects_per_repo_errors`**
   - Red: naive impl aborts on the first export error / never records it.
   - Green: `FakeBdClient::with_export_err(b, …)`, roster `[a, b]`. Result is
     `Ok`; `errors` contains a `RepoError::Export { repo: b, .. }`; `RepoSync`
     still recorded once; the prefix map still attributes `a`'s ids (a hydrates).

3. **`reads_prefix_from_metadata`**
   - Red: no metadata reader / prefix hard-coded.
   - Green: seed `<a>/.beads/metadata.json` with `dolt_database: "ra"`; the built
     `PrefixMap::repo_for("ra-2hc")` returns `a`.

4. **`builds_prefix_map`**
   - Red: map not constructed from all repos.
   - Green: two repos with prefixes `ra`, `rb` ⇒ `repo_for("ra-…")` → a,
     `repo_for("rb-…")` → b, `repo_for("zz-…")` → `None`.

5. **`flags_prefix_collisions`**
   - Red: duplicate prefixes silently overwrite / mis-attribute.
   - Green: two repos both `dolt_database: "dup"` ⇒ `prefix_map.collisions()`
     lists `dup` (with both repo paths); `repo_for("dup-x")` is `None`
     (ambiguous → unknown bucket).

6. **`longest_prefix_wins`**
   - Red: first/shortest match wins → mis-attribution.
   - Green: prefixes `app` (repo a) and `app2` (repo b); `repo_for("app2-xyz")`
     → b (never a); `repo_for("app-xyz")` → a.

7. **`metadata_read_failure_is_a_repo_error`**
   - Red: unreadable metadata panics / is silently dropped.
   - Green: a roster repo with no `.beads/metadata.json` ⇒ `errors` has a
     `RepoError::Metadata { repo, .. }`; other repos still map; result is `Ok`.

8. **`sync_failure_is_fatal`**
   - Red: sync error swallowed / mislabeled.
   - Green: `FakeBdClient::with_repo_sync_err(..)` ⇒ `run` returns
     `Err(RefreshError::Sync(..))`.

9. **`declines_when_lock_already_held`** (lock behavior)
   - Red: no lock / second attempt blocks or corrupts.
   - Green: `HubLock::try_acquire(&hub)` held open in the test; `run(...)` returns
     `Err(RefreshError::AlreadyRefreshing)` and records **no** `Export`/`RepoSync`
     calls (it declined before doing any work).

10. **`lock_releases_after_refresh`** (lock lifecycle)
    - Red: lock not released / re-acquire blocks.
    - Green: a full `run(...)` completes `Ok`; afterwards `HubLock::try_acquire(
      &hub)` succeeds (the refresh released the lock on drop).

### Integration (gated, `tests/bd_integration.rs`)

11. **`refresh_two_repos`**
    - Skips cleanly without bd (`SKIP: bd not installed`). With bd: build two
      real fixture repos (prefixes `ra`, `rb`), `ensure_hub` them, then
      `refresh::run`. Assert: `errors` empty; the hub's real `bd ready` (via
      `BdCli::ready(&hub_dir)`) contains issues from both repos; and
      `outcome.prefix_map.repo_for(<a ready id>)` / `repo_for(<b ready id>)`
      attribute each to the correct repo. Proves the export→sync→attribute
      pipeline end to end against real bd.

## Edge cases

- **One repo's export fails** → `RepoError::Export`, sync still runs, other repos
  hydrate. (#2)
- **Unreadable / missing metadata** → `RepoError::Metadata`, repo unattributed,
  refresh still `Ok`. (#7)
- **Prefix collision** → `Collision` recorded, ambiguous ids → `None`; refresh
  still `Ok`. (#5)
- **Prefix that is a dash-prefix of another** (`app` vs `app2`, `app` vs
  `app-foo`) → longest wins. (#6)
- **Lock already held** → `AlreadyRefreshing`, zero side effects. (#9)
- **Sync fails** → fatal `Err(Sync)`; stale view stays browsable. (#8)
- **Empty roster** → no exports; sync still runs once (bd no-ops); empty map,
  empty errors. (implicit; asserted via #1 shape with zero repos is not added as
  a separate test unless red reveals a gap.)

## Verification (all four must be green)

```
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
cargo test --test bd_integration
```

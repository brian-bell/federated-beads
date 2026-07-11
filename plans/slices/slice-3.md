# Slice 3 — Hub lifecycle: `ensure_hub` + `reset`

Bead: `federated-beads-dxh.4` (child of epic `federated-beads-dxh`).
Master plan: `plans/fbd-v1-implementation-plan.md` (Slice 3 + global sections).
Depends on: Slice 2 (`federated-beads-dxh.3`, merged — `BdClient` trait, `BdCli`,
`FakeBdClient`).

## Goal

First run creates a working bd "hub" workspace; subsequent runs reconcile the
roster into it. Introduce `hub::ensure_hub(bd, paths, roster) -> HubStatus` (init
the hub once when missing, then `repo add` only the roster entries the hub does
not already track, warning on absent roster paths instead of failing) and
`hub::reset(paths)` (delete the hub dir, guarded so it can only ever remove a
path inside fbd's data dir).

**This slice writes only hub lifecycle.** No refresh pipeline, prefix map,
snapshot, CLI subcommands, or TUI — those are Slices 4–12.

## Resolved open question — how to read the hub's current roster

The master plan left this open: read the hub roster via `bd repo list --json`, or
fall back to parsing `<hub>/.beads/config.yaml` `repos.additional`. **Resolved
empirically against real bd 1.1.0 (build 8e4e59d39)** with a throwaway hub +
two `bd init`'d repos + `bd repo add`:

- **`bd repo list --json` ignores `--json`.** It prints the same human-readable
  text as the plain command, not JSON:

  ```
  Primary repository: .

  Additional repositories:
    - /tmp/fbd-probe/ra
    - /tmp/fbd-probe/rb
  ```

  Because `BdCli::repo_list` deserializes stdout as JSON (`run_json`), it returns
  a `Parse` `BdError` against real bd. **`repo_list` is therefore unusable for
  roster reads.** (It stays in the trait — a Slice 2 artifact — but this slice
  does not call it. Follow-up bead filed to revisit/remove it.)

- **`<hub>/.beads/config.yaml` is the reliable source.** After `bd repo add`, bd
  rewrites config.yaml to a minimal active block:

  ```yaml
  repos:
    primary: "."
    additional:
      - "/tmp/fbd-probe/ra"
      - "/tmp/fbd-probe/rb"
  ```

  A **fresh** `bd init` (no repos added) writes a fully *commented* template with
  **no active `repos:` key** — so the parse must tolerate an absent/empty
  `repos` and yield an empty additional list.

**Decision: read the hub roster by parsing `<hub>/.beads/config.yaml`
`repos.additional` with `serde_yaml`.** Both observed shapes are covered by a
tolerant struct parsed as `Option<HubConfig>` (empty/all-comment doc → `None` →
empty roster). Evidence recorded above; the integration test asserts the chosen
path end to end. (serde_yaml is deprecated-but-stable and the plan-specified
choice; a follow-up bead tracks revisiting the YAML dependency.)

## Scope (in)

- `src/hub.rs` — new module:
  - `HubStatus { warnings: Vec<String> }`, `HubError` (wraps `BdError` / IO /
    unsafe-reset).
  - `hub_dir(paths) -> PathBuf` = `paths.data_dir().join("hub")` (master plan
    decision #5: `~/.local/share/federated-beads/hub/`).
  - `ensure_hub(bd, paths, roster) -> Result<HubStatus, HubError>`.
  - `reset(paths) -> Result<(), HubError>` with a containment guard.
  - `read_hub_roster(hub_dir) -> Result<Vec<PathBuf>, HubError>` (config.yaml
    parse; `pub` so the integration test asserts via the same path).
- `src/lib.rs` — add `pub mod hub;`.
- `Cargo.toml` — add `serde_yaml`.
- `tests/helpers/mod.rs` — generalize the fixture builder to take a prefix
  (`build_ready_fixture_repo_with_prefix`), keep `build_ready_fixture_repo` as
  the `"ra"` wrapper.
- `tests/bd_integration.rs` — add gated `ensure_hub_end_to_end`.

## Scope (out)

- No refresh pipeline, prefix map, snapshot, CLI subcommands, TUI.
- No removal of `BdClient::repo_list` (Slice 2 surface; only a follow-up bead).
- No `bd repo sync` / hydration here — `ensure_hub` only registers repos.
- No canonicalization of the *roster config file* (Slice 7 concern); this slice
  canonicalizes paths only for the repo-add reconciliation compare.

## Design

`ensure_hub`:
1. `hub = hub_dir(paths)`; `fs::create_dir_all(&hub)` (bd `init` needs the dir to
   exist and runs with it as cwd — no `-C`).
2. If the hub is not yet initialized (`<hub>/.beads` absent), `bd.init(&hub,
   "hub")` exactly once. If already initialized, skip init.
3. Read the hub's current additional roster from config.yaml
   (`read_hub_roster`); build a set of **normalized** entries
   (canonicalize-if-exists, else raw) for comparison.
4. For each roster entry:
   - If the path does not exist on disk → push a warning, `continue` (no
     `repo_add`).
   - Normalize; if already in the hub set → skip (idempotent).
   - Else `bd.repo_add(&hub, &canonical)` (pass the canonical path so a second
     run compares canonical-to-canonical and adds nothing twice).
5. Return `HubStatus { warnings }`.

`reset`:
- `hub = hub_dir(paths)`; `ensure_within(paths.data_dir(), &hub)?` (guard); if
  `hub.exists()`, `fs::remove_dir_all(&hub)`.
- `ensure_within(parent, target)` rejects `target == parent` or any `target` not
  a descendant of `parent` (component-wise `Path::starts_with`), returning
  `HubError::UnsafeResetPath`. Defense-in-depth: `hub_dir` is always
  `data_dir/hub`, but the guard is unit-tested directly with an outside path.

## Ordered TDD test list (red → green)

Unit tests live in `src/hub.rs` `#[cfg(test)]`, driving `FakeBdClient` +
tempdirs. The hub-roster read is a filesystem read, so tests seed
`<hub>/.beads/config.yaml` directly to simulate an existing hub.

1. **`creates_hub_when_missing`**
   - Red: `hub::ensure_hub` doesn't exist (compile error).
   - Green: hub dir absent under a tempdir base ⇒ `init` recorded exactly once
     with prefix `"hub"`. (Fake `init` is a no-op, so no `.beads` appears; that
     is fine — the assertion is on the recorded `Call::Init`.)

2. **`skips_init_when_hub_already_initialized`**
   - Red: naive impl always inits.
   - Green: seed `<hub>/.beads/config.yaml` (existing hub) ⇒ no `Call::Init`
     recorded.

3. **`adds_missing_repos_only`**
   - Red: naive impl adds every roster entry.
   - Green: seed `<hub>/.beads/config.yaml` with `additional: [<canonical ra>]`;
     roster = `[ra, rb]` (both real tempdirs). Exactly one `Call::RepoAdd` for
     `rb`; none for `ra`.

4. **`tolerates_absent_repo_paths`**
   - Red: naive impl `repo_add`s a nonexistent path (or errors).
   - Green: roster entry pointing at a nonexistent path ⇒ `HubStatus.warnings`
     mentions that path, and **no** `Call::RepoAdd` for it; the call still
     returns `Ok`.

5. **`ensure_hub_is_idempotent`** (refactor / master-plan refactor step)
   - Red/guard: run `ensure_hub` twice against a real seeded config.yaml that we
     update between runs to reflect the add. Simpler deterministic form: with the
     hub roster already listing the repo (canonical), two successive
     `ensure_hub` calls record **zero** `Call::RepoAdd`. Proves the
     normalize-compare prevents duplicate adds.

6. **`reset_guard_rejects_path_outside_data_dir`**
   - Red: `ensure_within` doesn't exist.
   - Green: `ensure_within(data_dir, "/etc")` is `Err(UnsafeResetPath)`;
     `ensure_within(data_dir, data_dir.join("hub"))` is `Ok`;
     `ensure_within(data_dir, data_dir)` (equal) is `Err`.

7. **`reset_removes_hub_dir`**
   - Red: `reset` doesn't exist.
   - Green: create `<data_dir>/hub/.beads/marker`; `reset(paths)` removes the hub
     dir but leaves `data_dir` intact. `reset` on an absent hub is `Ok` (no-op).

8. **`read_hub_roster_*`** (parse-path coverage)
   - `read_hub_roster_parses_additional`: write the minimal `repos:` block ⇒
     returns both paths.
   - `read_hub_roster_empty_when_no_repos_key`: write the commented template (no
     active `repos:`) ⇒ empty vec (not an error).
   - `read_hub_roster_missing_file_is_empty`: no config.yaml ⇒ empty vec.

### Integration (gated, `tests/bd_integration.rs`)

9. **`ensure_hub_end_to_end`**
   - Skips cleanly without bd. With bd: build two fixture repos (prefixes `ra`,
     `rb`) in a tempdir, roster = both, `Paths::with_base(tmp)`. Call
     `ensure_hub(&BdCli::new(), &paths, &roster)`; assert `read_hub_roster` of
     the hub lists both repos (canonicalized), and `warnings` is empty. Proves
     the config.yaml read path against real bd.

## Edge cases

- **Absent roster path** → warning, skipped (never a hard error). Covered by #4.
- **Fresh hub (no active `repos:` key)** → empty roster, all present repos added.
  Covered by #8b + #1/#3 interplay.
- **Non-canonical / duplicate roster entries** → normalize-compare dedupes vs the
  hub; canonical paths are what get stored. Covered by #3/#5.
- **Reset safety** → guard rejects equal/outside paths. Covered by #6.
- **Missing config.yaml on read** → empty, not error. Covered by #8c.

## Verification (all four must be green)

```
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
cargo test --test bd_integration
```

## Autoreview outcomes (post-implementation)

Fixed (in-scope, single-process correctness), each with a regression test:

1. Within-run dedupe: duplicate/aliased roster entries add exactly once (mutable
   tracked set, insert canonical after add).
2. Hub-relative stored entries: resolve non-absolute `config.yaml` entries
   against the hub dir before canonicalizing, so bd-stored relative paths match.
3. `reset` clears a **dangling symlink** at the hub path (`symlink_metadata`,
   no-follow), so recovery isn't a silent no-op.
4. `is_initialized` keys on the embedded Dolt db (`.beads/embeddeddolt`), not a
   bare `.beads/`, matching bd's own "already initialized" criterion — an
   interrupted init re-inits instead of masking a broken hub.
5. Genuine `bd init` failures propagate (no swallowing).

Deferred (out of scope for Slice 3; follow-up beads filed):

- **Cross-process concurrency** of `ensure_hub` (init check-then-act race;
  unlocked roster read/modify/write). Master plan line 81 assigns process-level
  hub locking to Slice 4's refresh path; `ensure_hub` is deliberately
  single-process here. An interim init-race re-check was reverted because it can
  mask a partial-init failure. See `federated-beads-dxh.5` comment.
- **Stale-entry pruning**: reconciliation is additive-only (the plan's
  `adds_missing_repos_only`); removing hub entries dropped from the roster needs
  a new `BdClient::repo_remove` capability or a reset-rebuild, best designed with
  Slice 7's roster CLI. `fbd reset` rebuilds from the current roster meanwhile.
  See `federated-beads-dxh.15`.

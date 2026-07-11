# Slice 6 — Headless tracer bullet: `fbd snapshot`, version gate, doctor, reset CLI

Bead: `federated-beads-dxh.7` (child of epic `federated-beads-dxh`).
Mol workflow root: `federated-beads-mol-4y2`.
Master plan: `plans/fbd-v1-implementation-plan.md` (Slice 6 + global sections).
Depends on: Slices 0–5 (merged). Uses `config::{Config, Paths, RepoEntry}`,
`bd::{BdClient, BdCli, BdVersion, FakeBdClient}`, `hub::{ensure_hub, reset,
hub_dir, HubStatus, HubError}`, `refresh::{run, RefreshOutcome, RefreshError,
PrefixMap, RepoError}`, `snapshot::{fetch, Snapshot, Row, UNKNOWN_REPO}`.

## Goal

Make the whole pipeline usable and debuggable from the command line **before any
TUI exists** — the end-to-end tracer bullet. `fbd snapshot` runs
`ensure_hub → refresh → fetch → print`; `fbd doctor` reports environment health;
`fbd reset` rebuilds the hub. A startup version gate protects the data-reading
path from bd schema drift.

This slice writes the CLI surface only. No TUI, no roster-editing subcommands
(Slice 7), no detail/search (Slices 10–11).

## CLI structure (clap derive)

```
fbd                      no subcommand: print a short note (TUI arrives in Slice 9) + exit 0
fbd snapshot [--json]    ensure_hub → refresh → fetch → print merged ready list
fbd reset                delete the hub dir (rebuilt on next snapshot/launch)
fbd doctor               print bd version + gate status, config path, hub path, roster health
fbd --help / --version   clap-provided
```

**Bare `fbd` decision (documented):** the master plan reserves bare `fbd` for
launching the TUI (Slice 9). Until then, `command` is an `Option<Command>`; when
`None`, `main` prints a one-line orientation note pointing at `fbd snapshot` /
`fbd doctor` and noting the interactive UI lands in a later slice, then exits 0.
This keeps `fbd --help` working (clap) and avoids a confusing error for a bare
invocation, while leaving the bare-invocation slot free for Slice 9 to claim.

## Module layout

- **`src/cli.rs`** (new, `pub mod cli;` in `lib.rs`): the testable command
  runners and the version-gate predicate. Everything here takes `&impl BdClient`,
  `&Paths`, and explicit `&mut impl Write` sinks — no process spawning, no XDG
  reads, no real clock in a way tests can't control — so every runner is unit
  tested against `FakeBdClient` and driven by the gated integration test against
  `BdCli`.
- **`src/main.rs`** (rewrite the scaffold): thin clap entry. Parses args,
  resolves real `Paths`, loads the roster, constructs `BdCli`, dispatches to a
  `cli::run_*` function, and maps `Ok(())`→`ExitCode::SUCCESS`,
  `Err(_)`→print `error: <e>` to stderr + `ExitCode::FAILURE`.

### Runner signatures

```rust
pub fn run_snapshot(
    roster: &Config,
    bd: &impl BdClient,
    paths: &Paths,
    json: bool,
    out: &mut impl Write,   // the snapshot itself (rows or JSON) — kept pure
    err: &mut impl Write,   // warnings (per-repo errors, collisions, AlreadyRefreshing)
) -> Result<(), CliError>;

pub fn run_doctor(
    roster: &Config,
    bd: &impl BdClient,
    paths: &Paths,
    out: &mut impl Write,
) -> Result<(), CliError>;

pub fn run_reset(paths: &Paths, out: &mut impl Write) -> Result<(), CliError>;

pub fn version_gate(v: &BdVersion) -> Result<(), String>;   // Err = actionable message
pub fn format_row(row: &Row) -> String;                     // shared with Slice 9 view
```

**Two writers for `run_snapshot`:** `out` carries only the snapshot (human rows
or serialized JSON); `err` carries warnings. Separation is required so `--json`
output stays machine-parseable — a warning interleaved into stdout would corrupt
the JSON document. `main` wires `out=stdout`, `err=stderr`.

### `CliError`

```rust
#[derive(Debug, thiserror::Error)]
pub enum CliError {
    #[error("{0}")] VersionGate(String),   // actionable, printed verbatim
    #[error(transparent)] Hub(#[from] HubError),
    #[error(transparent)] Refresh(#[from] RefreshError),
    #[error(transparent)] Bd(#[from] BdError),
    #[error("writing output: {0}")] Io(#[from] std::io::Error),
}
```

`Ok(())`→exit 0, any `Err`→exit nonzero. This is the exit-code mapping the
orchestrator asks be tested at the runner level (assert `.is_err()` /`.is_ok()`),
never by spawning the process.

## Behavior details

### Version gate (`version_gate`)
Accept iff `schema_version == 1` **and** parsed `version >= 1.1.0`. Version parse:
split on `.`, take the first three components as `u64` (ignore any trailing
`-pre`/build suffix on the patch component), compare as a `(u64,u64,u64)` tuple;
an unparseable version fails the gate. On failure the message names both the
requirement (`bd >= 1.1.0, schema_version 1`) and what was found (`found bd
<version>, schema_version <n>`) plus `upgrade bd` guidance — the test asserts the
message content, not just that it errored.

### `run_snapshot` control flow
1. `version_gate(&bd.version()?)` → `Err(CliError::VersionGate(msg))` on failure
   (fatal, before any hub work).
2. `ensure_hub(bd, paths, roster)?` — `HubStatus.warnings` (missing roster paths)
   written to `err`; a `HubError` (e.g. init failure) is **fatal**.
3. `refresh::run(bd, roster, paths)`:
   - `Ok(outcome)` → write each `RepoError` and each `PrefixMap` collision to
     `err`; use `outcome.prefix_map` and `fetched_at = outcome.synced_at`.
   - `Err(RefreshError::AlreadyRefreshing)` → **degraded, not fatal**: write a
     warning to `err`, use an empty `PrefixMap::default()` (rows attribute to
     `UNKNOWN_REPO`) and `fetched_at = SystemTime::now()`, and still fetch+print
     whatever the hub already holds. This is the "surface without aborting when
     data exists" path.
   - any other `RefreshError` (`Sync`/`Lock`/`Io`) → **fatal**.
4. `snapshot::fetch(bd, &hub, &prefix_map, fetched_at)?` — a `BdError` is fatal.
5. Print to `out`: `--json` → `serde_json::to_writer_pretty(out, &snapshot)` +
   newline; otherwise one `format_row` line per row.

### `format_row`
`"[{repo_name}] P{priority} {id} {title}"` — e.g.
`[ra] P1 ra-2hc Ready task one`. Pure `Row → String`; Slice 9's view reuses it
for the ready list so headless and TUI output never drift.

### `run_doctor`
Deliberately **not** version-gated — doctor is the diagnostic you run *when* the
gate fails, so it must still run. Prints:
- `bd version: <version> (schema <n>)` and `gate: OK` / `gate: FAIL — <msg>`;
  if `bd.version()` itself errors (bd absent), print `bd version: ERROR <e>` and
  continue (exit still 0 — reporting the breakage is doctor's job).
- `config: <config_file>` and `hub: <hub_dir>` (+ `(initialized)` /
  `(not created yet)`).
- `roster (<n> repos):` then per entry `  <path>  <OK|MISSING>  [prefix: <p>]`
  — an existing repo shows `OK` and its metadata prefix (or `prefix: ?` when
  `.beads/metadata.json` is unreadable); a path absent on disk shows `MISSING`.
Reuses `refresh::read_prefix` (promoted to `pub`) for the prefix column.

### `run_reset`
Calls `hub::reset(paths)` and reports to `out`: `hub reset: removed <hub_dir>`
(or `hub reset: nothing to remove` when the dir was already absent — detect by
checking existence before reset). No bd, no gate.

## Ordered TDD test list

Unit (`src/cli.rs`, `#[cfg(test)]`, `FakeBdClient` + tempdir-seeded repos):

1. **`format_row_matches_spec`** — red: `format_row` undefined. green: returns
   `"[ra] P1 ra-2hc Ready task one"` for the constructed row.
2. **`version_gate_accepts_supported`** — red: `version_gate` undefined. green:
   `{1.1.0, schema 1}` and `{1.2.0, schema 1}` → `Ok`.
3. **`version_gate_rejects_old_version`** — green: `{1.0.0, schema 1}` → `Err`
   whose message contains `1.1.0` and `1.0.0`.
4. **`version_gate_rejects_wrong_schema`** — green: `{1.1.0, schema 2}` → `Err`
   whose message mentions `schema`.
5. **`snapshot_prints_rows`** — seed repo dir `ra` (metadata prefix `ra`) under a
   temp base; `FakeBdClient::with_ready([P1 ra-2hc "Ready task one", …])`. red:
   `run_snapshot` undefined. green: `out` contains `[ra] P1 ra-2hc Ready task
   one`; returns `Ok`.
6. **`snapshot_json_emits_snapshot`** — same setup, `json=true`: `out` parses as
   JSON with `rows[0].issue.id` and a `fetched_at` key; parse must succeed
   (proves no warning leaked into `out`).
7. **`snapshot_surfaces_per_repo_warnings_without_aborting`** — two seeded repos,
   one programmed `with_export_err`: `out` still prints the healthy rows, `err`
   contains a warning naming the failed repo, returns `Ok`.
8. **`snapshot_already_refreshing_degrades`** — pre-create hub dir and hold the
   `HubLock`; `run_snapshot` writes an "another fbd is refreshing" warning to
   `err`, still prints rows to `out` (attributed `unknown`), returns `Ok`.
9. **`snapshot_version_gate_failure_is_fatal`** — `with_version({1.0.0})`:
   returns `Err(CliError::VersionGate(_))`, `out` empty (exit-code mapping).
10. **`snapshot_sync_failure_is_fatal`** — `with_repo_sync_err`: returns `Err`
    (exit-code mapping).
11. **`doctor_lists_missing_repo_as_MISSING`** — roster of one seeded repo
    (prefix `ra`) + one nonexistent path; `out` contains bd version `1.1.0`, the
    config path, the hub path, `OK` + `prefix: ra` for the real repo, and
    `MISSING` for the absent one.
12. **`doctor_reports_gate_status`** — with a doctored `with_version({2, schema
    1})`… (accepted) vs `{1.0.0}` (FAIL): `out` shows `gate: OK` / `gate: FAIL`.
13. **`reset_removes_hub_and_reports`** — seed a hub dir under a temp base; after
    `run_reset` the hub dir is gone and `out` mentions the removed path.

Integration (gated, `tests/bd_integration.rs`, real `BdCli`):

14. **`snapshot_command_end_to_end`** — build two fixture repos (`ra`, `rb`) via
    the existing helper, `Paths::with_base(tmp)`, roster of both, call
    `run_snapshot(json=false)` with `BdCli::new()`. Assert `out` contains an
    `[ra] …` line and an `[rb] …` line and the title `Ready task one`, proving
    the full ensure→refresh→fetch→print path attributes both repos. Skips
    cleanly when bd is absent.

## Manual smoke (recorded)

`cargo run -- doctor` on this machine:
```
bd version: 1.1.0 (schema 1)  gate: OK
config: /Users/brian/Library/Application Support/federated-beads/config.toml
hub: /Users/brian/Library/Application Support/federated-beads/hub (not created yet)
roster (0 repos):
```
Bare `fbd` prints the orientation note; `fbd snapshot` on the empty roster
builds the hub and prints nothing (no repos, no ready rows); `fbd reset` then
reports `removed <hub>` and a second `fbd reset` reports `nothing to remove`
(idempotent). Machine left clean (hub reset away).

## Edge cases handled

- `--json` output never interleaved with warnings (separate `err` sink).
- `AlreadyRefreshing` degrades to a stale, all-`unknown` snapshot instead of
  failing — matches the "browsable stale view" design premise.
- Doctor bypasses the gate and tolerates bd being absent, so it stays useful in
  exactly the broken environments it exists to diagnose.
- `reset` on an absent hub is a no-op with a clear message (delegates to
  `hub::reset`'s existing idempotence).
- Non-UTF-8 / spaced repo paths: runners pass `&Path` straight through; display
  uses `Path::display()`.

## Out of scope (later slices)

- The TUI and bare-`fbd` launch (Slice 9).
- `fbd repos add/remove/list/discover` roster editing (Slice 7).
- Detail pane / search / copy-context (Slices 10–12).
- Parallel exports, background refresh (v2).

## Verification

```
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
cargo test --test bd_integration
cargo run -- doctor        # manual smoke
```

## Autoreview outcomes (codex gpt-5.6-sol, branch vs main)

Ran to convergence over several rounds. Accepted and fixed:

- **Per-command roster load.** Unconditional load broke `fbd reset` and stopped
  doctor from diagnosing a bad config. `main` now loads the roster only for
  `snapshot` (fatal on error); `reset` needs none; `doctor` loads it itself and
  reports parse errors inline.
- **Fail-closed version gate.** `parse_version` now requires all three numeric
  `major.minor.patch` components, so `1.1`/`2` no longer slip past.
- **Terminal-control injection (bug class swept everywhere untrusted text hits
  the terminal).** `format_row`, doctor's roster output, and the snapshot warning
  loops all sanitize bd/config-derived fields (control chars → U+FFFD); JSON is
  untouched (serde escapes). Prevents forged rows / ANSI/OSC (incl. OSC 52
  clipboard) sequences from an issue title in a repo you don't control.
- **Fail-loud config load.** `load_roster` uses `symlink_metadata`, not
  `Path::exists`, so an unreadable or dangling config surfaces as an error
  instead of a silent empty roster.
- **Accurate reset report.** `run_reset` uses `symlink_metadata` too, so a
  removed dangling hub symlink reports "removed", not "nothing to remove".

Consciously rejected (filed as follow-up `federated-beads-dxh.16`):

- **Widening the advisory lock over `ensure_hub` reconciliation and `reset`.**
  The `<hub>/.fbd.lock` lock is deliberately scoped to `repo sync` (Slices 3–4,
  master plan) on a disposable, derived hub. Covering ensure/reset requires
  refactoring `refresh::run`'s lock API to accept an externally-held lock (a
  single process re-acquiring `HubLock` would self-deadlock via `flock`), a
  cross-cutting change spanning merged slices and beyond Slice 6's scope. Left
  as a designed follow-up rather than a reflexive patch.

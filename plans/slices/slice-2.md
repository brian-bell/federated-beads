# Slice 2 — `BdClient` trait + `BdCli` subprocess client + `FakeBdClient`

Bead: `federated-beads-dxh.3` (child of epic `federated-beads-dxh`).
Master plan: `plans/fbd-v1-implementation-plan.md` (Slice 2 + global sections).
Depends on: Slice 1 (`federated-beads-dxh.2`, merged — `bd::types` domain types +
recorded fixtures).

## Goal

fbd can drive a **real** `bd` binary against a **real** repo through a small,
mockable seam. Introduce the `BdClient` trait (the whole surface fbd will ever
call), a `BdCli` subprocess implementation that spawns `bd`, checks exit status,
parses stdout JSON, and wraps failures in a structured `BdError`, and a
`FakeBdClient` with programmable per-call responses that later slices' unit tests
drive without touching a real `bd`.

**This slice writes only the bd client seam.** No hub, refresh, snapshot,
prefix-map, CLI subcommands, or TUI — those are Slices 3–12.

## Scope (in)

- `src/bd/mod.rs` — `BdClient` trait + `BdError`; re-export `BdCli`, `FakeBdClient`.
- `src/bd/cli.rs` — `BdCli`: real `std::process::Command` impl.
  - Test-visible argv builders (one per call) so `builds_correct_argv` can assert
    the exact command vector without spawning anything.
  - A generic `run_json<T: DeserializeOwned>` helper: spawn, check
    `status.success()`, parse stdout, capture + truncate stderr on failure.
- `src/bd/fake.rs` — `FakeBdClient`: programmable responses/errors per method,
  usable from other modules' unit tests (NOT behind `cfg(test)`).
- `tests/bd_integration.rs` — add gated `version_and_ready_roundtrip`.
- `tests/helpers/` (shared module, e.g. `tests/helpers/mod.rs`) — fixture-repo
  builder: `bd init --prefix ra`, create 3 issues, one `blocks` link, export.

## Scope (out)

- No hub reconciliation, refresh pipeline, prefix map, snapshot read model.
- No clap subcommands, no TUI, no `config.rs` changes.
- `repo_list` returns raw JSON value / typed shape only as far as needed to
  compile the trait; its *consumption* (roster reconciliation) is Slice 3. We do
  NOT commit to the `repo list --json` shape here beyond a tolerant return type.
- No async; `BdCli` is blocking (matches the single-worker threading model).

## The `BdClient` trait surface

```rust
pub trait BdClient {
    fn version(&self) -> Result<BdVersion, BdError>;
    fn init(&self, dir: &Path, prefix: &str) -> Result<(), BdError>;
    fn repo_add(&self, hub: &Path, repo_path: &Path) -> Result<(), BdError>;
    fn repo_list(&self, hub: &Path) -> Result<serde_json::Value, BdError>;
    fn export(&self, repo: &Path) -> Result<(), BdError>;
    fn repo_sync(&self, hub: &Path) -> Result<(), BdError>;
    fn ready(&self, hub: &Path) -> Result<Vec<Issue>, BdError>;
    fn show(&self, hub: &Path, id: &str) -> Result<IssueDetail, BdError>;
    fn search(&self, hub: &Path, query: &str) -> Result<Vec<Issue>, BdError>;
}
```

Notes:
- `show` collapses bd's array-of-one via `IssueDetail::into_single` (Slice 1),
  mapping a shape mismatch into `BdError { kind: Shape, .. }`.
- `init`/`repo_add`/`export`/`repo_sync` produce non-JSON status text on stdout;
  they only need exit-status success, so they use a `run_ok` path, not
  `run_json`.

## Argv contract (asserted by `builds_correct_argv`)

`bd` is the program; args below. `export` output path is `repo/.beads/issues.jsonl`.

| Call | argv (after program `bd`) |
|---|---|
| `version` | `["version", "--json"]` |
| `init(dir, "ra")` | `["init", "--prefix", "ra"]` run with cwd = `dir` (bd rejects `-C` for init: it pre-checks for an existing project) |
| `repo_add(hub, path)` | `["-C", hub, "repo", "add", path]` |
| `repo_list(hub)` | `["-C", hub, "repo", "list", "--json"]` |
| `export(repo)` | `["-C", repo, "export", "-o", ".beads/issues.jsonl"]` (relative to `-C`; avoids path doubling for relative `repo`) |
| `repo_sync(hub)` | `["-C", hub, "repo", "sync"]` |
| `ready(hub)` | `["-C", hub, "ready", "--limit", "0", "--json"]` (bd caps `ready` at 100 by default; 0 = unlimited) |
| `show(hub, id)` | `["-C", hub, "show", id, "--json"]` |
| `search(hub, q)` | `["-C", hub, "search", "--query=<q>", "--limit", "0", "--json"]` (`--query=` keeps flag-like `q` literal; bd caps `search` at 50 by default) |

Design: each builder is a free/assoc fn `argv_version()`, `argv_ready(dir)`, …
returning `Vec<String>` (paths rendered with `Path::display`/`to_string_lossy`).
`BdCli`'s real methods call the same builder, so the test asserts exactly what
runs. Builders are `pub(crate)` (or `pub` + `#[doc(hidden)]`) so the in-crate
`cli::tests` module reaches them.

## `BdError`

```rust
#[derive(Debug, Error)]
pub struct BdError {
    pub command: String,   // e.g. "bd -C <hub> ready --json"
    pub stderr: String,    // captured, truncated to N chars for display
    pub kind: BdErrorKind,
}
pub enum BdErrorKind { Spawn, NonZeroExit { code: Option<i32> }, Parse, Shape }
```

- `Spawn`: `Command::output()` itself failed (bd not on PATH, etc.).
- `NonZeroExit`: ran but `!status.success()`.
- `Parse`: stdout was not valid JSON for `T`.
- `Shape`: JSON parsed but violated an invariant (e.g. show ≠ 1 element).
- `Display` shows `command` + truncated `stderr` + kind. stderr truncated to a
  const (e.g. 2000 chars) with an `…(truncated)` marker.

## FakeBdClient exposure decision

`FakeBdClient` must be callable from **other modules'** unit tests (hub, refresh,
snapshot in later slices), which run under `cargo test` but compile those modules
as non-test library code that *takes a `&impl BdClient`*. The test itself is
`cfg(test)`, but the fake type must be reachable from integration-style unit
tests in sibling modules.

**Chosen approach (simplest idiomatic):** put `FakeBdClient` in `src/bd/fake.rs`
as ordinary `pub` library code, `#[doc(hidden)]`, module doc-commented as
"test double, not part of the supported API." Rationale over alternatives:
- A `cfg(test)` gate would make it invisible to a sibling module's `#[cfg(test)]`
  tests only within the *same* crate compilation — actually it *is* visible
  intra-crate, but `cfg(test)` code cannot be used by `tests/` integration
  binaries. Keeping it plain `pub` + `#[doc(hidden)]` keeps one code path usable
  from both in-crate unit tests and, if ever needed, `tests/`. No extra feature
  flag to maintain.
- A `fake` cargo feature adds config surface for zero benefit at this stage.

Note the decision inline in the module doc so a reviewer sees the intent.

## TDD test list (red → green)

Order: unit argv tests first (pure, fast), then FakeBdClient behavior, then the
gated integration roundtrip.

1. **`cli::tests::builds_correct_argv`** (unit, in `src/bd/cli.rs`)
   - *Red*: argv builder fns don't exist → compile error.
   - *Green*: implement `argv_*` builders returning the vectors in the table
     above; assert each. Uses a fixed dir like `/tmp/hub`, `/tmp/ra`.
   - Edge: `export` path assembled as `repo.join(".beads/issues.jsonl")`; assert
     the last two args are `-o` and that joined path string.

2. **`cli::tests::bderror_display_truncates_stderr`** (unit)
   - *Red*: `BdError`/truncation not present.
   - *Green*: construct a `BdError` with 5000-char stderr; assert `Display`
     output contains the command, is bounded in length, and ends with the
     truncation marker.

3. **`fake::tests::returns_programmed_ready`** (unit, in `src/bd/fake.rs`)
   - *Red*: `FakeBdClient` doesn't exist.
   - *Green*: `FakeBdClient::new().with_ready(vec![issue])`; calling `.ready(dir)`
     returns those issues; a second call still returns them (or a queue —
     choose: **stored response reused**, simplest; document).

4. **`fake::tests::returns_programmed_error`** (unit)
   - *Green*: `.with_ready_err(BdError{..})` (or a generic per-method error slot)
     → `.ready(dir)` returns `Err`. Confirms later slices can drive the
     per-repo-error paths.

5. **`fake::tests::records_calls`** (unit)
   - *Green*: fake records invocations (method + args) so refresh/hub tests can
     assert "export called for repo A, then repo B, then sync once." Expose
     `fake.calls()` → `Vec<Call>` (an enum of recorded calls). This is what
     Slice 3/4 red tests need (`adds_missing_repos_only`,
     `exports_all_then_syncs_once`).

6. **`bd_integration::version_and_ready_roundtrip`** (gated integration)
   - *Red*: `BdCli` doesn't exist.
   - *Green*: helper builds a fixture repo (`bd init --prefix ra`; 3 issues; one
     `blocks` link so 1 issue is blocked; `bd export`). Because a single repo with
     no hub still answers `ready` directly (bd reads its own `.beads`), call
     `BdCli::ready(&repo)` and assert **2 of 3** issues returned, blocked one
     excluded. Also assert `BdCli::version()` parses and `schema_version == 1`.
   - Skip: helper first runs `bd version --json`; if it fails to spawn/succeed,
     `eprintln!("SKIP: bd not installed")` and return early.

## Fixture-repo helper (`tests/helpers/mod.rs`)

```
fn bd_available() -> bool                     // moved/shared from bd_integration
fn build_ready_fixture_repo(dir: &Path) -> ()  // init ra, 3 issues, 1 blocks, export
```

- Create three issues with `bd -C <repo> create "<title>" -p <n> [--json]`,
  capturing ids (parse `.id` or `.[0].id` from `--json`).
- Link: `bd -C <repo> link <blocked> <blocker> --type blocks` (blocked depends on
  blocker), per master plan Observations §1.
- Export: `bd -C <repo> export -o <repo>/.beads/issues.jsonl`.
- Uses `std::process::Command` directly (the helper is about *arranging* real bd
  state, independent of `BdCli`, so a `BdCli` bug can't mask a helper bug).
- `bd_integration.rs` keeps its existing `bd_probe_skips_cleanly_when_absent` and
  gains the new roundtrip; both go through the shared helper.

## Refactor step

- Collapse `version/ready/show/search` onto one generic
  `run_json<T: DeserializeOwned>(&self, dir: Option<&Path>, args: &[&str]) -> Result<T, BdError>`.
- Collapse `init/repo_add/export/repo_sync` onto `run_ok(dir, args)`.
- stderr capture + truncation centralized in the error-construction path.
- Keep argv builders as the single source of truth shared by real methods + tests.

## Edge cases

- bd absent → `version()` returns `BdError{kind: Spawn}`; integration test skips.
- Non-zero exit with stderr → `NonZeroExit` carrying truncated stderr.
- Valid exit but malformed JSON → `Parse`.
- `show` returning 0 or ≥2 elements → `Shape` (via `into_single`).
- Paths with spaces/unicode → argv carries them as single args (no shell), so
  `Command` handles quoting; argv test can include a spaced path to lock this in.

## Definition of done

- `cargo fmt --check` clean.
- `cargo clippy --all-targets -- -D warnings` clean.
- `cargo test` green (unit + fake tests; no bd needed).
- `cargo test --test bd_integration` green with bd present (roundtrip asserts
  2-of-3); prints `SKIP` and stays green with bd absent.
- `FakeBdClient` reachable as plain `pub` (doc-hidden) library code — documented.

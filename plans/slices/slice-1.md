# Slice 1 — Domain types: parse recorded bd `--json` fixtures

Bead: `federated-beads-dxh.2` (child of epic `federated-beads-dxh`).
Master plan: `plans/fbd-v1-implementation-plan.md` (Slice 1 + global sections).
Depends on: Slice 0 (`federated-beads-dxh.1`, merged — `config` module + harness).

## Goal

fbd can deserialize every `bd --json` payload it will ever read, using serde
types that tolerate both **omitted optional keys** (bd drops empty fields) and
**unknown future keys** (forward-compat with newer bd). Prove it against
fixtures recorded verbatim from real bd 1.1.0.

**This slice writes only domain types + their parsing tests.** No BdClient
trait, no subprocess spawning, no hub/refresh/snapshot/TUI — those are Slices
2–12.

## Scope (in)

- **Recorded fixtures** in `tests/fixtures/`, captured once from real bd 1.1.0
  (build `8e4e59d39`, `schema_version == 1`) via a throwaway probe repo:
  - `ready.json` — hub `bd ready --json` (JSON array of issue summaries).
  - `show.json` — hub `bd show <blocked-id> --json` (JSON **array of one**,
    with `description` and a `dependencies` array).
  - `search.json` — `bd search <term> --json` (JSON array of summaries).
  - `version.json` — `bd version --json` (single object).
- `src/bd/mod.rs` — new `bd` module (declared in `src/lib.rs`), re-exporting the
  types. No trait/client yet (Slice 2).
- `src/bd/types.rs` — serde types: `Issue`, `IssueDetail`, `Dependency`,
  `BdVersion`, plus the `into_single()` helper for the show-array shape.
- Parsing unit tests co-located in `src/bd/types.rs` (`#[cfg(test)] mod tests`).

## Scope (out)

- No `BdClient` trait, `BdCli` subprocess impl, or `FakeBdClient` (Slice 2).
- No hub, refresh, snapshot, prefix-map, CLI subcommands, or TUI.
- No `source_repo` field — bd does not expose it via `--json` (Observations §6);
  repo attribution is derived from ID prefixes in Slice 4, not here.
- No changes to `config.rs` or `tests/bd_integration.rs`.
- Fixtures are read by unit tests via `include_str!` / file read; we do NOT add a
  gated integration test in this slice (schema-drift tripwire lands Slice 2+).

## Fixture-recording procedure (run once, commit output verbatim)

Recorded with a throwaway probe repo in a temp dir, then cleaned up. bd 1.1.0
must be on PATH (`bd version --json` → `"version": "1.1.0"`, `schema_version: 1`).

```zsh
set -e
PROBE=$(mktemp -d)
FIX=/Users/brian/dev/federated-beads/tests/fixtures
mkdir -p "$FIX"

# --- source repo (prefix ra): 3 issues, one description, one blocks link ---
mkdir -p "$PROBE/ra"
bd -C "$PROBE/ra" init --prefix ra
# create returns the new id; capture to wire the blocks link
B1=$(bd -C "$PROBE/ra" create "Ready task one" -p 1 -d "First ready task with a description" --json | jq -r '.id // .[0].id')
B2=$(bd -C "$PROBE/ra" create "Blocker task" -p 0 --json | jq -r '.id // .[0].id')
B3=$(bd -C "$PROBE/ra" create "Blocked task" -p 2 -d "This one is blocked by the blocker" --json | jq -r '.id // .[0].id')
# B3 depends on (is blocked by) B2:  link <from> <to> --type blocks  => from depends on to
bd -C "$PROBE/ra" link "$B3" "$B2" --type blocks

# --- hub: add source repo, export, sync ---
mkdir -p "$PROBE/hub"
bd -C "$PROBE/hub" init --prefix hub
bd -C "$PROBE/hub" repo add "$PROBE/ra"
bd -C "$PROBE/ra"  export -o "$PROBE/ra/.beads/issues.jsonl"
bd -C "$PROBE/hub" repo sync

# --- capture fixtures verbatim ---
bd -C "$PROBE/hub" ready  --json          > "$FIX/ready.json"
bd -C "$PROBE/hub" show   "$B3" --json     > "$FIX/show.json"   # array-of-one, has dependencies
bd -C "$PROBE/hub" search "task" --json    > "$FIX/search.json"
bd version --json                           > "$FIX/version.json"

rm -rf "$PROBE"
```

Notes / fallbacks discovered at record time (adjust before committing):
- If `bd create --json` shape differs, capture ids from plain-text output
  (`create` prints the id) rather than `jq`. The exact id-capture mechanism is
  not load-bearing for the fixtures — only the four final JSON files are.
- The blocked id (`$B3`) must be the one passed to `show` so `show.json` carries
  a `dependencies` entry with `dependency_type == "blocks"`.
- `ready.json` should exclude the blocked issue (bd hides issues with open
  blockers) — expect the ready array to contain the un-blocked issues only.
- Commit all four files exactly as bd emitted them (no reformatting), so the
  parsing tests exercise real whitespace/key-ordering.

## Domain types (green target)

In `src/bd/types.rs`. Every field bd may omit is `Option<T>` or
`#[serde(default)]`. **No `#[serde(deny_unknown_fields)]` anywhere** — newer bd
may add keys and must still parse.

```rust
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
pub struct Issue {
    pub id: String,
    pub title: String,
    pub status: String,
    pub priority: i64,            // int; 0 = highest. i64 tolerates any int bd emits.
    #[serde(default)] pub issue_type: Option<String>,
    #[serde(default)] pub owner: Option<String>,
    #[serde(default)] pub created_at: Option<String>,
    #[serde(default)] pub updated_at: Option<String>,
    #[serde(default)] pub created_by: Option<String>,
    #[serde(default)] pub dependency_count: Option<i64>,
    #[serde(default)] pub dependent_count: Option<i64>,
    #[serde(default)] pub comment_count: Option<i64>,
    #[serde(default)] pub description: Option<String>,  // omitted when empty
}

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
pub struct Dependency {
    pub id: String,
    #[serde(default)] pub title: Option<String>,
    #[serde(default)] pub status: Option<String>,
    #[serde(default)] pub dependency_type: Option<String>,  // e.g. "blocks"
}

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
pub struct IssueDetail {
    #[serde(flatten)] pub issue: Issue,
    #[serde(default)] pub dependencies: Vec<Dependency>,
}

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
pub struct BdVersion {
    pub version: String,
    pub schema_version: i64,
    #[serde(default)] pub build: Option<String>,
    #[serde(default)] pub commit: Option<String>,
    #[serde(default)] pub branch: Option<String>,
}
```

Design notes (finalize from the real fixtures, not this sketch):
- The exact key set on `Issue`/`Dependency` is set from `ready.json`/`show.json`.
  Any key present in the fixture gets a field (Option/default if it can be
  absent); we do **not** need a field for every key, since no `deny_unknown_fields`
  means extras are ignored — but we add the ones the app will use.
- `priority` is a plain int in bd output → `i64` (not `u8`; avoids a parse panic
  if bd ever emits a negative or large value; the plan's "u8-ish" is satisfied by
  asserting the value equals the expected small int).
- `IssueDetail` uses `#[serde(flatten)]` over `Issue` so one `Issue` shape is the
  single source of truth (the master plan's refactor: "single `Issue` struct").
  If `flatten` fights `into_single`/priority typing in practice, fall back to a
  standalone `IssueDetail` with duplicated fields — decide from compiler output.

## `into_single()` refactor

`bd show --json` returns an array-of-one. Provide a helper turning
`Vec<IssueDetail>` into a single `IssueDetail` with a clear error for 0 or N≠1:

```rust
pub fn into_single(mut v: Vec<IssueDetail>) -> Result<IssueDetail, BdShapeError> {
    match v.len() {
        1 => Ok(v.pop().unwrap()),
        n => Err(BdShapeError::ExpectedOne { got: n }),
    }
}
```

`BdShapeError` is a small `thiserror` enum in `bd/mod.rs` (or `types.rs`) with an
`ExpectedOne { got: usize }` variant whose `Display` reads e.g.
`expected exactly one issue from 'bd show', got 0`. This is the seed of the
Slice 2 `BdError`; keep it minimal and self-contained here.

## TDD test list (strict red → green → refactor)

Record fixtures FIRST (they are test inputs, not code). Then, for each test:
write it, run to observe the exact red, implement minimally to green.

### 1. `bd::types::tests::parses_version`
- **Test**: deserialize `version.json` into `BdVersion`; assert
  `version == "1.1.0"`, `schema_version == 1`.
- **Red**: compile error — `bd` module / `BdVersion` do not exist.
- **Green**: define `BdVersion`; wire `pub mod bd;` in `lib.rs` and
  `pub mod types;` + re-exports in `bd/mod.rs`.

### 2. `bd::types::tests::parses_ready_fixture`
- **Test**: deserialize `ready.json` into `Vec<Issue>`; assert the count matches
  the fixture (un-blocked issues only), the first row's `id` is non-empty and
  starts with `"ra-"`, `priority` equals the expected small int, and that at
  least one row exercises `description: None` (omitted-key tolerance) — pick the
  row that had no `-d` at record time and assert its `description.is_none()`.
- **Red**: `Issue` does not exist (compile error).
- **Green**: define `Issue` with Option/default on every omittable field.

### 3. `bd::types::tests::parses_show_fixture_with_dependencies`
- **Test**: deserialize `show.json` into `Vec<IssueDetail>`; assert `len() == 1`;
  the single element has `description.is_some()`, `dependencies.len() >= 1`, and
  `dependencies[0].dependency_type.as_deref() == Some("blocks")`.
- **Red**: `IssueDetail` / `Dependency` do not exist.
- **Green**: define both; `IssueDetail` flattens `Issue` and adds
  `dependencies: Vec<Dependency>` with `#[serde(default)]`.

### 4. `bd::types::tests::parses_search_fixture`
- **Test**: deserialize `search.json` into `Vec<Issue>`; assert non-empty and
  every row has a non-empty `id`. (Same shape family as ready.)
- **Red**: covered once `Issue` exists; this pins that the search shape parses
  with the same type (characterization — still write it).
- **Green**: no new code beyond `Issue`.

### 5. `bd::types::tests::tolerates_unknown_future_keys`
- **Test**: hand-write a small JSON string for an `Issue` (and a `BdVersion`)
  that includes an unknown key (e.g. `"future_field": 42`); assert it
  deserializes Ok. This is the explicit forward-compat guard proving no
  `deny_unknown_fields` crept in.
- **Red**: passes only if types omit `deny_unknown_fields`; written to lock that
  contract permanently.
- **Green**: no new code (absence of `deny_unknown_fields`).

### 6. `bd::types::tests::into_single_ok` and `::into_single_rejects_zero_and_many`
- **Test (ok)**: `into_single(vec![one_detail])` returns `Ok` with that detail.
- **Test (err)**: `into_single(vec![])` and `into_single(vec![a, b])` both return
  `Err`, and the error `Display` mentions the count (`got: 0` / `got: 2`).
- **Red**: `into_single` / `BdShapeError` do not exist.
- **Green**: implement the helper + error enum.

## Edge cases covered

- **Omitted optional key** (`description` absent) → `None`, not a parse error
  (test 2).
- **Present optional key** (`description` set on the blocked issue) → `Some`
  (test 3).
- **Unknown future key** → ignored, still parses (test 5) — no `deny_unknown_fields`.
- **Array-of-one show shape** collapsed safely; 0 and N≠1 rejected with a clear
  message (test 6).
- **Dependency embedded shape** with `dependency_type == "blocks"` (test 3).
- `schema_version` gate value is available for the Slice 6 startup gate (test 1).

## Verification (all clean at slice end)

```zsh
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test                       # unit + doctests, green without bd
cargo test --test bd_integration # unchanged placeholder, still green
```

Evidence of success: tests 1–6 go red for the stated reasons, then green; the
four fixture files are committed verbatim; fmt/clippy clean.

## Git plan

- Branch `slice-1-domain-types` off `main`.
- Logical commits:
  1. `fixtures: record real bd 1.1.0 --json outputs` (the four `tests/fixtures/*.json`).
  2. `bd types: Issue/IssueDetail/Dependency/BdVersion + parsing tests` (module +
     tests, red→green), including `into_single` + `BdShapeError`.
- `git checkout main && git merge --no-ff slice-1-domain-types -m "Merge slice 1: domain types + recorded fixtures"`.
- Verify fmt/clippy/test clean on main. NO push, NO remote (user authorized
  local-only).

## Autoreview

Run the autoreview skill on branch vs main. Fix actionable findings on-branch
(tests stay green). Record skipped findings + reasons as
`bd comment federated-beads-dxh.2 "..."`.

## Stop conditions (halt + consult)

- Recorded fixture shapes differ materially from Observations §2–5 (beyond added
  keys) — re-confirm before building types on a wrong shape.
- `bd ready --json` does NOT exclude the blocked issue, or `bd show --json` is
  not an array-of-one — both are load-bearing assumptions; stop and report.

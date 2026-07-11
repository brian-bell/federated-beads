# fbd v1 — Federated Beads TUI: TDD Implementation Plan

## Goal

Build `fbd`, a shippable Rust + ratatui **read-only** TUI that answers "what's ready to work on across all my beads repos?" It federates N beads repositories into a persistent hub database that `bd` itself maintains (multi-repo hydration), and presents a cross-repo ready-work list with a detail pane, cross-repo search, and a copy-context action.

## Non-goals (v1)

- **No writes** to any beads data from the TUI (no create/update/close/comment).
- No blocked-issues view (v2).
- No background daemon or file watcher; refresh is user-triggered (launch + keypress).
- No use of `bd --global` shared-server mode.
- No Windows support commitment (target macOS + Linux; nothing should gratuitously break Windows).

## Fixed design decisions (from design interview — do not re-litigate)

1. **Central DB**: a bd "hub" workspace using built-in multi-repo hydration (`bd repo add` + `bd repo sync`), not a custom aggregation store.
2. **Read path**: all queries go through the hub via `bd ... --json` subprocess calls. bd owns ready/blocked semantics; fbd never reimplements them.
3. **Write path**: none in v1. The copy-context action is the bridge to acting in a terminal.
4. **Refresh**: TUI-owned. On launch and on refresh key: `bd -C <repo> export -o <repo>/.beads/issues.jsonl` per repo, then `bd -C <hub> repo sync`. Async; status bar shows progress and last-refreshed age.
5. **Hub home**: `~/.local/share/federated-beads/hub/` (XDG data dir), auto-created on first run. Disposable derived data.
6. **Roster**: `~/.config/federated-beads/config.toml` is the source of truth; `fbd repos add/remove/list` edit it; `fbd repos discover <dir>` scans for `*/.beads`. Missing paths warn, never fail.
7. **Stack**: Rust + ratatui (crossterm backend), serde/serde_json, matching the user's session-tui experience.
8. **Testing**: `BdClient` trait; unit tests against recorded `--json` fixtures; integration suite that builds real fixture repos with `bd init` in tempdirs, auto-skipped when bd is absent.
9. **v1 features**: ready list (grouped/filterable by repo & priority) · detail pane · cross-repo search · copy-context key · refresh key + staleness indicator.

## Current system observations (verified against bd 1.1.0, build 8e4e59d39)

All of the following were verified by building a live probe (fixture repo + hub) with bd 1.1.0. Fixture repos live in a scratch dir; the same construction is reused by the integration suite.

1. **Pipeline works end-to-end**:
   ```
   bd init --prefix ra                                  # in fixture repo dir
   bd -C <repo> create "Title" -p 1 -d "desc"
   bd -C <repo> link <from-id> <to-id> --type blocks    # from depends on to
   bd init --prefix hub                                 # in hub dir
   bd -C <hub> repo add <repo-path>                     # writes hub .beads/config.yaml
   bd -C <repo> export -o <repo>/.beads/issues.jsonl    # "Exported 3 issues to ..."
   bd -C <hub> repo sync                                # "Multi-repo sync complete: imported 3 issue(s) from 1 repo(s)"
   bd -C <hub> ready --json                             # hydrated issues, blocked ones excluded
   ```
2. **`bd ready --json`** returns a JSON array of issue objects. Observed keys: `id`, `title`, `status`, `priority` (int, 0 = highest), `issue_type`, `owner`, `created_at`, `created_by`, `updated_at`, `dependency_count`, `dependent_count`, `comment_count`. **Optional keys are omitted when empty** (e.g. `description` absent if empty). Issues with open blockers are correctly excluded.
3. **`bd show <id> --json`** returns a **JSON array with one element** (not a bare object). Includes `description` (when set) and a `dependencies` array of embedded issue objects each carrying `dependency_type` (e.g. `"blocks"`).
4. **`bd search <term> --json`** returns a JSON array of issue summaries (same shape family as `ready`).
5. **`bd version --json`** returns `{"version": "1.1.0", "schema_version": 1, "build": ..., "commit": ...}` — usable as a startup gate.
6. **No `source_repo` via CLI**: hydration stores `source_repo` internally, but no `--json` output exposes it, and **`bd sql` errors with "not yet supported in embedded mode"**. Therefore **repo attribution must be derived from the issue-ID prefix**: each source repo's `.beads/metadata.json` has `"dolt_database": "<prefix>"` (e.g. `"ra"`), and issue IDs are `<prefix>-<hash>` (e.g. `ra-2hc`, not sequential). fbd builds a prefix→repo map at refresh time and must **detect prefix collisions** across repos (warn, attribute to "ambiguous").
   - Prefix matching rule: IDs are matched by **longest configured prefix** followed by `-`. This avoids misattribution when one prefix is a prefix of another (e.g. `app` and `app2` — an ID `app2-xyz` must match `app2`, never `app`).
7. **Latency**: a single `bd ready --json` call ≈ 0.19 s wall on an M-series Mac (embedded Dolt startup included). A 5-repo refresh (5 exports + 1 sync) ≈ 1–2 s → async refresh with the stale view still browsable is required, matching decision #4.
8. **`bd repo sync` uses mtime caching** ("skips repos whose JSONL hasn't changed") — safe for us because we always re-export before syncing; re-exports touch mtime.
9. **User's 5 real repos** (`agent-skills`, `approach`, `megaclock`, `reading-lite`, `session-tui` under `~/dev`) currently have **no `issues.jsonl`** — fbd's export step is what creates/refreshes them.
10. This directory (`~/dev/federated-beads`) is empty and not yet a git repository.

## Architecture

```
src/
  main.rs           CLI entry: clap subcommands (tui default, repos, snapshot, reset, doctor)
  config.rs         Roster config: load/save config.toml, XDG path resolution (overridable for tests)
  bd/
    mod.rs          BdClient trait + BdError
    cli.rs          BdCli: real subprocess impl (spawns `bd`, parses JSON, captures stderr)
    types.rs        serde types: Issue, IssueDetail, Dependency, BdVersion
    fake.rs         FakeBdClient for unit tests (cfg(test) + used by TUI state tests)
  hub.rs            Hub lifecycle: ensure_hub (init + repo add reconciliation), reset
  refresh.rs        Refresh pipeline: exports + sync + prefix map + per-repo error collection
  snapshot.rs       Read model: fetch ready list, attribute repos, produce Snapshot for UI
  app/
    mod.rs          App state machine (pure: state + Msg -> state), no I/O
    view.rs         ratatui rendering (pure: &App -> Frame), tested with TestBackend
    keys.rs         Key -> Msg mapping
    context.rs      Copy-context string builder + clipboard adapter (OSC52 primary)
  runtime.rs        Event loop: terminal events + async refresh task channel
tests/
  fixtures/         Recorded real bd --json outputs (ready.json, show.json, search.json, version.json)
  bd_integration.rs Gated end-to-end tests against real bd in tempdirs
  helpers/          Fixture-repo builder (bd init/create/link/export in tempdir)
```

**Threading model**: single UI thread + one background refresh task (std::thread + mpsc channel; no tokio — bd calls are blocking subprocesses and there is exactly one concurrent job). The app state machine consumes `Msg::RefreshProgress/RefreshDone/RefreshFailed` messages, so all concurrency is testable synchronously by feeding messages.

**Process-level locking**: refresh takes an advisory file lock on `<hub>/.fbd.lock` (via `fs2` or `std` on unix) so two fbd instances can't run `repo sync` concurrently against the same embedded-Dolt hub. Lock-held ⇒ second instance's refresh reports "another fbd is refreshing" in the status bar and retries on next manual refresh.

## Test strategy

- **Unit tests (default `cargo test`)**: everything except `bd/cli.rs` internals runs against `FakeBdClient` and recorded fixtures. App state machine and views are pure functions — test exhaustively. No network, no bd, no XDG writes (config paths injected).
- **Recorded fixtures**: captured once from real bd 1.1.0 (shapes in "Observations"), checked into `tests/fixtures/`. Parsing tests use them verbatim; **serde structs must use `#[serde(default)]`/`Option` for every field observed to be omittable and must NOT use `deny_unknown_fields`** (forward compatibility with newer bd).
- **Gated integration tests** (`tests/bd_integration.rs`): first line of each test calls a helper that runs `bd version --json`; if bd is missing, the test returns early with an explicit `eprintln!("SKIP: bd not installed")`. When present: build fixture repos in `tempfile::tempdir()` exactly as in Observations §1, then drive the real `BdCli` + `hub.rs` + `refresh.rs` pipeline and assert on results. These tests are the schema-drift tripwire.
- **TUI render tests**: `ratatui::backend::TestBackend` snapshot-ish assertions (assert on buffer contents for key cells/rows, not whole-screen golden files, to keep tests non-brittle).
- **Isolation**: config/data dirs are constructor parameters everywhere; only `main.rs` resolves real XDG paths. Tests never touch `~/.config` or `~/.local/share`.

## Verification commands

Discovered/none exist yet (greenfield); the plan establishes them in Slice 0 and they stay constant:

```
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test                    # unit + render tests, always green without bd
cargo test --test bd_integration   # gated e2e (skips cleanly without bd)
cargo run -- snapshot         # manual smoke: prints merged ready list as text/JSON
```

Evidence of success per slice = the named failing test goes red for the stated reason, then green, with `fmt`/`clippy` clean. Final acceptance = Slice 12's e2e checklist below.

## Git workflow

`git init` + initial empty scaffold commit, then **every slice is a feature branch + PR** (per user's global rule: never commit directly to main). Create the GitHub repo (`gh repo create`) in Slice 0. One slice = one PR = one reviewable red→green→refactor unit.

---

## Implementation slices (TDD tracer bullets, dependency order)

Each slice: **(a)** write the named failing test(s) first, **(b)** confirm the exact red state, **(c)** implement minimally to green, **(d)** refactor with tests green, **(e)** fmt/clippy, PR.

### Slice 0 — Harness: project scaffold + the first red/green

**Behavior**: `cargo test` runs; a trivial config round-trip proves the harness.

- `cargo init --name fbd`; add deps: `ratatui`, `crossterm`, `serde`, `serde_json`, `toml`, `clap` (derive), `anyhow`, `thiserror`, `tempfile` (dev), `dirs`.
- **Red**: `config::tests::roundtrip_roster` — build `Config { repos: vec![...] }`, save to a tempdir path, load, assert equality. Fails: module doesn't exist (compile error is the red state).
- **Green**: `config.rs` with `Config { repos: Vec<RepoEntry> }`, `RepoEntry { path: PathBuf }`, `load(path)`, `save(path)` using `toml`.
- **Refactor**: extract `Paths { config_file, data_dir }` resolver with an injectable base (env-independent tests); real XDG resolution behind it (`dirs::config_dir()`/`data_local_dir()` + `federated-beads/`).
- Also in this slice: `git init`, `.gitignore` (`/target`), CI-free but document the four verification commands in `README.md`. Initial commit; `gh repo create`.

### Slice 1 — Domain types: parse recorded bd JSON

**Behavior**: fbd can deserialize every bd payload it will ever read.

- Record fixtures from the live probe into `tests/fixtures/`: `ready.json`, `show.json` (array-of-one, with `dependencies`), `search.json`, `version.json`.
- **Red**: `bd::types::tests::{parses_ready_fixture, parses_show_fixture_with_dependencies, parses_search_fixture, parses_version}` — assert counts, ids, `priority` as `u8`-ish int, `description: Option<String>` absent-tolerant, `dependencies[0].dependency_type == "blocks"`, `schema_version == 1`. Fails: types don't exist.
- **Green**: `Issue`, `IssueDetail` (Issue + `description` + `dependencies: Vec<Dependency>` + defaults), `BdVersion`. All optional-tolerant, no `deny_unknown_fields`.
- **Refactor**: single `Issue` struct with optional detail fields if duplication emerges; keep `show` returning `Vec<IssueDetail>` and expose `.into_single()` with a clear error for 0/N≠1.

### Slice 2 — BdClient trait + real subprocess client

**Behavior**: fbd can drive a real bd against a real repo.

- **Red (unit)**: `bd::cli::tests::builds_correct_argv` — `BdCli` exposes (for test) the argv it would run for each call: `version` → `["bd","version","--json"]`, `ready(hub)` → `["bd","-C",hub,"ready","--json"]`, `export(repo)` → `["bd","-C",repo,"export","-o",repo/".beads/issues.jsonl"]`, `repo_sync(hub)`, `repo_add(hub,path)`, `init(hub,prefix)`, `show(hub,id)`, `search(hub,query)`. Fails: trait/struct don't exist.
- **Red (integration, gated)**: `bd_integration::version_and_ready_roundtrip` — helper builds a fixture repo (init/create×3/link/export), asserts `BdCli::ready(&repo)` returns 2 issues excluding the blocked one. Skips without bd.
- **Green**: `BdClient` trait (`version, init, repo_add, repo_list, export, repo_sync, ready, show, search`); `BdCli` spawns `bd` via `std::process::Command`, checks exit status, parses stdout JSON, wraps failures in `BdError { command, stderr, kind }`. `FakeBdClient` in `bd/fake.rs` with programmable per-call responses/errors.
- **Refactor**: centralize spawn/parse in one generic `run_json<T>` helper; ensure stderr is captured and truncated for display.

### Slice 3 — Hub lifecycle: ensure_hub

**Behavior**: first run creates a working hub; subsequent runs reconcile the roster.

- **Red (unit)**: `hub::tests::{creates_hub_when_missing, adds_missing_repos_only, tolerates_absent_repo_paths}` with `FakeBdClient` — asserts `init` called once with prefix `hub` when data dir empty; `repo_add` called only for roster entries not already in hub config; a roster path that doesn't exist on disk produces a `Warning` in the result, not an error.
- **Red (integration)**: `bd_integration::ensure_hub_end_to_end` — real hub in tempdir, two fixture repos, assert hub `.beads/config.yaml` lists both after ensure.
- **Green**: `hub::ensure_hub(bd, paths, roster) -> HubStatus { warnings }`. Read hub roster via `bd repo list --json` (verify shape in integration test; if `--json` unsupported for `repo list`, fall back to parsing `.beads/config.yaml` `repos.additional` with `serde_yaml` — decide in red phase from the real output, record whichever as fixture).
- **Green (also)**: `hub::reset(paths)` = delete hub dir (only ever under our data dir — assert path is inside data dir before removing) so `fbd reset` is trivial later.
- **Refactor**: idempotency — calling `ensure_hub` twice yields no duplicate `repo_add` calls.

### Slice 4 — Refresh pipeline + prefix map

**Behavior**: one refresh turns N source repos into a fresh hub + repo-attribution map, never failing wholesale on one bad repo.

- **Red (unit)**: `refresh::tests::{exports_all_then_syncs_once, collects_per_repo_errors, builds_prefix_map, flags_prefix_collisions, longest_prefix_wins}` with `FakeBdClient` — e.g. repo B's export errors ⇒ result contains `RepoError { repo: B, .. }` while sync still runs and A's data flows; two repos with prefix `ra` ⇒ `Collision` warning; prefixes `app`/`app2` attribute `app2-xyz` to `app2`.
- **Red (unit)**: `refresh::tests::reads_prefix_from_metadata` — prefix comes from parsing `<repo>/.beads/metadata.json` `dolt_database` (fixture file in tempdir).
- **Red (integration)**: `bd_integration::refresh_two_repos` — two fixture repos (prefixes `ra`, `rb`), full refresh, assert hub `ready` contains issues from both and the map attributes each id correctly.
- **Green**: `refresh::run(bd, roster, hub) -> RefreshOutcome { prefix_map, errors, synced_at }`. Prefix map type owns the longest-prefix-match lookup: `PrefixMap::repo_for(id) -> Option<&RepoEntry>`.
- **Refactor**: exports run sequentially in v1 (5 × ~0.3 s is fine; parallelism is a later optimization — note as such).

### Slice 5 — Snapshot: the read model the UI consumes

**Behavior**: one call produces everything the ready screen needs.

- **Red (unit)**: `snapshot::tests::{merges_ready_with_attribution, sorts_by_priority_then_updated, groups_by_repo, unattributed_goes_to_unknown_bucket}` using `FakeBdClient` returning the `ready.json` fixture.
- **Green**: `snapshot::fetch(bd, hub, prefix_map) -> Snapshot { rows: Vec<Row { issue, repo_name }>, fetched_at }` with sorting priority asc (0 first), then `updated_at` desc; grouping is a view concern but repo_name lives on the row.
- **Refactor**: none expected; keep `Snapshot` UI-agnostic (also serialize it for `fbd snapshot --json`).

### Slice 6 — Headless tracer bullet: `fbd snapshot`

**Behavior**: the full pipeline is usable and debuggable before any TUI exists. **This is the end-to-end tracer bullet.**

- **Red (unit)**: `main` split so `run_snapshot(cfg, bd, out: &mut impl Write)` is testable: `cli::tests::snapshot_prints_rows` with FakeBdClient asserts human-readable lines `"[repo] P1 ra-2hc Ready task one"` and `--json` variant emits the serialized `Snapshot`.
- **Red (integration)**: `bd_integration::snapshot_command_end_to_end` — invoke `run_snapshot` against real fixture repos; assert both repos' ready issues appear with correct attribution.
- **Green**: clap CLI: `fbd snapshot [--json]` = ensure_hub → refresh → fetch → print. Also wire `fbd reset`.
- **Refactor**: startup version gate lives here: `bd version --json` must parse and `version >= 1.1.0` && `schema_version == 1`, else exit with a clear message (unit-test the gate predicate against fixture + doctored versions). Also `fbd doctor`: prints bd version, config path, hub path, roster with per-repo existence/prefix — cheap, mostly reuses the above (test: doctor output lists a missing repo as `MISSING`).

### Slice 7 — Roster CLI: `fbd repos add/remove/list/discover`

**Behavior**: roster management without hand-editing TOML.

- **Red (unit)**: `repos_cmd::tests::{add_appends_and_dedupes, remove_by_path, list_prints_roster, discover_finds_beads_dirs, discover_skips_already_added, add_rejects_dir_without_beads}` — discover scans a tempdir tree containing `x/.beads/`, `y/.beads/`, `z/` (no .beads); assert found set. Add of a path lacking `.beads` fails with instructive error.
- **Green**: subcommands mutate `Config` via `config.rs` and print results; `discover` takes a root dir, one-level-deep scan (`<root>/*/.beads`) matching the interview decision, with `--depth` left as a documented v2 idea.
- **Refactor**: canonicalize paths on store (symlink/`~` handling; expand `~` explicitly since clap won't).

### Slice 8 — App state machine (no I/O)

**Behavior**: the entire TUI logic as a pure `reduce(state, msg) -> state` core.

- **Red (unit)**: `app::tests::` —
  - `starts_in_loading_then_shows_rows` (`RefreshStarted` → `SnapshotReady(snapshot)`),
  - `selection_moves_and_clamps` (j/k/↑/↓ over N rows),
  - `repo_filter_cycles` (f key cycles All → repo1 → repo2 → All; filtered rows recompute),
  - `priority_filter_toggles` (p key: all → P0/P1 only → all),
  - `refresh_while_stale_keeps_rows` (old rows remain visible during `RefreshStarted`, staleness flag set),
  - `refresh_error_surfaces_in_status` (per-repo errors land in `state.status_warnings`),
  - `quit_msg_sets_done`.
- **Green**: `App { rows, filtered_ix, selection, filter, status, view_mode }`, `Msg` enum covering keys + refresh lifecycle, `reduce` pure function. `keys.rs` maps crossterm `KeyEvent` → `Msg` (own tiny tests: `q→Quit`, `/→OpenSearch`, `r→Refresh`, `y→CopyContext`, `Enter→OpenDetail`, `Esc→Back`).
- **Refactor**: extract `FilterSet` applied uniformly; property: selection always within filtered bounds.

### Slice 9 — Ready view rendering + terminal runtime

**Behavior**: a human sees the ready list, grouped by repo, filters and staleness included.

- **Red (render)**: `view::tests::{renders_group_headers_and_rows, renders_selection_highlight, renders_status_bar_with_age_and_warnings, renders_empty_state_hint}` — `TestBackend(80×24)`, feed a known `App` state, assert specific buffer rows contain `▸ session-tui`, `P1 ra-2hc Ready task one`, status bar contains `refreshed 3m ago` and `1 repo failed (see doctor)`, empty roster renders "no repos configured — run: fbd repos discover ~/dev".
  - Staleness age is computed from an injected `now: DateTime` (clock passed into render/state, never `Utc::now()` inside logic) so tests are deterministic.
- **Green**: `view::draw(frame, &app, now)`; `runtime.rs` event loop: crossterm event thread → `Msg`, refresh worker thread → `Msg` over one mpsc channel; terminal setup/teardown with panic hook restoring the terminal (pattern from session-tui).
- **Red (integration-ish, unit-level)**: `runtime::tests::refresh_task_sends_started_then_done` with FakeBdClient (threads join deterministically via channel assertions).
- **Green**: `fbd` with no subcommand launches the TUI: ensure_hub (warnings → status), spawn initial refresh, loop.
- **Refactor**: rendering helpers (row formatting shared with `fbd snapshot` output).
- **Manual smoke (documented, not automated)**: run `fbd` against the 5 real repos; verify list appears < 3 s, `r` refresh keeps UI responsive.

### Slice 10 — Detail pane

**Behavior**: Enter on a row shows description, labels, dependencies (with blocker status), comments count.

- **Red (unit)**: `app::tests::{enter_requests_detail, detail_arrives_and_renders, detail_fetch_error_shows_message, esc_returns_to_list}` — `Msg::OpenDetail` emits an `Effect::FetchDetail(id)` (reduce returns optional effects; runtime executes them via `bd show <id> --json` on the hub), `Msg::DetailReady(IssueDetail)` stores it.
- **Red (render)**: `view::tests::renders_detail_pane` — buffer contains title, wrapped description, `⛔ blocks: ra-sbu Blocker task (open)` dependency line.
- **Green**: split layout (list left, detail right ≥100 cols, stacked below otherwise); detail fetched lazily per selection confirm (not per cursor move — one bd call per Enter, ~0.2 s, run on the worker thread so UI never blocks).
- **Refactor**: effect-executor table in runtime so Search (next slice) reuses the pattern.

### Slice 11 — Cross-repo search

**Behavior**: `/` opens an input; submitting queries the hub; results replace the list until Esc.

- **Red (unit)**: `app::tests::{slash_opens_search_input, typing_edits_query, enter_submits_search_effect, results_replace_rows_with_attribution, esc_restores_ready_list}` — search results flow through the same `PrefixMap` attribution as ready rows.
- **Red (render)**: `view::tests::renders_search_input_and_result_count`.
- **Green**: `Effect::Search(query)` → `bd search <q> --json` on hub; results are `Issue` rows, reuse row rendering; mode flag in `App`.
- **Refactor**: unify "row list" state so ready/search/detail-return share selection & filter code paths.

### Slice 12 — Copy-context action + release polish

**Behavior**: `y` copies an actionable context string; the tool is installable and documented.

- **Red (unit)**: `context::tests::{builds_cd_command, builds_markdown_block, unattributed_issue_falls_back_to_hub_show}` — for selected row in repo `~/dev/megaclock`, id `mc-abc`: `cd ~/dev/megaclock && bd show mc-abc`; markdown variant contains title/description; issue with unknown repo yields `bd -C <hub> show <id>` fallback. `Y` (shift) = markdown, `y` = cd command.
- **Green**: clipboard via **OSC 52 escape sequence** written to the tty (works over ssh/tmux, zero deps); status bar confirms `copied: cd … && bd show mc-abc`. Keep the string builder pure; the OSC writer is a 5-line adapter.
- **Refactor / polish (still test-first where logic exists)**:
  - `--version`, `--help` text review; README: install (`cargo install --path .`), quickstart (`fbd repos discover ~/dev && fbd`), keybindings table, architecture sketch, v2 notes (blocked view, writes, watcher).
  - Final e2e acceptance checklist (manual, recorded in PR description):
    1. Fresh machine simulation: move `~/.config/federated-beads` + data dir aside → `fbd repos discover ~/dev` finds the 5 repos → `fbd` shows merged ready list.
    2. Kill a repo path in config → status warning, UI still works.
    3. `fbd reset` → next launch rebuilds hub identically.
    4. `cargo test` green without bd on PATH (rename bd temporarily) — integration tests print SKIP.

## Risks & mitigations

| Risk | Mitigation |
|---|---|
| bd `--json` schema drift across versions | Startup gate on `version`/`schema_version`; tolerant serde (no deny_unknown_fields, Options everywhere); gated integration suite is the tripwire; fixtures re-recorded consciously |
| `bd repo list` may lack `--json` (unverified) | Slice 3 red phase verifies against real bd first; fallback path (parse hub config.yaml) specified in advance |
| Prefix collisions / ambiguous attribution | Explicit `Collision` warning + "unknown" bucket + longest-prefix match rule; unit-tested |
| Concurrent fbd instances corrupting hub sync | Advisory file lock around refresh; second instance degrades gracefully |
| Embedded Dolt locking between fbd's bd calls and a user's simultaneous bd usage in a source repo | fbd only ever writes to the hub (exports write to source repo's .beads/issues.jsonl file only, via that repo's own bd process — same as any bd command the user runs); document that a rare export failure surfaces as a per-repo warning and self-heals on next refresh |
| Refresh latency growth with repo count | Sequential v1 measured (~0.3 s/repo); parallel exports noted as v2 optimization; stale-but-browsable UI regardless |
| Clipboard portability | OSC 52 (terminal-native), no native clipboard deps; documented terminal support caveat |
| Hub corruption | Hub is derived data; `fbd reset` rebuilds from scratch; reset path-containment asserted in code |

## Migrations & compatibility

None — greenfield tool; the only external contract is bd ≥ 1.1.0 with `schema_version == 1`, enforced at startup. fbd never mutates source-repo issue data (read-only guarantee: the only writes to source repos are `bd export` refreshing their own `issues.jsonl`, which bd itself owns).

## Stop conditions (halt and consult the user)

1. Integration tests reveal a bd `--json` shape materially different from the recorded fixtures (beyond added fields) — re-confirm fixtures before continuing.
2. `bd repo sync` fails to hydrate on the user's real repos for reasons the plan doesn't cover (e.g. version skew between repos' schema).
3. `bd repo list`/config.yaml both prove unusable for hub-roster reconciliation (Slice 3).
4. Any point where honoring read-only would be violated (e.g. discovering `bd export` mutates issue data).
5. Refresh of the real 5 repos exceeds ~10 s, invalidating the UX premise — revisit design (parallel exports or per-repo direct reads) with the user.

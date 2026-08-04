# fbd — Federated Beads

A read-only terminal UI that answers **"what's ready to work on across all my
[beads](https://github.com/gastownhall/beads) repos?"** It federates N beads
repositories into a persistent hub database that `bd` itself maintains
(multi-repo hydration) and presents a cross-repo ready-work list with a detail
pane, cross-repo search, and a copy-context action.

fbd never writes to your issue data. The only writes it makes are `bd export`
refreshing each source repo's own `.beads/issues.jsonl` — which `bd` owns — so
your repos are safe. Acting on an issue happens in your terminal: the
copy-context key hands you a ready-to-run command.

## Requirements

- `bd` (beads) **>= 1.1.0** with `schema_version == 1` on `PATH` at runtime.
  fbd checks this at startup and refuses a version it cannot vouch for.

Prebuilt binaries are available for Apple Silicon and Intel macOS, plus ARM64
and x86_64 GNU/Linux. Building from source requires Rust 1.88 or newer.

## Install

Install the `0.1.0-rc.1` prerelease:

```bash
curl --proto '=https' --tlsv1.2 -LsSf https://github.com/brian-bell/federated-beads/releases/download/v0.1.0-rc.1/fbd-installer.sh | sh
```

The installer places `fbd` in `$CARGO_HOME/bin`, falling back to
`~/.cargo/bin`, and tells you if that directory needs to be added to `PATH`.
`bd` remains a separate runtime requirement.

To build from a local source checkout instead:

```bash
cargo install --path .
```

This builds and installs `fbd` into Cargo's binary directory.

## Quickstart

```bash
fbd repos discover ~/dev --add   # find every ~/dev/*/.beads repo and add it
fbd                              # launch the TUI
```

`discover` without `--add` previews what it found without changing anything.
The hub database is created automatically on first run under your XDG data dir
and is disposable derived data (see `fbd reset`).

## Keybindings (TUI)

| Key            | Action                                                    |
| -------------- | --------------------------------------------------------- |
| `j` / `↓`      | Move selection down (scrolls the detail pane in detail)   |
| `k` / `↑`      | Move selection up                                         |
| `f`            | Open the repository picker (`All repos` is first)          |
| `p`            | Toggle the priority filter: All ↔ P0/P1 only              |
| `/`            | Open cross-repo search                                    |
| `Enter`        | Open the detail pane for the selected issue               |
| `y`            | Copy `cd <repo> && bd show <id>` for the selected issue   |
| `Y`            | Copy a markdown block (title / id / repo / description)   |
| `r`            | Refresh (re-export every repo, re-sync the hub)           |
| `Esc`          | Leave the current sub-mode (detail / search) → list       |
| `q`            | Quit                                                      |

The repository picker is available from the ready list and settled search
results. Move its pending choice with `j`/`k` or the arrow keys, confirm with
`Enter`, or cancel with `Esc`. A confirmed repository view applies globally to
both ready and search results and is restored on the next launch. `All repos`
is used on first run and whenever saved UI state is missing or invalid.

`y`/`Y` place the text on your system clipboard via an **OSC 52** terminal
escape — no native clipboard dependency, and it works over ssh. For an
unattributed issue (an id matching no configured repo prefix) the copied command
falls back to `bd -C <hub> show <id>`, which is always runnable.

**Clipboard/tmux caveat:** OSC 52 requires a terminal that honors it (most
modern terminals do). Under tmux, enable it with `set -g set-clipboard on` (and,
depending on version, `set -g allow-passthrough on`). fbd emits the standard
sequence and does not wrap it for tmux passthrough in v1.

## Commands (headless)

```bash
fbd snapshot [--json]   # print the merged, attributed ready list (no TUI)
fbd doctor              # bd version + gate, config/hub paths, per-repo health
fbd reset               # delete the hub DB; rebuilt on the next snapshot/launch
fbd repos add <path>    # add a beads repo to the roster
fbd repos remove <path> # drop a repo from the roster
fbd repos list          # print the roster
fbd repos discover <dir> [--add]   # scan <dir>/*/.beads one level deep
```

The roster's source of truth is `federated-beads/config.toml` under your
platform config dir (`~/.config` on Linux, `~/Library/Application Support` on
macOS); the `repos` subcommands edit it, and `fbd doctor` prints the exact
paths in use. Missing paths warn, never fail.

The last confirmed repository view is stored independently at
`federated-beads/ui_state.json` under the platform data directory. `fbd reset`
does not remove this user preference; it only discards derived hub/cache data.

## Architecture

```
Source repos            fbd                              Hub (bd workspace)
──────────────   ──────────────────────────────   ─────────────────────────
~/dev/megaclock  refresh:  bd export per repo  →   <XDG data dir>/
~/dev/reading-…            bd repo sync (once)  →     federated-beads/hub/
     …           read:     bd ready/show/search --json (all through the hub)
```

- **Central DB**: a `bd` "hub" workspace using built-in multi-repo hydration
  (`bd repo add` + `bd repo sync`), not a custom aggregation store.
- **Read path**: every query goes through the hub via `bd … --json` subprocess
  calls. `bd` owns ready/blocked semantics; fbd never reimplements them.
- **Repo attribution**: `bd`'s JSON does not expose a source repo, so fbd maps
  each issue id to its repo by **longest id prefix** (read from each repo's
  effective `bd` prefix), detecting and flagging prefix collisions.
- **Refresh**: TUI-owned and async. Launch and the `r` key run content-stable
  exports with at most four source repositories in parallel, then one hub sync.
  Unchanged exports retain their canonical inode and mtime; changed exports are
  atomically replaced while preserving the platform's standard permission
  object. Ownership, ACLs, xattrs, labels, file flags, and inode identity are
  not part of the changed-export metadata contract. The stale list stays
  browsable, and a process-level advisory lock serializes concurrent fbd
  instances.
- **State core**: the whole TUI is a pure `reduce(&mut App, Msg) -> Vec<Effect>`
  state machine (no I/O, no clock, no threads inside), so it is exhaustively
  unit-tested; the runtime performs the effects. The last confirmed repository
  view is stored separately in versioned `ui_state.json`; snapshot-cache
  freshness and roster configuration remain independent.

Module map: `config` (roster + XDG paths) · `ui_state` (persisted TUI
preferences) · `bd` (the `BdClient` trait, real subprocess + fake impls, serde
types) · `hub` (lifecycle) · `refresh` (export + sync + prefix map) · `snapshot`
(the read model) · `app` (`reduce` core, `view` renderer, `keys` mapping,
`context` copy builders) · `runtime` (the event loop and workers) · `cli`
(headless subcommands).

## Verification commands

These four commands are the project's quality gate and stay constant across
slices:

```bash
cargo fmt --check                              # formatting
cargo clippy --all-targets -- -D warnings      # lints (warnings are errors)
cargo test                                     # unit + render tests (green without bd)
cargo test --test bd_integration               # gated e2e (skips cleanly without bd)
```

The integration suite builds real fixture repos with `bd` in tempdirs; each test
skips with an explicit `SKIP` line when `bd` is not installed.

The ignored, machine-dependent refresh matrix is available separately:

```bash
cargo test refresh_performance_matrix -- --ignored --nocapture
```

Recorded phase timings for the refresh-performance epic live in
`docs/performance/federated-beads-9dt.md`.

The central-Dolt topology spike has a separate, machine-dependent black-box
matrix. It requires pinned real `bd` and `dolt` executables and is not part of
ordinary `cargo test`:

```bash
BD_BIN=/path/to/bd DOLT_BIN=/path/to/dolt make test-central-dolt
BD_BIN=/path/to/bd DOLT_BIN=/path/to/dolt EXPECT=compatible \
  make test-central-dolt-version
```

Its selected per-project-authority contract, evidence matrix, and deferred
deployment gates are documented in
`docs/architecture/central-dolt-compatibility.md`.

## Not in v1 (planned)

- A blocked-issues view (v1 shows only ready work).
- Any write path from the TUI (create/update/close/comment) — the copy-context
  key is the bridge to acting in a terminal.
- A background daemon / file watcher; refresh is user-triggered.

See `plans/fbd-v1-implementation-plan.md` for the full design and the per-slice
TDD plan; `plans/slices/` for individual slices.

## License

Licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE))
- MIT license ([LICENSE-MIT](LICENSE-MIT))

at your option.

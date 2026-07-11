# Slice 0 — Harness: cargo scaffold + config round-trip

Bead: `federated-beads-dxh.1` (child of epic `federated-beads-dxh`).
Master plan: `plans/fbd-v1-implementation-plan.md` (Slice 0 + global sections).

## Goal

Stand up the `fbd` Rust project so TDD is possible for every later slice, and
prove the harness with one real red→green→refactor cycle: a config round-trip
(save/load `Config { repos }` via TOML in a tempdir). Establish the four
verification commands and the git baseline.

**This slice writes no domain/bd/TUI logic.** Only the project skeleton, the
`config` module, and the `Paths` resolver.

## Scope (in)

- `cargo init --name fbd` (binary crate) with the pinned dependency set.
- `src/config.rs`: `Config`, `RepoEntry`, `load(path)`, `save(path)` via TOML.
- `Paths` resolver (`config_file`, `data_dir`) with an injectable base so tests
  never touch real XDG dirs; real XDG resolution (`dirs`) only at the edge.
- `src/main.rs`: minimal entry that compiles and references the modules (no real
  CLI behavior yet — a stub is fine; later slices build the clap surface).
- `README.md` documenting the four verification commands.
- `.gitignore`: add `/target` (keep existing beads/dolt ignores).
- `tests/bd_integration.rs`: a placeholder gated integration test file so
  `cargo test --test bd_integration` is a real, stable command from Slice 0 on.
- Stage `plans/` and `.beads/formulas/fbd-slice.formula.toml` on the branch so
  they land in main with the merge.

## Scope (out)

- No `bd` subprocess client, domain types, hub, refresh, snapshot, TUI, or CLI
  subcommands (Slices 1–12).
- No `gh repo create` / no pushes / no remote — user authorized local-only.
- No CI config.
- `Paths` does not need env-var overrides beyond the injectable base in this
  slice; real XDG is wired but only exercised via the edge constructor.

## Files to create / change

| File | Action | Purpose |
|---|---|---|
| `Cargo.toml` | create (via `cargo init`) then edit | crate name `fbd`, dependencies |
| `src/main.rs` | create | compiles, wires modules, stub entry |
| `src/config.rs` | create | `Config`/`RepoEntry`/`load`/`save`/`Paths` |
| `tests/bd_integration.rs` | create | gated placeholder integration test |
| `README.md` | create | document 4 verification commands + quickstart |
| `.gitignore` | edit | add `/target` |

## Dependencies (Cargo.toml)

Runtime: `ratatui`, `crossterm`, `serde` (derive), `serde_json`, `toml`,
`clap` (derive), `anyhow`, `thiserror`, `dirs`.
Dev: `tempfile`.

Only `serde`, `toml`, `dirs` are actually used by Slice 0 code; the rest are
declared now so the pinned set is fixed once and later slices don't churn
`Cargo.toml`. Unused-dep warnings are not clippy errors for declared-but-unused
crates, so `-D warnings` stays clean. (If clippy/cargo flags any as unused in a
way that fails the gate, drop the unused ones and reintroduce them per-slice —
decide from the real clippy output, not speculation.)

## TDD test list (in order)

Strict red→green→refactor. For each: write test, run to observe the exact red,
implement minimally to green, refactor with tests green.

### 1. `config::tests::roundtrip_roster` (the harness proof)

- **Test**: build `Config { repos: vec![RepoEntry { path: "/a".into() }, RepoEntry { path: "/b/c".into() }] }`; `save` it to a path inside `tempfile::tempdir()`; `load` it back; assert the loaded `Config` equals the original.
- **Expected red**: compile error — `config` module / `Config` / `RepoEntry` / `save` / `load` do not exist. (Compile failure is the legitimate red state for the first test.)
- **Minimal green**: define `#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)] struct RepoEntry { path: PathBuf }` and `Config { repos: Vec<RepoEntry> }`; `save(&self, path) -> anyhow::Result<()>` serializes to TOML and writes; `load(path) -> anyhow::Result<Config>` reads and parses. Derive `PartialEq` so the test can assert equality.
- **Notes**: TOML needs a table/array-of-tables; `repos` as `Vec<RepoEntry>` serializes as `[[repos]]` with a `path` key — fine. `PathBuf` serializes as a string.

### 2. `config::tests::load_missing_file_errors`

- **Test**: `load` a path that does not exist inside a tempdir; assert it returns `Err`.
- **Expected red**: passes trivially only once `load` exists; written alongside test 1's green to pin the error contract. If `load` were to silently return a default, this fails. (If it already errors from test 1's impl, this is a characterization test — still write it to lock the behavior.)
- **Minimal green**: ensure `load` surfaces the read error via `?` / `anyhow` rather than swallowing it. No default-on-missing.

### 3. `config::tests::save_creates_parent_dirs` (edge case)

- **Test**: `save` to `<tempdir>/nested/does/not/exist/config.toml`; assert the file exists and round-trips.
- **Expected red**: `save` fails because parent dir is missing (write error).
- **Minimal green**: in `save`, `create_dir_all(parent)` before writing. This matters because the real config path (`~/.config/federated-beads/config.toml`) won't have its parent on first run.

### 4. `config::tests::paths_uses_injected_base` (the refactor target)

- **Test**: construct `Paths::with_base(base_dir)` for a tempdir `base`; assert `paths.config_file()` == `base/config.toml` (or `base/federated-beads/config.toml` — pick one and pin it) and `paths.data_dir()` == the expected data path under `base`. No environment reads, no real XDG.
- **Expected red**: `Paths` does not exist.
- **Minimal green**: `struct Paths { config_file: PathBuf, data_dir: PathBuf }` with `with_base(base) -> Paths` deriving both deterministically from `base`, and accessors. Then add the real edge constructor `Paths::resolve() -> anyhow::Result<Paths>` using `dirs::config_dir()` + `dirs::data_local_dir()` joined with `federated-beads/` (config file at `<config>/federated-beads/config.toml`, data at `<data>/federated-beads/`). `resolve()` is NOT unit-tested against real dirs (would touch the user's home); it is exercised only from `main`. Its correctness is by construction + the injected-base test covering the join logic via a shared private helper `from_config_and_data_roots(config_root, data_root)`.
- **Refactor**: route both `with_base` and `resolve` through the shared helper so the path-join logic is tested once via `with_base`.

### 5. (integration placeholder) `bd_integration.rs` skip guard

- **Test**: `tests/bd_integration.rs` with one test `harness_present` that (for now) just asserts `true`, OR probes `bd version` and `eprintln!("SKIP: bd not installed")` + early-return when absent. Slice 0 keeps it minimal: a compiling test file so `cargo test --test bd_integration` is a stable command. Real gated e2e arrives in Slice 2.
- **Expected red**: `cargo test --test bd_integration` fails because the file doesn't exist / doesn't compile.
- **Minimal green**: create the file with the trivial test.

## Edge cases covered

- Missing config file on load → error, not silent default (test 2).
- Missing parent directory on save → auto-created (test 3), needed for first run.
- Tests never read env or touch `~/.config` / `~/.local/share` (injected base, test 4).
- Empty roster (`Config { repos: vec![] }`) round-trips — implicitly covered; add an assertion in test 1 variant if cheap, else rely on Vec handling.

## Verification (must all be clean at slice end)

```
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
cargo test --test bd_integration
```

## Git plan

- Branch `slice-0-harness` off `main`.
- Logical commits (or fewer if natural):
  1. scaffold: `cargo init`, Cargo.toml deps, .gitignore `/target`, README, stub main, plans/ + formula staged.
  2. config: tests + `config.rs` (Config/RepoEntry/load/save) + Paths resolver.
- `git checkout main && git merge --no-ff slice-0-harness -m "Merge slice 0: harness scaffold"`.
- Verify fmt/clippy/test clean on main. NO push, NO remote.

## Autoreview

Run the autoreview skill on branch vs main. Fix actionable findings on-branch
(tests stay green). Record skipped findings + reasons as a comment on
`federated-beads-dxh.1`.

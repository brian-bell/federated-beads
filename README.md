# fbd — Federated Beads

A read-only terminal UI that answers "what's ready to work on across all my
[beads](https://github.com/gastownhall/beads) repos?" It federates N beads
repositories into a persistent hub database that `bd` maintains, and presents a
cross-repo ready-work list with a detail pane, cross-repo search, and a
copy-context action.

Status: early development. See `plans/fbd-v1-implementation-plan.md` for the
full design and the per-slice TDD plan; `plans/slices/` for individual slices.

## Requirements

- Rust (edition 2024) toolchain.
- `bd` (beads) >= 1.1.0 with `schema_version == 1` on `PATH` at runtime.
  Integration tests skip cleanly when `bd` is absent.

## Verification commands

These four commands are the project's quality gate and stay constant across
slices:

```bash
cargo fmt --check                              # formatting
cargo clippy --all-targets -- -D warnings      # lints (warnings are errors)
cargo test                                     # unit + render tests (green without bd)
cargo test --test bd_integration               # gated e2e (skips cleanly without bd)
```

## Layout

- `src/config.rs` — roster config (`config.toml`) load/save and XDG path
  resolution (`Paths`, injectable for tests).
- `tests/bd_integration.rs` — gated end-to-end tests against a real `bd`.
- `plans/` — design and per-slice implementation plans.

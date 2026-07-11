# Slice 9b — Fix hyphenated-repo prefix attribution (dxh.17)

Bug: `federated-beads-dxh.17` (P1, child of epic `federated-beads-dxh`).
Interstitial slice between slices 9 and 10.
Mol workflow root: `federated-beads-mol-m9t`.
Depends on: Slice 4 (`refresh.rs` / `PrefixMap` / `read_prefix`, merged).
Touches: `src/refresh.rs`, `tests/bd_integration.rs`, `tests/helpers/mod.rs`.

## The bug

`refresh::read_prefix` returns `<repo>/.beads/metadata.json`'s `dolt_database`
as the id prefix. But bd sanitizes the Dolt database name (Dolt disallows
hyphens/dots) while issue IDs keep their original prefix. So a repo whose real
prefix contains a hyphen is misattributed: its ids never match the sanitized
prefix and fall into the `[unknown]` bucket.

Real-machine evidence (before fix, `cargo run -- snapshot`, 7-repo roster):
`[unknown]` bucket = 21 issues, containing **every** `federated-beads-*` and
`reading-lite-*` id. `agent-skills` (prefix `as`, no hyphen), `approach`,
`megaclock` attribute fine.

## Probe evidence (real bd 1.1.0, build 8e4e59d39)

I created throwaway `bd init --prefix <p>` repos, made one issue in each, and
compared `metadata.json` `dolt_database` against the created issue id:

| `--prefix`     | `dolt_database` | first issue id      |
|----------------|-----------------|---------------------|
| `has-hyphen`   | `has_hyphen`    | `has-hyphen-rf0`    |
| `reading-lite` | `reading_lite`  | `reading-lite-hck.1`|
| `a_b-c`        | `a_b_c`         | `a_b-c-wy3`         |
| `Foo.Bar-baz`  | `Foo_Bar_baz`   | `Foo_Bar-baz-tj8`   |
| `UPPER-case`   | `UPPER_case`    | `UPPER-case-hf2`    |
| `dot.dot`      | `dot_dot`       | `dot_dot-2ta`       |

Findings:

1. **The id prefix is the ground truth for attribution** — it is literally the
   leading segment of every id in the repo. `dolt_database` is a *lossy* further
   sanitization of it.
2. **The exact, verified invariant:** `id_prefix.replace('-', '_') == dolt_database`.
   bd keeps `-` in ids but maps `-`→`_` for the DB name; other non-alphanumerics
   (`.`) are already mapped to `_` in *both* the id prefix and the DB name (see
   `Foo.Bar-baz` → id prefix `Foo_Bar-baz`, db `Foo_Bar_baz`). So the *only*
   character that differs between id prefix and `dolt_database` is `-`↔`_`.
   Uppercase is preserved in both.
3. **`bd init` does NOT write `issue-prefix` into `.beads/config.yaml`** — the
   file key stays commented out. But **bd still reports the effective prefix**:
   `bd -C <repo> config get issue_prefix --json` returns
   `{"key":"issue_prefix","schema_version":1,"value":"<prefix>"}` with hyphens
   intact, even when auto-detected and not written to config.yaml. Verified
   against every real repo: `agent-skills`→`as`, `approach`→`approach`,
   `federated-beads`→`federated-beads`, `reading-lite`→`reading-lite`,
   `session-tui`→`session-tui`, `megaclock`→`megaclock`. A no-project dir
   (`~/dev/beads`) errors (`no beads project found`). This is the **authoritative,
   loss-free** prefix — no sanitization, no id parsing, no ownership guessing.
4. **fbd's `argv_export` writes `<repo>/.beads/issues.jsonl`** with an explicit
   `-o` (the slice-4 P1 fix), but the prefix no longer needs the export: it comes
   straight from `bd config get issue_prefix`.

## Approach evaluation

The task listed four candidate approaches. Evidence rules out all four in favor
of a fifth — **bd's own effective `issue_prefix`** — which autoreview surfaced
as the only non-lossy option:

- **(a) un-sanitize `_`→`-` from `dolt_database`.** WRONG. `dolt_database`
  `a_b_c` could come from real prefix `a-b-c`, `a_b-c`, `a-b_c`, or `a_b_c`
  (2^n ambiguity). Fails on any underscore-bearing or mixed prefix.
- **(b) read `issue-prefix` from `.beads/config.yaml`.** The file key is unset on
  init — but **bd's effective config is authoritative** (probe finding 3):
  `bd config get issue_prefix --json` returns the exact prefix. This is (b) done
  via the bd CLI instead of the raw file. **CHOSEN.**
- **(c) derive the prefix from the repo's own issue ids.** Was the initial choice
  and works for the common case, but matching an id against `dolt_database` is
  *lossy* (`-`→`_` is many-to-one): a same-sanitizing foreign hydrated id can
  validate, and a repo could be misattributed. Rejected after autoreview
  (P1/P2 findings) — ownership can't be proven from a lossy match.
- **(d) candidate-set (both sanitized and unsanitized forms).** Same lossiness
  as (c); fails on mixed prefixes and risks false collisions.

### Chosen design: authoritative `bd config get issue_prefix`

Attribution asks bd for each repo's effective, hyphen-preserving prefix rather
than reconstructing it from lossy metadata or ids:

1. Add `BdClient::issue_prefix(repo) -> Result<String, BdError>`, backed by
   `bd -C <repo> config get issue_prefix --json` (`{"value":"<prefix>"}`). This
   is bd's effective value — reported even when auto-detected and not written to
   `config.yaml` — with hyphens intact and no sanitization.
2. `refresh::run` builds the `PrefixMap` from `bd.issue_prefix(&entry.path)` for
   each roster repo. A failure (e.g. a non-project dir) becomes
   `RepoError::Metadata` — the repo is unattributed but the refresh still
   succeeds, preserving the existing contract.
3. `run_doctor` displays the same authoritative prefix.

Why this is correct where (a)/(c)/(d) are not:
- bd is the single source of truth for the prefix it stamps onto ids; there is no
  reconstruction, no lossy `-`↔`_` guessing, and no id-ownership inference.
- Correct for hyphens, underscores, mixed, dots, uppercase, and any custom
  `--prefix` — verified against all seven real repos.

`read_prefix` (the old `metadata.json` → `dolt_database` reader) is retained only
as the `FakeBdClient`'s default `issue_prefix` (mirroring a real no-hyphen repo
where prefix == `dolt_database`), so metadata-seeded fixtures keep working
without explicitly programming a prefix.

## Scope

**In:**
- `src/bd/mod.rs` — new `BdClient::issue_prefix` trait method.
- `src/bd/cli.rs` — `BdCli::issue_prefix` (+ `argv_issue_prefix`, `ConfigValue`,
  argv test).
- `src/bd/fake.rs` — `Call::IssuePrefix`, `with_issue_prefix`, default that reads
  the seeded `metadata.json` via `read_prefix`.
- `src/refresh.rs` — `run` uses `bd.issue_prefix`; `read_prefix` reverts to the
  trivial `dolt_database` reader (fake default only); new attribution tests.
- `src/cli.rs` — `run_doctor` uses `bd.issue_prefix`.
- `tests/bd_integration.rs` — new gated `refresh_attributes_hyphenated_repo`
  (real `bd init --prefix ready-fix`).

**Out:** no change to `PrefixMap`/collision semantics, snapshot rendering, or TUI.
No new dependencies.

## Ordered TDD test list (red → green)

Unit tests in `src/refresh.rs` `#[cfg(test)]` drive `FakeBdClient`, programming
`with_issue_prefix` for the authoritative prefix; the fake's default (seeded
`metadata.json`) keeps the slice-4 tests green unchanged.

1. **`attributes_hyphenated_repo_from_bd_prefix`** (the bug)
   - Red (pre-fix): `repo_for("reading-lite-hck.1")` is `None` (unknown bucket).
   - Green: repo seeded `dolt_database:"reading_lite"`, fake programmed
     `issue_prefix = "reading-lite"` ⇒ `repo_for("reading-lite-hck.1")` is the
     repo; `repo_for("reading_lite-hck.1")` (sanitized form) is `None`.

2. **`underscore_prefix_is_not_remapped`**
   - Green: `issue_prefix = "foo_bar"` ⇒ `foo_bar-abc` attributes, `foo-bar-abc`
     does not. No `_`↔`-` guessing.

3. **`custom_prefix_unrelated_to_dir_name_attributes`**
   - Green: dir `whatever`, `issue_prefix = "ready-fix"` ⇒ `ready-fix-1`
     attributes. Prefix-driven, not dir-name-driven.

4. **`two_hyphenated_repos_attribute_independently`**
   - Green: `reading-lite` and `session-tui` each attribute their own ids, no
     collision.

5. **`read_prefix_returns_sanitized_dolt_database`**
   - Green: the `metadata.json` helper (fake default) returns the sanitized DB
     name verbatim (`reading_lite`), documenting why the fake needs an explicit
     prefix for hyphenated repos.

6. **`exports_all_then_syncs_once`** (updated) — asserts the per-repo call order
   is now `Export`, `IssuePrefix`, … then one `RepoSync`.

7. **Collision / lock / error behavior unchanged** — `flags_prefix_collisions`,
   `longest_prefix_wins`, `collided_longer_prefix_is_not_masked_by_shorter`,
   `duplicate_roster_entry_is_not_a_collision`, `metadata_read_failure_is_a_repo_error`
   (now exercises the fake's default `issue_prefix` failure), and the lock tests
   stay green.

### Integration (gated, `tests/bd_integration.rs`)

8. **`refresh_attributes_hyphenated_repo`**
   - Skips cleanly without bd. With bd: `build_ready_fixture_repo_with_prefix(&r,
     "ready-fix")` (hyphenated), `ensure_hub`, `refresh::run`. Assert
     `errors`/`collisions` empty, and for the real hub `ready` id starting
     `ready-fix-`, `outcome.prefix_map.repo_for(id)` canonicalizes to the repo —
     proving the export→sync→derive→attribute pipeline works end to end for a
     hyphenated prefix against real bd.

## Verification (all four must be green)

```
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
cargo test --test bd_integration
```

Plus the real-world proof: `cargo run -- snapshot | head -30` against the user's
existing 7-repo config — `reading-lite` and `federated-beads` rows must no
longer be `[unknown]`. Record before/after in a `bd comment` on dxh.17.
(Before: 21 `[unknown]`. The `beads` repo's export error — "no beads project
found" — is a genuine unrelated roster issue, not this bug, and will remain.)

## Edge cases

- **Hyphenated prefix** → bd reports it with hyphens intact; attributes. (#1)
- **Genuine underscore prefix** → bd reports the underscore; no `_`↔`-`
  guessing. (#2)
- **Mixed/dotted/uppercase/custom prefix** → whatever bd stamps is what bd
  reports; correct by construction. (#3)
- **Non-project or unreadable repo** → `bd config get issue_prefix` fails →
  `RepoError::Metadata`; repo unattributed, refresh still `Ok`. (existing
  `metadata_read_failure_is_a_repo_error`)

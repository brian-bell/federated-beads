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
3. **`bd init` does NOT write `issue-prefix` into `.beads/config.yaml`** — it
   stays commented out (auto-detected from the dir name at init). So config.yaml
   is not a reliable prefix source.
4. **fbd's `argv_export` writes `<repo>/.beads/issues.jsonl`** with an explicit
   `-o` (the slice-4 P1 fix). So after `refresh::run` exports a repo, its
   `issues.jsonl` exists and its first records are `{"_type":"issue","id":"…"}`
   lines (the real repo has only `_type":"issue"` records). Bare `bd export`
   with no `-o` prints to stdout and writes no file — so we rely on fbd's own
   `-o` export, which always ran before `read_prefix` in `run`.

## Approach evaluation

The task listed four candidate approaches. Evidence rules out all but one:

- **(a) un-sanitize `_`→`-` from `dolt_database`.** WRONG. `dolt_database`
  `a_b_c` could come from real prefix `a-b-c`, `a_b-c`, `a-b_c`, or `a_b_c`
  (2^n ambiguity). Verified: `a_b-c` is a real prefix whose `dolt_database` is
  `a_b_c`; blind `_`→`-` yields `a-b-c` ≠ real. Fails on any underscore-bearing
  or mixed prefix.
- **(b) read `issue-prefix` from `.beads/config.yaml`.** Dead: bd doesn't write
  it on init (probe finding 3).
- **(d) candidate-set (both sanitized and unsanitized forms).** Only correct
  when the real prefix has no underscores; fails on mixed prefixes like
  `foo_bar-baz` (would need 2^n candidates) and risks false collisions between
  a hypothetical `a-b` and `a_b`.
- **(c) derive the prefix from the repo's own issue ids** — CHOSEN. The id
  prefix is the ground truth (finding 1). fbd's export already wrote
  `issues.jsonl` (finding 4), so the ids are on disk for free — no extra bd
  invocation. Correct for hyphens, underscores, mixed, dots, uppercase, and any
  custom `--prefix` (attribution never depends on the dir name).

### Chosen design: derive-and-validate

Extend `read_prefix(repo)` to prefer the id-derived prefix, using
`metadata.json` `dolt_database` as both the fallback and a **validation anchor**:

1. Read `dolt_database` from `metadata.json` (unchanged: missing/unparseable
   metadata is still a `RepoError::Metadata`, preserving the existing contract
   and the `metadata_read_failure_is_a_repo_error` test).
2. Scan `<repo>/.beads/issues.jsonl` line by line. For each `_type == "issue"`
   record, derive a candidate prefix from its id via `rsplit_once('-')` (the id
   is `<prefix>-<hash>`; the prefix itself may contain `-`, so split on the
   *last* `-`). Accept the first candidate whose `candidate.replace('-', '_')`
   equals `dolt_database`. Return it.
3. If no id validates (empty repo, no `issues.jsonl`, or only foreign hydrated
   ids), fall back to `dolt_database` unchanged.

Why validate against `dolt_database` (finding 2's invariant):
- It guarantees we only ever return the repo's *own* prefix, never a foreign
  hydrated id's prefix that happens to lead the file.
- It is provably correct: for any of the repo's own ids the invariant holds by
  construction, so genuine ids always validate; anything that fails validation
  is not this repo's prefix and is correctly skipped.
- The fallback is harmless: a repo with no validating id has no ids of its own
  in the hub, so its prefix is never queried for attribution anyway.

`rsplit_once('-')` correctly handles child/hierarchical ids too:
`federated-beads-dxh.17`.rsplit_once('-') → prefix `federated-beads`,
rest `dxh.17`; validates (`federated_beads == dolt_database`). ✓

Streaming with `BufReader::lines()` and stopping at the first validating id
keeps large `issues.jsonl` (real ones are tens of KB) cheap.

## Scope

**In:**
- `src/refresh.rs` — extend `read_prefix` with the derive-and-validate step and
  a small private `derive_prefix_from_issues` / sanitization helper; new unit
  tests. No change to `PrefixMap`, `run`'s order of operations, error types, or
  the `RepoError::Metadata` contract.
- `tests/helpers/mod.rs` — already parameterized on `prefix`; used as-is for a
  hyphenated fixture (no change expected).
- `tests/bd_integration.rs` — new gated `refresh_attributes_hyphenated_repo`.

**Out:** no change to attribution/collision semantics, snapshot rendering, CLI,
or TUI. No new dependencies (reuse `serde_json`, `std::io::BufRead`).

## Ordered TDD test list (red → green)

Unit tests in `src/refresh.rs` `#[cfg(test)]`. A new helper seeds
`issues.jsonl` alongside the existing `seed_repo` (which seeds only
`metadata.json`); existing seed-only tests keep passing via the fallback (step
3), which is itself a regression guard that the fix doesn't disturb the
no-jsonl path.

1. **`read_prefix_derives_hyphenated_prefix_from_ids`** (the bug, unit level)
   - Red: `read_prefix` returns `reading_lite` (sanitized).
   - Green: seed `dolt_database:"reading_lite"` + `issues.jsonl` with id
     `reading-lite-hck.1` ⇒ `read_prefix` returns `"reading-lite"`.

2. **`read_prefix_falls_back_to_dolt_database_without_jsonl`**
   - Green: seed only `metadata.json` (`ra`), no `issues.jsonl` ⇒ returns `"ra"`.
     Locks the fallback that keeps the existing seed-only tests valid.

3. **`read_prefix_does_not_remap_a_genuine_underscore_prefix`** (no false remap)
   - Green: `dolt_database:"foo_bar"` + id `foo_bar-abc` ⇒ returns `"foo_bar"`,
     not `"foo-bar"`. (`foo_bar`.replace('-','_') == `foo_bar` validates the id
     as-is.)

4. **`read_prefix_skips_foreign_ids_that_fail_validation`**
   - Green: `dolt_database:"reading_lite"` + `issues.jsonl` whose first line is a
     foreign id `other-thing-xyz` (sanitizes to `other_thing` ≠ `reading_lite`)
     followed by `reading-lite-hck.1` ⇒ returns `"reading-lite"`. And a file of
     *only* foreign ids ⇒ falls back to `"reading_lite"`.

5. **`attributes_hyphenated_repo_end_to_end`** (through `run` + `PrefixMap`)
   - Red: `repo_for("reading-lite-hck.1")` is `None` (unknown bucket).
   - Green: a seeded repo (`dolt_database:"reading_lite"` + hyphenated
     `issues.jsonl` ids) run through `refresh::run` (FakeBdClient) ⇒
     `repo_for("reading-lite-hck.1")` is the repo; `repo_for("reading-lite-x1u")`
     too; `repo_for("reading_lite-hck.1")` (underscored form) is `None`.

6. **`custom_prefix_unrelated_to_dir_name_attributes`**
   - Green: repo dir `whatever` with `dolt_database:"ready_fix"` + ids
     `ready-fix-1` ⇒ `repo_for("ready-fix-1")` is the repo. Proves attribution
     is prefix-driven, not dir-name-driven.

7. **Collision behavior unchanged** — the existing `flags_prefix_collisions`,
   `longest_prefix_wins`, `collided_longer_prefix_is_not_masked_by_shorter`, and
   `duplicate_roster_entry_is_not_a_collision` tests must stay green unmodified.
   Add `two_hyphenated_repos_attribute_independently`: repos `reading-lite` and
   `session-tui` (both hyphenated, distinct) ⇒ each id attributes to its own
   repo, no collision.

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

- **Empty repo / no `issues.jsonl`** → fall back to `dolt_database`; harmless
  (no own ids to attribute). (#2)
- **Genuine underscore prefix** → id validates as-is; no false `_`→`-` remap. (#3)
- **Foreign hydrated id leads the file** → fails validation, skipped. (#4)
- **Mixed prefix (`foo_bar-baz`)** → derived from id, exact; (a)/(d) can't. (implicit)
- **Missing/unparseable `metadata.json`** → still `RepoError::Metadata`
  (unchanged). (existing test)

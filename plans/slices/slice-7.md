# Slice 7 — Roster CLI: `fbd repos add/remove/list/discover`

Bead: `federated-beads-dxh.8` (child of epic `federated-beads-dxh`).
Mol workflow root: `federated-beads-mol-c22`.
Master plan: `plans/fbd-v1-implementation-plan.md` (Slice 7 + global sections).
Depends on: Slices 0–6 (merged). Uses `config::{Config, Paths, RepoEntry}` and the
CLI conventions established in `plans/slices/slice-6.md` (`src/cli.rs` testable
runners, `CliError`, `load_roster`, `sanitize`).

## Goal

Let a user manage the roster (`config.toml`) without hand-editing TOML:

```
fbd repos add <path>        canonicalize + append <path> (must contain .beads); dedupe; save
fbd repos remove <path>     drop the entry naming <path>; save; hint `fbd reset`
fbd repos list              print the roster (path per line)
fbd repos discover <root>   scan <root>/*/.beads one level deep; list new repos
fbd repos discover <root> --add   ...and add the ones found
```

This slice writes roster-editing subcommands only. No hub work, no bd calls (roster
editing is pure config I/O), no TUI.

## discover UX decision (documented, per orchestrator ask)

`discover <root>` **lists** the beads repos it finds by default and mutates nothing;
`--add` is the explicit opt-in that appends them. Rationale: this tool is read-only
by design (master plan non-goal #1), and editing the roster is a side effect. A
preview-first `discover` lets the user see what a scan turned up — and which entries
are already in the roster — before committing, and `--add` makes the mutation a
conscious act rather than a surprise from a bare scan. Both modes filter out repos
already in the roster (`discover_skips_already_added`). Tested in both modes.

## Module layout

- **`src/cli.rs`**: four new runners plus a `~`-expansion helper. Each runner takes
  `&Paths` and an injected `&mut impl Write` sink (no bd, no XDG reads) so the whole
  surface is unit-tested against a tempdir-backed `Paths::with_base`. Mirrors the
  slice-6 runner style exactly.

  ```rust
  pub fn run_repos_add(paths: &Paths, path: &Path, out: &mut impl Write) -> Result<(), CliError>;
  pub fn run_repos_remove(paths: &Paths, path: &Path, out: &mut impl Write) -> Result<(), CliError>;
  pub fn run_repos_list(paths: &Paths, out: &mut impl Write) -> Result<(), CliError>;
  pub fn run_repos_discover(paths: &Paths, root: &Path, add: bool, out: &mut impl Write)
      -> Result<(), CliError>;

  fn expand_tilde(p: &Path) -> PathBuf;   // leading `~` / `~/…` → $HOME; else unchanged
  fn store_path(p: &Path) -> PathBuf;     // expand_tilde then canonicalize-or-absolutize
  ```

- **`src/main.rs`**: add a `Repos { #[command(subcommand)] action: ReposAction }`
  arm and a `ReposAction` enum (`Add{path}`, `Remove{path}`, `List`,
  `Discover{root, --add}`). Dispatch needs only `Paths` + stdout — no `BdCli`,
  no roster preload (each runner loads/saves the roster itself, like `doctor`).

- **`CliError`**: add one variant `#[error("{0}")] Roster(String)` for the
  instructive "not a beads repo" rejection. Config save/load errors continue to map
  through the existing `CliError::Io(io::Error::other(e))` pattern used by
  `load_roster`.

## Path handling (refactor step)

- **`~` expansion**: clap hands `~` through literally, so `expand_tilde` replaces a
  leading `~` or `~/…` with `dirs::home_dir()` (unchanged if home can't be resolved
  or the path doesn't start with `~`). Only a leading bare-`~` segment is expanded,
  not `~user`.
- **Canonicalize on store**: `store_path` = `expand_tilde` then `fs::canonicalize`
  (falls back to an absolutized/as-is path when the target doesn't exist — matches
  `hub::normalize` / `refresh::normalize`). `add` stores the canonical path; dedupe
  compares canonical paths, consistent with `ensure_hub`'s roster dedupe. So adding
  `~/dev/x`, `./x`, and an absolute alias of the same repo all collapse to one entry.
- **`remove` matching**: a repo being removed may already be gone from disk (that is
  a reason to remove it), so `canonicalize` can fail. Match an entry if
  `store_path(input) == store_path(entry.path)` OR the raw paths are equal — so both
  a live canonical match and a stale exact-string match remove.

## Behavior details

- **add**: reject when `<expanded>/.beads` is not a directory →
  `CliError::Roster("not a beads repo: <path> has no .beads directory — run \`bd
  init\` there first")`. On success, if the canonical path is already present, print
  `already in the roster: <path>` (no dup, still `Ok`, no save); else append, save,
  print `added <path> to the roster`.
- **remove**: if a matching entry exists, drop it, save, print `removed <path> from
  the roster` **and** a follow-up hint `note: run \`fbd reset\` so the hub drops this
  repo (the hub is rebuilt from the roster)` — the cheap in-scope hook for
  `federated-beads-dxh.15` (stale hub entries), without implementing hub pruning. If
  no entry matches, print `not in the roster: <path>` and return `Ok` (idempotent,
  like `reset`).
- **list**: `roster (<n> repos):` then `  <path>` per entry; empty roster prints
  `roster is empty; add repos with \`fbd repos add <path>\``.
- **discover**: `read_dir(root)` one level deep; a child is a candidate iff
  `child.join(".beads").is_dir()`. Canonicalize candidates, drop any already in the
  roster, sort for deterministic output.
  - `--add` absent: `found <n> beads repo(s) under <root>:` + one line each + footer
    `re-run with --add to add them`; nothing found → `no new beads repos found under
    <root>`. No save.
  - `--add` present: append all (deduped), save, `added <n> repo(s) from <root>:` +
    one line each; nothing found → `no new beads repos found under <root>`.
  - A missing/unreadable `<root>` is a `CliError` (a real error, unlike a roster path
    that legitimately may not exist yet).
- **Sanitization**: every path written to a terminal goes through `sanitize` (as
  doctor already does for roster paths) so a control-char-laden path can't drive the
  terminal.

## Ordered TDD test list (`src/cli.rs`, `#[cfg(test)]`, `Paths::with_base`)

Reuse the existing `seed_repo` / `roster` test helpers.

1. **`add_appends_and_dedupes`** — red: `run_repos_add` undefined. green: add a
   seeded repo → roster has 1 entry + `out` says "added"; add the *same* path again →
   still 1 entry (load config back and assert `repos.len()==1`) + `out` says "already
   in the roster".
2. **`add_rejects_dir_without_beads`** — red: no rejection. green: a dir lacking
   `.beads` → `Err(CliError::Roster(_))` whose message contains the path and
   `bd init`; roster unchanged (no config written).
3. **`remove_by_path`** — red: `run_repos_remove` undefined. green: seed+add two
   repos, remove one by path → config reloads with just the other; `out` says
   "removed".
4. **`remove_hints_reset`** — green: after a successful remove, `out` mentions
   `fbd reset` (the `.15` hook).
5. **`remove_missing_is_friendly`** — green: removing a path not in the roster
   returns `Ok` and prints "not in the roster" (idempotent).
6. **`list_prints_roster`** — red: `run_repos_list` undefined. green: two seeded
   repos in the saved roster → `out` names both paths and `roster (2 repos)`; an
   empty roster prints the "roster is empty" hint.
7. **`discover_finds_beads_dirs`** — red: `run_repos_discover` undefined. green:
   build `<root>/{x/.beads, y/.beads, z}` (z has no `.beads`); `discover(add=false)`
   → `out` contains `x` and `y`, not `z`; roster unchanged.
8. **`discover_skips_already_added`** — green: pre-add `x` to the roster; discover
   the same root → `out` no longer offers `x` (offers only `y`).
9. **`discover_add_persists`** — green: `discover(add=true)` on the x/y/z tree →
   config reloads with `x` and `y` (canonical), `out` says "added".
10. **`add_expands_tilde_and_canonicalizes`** (refactor) — green: with `$HOME` set to
    a tempdir holding a seeded repo `~/r`, `run_repos_add("~/r")` stores the
    canonical absolute path (config entry is absolute, starts with the temp home,
    contains no `~`).

`cargo test --test bd_integration` must still pass unchanged (no bd surface added).

## Manual smoke (recorded)

Ran the binary with `HOME` pointed at a scratch tempdir (so `Paths::resolve` and
`~` expansion both landed under the sandbox, real config untouched), over a
`dev/{x/.beads, y/.beads, z}` tree:

```
repos list            → roster is empty; add repos with `fbd repos add <path>`
repos add ~/dev/x     → added <canonical>/dev/x to the roster
repos add ~/dev/x     → already in the roster: <canonical>/dev/x   (no dup)
repos add dev/z       → error: not a beads repo: …/dev/z has no .beads directory —
                         run `bd init` there first   (exit 1)
repos discover dev    → found 1 beads repo(s) …: …/dev/y  + "re-run with --add"
                         (x already rostered → skipped)
repos discover dev --add → added 1 repo(s) …: …/dev/y
repos list            → roster (2 repos): x, y
repos remove ~/dev/x  → removed …/dev/x + "note: run `fbd reset` …"
repos remove dev/nope → not in the roster: …/dev/nope
```

`~` expanded and canonicalized to the sandbox home; duplicate add and
already-added discover both deduped; non-repo add rejected with the `bd init` hint
and a nonzero exit; remove printed the `fbd reset` hub hint. Sandbox removed after.

## Edge cases handled

- Aliased/`~`/relative duplicates collapse via canonical dedupe (add + discover).
- `remove` of an on-disk-absent repo still works (raw-path fallback match).
- `discover` root that doesn't exist is a real error; a roster *entry* that doesn't
  exist is not (warned elsewhere, at hub/doctor time).
- Control-char paths sanitized before hitting the terminal.
- `remove` emits the `fbd reset` hint for stale hub entries (`federated-beads-dxh.15`)
  without pruning the hub here.

## Out of scope (later slices / other beads)

- Hub pruning on `repos remove` (`federated-beads-dxh.15`) — only the advisory hint.
- `discover --depth N` (v2 idea per master plan); this slice is one level deep.
- TUI / bare-`fbd` launch (Slice 9); snapshot/doctor/reset (Slice 6, done).

## Verification

```
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
cargo test --test bd_integration
cargo run -- repos list        # manual smoke (scratch base)
```

## Autoreview outcomes (codex gpt-5.6-sol, branch vs main)

**Round 1 — accepted and fixed** (path normalization was inconsistent):

- **Stale-repo removal.** `store_path` returned a relative input unchanged when
  `canonicalize` failed, so `fbd repos remove ./repo` could not remove a deleted repo
  (stored canonical) by its original relative spelling. Fallback now canonicalizes the
  *parent* and rejoins the leaf (resolving the parent's symlinks the way the original
  store did), then `std::path::absolute`. Regression test
  `remove_after_repo_deleted_still_matches`.
- **Dedupe against hand-edited entries.** `add`/`discover` compared the new canonical
  path against raw stored entries, so a hand-edited `config.toml` with relative/aliased
  entries would duplicate on add and re-offer on discover. Both now normalize each
  stored entry through `store_path` before comparing (mirroring `ensure_hub`'s
  both-sides normalization). Regression tests `add_dedupes_against_relative_hand_edited_entry`,
  `discover_skips_relative_hand_edited_entry`.

**Round 2 — consciously rejected** (recorded on `federated-beads-dxh.8`):

- **Serialize roster read-modify-write.** A lost-update race exists if two
  `fbd repos add/remove/discover --add` run simultaneously. Rejected for v1: this is a
  human-driven personal roster CLI (not a server); `Config::save` is already atomic so
  no corruption is possible, and the worst case is a visibly-missing entry the user
  re-runs. Widening file locking to the roster mirrors the same speculative-concurrency
  call slice-6 deferred for the hub lock (`federated-beads-dxh.16`). Filed as
  `federated-beads-yvp`.
- **Remove by a since-dangling symlink alias.** A repo added *through* a symlink is
  stored under its target; once the link dangles, `remove` by the original link spelling
  can't resolve to the target. Genuinely unresolvable by path without the link. Instead
  of over-engineering, the `store_path`/`remove` docstrings now state the limitation and
  point at removing via the canonical path shown by `fbd repos list` (which always
  matches). No logic change.

# Slice 12 — Copy-context action + release polish (FINAL)

Bead: `federated-beads-dxh.13` (child of epic `federated-beads-dxh`).
Mol workflow root: `federated-beads-mol-02k`.
Master plan: `plans/fbd-v1-implementation-plan.md` (Slice 12 + global sections).
Depends on: Slices 0–11 + 9b (merged). Uses `app::{App, Msg, Effect, ViewMode,
DetailState, keys::map_key}`, `app::view::{draw, status_line}`,
`snapshot::{Row, attribute, UNKNOWN_REPO}`, `refresh::{PrefixMap,
attribution_map}`, `runtime::{execute_effect, spawn_*, gather_*}`,
`hub::hub_dir`, `bd::{BdClient, Issue}`, `cli::sanitize`.

## Goal

`y` copies an actionable `cd <repo> && bd show <id>` command for the selected
issue; `Y` copies a markdown block (title/id/repo/description). The copy uses an
**OSC 52** escape sequence written to the tty (works over ssh/tmux, zero
clipboard deps). The status bar confirms with `copied: …` (sensibly truncated).
Then release polish: README (install/quickstart/keybindings/architecture/v2),
`--help`/`--version` review, Cargo.toml metadata, and a manual acceptance
checklist recorded in the merge summary + a bead comment.

## Design decisions (recorded so autoreview doesn't re-litigate)

1. **Path resolution lives in the runtime worker, not `reduce`.** `App` holds
   only `Row`s (`issue` + `repo_name`); by deliberate design (`snapshot.rs`) a
   `Row` never carries a filesystem path, so `reduce` cannot build the `cd`
   string. `reduce` therefore emits `Effect::Copy { row, markdown }`; the runtime
   resolves the repo path from the issue id via `refresh::attribution_map`
   (the **same** prefix-map path search uses), builds the string, and sends the
   result back as `Msg::Copied`. This keeps `reduce` pure and free of path/`bd`
   types, exactly as detail/search already do.
2. **The OSC 52 write happens on the UI thread**, never the worker. A worker
   writing escape bytes to a stdout that ratatui also owns could interleave
   mid-sequence and corrupt the screen. So the worker only *computes* (id→path,
   string build — the slow, subprocess part, safely off the UI thread) and sends
   `Msg::Copied { payload, summary }`; `reduce(Copied)` stores `summary` for the
   status bar and returns `Effect::WriteClipboard(payload)`, which
   `execute_effect` performs by writing `osc52(payload)` to stdout on the UI
   thread. Two hops, but each side does only what it may safely do.
3. **OSC 52 sequence construction is a pure, tested function** in
   `app::context` (`osc52` + a dependency-free `base64_encode`), so the runtime
   adapter is the literal "write these bytes + flush" 5-liner the master plan
   asks for, and the wire format is unit-tested against known base64 vectors.
4. **Unattributed issue → hub fallback.** When the id matches no configured
   prefix (or a collided one), `repo_for` is `None` and the command becomes
   `bd -C <hub> show <id>` — always correct because the hub holds every issue.
   The `cd` form is a nicety on top of that guarantee.
5. **All bd-sourced fields copied are `sanitize`d** (id, title, description,
   repo_name). The clipboard content can be pasted into a terminal, and the OSC
   52 payload is what the terminal stores; control chars are neutralized exactly
   as `format_row`/the detail pane already do. (The base64 envelope already stops
   a title from breaking *out* of the escape; sanitizing protects the *paste
   target*.) Ids/paths are bd/roster-controlled and well-formed, so no shell
   quoting beyond that.
6. **`y`/`Y` only act while browsing.** `keys::map_key` routes any `Char` to
   `SearchInput` while the query editor is focused, so a typed `y`/`Y` never
   fires a copy. With no selected row (empty list, or the search editor) the copy
   is a no-op emitting no effect.
7. **The copy confirmation clears on the next refresh cycle** (`RefreshCompleted`
   sets `copy_flash = None`), so a stale "copied: …" never lingers past a manual
   `r`; otherwise it persists (and is replaced by the next copy), which is the
   expected transient-flash behavior.

## Files

- `src/app/context.rs` — **new** pure module: `shell_command`, `markdown_block`,
  `summarize`, `base64_encode`, `osc52`. `#[cfg(test)]` unit tests.
- `src/app/mod.rs` — `Msg::CopyMarkdown`, `Msg::Copied { payload, summary }`;
  `Effect::Copy { row: Box<Row>, markdown: bool }`, `Effect::WriteClipboard(String)`;
  `App.copy_flash: Option<String>` + `copy_flash()` accessor; `reduce` arms for
  `CopyContext`/`CopyMarkdown`/`Copied`; clear `copy_flash` in `RefreshCompleted`;
  `pub mod context;`.
- `src/app/keys.rs` — map `Char('Y')` → `Msg::CopyMarkdown` (unchanged `y`).
- `src/app/view.rs` — `status_line` shows `· copied: <summary>` when
  `copy_flash` is set (sanitized).
- `src/runtime.rs` — `execute_effect` arms for `Copy` (spawn `copy_worker`) and
  `WriteClipboard` (write on the UI thread, no handle); `spawn_copy`,
  `copy_worker`, `build_copy` (pure-ish, tested via the worker), `write_clipboard`.
- `README.md` — full rewrite for release (install, quickstart, keybindings,
  architecture, v2 notes, terminal/tmux clipboard caveat).
- `Cargo.toml` — `description`, `license`, `readme`, `repository`, `keywords`.

## TDD test list (red → green), in order

### 1. `src/app/context.rs` (pure)
- `builds_cd_command` — `shell_command(Some(Path::new("/Users/x/dev/megaclock")),
  hub, "mc-abc")` == `cd /Users/x/dev/megaclock && bd show mc-abc`.
- `unattributed_issue_falls_back_to_hub_show` — `shell_command(None, Path::new(
  "/hub"), "mc-abc")` == `bd -C /hub show mc-abc`.
- `builds_markdown_block` — contains the title, id, repo name, and description
  (assert substrings, non-brittle); a `None` description omits the description
  section but still renders title/id/repo.
- `sanitizes_control_chars` — a hostile title/id with `\x1b]52;…\x07\n` yields a
  command/markdown string free of raw ESC/BEL/newline.
- `base64_encode_matches_known_vectors` — `""`→`""`, `"f"`→`"Zg=="`,
  `"fo"`→`"Zm8="`, `"foo"`→`"Zm9v"`, `"foobar"`→`"Zm9vYmFy"` (RFC 4648 §10).
- `osc52_wraps_base64_payload` — `osc52("hi")` == `"\x1b]52;c;aGk=\x07"`.
- `summarize_truncates_first_line` — first line only, capped with `…` past the
  cap; a short single line is returned unchanged.

### 2. `src/app/keys.rs`
- extend `maps_command_keys`: `Char('Y')` (not editing) → `Msg::CopyMarkdown`;
  `Char('y')` still → `Msg::CopyContext`.
- extend `maps_search_input_keys`: `Char('Y')` while editing → `SearchInput('Y')`.

### 3. `src/app/mod.rs` (reduce)
- `copy_context_emits_effect` — List mode, a selection → one
  `Effect::Copy { row, markdown: false }` carrying the selected row.
- `copy_markdown_emits_effect` — `Msg::CopyMarkdown` → `markdown: true`.
- `copy_in_search_results_emits_effect` — in `Search`+`Results`, `y` copies the
  selected *result* row.
- `copy_in_detail_emits_effect` — in `Detail`, `y` copies the opened row.
- `copy_no_selection_noops` — empty list → `reduce(CopyContext) == vec![]`, no
  state change.
- `copied_sets_flash_and_writes` — `reduce(Copied { payload, summary })` returns
  `vec![Effect::WriteClipboard(payload)]` and `app.copy_flash() == Some(summary)`.
- `copy_flash_clears_on_refresh` — after `Copied`, a `RefreshCompleted` clears
  `copy_flash` to `None`.

### 4. `src/runtime.rs` (worker)
- `copy_worker_builds_cd_for_attributed` — seed a repo (prefix `ra`) in a
  tempdir, `copy_worker` for `row("ra","ra-1",…)` markdown=false → one
  `Msg::Copied` whose `payload` == `cd <repo> && bd show ra-1` and whose
  `summary` contains that command.
- `copy_worker_falls_back_to_hub_for_unattributed` — a row whose id matches no
  roster prefix → `payload` == `bd -C <hub> show <id>`.
- `copy_worker_markdown_block` — markdown=true → `payload` contains the title +
  repo + `ra-1`.

### 5. `src/app/view.rs` (render)
- `renders_copy_confirmation` — an app with `copy_flash` set (drive
  `reduce(Copied{…})`) renders `copied: …` in the status bar (row `H-1`).

## Edge cases covered
- Unattributed / collided-prefix id → hub fallback command (still runnable).
- Copy requested with no selection (empty ready list, or the search editor
  focused) → no-op, no effect, no flash.
- Copy in each of List / Search-results / Detail resolves the right issue.
- Hostile bd text (escape/newline injection) neutralized before it reaches the
  clipboard or the tty.
- `y`/`Y` typed into the search query are text, never a copy (keys routing).
- OSC 52 in tmux: documented caveat (needs `set -g set-clipboard on` /
  `allow-passthrough on`); fbd emits the standard sequence and does not wrap for
  tmux passthrough in v1.

## Out of scope (v2)
- Native clipboard backends; tmux passthrough wrapping; auto-detecting terminal
  clipboard support.
- Copying from the detail pane's *dependencies* (only the primary issue).
- A timed auto-dismiss of the copy confirmation (it clears on next refresh).

## Release polish
- README: install (`cargo install --path .`), quickstart (`fbd repos discover
  ~/dev --add && fbd`), full keybindings table (j/k/↑/↓, f, p, r, /, Enter, Esc,
  y, Y, q), architecture sketch, v2 notes (blocked view, writes, watcher,
  parallel exports), clipboard/tmux caveat.
- `--help`/`--version`: clap `version` is wired; review `about`/arg help; confirm
  `y`/`Y` reachable only in the TUI (no CLI subcommand needed).
- Cargo.toml: `description`, `license = "MIT OR Apache-2.0"`, `readme`,
  `repository`, `keywords`.

## Acceptance checklist (run for real; record in merge summary + bead comment)
1. Fresh-machine simulation: move the real config + data dir aside (RESTORE
   after), `fbd repos discover /Users/brian/dev --add` finds the beads repos,
   `fbd snapshot` shows a merged attributed list; restore originals.
2. Dead repo path: scratch config with a nonexistent path → snapshot degrades to
   a warning, still exits usefully.
3. `fbd reset` → next snapshot rebuilds the hub identically (row counts match).
4. `cargo test` green with `bd` off PATH (integration prints SKIP), then green
   with `bd` present.

## Verification commands (must all be green)
```bash
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
cargo test --test bd_integration
```

## Autoreview outcomes

Round 1 (codex, gpt-5.6-sol) — two findings, both accepted and fixed:

1. **[P1] Shell-quote the copied command's arguments.** `shell_command`
   interpolated repo/hub paths and the id verbatim; a valid repo path with a
   space breaks the pasted `cd`, and a shell metacharacter could execute. Added a
   conditional POSIX `shell_quote` (bare for a safe word, single-quoted with
   `'\''` escaping otherwise) applied to the repo path, hub path, and id; new
   `shell_quotes_paths_with_spaces` / `shell_quotes_metacharacters` tests.
2. **[P2] Drop stale copy results.** Copy workers carried no generation token, so
   a slower earlier copy could overwrite a later one's clipboard/confirmation.
   Added `copy_seq` + a `token` on `Effect::Copy`/`Msg::Copied` and a guard in
   `reduce(Copied)`, mirroring `detail_seq`/`search_seq`; new
   `stale_copy_result_dropped` test.

Round 2 (codex, gpt-5.6-sol) — two findings, both accepted and fixed:

3. **[P2] Copy the pinned detail issue.** In `Detail` mode `copy_effect` used the
   active *list* selection, which a refresh can re-clamp to a different row while
   the pane stays open (the opened issue was dropped). `y` then copied the wrong
   issue. Added `copy_source_row`: in `Detail` it resolves the row by the detail's
   id (falling back to the loaded detail's own issue if the row is gone), so the
   copy always matches what the pane shows.
4. **[P2] No copy of retained results while a search is loading.** Resubmitting a
   query enters `Loading` with the previous results still populated but invisible;
   `y` (not text there, since editing is false) copied a hidden result. Copying is
   now gated to a browsable list (ready or *settled* results) or the detail pane.

New tests: `copy_in_detail_uses_pinned_issue_after_refresh`,
`copy_noop_while_search_loading`.

Round 3 (codex, gpt-5.6-sol) — two findings, both accepted and fixed:

5. **[P2] Preserve line breaks in copied markdown descriptions.** `sanitize`
   replaced `\n`/`\t` with U+FFFD, mangling a multi-paragraph/list description
   into one line. `markdown_block` now sanitizes the description with a new
   `sanitize_multiline` (keeps `\n`/`\t`, normalizes `\r\n`→`\n`, still strips
   ESC/BEL/other controls); single-line fields keep plain `sanitize`.
6. **[P2] Retain repo attribution for a refreshed-away detail.** The round-2
   fallback fabricated `repo_name = "unknown"` when the pinned issue left the
   list. The pane now keeps the opened row (`detail_row`), so a copy from `Detail`
   uses that row's real attribution while taking the fetched detail's richer issue
   body. This also simplifies `copy_source_row` (no list re-scan).

New tests: `markdown_preserves_multiline_description`, and
`copy_in_detail_uses_pinned_issue_after_refresh` extended to assert attribution.

Round 4: clean (no accepted/actionable findings).

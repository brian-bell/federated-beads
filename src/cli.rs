//! The headless command runners behind fbd's clap CLI: `snapshot`, `doctor`,
//! and `reset`, plus the startup version gate and the shared row formatter.
//!
//! Every runner takes an injected `&impl BdClient`, `&Paths`, and explicit
//! `&mut impl Write` sinks — no process spawning, no XDG reads, no hidden clock —
//! so the whole surface is unit-tested against `FakeBdClient` and driven
//! end-to-end by the gated integration suite against the real `BdCli`. `main` is
//! the only caller that resolves real paths and wires stdout/stderr.

use std::io::Write;
use std::time::SystemTime;

use crate::bd::{BdClient, BdError, BdVersion};
use crate::config::{Config, Paths};
use crate::hub::{self, HubError, hub_dir};
use crate::refresh::{self, PrefixMap, RefreshError};
use crate::snapshot::{self, Row};

/// The minimum bd the version gate accepts.
const MIN_BD_VERSION: (u64, u64, u64) = (1, 1, 0);
/// The bd `schema_version` fbd's `--json` parsing is written against.
const REQUIRED_SCHEMA: i64 = 1;

/// A fatal command failure. `Ok(())` maps to exit 0 in `main`; any `Err` prints
/// `error: <e>` to stderr and exits nonzero.
#[derive(Debug, thiserror::Error)]
pub enum CliError {
    /// The startup version gate rejected the installed bd. The message is
    /// actionable and printed verbatim.
    #[error("{0}")]
    VersionGate(String),
    /// Hub lifecycle failed fatally (e.g. init).
    #[error(transparent)]
    Hub(#[from] HubError),
    /// Refresh failed fatally (sync/lock/io — `AlreadyRefreshing` is handled as a
    /// non-fatal degraded path inside `run_snapshot`, never surfaced here).
    #[error(transparent)]
    Refresh(#[from] RefreshError),
    /// A `bd` read (version/ready) failed fatally.
    #[error(transparent)]
    Bd(#[from] BdError),
    /// Writing to an output sink failed.
    #[error("writing output: {0}")]
    Io(#[from] std::io::Error),
}

/// Format one snapshot row for display: `[<repo>] P<priority> <id> <title>`.
///
/// Pure `Row → String`; Slice 9's ready-list view reuses it so the headless and
/// TUI renderings of a row never drift.
///
/// bd-sourced fields (repo name, id, title) are [`sanitize`]d: they are attacker-
/// influenceable data (an issue title in a federated repo you don't control), and
/// this string is written straight to a terminal. Left raw, a title carrying
/// newlines or ANSI/OSC escapes could forge extra rows or drive the terminal
/// (cursor moves, or an OSC 52 clipboard write). JSON output is unaffected —
/// serde escapes control characters on its own.
pub fn format_row(row: &Row) -> String {
    format!(
        "[{}] P{} {} {}",
        sanitize(&row.repo_name),
        row.issue.priority,
        sanitize(&row.issue.id),
        sanitize(&row.issue.title),
    )
}

/// Replace every control character (C0/C1, DEL, and the line breaks that would
/// let a value span rows) with the Unicode replacement character, so bd-sourced
/// text cannot inject terminal-control sequences into human-readable output.
fn sanitize(s: &str) -> String {
    s.chars()
        .map(|c| if c.is_control() { '\u{FFFD}' } else { c })
        .collect()
}

/// Accept a bd version iff `schema_version == 1` and `version >= 1.1.0`.
/// On rejection returns an actionable message naming both the requirement and
/// what was found.
pub fn version_gate(v: &BdVersion) -> Result<(), String> {
    let version_ok = parse_version(&v.version).is_some_and(|got| got >= MIN_BD_VERSION);
    let schema_ok = v.schema_version == REQUIRED_SCHEMA;
    if version_ok && schema_ok {
        return Ok(());
    }
    let (maj, min, pat) = MIN_BD_VERSION;
    Err(format!(
        "fbd requires bd >= {maj}.{min}.{pat} with schema_version {REQUIRED_SCHEMA}, \
         but found bd {} (schema_version {}). Upgrade bd \
         (https://github.com/gastownhall/beads).",
        v.version, v.schema_version,
    ))
}

/// Parse the leading `major.minor.patch` of a version string into a comparable
/// tuple, ignoring any `-pre`/`+build` suffix. Requires all three numeric
/// components — an incomplete or non-numeric version (`1.1`, `2`, `x`) yields
/// `None` and fails the gate closed, since the gate exists to refuse a bd whose
/// schema fbd cannot vouch for.
fn parse_version(s: &str) -> Option<(u64, u64, u64)> {
    let core = s.split(['-', '+']).next().unwrap_or(s);
    let mut parts = core.split('.');
    let major = parts.next()?.trim().parse().ok()?;
    let minor = parts.next()?.trim().parse().ok()?;
    let patch = parts.next()?.trim().parse().ok()?;
    Some((major, minor, patch))
}

/// `ensure_hub → refresh → fetch → print`. See `plans/slices/slice-6.md` for the
/// full control flow (version gate fatal; per-repo errors and `AlreadyRefreshing`
/// degrade with a warning; sync/hub/ready failures fatal).
pub fn run_snapshot(
    roster: &Config,
    bd: &impl BdClient,
    paths: &Paths,
    json: bool,
    out: &mut impl Write,
    err: &mut impl Write,
) -> Result<(), CliError> {
    // The gate protects the whole data-reading path: reject a bd whose schema
    // fbd's `--json` parsing was not written against, before touching the hub.
    version_gate(&bd.version()?).map_err(CliError::VersionGate)?;

    let status = hub::ensure_hub(bd, paths, roster)?;
    for warning in &status.warnings {
        // Warnings embed config/repo-derived text (paths, bd stderr, prefixes)
        // and go to a terminal, so sanitize them like rows and doctor output.
        writeln!(err, "warning: {}", sanitize(warning))?;
    }

    let hub = hub_dir(paths);
    let (prefix_map, fetched_at) = match refresh::run(bd, roster, paths) {
        Ok(outcome) => {
            // Per-repo failures and prefix collisions are surfaced but never fatal
            // — the hub still synced whatever exported cleanly.
            for repo_error in &outcome.errors {
                writeln!(err, "warning: {}", sanitize(&repo_error.to_string()))?;
            }
            for collision in outcome.prefix_map.collisions() {
                writeln!(
                    err,
                    "warning: id prefix `{}` is claimed by {} repos; its issues show as `{}`",
                    sanitize(&collision.prefix),
                    collision.repos.len(),
                    snapshot::UNKNOWN_REPO,
                )?;
            }
            (outcome.prefix_map, outcome.synced_at)
        }
        // Degraded, not fatal: another fbd holds the lock, so print the last
        // synced data (attribution unavailable → every row falls to `unknown`).
        Err(RefreshError::AlreadyRefreshing) => {
            writeln!(
                err,
                "warning: another fbd is refreshing this hub; showing the last synced data",
            )?;
            (PrefixMap::default(), SystemTime::now())
        }
        Err(fatal) => return Err(fatal.into()),
    };

    let snapshot = snapshot::fetch(bd, &hub, &prefix_map, fetched_at)?;

    if json {
        serde_json::to_writer_pretty(&mut *out, &snapshot)
            .map_err(|e| CliError::Io(std::io::Error::other(e)))?;
        writeln!(out)?;
    } else {
        for row in &snapshot.rows {
            writeln!(out, "{}", format_row(row))?;
        }
    }
    Ok(())
}

/// Report environment health: bd version + gate status, config/hub paths, and
/// per-repo roster existence + prefix. Deliberately **not** version-gated.
///
/// Doctor loads the roster itself (rather than being handed one) precisely so a
/// malformed config becomes a *reported* diagnostic instead of an error that
/// aborts the command before it can diagnose anything — the config being broken
/// is one of the things you run doctor to discover.
pub fn run_doctor(bd: &impl BdClient, paths: &Paths, out: &mut impl Write) -> Result<(), CliError> {
    // Doctor is the diagnostic you run *because* something is wrong, so it never
    // gates: it reports the version and whether the gate would pass, and tolerates
    // bd being absent entirely.
    match bd.version() {
        Ok(v) => {
            write!(
                out,
                "bd version: {} (schema {})",
                v.version, v.schema_version
            )?;
            match version_gate(&v) {
                Ok(()) => writeln!(out, "  gate: OK")?,
                Err(msg) => writeln!(out, "  gate: FAIL — {msg}")?,
            }
        }
        Err(e) => writeln!(out, "bd version: ERROR {e}")?,
    }

    let config_file = paths.config_file();
    writeln!(out, "config: {}", config_file.display())?;
    let hub = hub_dir(paths);
    let initialized = hub.join(".beads").join("embeddeddolt").is_dir();
    writeln!(
        out,
        "hub: {} ({})",
        hub.display(),
        if initialized {
            "initialized"
        } else {
            "not created yet"
        },
    )?;

    // Load the roster here so a missing or malformed config is a reported line,
    // not a failure that would defeat doctor's whole purpose.
    match load_roster(paths) {
        Ok(roster) => {
            writeln!(out, "roster ({} repos):", roster.repos.len())?;
            for entry in &roster.repos {
                // Both the path (from config) and the prefix (from repo metadata)
                // are repo/config-influenceable and go to a terminal, so apply the
                // same control-char sanitizer format_row uses.
                let shown = sanitize(&entry.path.display().to_string());
                if entry.path.exists() {
                    let prefix =
                        refresh::read_prefix(&entry.path).unwrap_or_else(|_| "?".to_string());
                    writeln!(out, "  {}  OK  [prefix: {}]", shown, sanitize(&prefix))?;
                } else {
                    writeln!(out, "  {}  MISSING", shown)?;
                }
            }
        }
        Err(e) => writeln!(out, "roster: ERROR reading {}: {e}", config_file.display())?,
    }
    Ok(())
}

/// Load the roster from `<config>/config.toml`, treating an absent file as an
/// empty roster (first run) while surfacing a present-but-invalid file as an
/// error. Shared by `main`'s snapshot path (where a bad config is fatal) and
/// `run_doctor` (where it is reported, not fatal).
pub fn load_roster(paths: &Paths) -> Result<Config, CliError> {
    let config_file = paths.config_file();
    // `Path::exists` collapses "absent" and "present but unreadable" (permission
    // error, dangling symlink) into the same `false`, which would silently
    // downgrade a broken config to an empty first-run roster. Distinguish with
    // `symlink_metadata`: only a genuine `NotFound` is first-run; anything else is
    // attempted so a real error surfaces per this function's contract.
    match std::fs::symlink_metadata(config_file) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Config::default()),
        _ => Config::load(config_file).map_err(|e| CliError::Io(std::io::Error::other(e))),
    }
}

/// Delete the hub dir (rebuilt on the next snapshot/launch) and report.
pub fn run_reset(paths: &Paths, out: &mut impl Write) -> Result<(), CliError> {
    let hub = hub_dir(paths);
    // Mirror `hub::reset`'s own `symlink_metadata` test rather than `Path::exists`:
    // exists() follows symlinks and reports false for a dangling hub symlink that
    // reset nonetheless removes, which would misreport it as "nothing to remove".
    let existed = std::fs::symlink_metadata(&hub).is_ok();
    hub::reset(paths)?;
    if existed {
        writeln!(out, "hub reset: removed {}", hub.display())?;
    } else {
        writeln!(out, "hub reset: nothing to remove ({})", hub.display())?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bd::{BdErrorKind, FakeBdClient, Issue};
    use crate::config::RepoEntry;
    use crate::refresh::HubLock;
    use std::fs;
    use std::path::{Path, PathBuf};

    fn version(v: &str, schema: i64) -> BdVersion {
        BdVersion {
            version: v.to_string(),
            schema_version: schema,
            build: None,
            commit: None,
            branch: None,
        }
    }

    fn issue(id: &str, priority: i64, title: &str) -> Issue {
        Issue {
            id: id.to_string(),
            title: title.to_string(),
            status: "open".into(),
            priority,
            description: None,
            issue_type: None,
            owner: None,
            created_at: None,
            created_by: None,
            updated_at: Some("2026-07-11T00:00:00Z".into()),
            dependency_count: None,
            dependent_count: None,
            comment_count: None,
        }
    }

    /// A repo dir under `base` with a seeded `.beads/metadata.json` prefix, so
    /// refresh/doctor can read a real prefix while bd itself stays faked.
    fn seed_repo(base: &Path, name: &str, prefix: &str) -> PathBuf {
        let repo = base.join(name);
        let beads = repo.join(".beads");
        fs::create_dir_all(&beads).unwrap();
        fs::write(
            beads.join("metadata.json"),
            format!(r#"{{"database":"dolt","dolt_database":"{prefix}"}}"#),
        )
        .unwrap();
        repo
    }

    fn roster(paths: &[&Path]) -> Config {
        Config {
            repos: paths
                .iter()
                .map(|p| RepoEntry {
                    path: p.to_path_buf(),
                })
                .collect(),
        }
    }

    fn row(repo_name: &str, id: &str, priority: i64, title: &str) -> Row {
        Row {
            issue: issue(id, priority, title),
            repo_name: repo_name.to_string(),
        }
    }

    #[test]
    fn format_row_matches_spec() {
        let r = row("ra", "ra-2hc", 1, "Ready task one");
        assert_eq!(format_row(&r), "[ra] P1 ra-2hc Ready task one");
    }

    #[test]
    fn format_row_neutralizes_terminal_control_chars() {
        // A hostile title: an OSC 52 clipboard-write escape plus a newline that
        // would otherwise forge a second row.
        let r = row("ra", "ra-2hc", 1, "pwn\u{1b}]52;c;aGk=\u{07}\nfake row");
        let line = format_row(&r);
        assert!(
            !line.contains('\u{1b}') && !line.contains('\n') && !line.contains('\u{07}'),
            "no raw control chars survive: {line:?}"
        );
        assert!(
            line.starts_with("[ra] P1 ra-2hc "),
            "the row prefix is intact: {line:?}"
        );
    }

    #[test]
    fn version_gate_accepts_supported() {
        assert!(version_gate(&version("1.1.0", 1)).is_ok());
        assert!(version_gate(&version("1.2.0", 1)).is_ok());
        assert!(version_gate(&version("2.0.0", 1)).is_ok());
    }

    #[test]
    fn version_gate_rejects_old_version() {
        let msg = version_gate(&version("1.0.0", 1)).expect_err("too old");
        assert!(msg.contains("1.1.0"), "names the requirement: {msg}");
        assert!(msg.contains("1.0.0"), "names what was found: {msg}");
    }

    #[test]
    fn version_gate_rejects_wrong_schema() {
        let msg = version_gate(&version("1.1.0", 2)).expect_err("wrong schema");
        assert!(
            msg.to_lowercase().contains("schema"),
            "message mentions schema: {msg}"
        );
    }

    #[test]
    fn version_gate_rejects_incomplete_version() {
        // Fail closed: an incomplete or non-numeric version must not be trusted
        // even though its numeric prefix would compare >= the minimum.
        for bad in ["1.1", "2", "1.x.0", "", "v1.1.0"] {
            assert!(
                version_gate(&version(bad, 1)).is_err(),
                "incomplete/non-numeric version {bad:?} must fail the gate"
            );
        }
    }

    #[test]
    fn snapshot_prints_rows() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = Paths::with_base(tmp.path());
        let ra = seed_repo(tmp.path(), "ra", "ra");
        let bd = FakeBdClient::new().with_ready(vec![issue("ra-2hc", 1, "Ready task one")]);
        let mut out = Vec::new();
        let mut err = Vec::new();

        run_snapshot(&roster(&[&ra]), &bd, &paths, false, &mut out, &mut err).expect("ok");

        let stdout = String::from_utf8(out).unwrap();
        assert!(
            stdout.contains("[ra] P1 ra-2hc Ready task one"),
            "human row present: {stdout:?}"
        );
    }

    #[test]
    fn snapshot_json_emits_snapshot() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = Paths::with_base(tmp.path());
        let ra = seed_repo(tmp.path(), "ra", "ra");
        let bd = FakeBdClient::new().with_ready(vec![issue("ra-2hc", 1, "Ready task one")]);
        let mut out = Vec::new();
        let mut err = Vec::new();

        run_snapshot(&roster(&[&ra]), &bd, &paths, true, &mut out, &mut err).expect("ok");

        // stdout must be clean JSON (no warning leaked in).
        let v: serde_json::Value =
            serde_json::from_slice(&out).expect("stdout parses as JSON snapshot");
        assert_eq!(
            v["rows"][0]["issue"]["id"].as_str(),
            Some("ra-2hc"),
            "serialized snapshot exposes the row's issue id: {v}"
        );
        assert!(v.get("fetched_at").is_some(), "fetch time serialized: {v}");
    }

    #[test]
    fn snapshot_surfaces_per_repo_warnings_without_aborting() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = Paths::with_base(tmp.path());
        let ra = seed_repo(tmp.path(), "ra", "ra");
        let rb = seed_repo(tmp.path(), "rb", "rb");
        let bd = FakeBdClient::new()
            .with_ready(vec![issue("ra-2hc", 1, "Ready task one")])
            .with_export_err(
                rb.clone(),
                BdError {
                    command: "bd export".into(),
                    stderr: "disk full".into(),
                    kind: BdErrorKind::NonZeroExit { code: Some(1) },
                },
            );
        let mut out = Vec::new();
        let mut err = Vec::new();

        run_snapshot(&roster(&[&ra, &rb]), &bd, &paths, false, &mut out, &mut err).expect("ok");

        let stdout = String::from_utf8(out).unwrap();
        let stderr = String::from_utf8(err).unwrap();
        assert!(
            stdout.contains("ra-2hc"),
            "healthy rows still printed: {stdout:?}"
        );
        assert!(
            stderr.contains("rb"),
            "the failed repo is warned about on err: {stderr:?}"
        );
    }

    #[test]
    fn snapshot_already_refreshing_degrades() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = Paths::with_base(tmp.path());
        let ra = seed_repo(tmp.path(), "ra", "ra");
        // Pre-hold the hub lock so refresh declines.
        let hub = hub_dir(&paths);
        fs::create_dir_all(&hub).unwrap();
        let _held = HubLock::try_acquire(&hub).unwrap().expect("acquired lock");

        let bd = FakeBdClient::new().with_ready(vec![issue("ra-2hc", 1, "Ready task one")]);
        let mut out = Vec::new();
        let mut err = Vec::new();

        run_snapshot(&roster(&[&ra]), &bd, &paths, false, &mut out, &mut err)
            .expect("degrades, ok");

        let stdout = String::from_utf8(out).unwrap();
        let stderr = String::from_utf8(err).unwrap();
        assert!(
            stdout.contains("ra-2hc"),
            "stale rows still printed: {stdout:?}"
        );
        assert!(
            stderr.to_lowercase().contains("refreshing"),
            "AlreadyRefreshing warned on err: {stderr:?}"
        );
    }

    #[test]
    fn snapshot_version_gate_failure_is_fatal() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = Paths::with_base(tmp.path());
        let ra = seed_repo(tmp.path(), "ra", "ra");
        let bd = FakeBdClient::new().with_version(version("1.0.0", 1));
        let mut out = Vec::new();
        let mut err = Vec::new();

        let e = run_snapshot(&roster(&[&ra]), &bd, &paths, false, &mut out, &mut err)
            .expect_err("gate is fatal");
        assert!(matches!(e, CliError::VersionGate(_)), "got {e:?}");
        assert!(out.is_empty(), "no snapshot printed when the gate fails");
    }

    #[test]
    fn snapshot_sync_failure_is_fatal() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = Paths::with_base(tmp.path());
        let ra = seed_repo(tmp.path(), "ra", "ra");
        let bd = FakeBdClient::new().with_repo_sync_err(BdError {
            command: "bd repo sync".into(),
            stderr: "boom".into(),
            kind: BdErrorKind::NonZeroExit { code: Some(1) },
        });
        let mut out = Vec::new();
        let mut err = Vec::new();

        let e = run_snapshot(&roster(&[&ra]), &bd, &paths, false, &mut out, &mut err)
            .expect_err("sync failure is fatal");
        assert!(
            matches!(e, CliError::Refresh(RefreshError::Sync(_))),
            "got {e:?}"
        );
    }

    #[test]
    #[allow(non_snake_case)] // name mirrors the `MISSING` marker under test
    fn doctor_lists_missing_repo_as_MISSING() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = Paths::with_base(tmp.path());
        let ra = seed_repo(tmp.path(), "ra", "ra");
        let gone = tmp.path().join("gone");
        // Doctor loads the roster from the config file itself, so persist one.
        roster(&[&ra, &gone]).save(paths.config_file()).unwrap();
        let bd = FakeBdClient::new(); // default version 1.1.0 / schema 1
        let mut out = Vec::new();

        run_doctor(&bd, &paths, &mut out).expect("ok");

        let stdout = String::from_utf8(out).unwrap();
        assert!(stdout.contains("1.1.0"), "bd version reported: {stdout}");
        assert!(
            stdout.contains(&paths.config_file().display().to_string()),
            "config path reported: {stdout}"
        );
        assert!(
            stdout.contains(&hub_dir(&paths).display().to_string()),
            "hub path reported: {stdout}"
        );
        assert!(
            stdout.contains("ra"),
            "existing repo prefix reported: {stdout}"
        );
        assert!(
            stdout.contains("MISSING"),
            "absent repo flagged MISSING: {stdout}"
        );
    }

    #[test]
    fn doctor_reports_gate_status() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = Paths::with_base(tmp.path());
        let ok = FakeBdClient::new().with_version(version("1.1.0", 1));
        let bad = FakeBdClient::new().with_version(version("1.0.0", 1));

        // No config file present: doctor loads an empty roster and still runs.
        let mut out_ok = Vec::new();
        run_doctor(&ok, &paths, &mut out_ok).expect("ok");
        assert!(
            String::from_utf8(out_ok).unwrap().contains("gate: OK"),
            "supported bd shows gate: OK"
        );

        let mut out_bad = Vec::new();
        run_doctor(&bad, &paths, &mut out_bad).expect("ok");
        assert!(
            String::from_utf8(out_bad).unwrap().contains("gate: FAIL"),
            "old bd shows gate: FAIL"
        );
    }

    #[test]
    fn doctor_reports_malformed_config_without_failing() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = Paths::with_base(tmp.path());
        // A present-but-invalid config must be reported, not abort doctor.
        let config_file = paths.config_file();
        fs::create_dir_all(config_file.parent().unwrap()).unwrap();
        fs::write(config_file, "this is not = valid toml [[[").unwrap();
        let bd = FakeBdClient::new();
        let mut out = Vec::new();

        run_doctor(&bd, &paths, &mut out).expect("doctor still succeeds");

        let stdout = String::from_utf8(out).unwrap();
        assert!(stdout.contains("1.1.0"), "version still reported: {stdout}");
        assert!(
            stdout.contains("roster: ERROR"),
            "malformed config surfaced as a reported line: {stdout}"
        );
    }

    #[test]
    fn load_roster_absent_is_empty_but_invalid_errors() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = Paths::with_base(tmp.path());
        // No config file: a genuine first run yields an empty roster.
        assert_eq!(load_roster(&paths).unwrap(), Config::default());

        // Present but malformed: surfaced as an error, never silently empty.
        let config_file = paths.config_file();
        fs::create_dir_all(config_file.parent().unwrap()).unwrap();
        fs::write(config_file, "not = [valid").unwrap();
        assert!(load_roster(&paths).is_err(), "invalid config must error");
    }

    #[cfg(unix)]
    #[test]
    fn load_roster_dangling_symlink_errors_not_empty() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = Paths::with_base(tmp.path());
        let config_file = paths.config_file();
        fs::create_dir_all(config_file.parent().unwrap()).unwrap();
        // A config symlink to a nonexistent target: `Path::exists()` is false, but
        // it is not a first run — the misconfiguration must surface, not be masked.
        std::os::unix::fs::symlink(tmp.path().join("nowhere.toml"), config_file).unwrap();
        assert!(!config_file.exists(), "precondition: dangling symlink");

        assert!(
            load_roster(&paths).is_err(),
            "a dangling config symlink must error, not read as empty"
        );
    }

    #[test]
    fn reset_removes_hub_and_reports() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = Paths::with_base(tmp.path());
        let hub = hub_dir(&paths);
        fs::create_dir_all(hub.join(".beads")).unwrap();
        fs::write(hub.join(".beads").join("marker"), "x").unwrap();
        let mut out = Vec::new();

        run_reset(&paths, &mut out).expect("ok");

        assert!(!hub.exists(), "hub dir removed");
        let stdout = String::from_utf8(out).unwrap();
        assert!(
            stdout.contains(&hub.display().to_string()),
            "reset reports the removed path: {stdout}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn reset_reports_removed_for_dangling_hub_symlink() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = Paths::with_base(tmp.path());
        let hub = hub_dir(&paths);
        fs::create_dir_all(hub.parent().unwrap()).unwrap();
        // A dangling symlink: `exists()` is false, but reset removes it, so the
        // report must say "removed", not "nothing to remove".
        std::os::unix::fs::symlink(tmp.path().join("nowhere"), &hub).unwrap();
        let mut out = Vec::new();

        run_reset(&paths, &mut out).expect("ok");

        let stdout = String::from_utf8(out).unwrap();
        assert!(
            stdout.contains("removed") && !stdout.contains("nothing to remove"),
            "a removed dangling symlink is reported as removed: {stdout}"
        );
        assert!(
            fs::symlink_metadata(&hub).is_err(),
            "the dangling symlink was actually removed"
        );
    }

    #[test]
    fn snapshot_warnings_are_sanitized() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = Paths::with_base(tmp.path());
        // A roster path carrying terminal-control bytes: it does not exist, so
        // ensure_hub emits a "does not exist" warning that echoes the raw path.
        let hostile = tmp.path().join("evil\u{1b}]52;c;x\u{07}\nforged");
        let bd = FakeBdClient::new().with_ready(vec![issue("ra-2hc", 1, "t")]);
        let mut out = Vec::new();
        let mut err = Vec::new();

        run_snapshot(&roster(&[&hostile]), &bd, &paths, false, &mut out, &mut err).expect("ok");

        let stderr = String::from_utf8(err).unwrap();
        assert!(
            !stderr.contains('\u{1b}') && !stderr.contains('\u{07}'),
            "warning output carries no raw terminal-control bytes: {stderr:?}"
        );
    }
}

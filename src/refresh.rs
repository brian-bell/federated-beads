//! Refresh pipeline: export every roster repo, sync the hub once, and build a
//! prefix→repo attribution map — collecting per-repo failures instead of
//! aborting on the first bad repo.
//!
//! A process-level advisory lock on `<hub>/.fbd.lock` serializes refreshes
//! across concurrent fbd instances so two cannot run `repo sync` against the
//! same embedded-Dolt hub at once. See `plans/slices/slice-4.md`.

use std::collections::{HashMap, HashSet};
use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, ErrorKind};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use fs2::FileExt;
use serde::Deserialize;

use crate::bd::{BdClient, BdError};
use crate::config::{Config, Paths, RepoEntry};
use crate::hub::hub_dir;

/// Advisory lock file, inside the hub dir.
const LOCK_FILE: &str = ".fbd.lock";

/// A completed refresh. Individual repos may still appear in `errors`; a
/// completed refresh with per-repo errors is still a success (the hub was
/// synced from whatever exported cleanly).
#[derive(Debug)]
pub struct RefreshOutcome {
    /// Id-prefix → source repo attribution built from each repo's metadata.
    pub prefix_map: PrefixMap,
    /// Per-repo operational failures surfaced but not fatal.
    pub errors: Vec<RepoError>,
    /// Wall-clock time the hub sync completed.
    pub synced_at: SystemTime,
}

/// A per-repo failure during refresh: surfaced to the user but never aborts the
/// whole refresh (other repos still export and the hub still syncs).
#[derive(Debug, Clone, thiserror::Error)]
pub enum RepoError {
    /// This repo's `bd export` failed; the hub still synced without its latest
    /// data, and other repos still hydrate.
    #[error("export failed for {repo}: {source}")]
    Export { repo: PathBuf, source: BdError },
    /// This repo's `.beads/metadata.json` prefix could not be read, so its
    /// issues cannot be attributed.
    #[error("cannot read prefix for {repo}: {detail}")]
    Metadata { repo: PathBuf, detail: String },
}

/// A fatal refresh failure, or a declined refresh.
#[derive(Debug, thiserror::Error)]
pub enum RefreshError {
    /// Another fbd instance holds the hub lock; this refresh declined to run and
    /// performed no exports or sync. The caller retries on the next refresh.
    #[error("another fbd instance is refreshing this hub")]
    AlreadyRefreshing,
    /// The single `bd repo sync` failed, so the hub was not updated at all.
    #[error("hub sync failed: {0}")]
    Sync(#[source] BdError),
    /// A lock-file IO error (open or `flock`).
    #[error("hub lock error at {path}: {source}")]
    Lock {
        path: PathBuf,
        source: std::io::Error,
    },
    /// Preparing the hub directory failed.
    #[error("preparing hub dir {path}: {source}")]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
}

/// Two or more roster repos declared the same id prefix. Ids under a collided
/// prefix are ambiguous and resolve to `None` (the "unknown" bucket).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Collision {
    pub prefix: String,
    pub repos: Vec<PathBuf>,
}

/// Maps an issue id to its source repo by longest configured prefix followed by
/// `-`. A prefix claimed by more than one repo resolves to `None` (ambiguous)
/// but stays in the lookup table so it can still win the longest-match contest —
/// a shorter unique prefix must never mask a longer, collided one.
#[derive(Debug, Default, Clone)]
pub struct PrefixMap {
    /// Every configured prefix → its resolution: `Some(repo)` when a single repo
    /// claims it, `None` when it collided. Lookup scans for the longest match.
    entries: Vec<(String, Option<RepoEntry>)>,
    /// Prefixes claimed by more than one repo, for surfacing to the user.
    collisions: Vec<Collision>,
}

impl PrefixMap {
    /// Build the map from `(prefix, repo)` pairs. A prefix claimed by more than
    /// one repo becomes a [`Collision`] and a `None` entry; a unique prefix maps
    /// to its repo. First-seen order is preserved for deterministic reporting.
    ///
    /// Public so consumers (e.g. `snapshot`'s tests) can construct a populated
    /// map without running a whole refresh; `run` builds it from the prefixes it
    /// reads from each repo's metadata.
    pub fn from_pairs(pairs: Vec<(String, RepoEntry)>) -> PrefixMap {
        let mut order: Vec<String> = Vec::new();
        let mut grouped: HashMap<String, Vec<RepoEntry>> = HashMap::new();
        for (prefix, repo) in pairs {
            if !grouped.contains_key(&prefix) {
                order.push(prefix.clone());
            }
            grouped.entry(prefix).or_default().push(repo);
        }

        let mut entries = Vec::new();
        let mut collisions = Vec::new();
        for prefix in order {
            let mut repos = grouped.remove(&prefix).expect("prefix was inserted");
            if repos.len() == 1 {
                entries.push((prefix, Some(repos.pop().expect("length checked to be 1"))));
            } else {
                collisions.push(Collision {
                    prefix: prefix.clone(),
                    repos: repos.into_iter().map(|r| r.path).collect(),
                });
                // Keep the collided prefix in the lookup table (as `None`) so it
                // still participates in longest-match; otherwise a shorter unique
                // prefix could wrongly claim an id under the longer collided one.
                entries.push((prefix, None));
            }
        }
        PrefixMap {
            entries,
            collisions,
        }
    }

    /// The repo whose configured prefix, followed by `-`, is the longest prefix
    /// of `id`. `None` when nothing matches, or when the longest matching prefix
    /// is a collided (ambiguous) one.
    pub fn repo_for(&self, id: &str) -> Option<&RepoEntry> {
        self.attribution(id).map(|(_, repo)| repo)
    }

    /// Like [`repo_for`](Self::repo_for) but also yields the matched prefix. The
    /// prefix is a unique, short, non-sensitive repo identity (a collided prefix
    /// resolves to `None` here), useful to disambiguate repos that share a
    /// directory basename without exposing a filesystem path.
    pub fn attribution(&self, id: &str) -> Option<(&str, &RepoEntry)> {
        let (prefix, repo) = self
            .entries
            .iter()
            .filter(|(prefix, _)| {
                id.strip_prefix(prefix.as_str())
                    .is_some_and(|rest| rest.starts_with('-'))
            })
            .max_by_key(|(prefix, _)| prefix.len())?;
        repo.as_ref().map(|repo| (prefix.as_str(), repo))
    }

    /// Prefixes claimed by more than one roster repo.
    pub fn collisions(&self) -> &[Collision] {
        &self.collisions
    }
}

/// An acquired advisory lock on `<hub>/.fbd.lock`. The OS lock is released when
/// the held `File` drops (closing the fd releases the `flock`).
#[derive(Debug)]
pub struct HubLock {
    _file: File,
}

impl HubLock {
    /// Try to acquire the hub lock without blocking: `Ok(Some(lock))` on
    /// success, `Ok(None)` when another holder has it, `Err` on a real IO error.
    pub fn try_acquire(hub: &Path) -> Result<Option<HubLock>, RefreshError> {
        let path = hub.join(LOCK_FILE);
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(&path)
            .map_err(|source| RefreshError::Lock {
                path: path.clone(),
                source,
            })?;
        match file.try_lock_exclusive() {
            Ok(()) => Ok(Some(HubLock { _file: file })),
            // Contended: another holder (possibly this process via a separate
            // open) has the exclusive lock. Decline rather than block.
            Err(e) if e.kind() == ErrorKind::WouldBlock => Ok(None),
            Err(source) => Err(RefreshError::Lock { path, source }),
        }
    }
}

/// Run one refresh: export every roster repo (sequentially), sync the hub once,
/// and build the prefix map. Declines with [`RefreshError::AlreadyRefreshing`]
/// if another instance holds the hub lock.
pub fn run(
    bd: &impl BdClient,
    roster: &Config,
    paths: &Paths,
) -> Result<RefreshOutcome, RefreshError> {
    let hub = hub_dir(paths);
    // ensure_hub normally created this already; create defensively so the lock
    // file below always has a directory to live in.
    fs::create_dir_all(&hub).map_err(|source| RefreshError::Io {
        path: hub.clone(),
        source,
    })?;

    // Hold the lock across the whole refresh; it releases when `_lock` drops.
    let _lock = match HubLock::try_acquire(&hub)? {
        Some(lock) => lock,
        None => return Err(RefreshError::AlreadyRefreshing),
    };

    let mut errors = Vec::new();
    let mut pairs: Vec<(String, RepoEntry)> = Vec::new();
    // Canonical paths already handled, so an aliased/duplicate roster entry is
    // exported once and never mistaken for a prefix collision with itself
    // (mirrors ensure_hub's roster dedupe).
    let mut seen: HashSet<PathBuf> = HashSet::new();

    for entry in &roster.repos {
        if !seen.insert(normalize(&entry.path)) {
            continue;
        }
        // Export refreshes the repo's passive JSONL. A failure is recorded but
        // never aborts the run — the hub still syncs and other repos hydrate.
        if entry.path.exists()
            && let Err(source) = bd.export(&entry.path)
        {
            errors.push(RepoError::Export {
                repo: entry.path.clone(),
                source,
            });
        }
        // Attribution needs the prefix regardless of export success (already-
        // synced ids stay attributable even if this refresh's export failed).
        match read_prefix(&entry.path) {
            Ok(prefix) => pairs.push((prefix, entry.clone())),
            Err(detail) => errors.push(RepoError::Metadata {
                repo: entry.path.clone(),
                detail,
            }),
        }
    }

    // One sync hydrates the hub from every repo's fresh export. A sync failure
    // is fatal: the hub was not updated, so the whole refresh failed.
    bd.repo_sync(&hub).map_err(RefreshError::Sync)?;

    Ok(RefreshOutcome {
        prefix_map: PrefixMap::from_pairs(pairs),
        errors,
        synced_at: SystemTime::now(),
    })
}

/// The subset of `<repo>/.beads/metadata.json` fbd reads: the id prefix, stored
/// under `dolt_database`. Tolerant (no `deny_unknown_fields`) — bd writes other
/// keys fbd ignores.
#[derive(Debug, Deserialize)]
struct Metadata {
    dolt_database: String,
}

/// Canonicalize `p` if it exists on disk; otherwise return it unchanged. Used to
/// dedupe roster entries that name the same repo via different (aliased) paths.
fn normalize(p: &Path) -> PathBuf {
    fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf())
}

/// One line of a repo's exported `issues.jsonl`. Tolerant: only the fields we
/// need, everything else ignored.
#[derive(Debug, Deserialize)]
struct IssueLine {
    #[serde(rename = "_type")]
    kind: Option<String>,
    id: Option<String>,
}

/// Read a repo's id prefix — the leading segment every one of its issue ids
/// carries (e.g. `reading-lite` for `reading-lite-hck.1`).
///
/// The prefix is derived from the repo's own exported ids, which are the ground
/// truth, and validated against `<repo>/.beads/metadata.json`'s `dolt_database`.
/// bd keeps `-` in ids but sanitizes `-`→`_` for the Dolt database name (Dolt
/// disallows hyphens), so `dolt_database` alone is a lossy, hyphen-erased form
/// that misattributes hyphenated repos. The exact, verified invariant is
/// `id_prefix.replace('-', "_") == dolt_database`, so we take the first exported
/// id whose derived prefix satisfies it — guaranteeing we return this repo's own
/// prefix and never a foreign hydrated id's. When no id validates (an empty repo
/// or a missing `issues.jsonl`), fall back to `dolt_database`: such a repo has no
/// ids of its own in the hub, so its prefix is never queried anyway.
///
/// `metadata.json` remains required (a missing or unparseable one is an error),
/// preserving the attribution contract.
///
/// Public so `doctor` can report each roster repo's prefix without re-running a
/// whole refresh; `run` uses it to build the attribution map.
pub fn read_prefix(repo: &Path) -> Result<String, String> {
    let path = repo.join(".beads").join("metadata.json");
    let text = fs::read_to_string(&path).map_err(|e| format!("reading {}: {e}", path.display()))?;
    let meta: Metadata =
        serde_json::from_str(&text).map_err(|e| format!("parsing {}: {e}", path.display()))?;
    Ok(derive_prefix_from_ids(repo, &meta.dolt_database).unwrap_or(meta.dolt_database))
}

/// Scan `<repo>/.beads/issues.jsonl` for the first issue id whose derived prefix
/// matches `dolt_database` under bd's `-`→`_` sanitization, returning that
/// (hyphen-preserving) prefix. `None` when the file is absent/unreadable or no id
/// validates — the caller then falls back to `dolt_database`.
fn derive_prefix_from_ids(repo: &Path, dolt_database: &str) -> Option<String> {
    let file = File::open(repo.join(".beads").join("issues.jsonl")).ok()?;
    for line in BufReader::new(file).lines() {
        let line = line.ok()?;
        if line.trim().is_empty() {
            continue;
        }
        let Ok(record) = serde_json::from_str::<IssueLine>(&line) else {
            continue;
        };
        if record.kind.as_deref() != Some("issue") {
            continue;
        }
        // Ids are `<prefix>-<hash>`; the prefix may itself contain `-`, so split
        // on the last one. Accept only this repo's own prefix (validated below).
        if let Some((prefix, _)) = record.id.as_deref().and_then(|id| id.rsplit_once('-'))
            && prefix.replace('-', "_") == dolt_database
        {
            return Some(prefix.to_string());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bd::{BdErrorKind, Call, FakeBdClient};
    use crate::config::RepoEntry;

    /// A repo dir under `base` with a seeded `.beads/metadata.json` prefix.
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

    /// Like [`seed_repo`] but also writes a `.beads/issues.jsonl` with one
    /// `_type":"issue"` record per id — mirroring what fbd's real export writes,
    /// so the id-derived prefix path is exercised. `dolt_database` is the
    /// sanitized DB name bd stores; `ids` carry the real (possibly hyphenated)
    /// prefix.
    fn seed_repo_with_ids(base: &Path, name: &str, dolt_database: &str, ids: &[&str]) -> PathBuf {
        let repo = seed_repo(base, name, dolt_database);
        let lines: String = ids
            .iter()
            .map(|id| format!(r#"{{"_type":"issue","id":"{id}","title":"t"}}"#))
            .collect::<Vec<_>>()
            .join("\n");
        fs::write(repo.join(".beads").join("issues.jsonl"), lines).unwrap();
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

    fn bd_err() -> BdError {
        BdError {
            command: "bd ...".into(),
            stderr: "boom".into(),
            kind: BdErrorKind::NonZeroExit { code: Some(1) },
        }
    }

    #[test]
    fn exports_all_then_syncs_once() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = Paths::with_base(tmp.path());
        let a = seed_repo(tmp.path(), "a", "ra");
        let b = seed_repo(tmp.path(), "b", "rb");
        let fake = FakeBdClient::new();

        run(&fake, &roster(&[&a, &b]), &paths).unwrap();

        let calls = fake.calls();
        assert_eq!(
            calls,
            vec![
                Call::Export(a.clone()),
                Call::Export(b.clone()),
                Call::RepoSync(hub_dir(&paths)),
            ],
            "exports run in order, then exactly one sync"
        );
    }

    #[test]
    fn collects_per_repo_errors() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = Paths::with_base(tmp.path());
        let a = seed_repo(tmp.path(), "a", "ra");
        let b = seed_repo(tmp.path(), "b", "rb");
        let fake = FakeBdClient::new().with_export_err(b.clone(), bd_err());

        let outcome = run(&fake, &roster(&[&a, &b]), &paths).unwrap();

        assert!(
            outcome
                .errors
                .iter()
                .any(|e| matches!(e, RepoError::Export { repo, .. } if repo == &b)),
            "b's export failure is recorded: {:?}",
            outcome.errors
        );
        assert!(
            fake.calls().iter().any(|c| matches!(c, Call::RepoSync(_))),
            "sync still runs despite a per-repo export failure"
        );
        assert!(
            outcome.prefix_map.repo_for("ra-2hc").is_some(),
            "the healthy repo still hydrates and is attributed"
        );
    }

    #[test]
    fn reads_prefix_from_metadata() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = Paths::with_base(tmp.path());
        let a = seed_repo(tmp.path(), "a", "ra");
        let fake = FakeBdClient::new();

        let outcome = run(&fake, &roster(&[&a]), &paths).unwrap();

        assert_eq!(
            outcome.prefix_map.repo_for("ra-2hc").map(|r| &r.path),
            Some(&a),
            "prefix comes from metadata.json dolt_database"
        );
    }

    #[test]
    fn builds_prefix_map() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = Paths::with_base(tmp.path());
        let a = seed_repo(tmp.path(), "a", "ra");
        let b = seed_repo(tmp.path(), "b", "rb");
        let fake = FakeBdClient::new();

        let outcome = run(&fake, &roster(&[&a, &b]), &paths).unwrap();
        let map = outcome.prefix_map;

        assert_eq!(map.repo_for("ra-2hc").map(|r| &r.path), Some(&a));
        assert_eq!(map.repo_for("rb-9zz").map(|r| &r.path), Some(&b));
        assert!(map.repo_for("zz-1").is_none(), "unknown prefix -> None");
    }

    #[test]
    fn flags_prefix_collisions() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = Paths::with_base(tmp.path());
        let a = seed_repo(tmp.path(), "a", "dup");
        let b = seed_repo(tmp.path(), "b", "dup");
        let fake = FakeBdClient::new();

        let outcome = run(&fake, &roster(&[&a, &b]), &paths).unwrap();

        let collisions = outcome.prefix_map.collisions();
        assert_eq!(collisions.len(), 1, "one collided prefix");
        assert_eq!(collisions[0].prefix, "dup");
        assert!(collisions[0].repos.contains(&a) && collisions[0].repos.contains(&b));
        assert!(
            outcome.prefix_map.repo_for("dup-x").is_none(),
            "a collided prefix is ambiguous -> None"
        );
    }

    #[test]
    fn longest_prefix_wins() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = Paths::with_base(tmp.path());
        let a = seed_repo(tmp.path(), "a", "app");
        let b = seed_repo(tmp.path(), "b", "app2");
        let fake = FakeBdClient::new();

        let outcome = run(&fake, &roster(&[&a, &b]), &paths).unwrap();
        let map = outcome.prefix_map;

        assert_eq!(
            map.repo_for("app2-xyz").map(|r| &r.path),
            Some(&b),
            "app2-xyz must attribute to app2, never app"
        );
        assert_eq!(map.repo_for("app-xyz").map(|r| &r.path), Some(&a));
    }

    #[test]
    fn metadata_read_failure_is_a_repo_error() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = Paths::with_base(tmp.path());
        // `bad` exists (so export is attempted) but has no metadata.json.
        let bad = tmp.path().join("bad");
        fs::create_dir_all(&bad).unwrap();
        let good = seed_repo(tmp.path(), "good", "rg");
        let fake = FakeBdClient::new();

        let outcome = run(&fake, &roster(&[&bad, &good]), &paths).unwrap();

        assert!(
            outcome
                .errors
                .iter()
                .any(|e| matches!(e, RepoError::Metadata { repo, .. } if repo == &bad)),
            "unreadable metadata -> RepoError::Metadata: {:?}",
            outcome.errors
        );
        assert!(
            outcome.prefix_map.repo_for("rg-1").is_some(),
            "the readable repo is still attributed"
        );
    }

    #[test]
    fn sync_failure_is_fatal() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = Paths::with_base(tmp.path());
        let a = seed_repo(tmp.path(), "a", "ra");
        let fake = FakeBdClient::new().with_repo_sync_err(bd_err());

        let err = run(&fake, &roster(&[&a]), &paths).unwrap_err();

        assert!(matches!(err, RefreshError::Sync(_)), "got {err:?}");
    }

    #[test]
    fn collided_longer_prefix_is_not_masked_by_shorter() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = Paths::with_base(tmp.path());
        // A unique short prefix `app`, plus two repos both claiming `app-foo`.
        let a = seed_repo(tmp.path(), "a", "app");
        let b = seed_repo(tmp.path(), "b", "app-foo");
        let c = seed_repo(tmp.path(), "c", "app-foo");
        let fake = FakeBdClient::new();

        let outcome = run(&fake, &roster(&[&a, &b, &c]), &paths).unwrap();
        let map = outcome.prefix_map;

        // The longest match for `app-foo-123` is the collided `app-foo`, so it is
        // ambiguous — the shorter unique `app` must not claim it.
        assert!(
            map.repo_for("app-foo-123").is_none(),
            "a collided longer prefix must not fall through to a shorter one"
        );
        // The unique `app` still resolves ids that only it matches.
        assert_eq!(map.repo_for("app-xyz").map(|r| &r.path), Some(&a));
    }

    #[test]
    fn duplicate_roster_entry_is_not_a_collision() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = Paths::with_base(tmp.path());
        let a = seed_repo(tmp.path(), "a", "ra");
        let fake = FakeBdClient::new();

        // The same repo listed twice must dedupe, not self-collide.
        let outcome = run(&fake, &roster(&[&a, &a]), &paths).unwrap();

        assert!(
            outcome.prefix_map.collisions().is_empty(),
            "an aliased duplicate is not a collision: {:?}",
            outcome.prefix_map.collisions()
        );
        assert_eq!(
            outcome.prefix_map.repo_for("ra-1").map(|r| &r.path),
            Some(&a),
            "the deduped repo still attributes its ids"
        );
        let exports = fake
            .calls()
            .into_iter()
            .filter(|c| matches!(c, Call::Export(_)))
            .count();
        assert_eq!(exports, 1, "a duplicate roster entry exports once");
    }

    #[test]
    fn declines_when_lock_already_held() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = Paths::with_base(tmp.path());
        let a = seed_repo(tmp.path(), "a", "ra");
        let hub = hub_dir(&paths);
        fs::create_dir_all(&hub).unwrap();

        // Hold the lock, then a refresh must decline without doing any work.
        let held = HubLock::try_acquire(&hub).unwrap();
        assert!(held.is_some(), "precondition: acquired the lock");
        let fake = FakeBdClient::new();

        let err = run(&fake, &roster(&[&a]), &paths).unwrap_err();

        assert!(
            matches!(err, RefreshError::AlreadyRefreshing),
            "got {err:?}"
        );
        assert!(
            fake.calls().is_empty(),
            "a declined refresh performs no exports or sync: {:?}",
            fake.calls()
        );
    }

    #[test]
    fn read_prefix_derives_hyphenated_prefix_from_ids() {
        // The bug: dolt_database underscore-sanitizes the prefix, but ids keep
        // the hyphen. read_prefix must return the id-derived (hyphenated) form.
        let tmp = tempfile::tempdir().unwrap();
        let repo = seed_repo_with_ids(
            tmp.path(),
            "reading-lite",
            "reading_lite",
            &["reading-lite-hck.1"],
        );

        assert_eq!(read_prefix(&repo).unwrap(), "reading-lite");
    }

    #[test]
    fn read_prefix_falls_back_to_dolt_database_without_jsonl() {
        // No issues.jsonl (e.g. an empty repo): fall back to dolt_database. This
        // is why the seed-only tests above stay valid.
        let tmp = tempfile::tempdir().unwrap();
        let repo = seed_repo(tmp.path(), "a", "ra");

        assert_eq!(read_prefix(&repo).unwrap(), "ra");
    }

    #[test]
    fn read_prefix_does_not_remap_a_genuine_underscore_prefix() {
        // A prefix that legitimately contains an underscore must not be rewritten
        // to a hyphen: the id itself is authoritative.
        let tmp = tempfile::tempdir().unwrap();
        let repo = seed_repo_with_ids(tmp.path(), "r", "foo_bar", &["foo_bar-abc"]);

        assert_eq!(read_prefix(&repo).unwrap(), "foo_bar");
    }

    #[test]
    fn read_prefix_skips_foreign_ids_that_fail_validation() {
        let tmp = tempfile::tempdir().unwrap();
        // A foreign hydrated id leads the file; the repo's own id follows.
        let repo = seed_repo_with_ids(
            tmp.path(),
            "r",
            "reading_lite",
            &["other-thing-xyz", "reading-lite-hck.1"],
        );
        assert_eq!(read_prefix(&repo).unwrap(), "reading-lite");

        // A file of only foreign ids validates none, so fall back to dolt_database.
        let only_foreign =
            seed_repo_with_ids(tmp.path(), "r2", "reading_lite", &["other-thing-xyz"]);
        assert_eq!(read_prefix(&only_foreign).unwrap(), "reading_lite");
    }

    #[test]
    fn attributes_hyphenated_repo_end_to_end() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = Paths::with_base(tmp.path());
        let repo = seed_repo_with_ids(
            tmp.path(),
            "reading-lite",
            "reading_lite",
            &["reading-lite-hck.1", "reading-lite-x1u"],
        );
        let fake = FakeBdClient::new();

        let outcome = run(&fake, &roster(&[&repo]), &paths).unwrap();
        let map = outcome.prefix_map;

        assert_eq!(
            map.repo_for("reading-lite-hck.1").map(|r| &r.path),
            Some(&repo),
            "a hyphenated id attributes to its repo, not the unknown bucket"
        );
        assert_eq!(
            map.repo_for("reading-lite-x1u").map(|r| &r.path),
            Some(&repo)
        );
        assert!(
            map.repo_for("reading_lite-hck.1").is_none(),
            "the underscored (dolt_database) form must not attribute"
        );
    }

    #[test]
    fn custom_prefix_unrelated_to_dir_name_attributes() {
        // Attribution is prefix-driven, never dir-name-driven: a repo dir named
        // `whatever` whose prefix is `ready-fix` still attributes correctly.
        let tmp = tempfile::tempdir().unwrap();
        let paths = Paths::with_base(tmp.path());
        let repo = seed_repo_with_ids(tmp.path(), "whatever", "ready_fix", &["ready-fix-1"]);
        let fake = FakeBdClient::new();

        let outcome = run(&fake, &roster(&[&repo]), &paths).unwrap();

        assert_eq!(
            outcome.prefix_map.repo_for("ready-fix-1").map(|r| &r.path),
            Some(&repo)
        );
    }

    #[test]
    fn two_hyphenated_repos_attribute_independently() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = Paths::with_base(tmp.path());
        let a = seed_repo_with_ids(
            tmp.path(),
            "reading-lite",
            "reading_lite",
            &["reading-lite-1"],
        );
        let b = seed_repo_with_ids(tmp.path(), "session-tui", "session_tui", &["session-tui-9"]);
        let fake = FakeBdClient::new();

        let outcome = run(&fake, &roster(&[&a, &b]), &paths).unwrap();
        let map = outcome.prefix_map;

        assert!(
            map.collisions().is_empty(),
            "distinct prefixes don't collide"
        );
        assert_eq!(map.repo_for("reading-lite-1").map(|r| &r.path), Some(&a));
        assert_eq!(map.repo_for("session-tui-9").map(|r| &r.path), Some(&b));
    }

    #[test]
    fn lock_releases_after_refresh() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = Paths::with_base(tmp.path());
        let a = seed_repo(tmp.path(), "a", "ra");
        let fake = FakeBdClient::new();

        run(&fake, &roster(&[&a]), &paths).unwrap();

        // The refresh released the lock, so it can be re-acquired now.
        let reacquired = HubLock::try_acquire(&hub_dir(&paths)).unwrap();
        assert!(reacquired.is_some(), "lock must be released after refresh");
    }
}

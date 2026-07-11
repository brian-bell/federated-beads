//! The snapshot read model: one call turns the hub's `bd ready` output into the
//! attributed, sorted rows the ready screen consumes — UI-agnostic and
//! serializable (Slice 6 emits it verbatim as `fbd snapshot --json`).
//!
//! Grouping is a view concern (Slice 9): rows merely *carry* `repo_name` so a
//! view can group by it. See `plans/slices/slice-5.md`.

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::time::SystemTime;

use serde::Serialize;

use crate::bd::{BdClient, BdError, Issue};
use crate::refresh::PrefixMap;

/// The `repo_name` given to a row whose issue id matches no configured prefix
/// (or matches a collided, ambiguous one). Slice 9 renders this as its own
/// group.
pub const UNKNOWN_REPO: &str = "unknown";

/// One ready issue plus the source repo it was attributed to. `repo_name` is the
/// repo directory's basename, or `"basename (prefix)"` when another attributed
/// repo shares that basename (so grouping/filtering never conflates two repos —
/// see [`fetch`]), or [`UNKNOWN_REPO`] when unattributed. Never a filesystem
/// path: the serialized snapshot must not leak local layout.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Row {
    pub issue: Issue,
    pub repo_name: String,
}

/// Everything the ready screen needs: attributed, display-sorted rows plus the
/// time the underlying data was fetched (injected, never read from a hidden
/// clock — see the module/slice notes on determinism).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Snapshot {
    pub rows: Vec<Row>,
    pub fetched_at: SystemTime,
}

/// Fetch the hub's ready issues, attribute each to its source repo via
/// `prefix_map`, and sort for display: priority ascending (0 = highest, first),
/// then `updated_at` descending (newest first, absent last), then id ascending
/// (a total, deterministic order for the serialized output).
///
/// `fetched_at` is supplied by the caller (typically `RefreshOutcome::synced_at`
/// or a real `now`) so this stays a pure, deterministic transform of `bd ready`.
pub fn fetch(
    bd: &impl BdClient,
    hub: &Path,
    prefix_map: &PrefixMap,
    fetched_at: SystemTime,
) -> Result<Snapshot, BdError> {
    Ok(attribute(bd.ready(hub)?, prefix_map, fetched_at))
}

/// The pure core of [`fetch`]: attribute a list of issues to their source repos
/// via `prefix_map` and sort them for display. Shared with cross-repo search
/// (Slice 11), whose worker feeds `bd search` results through this same path so
/// search rows carry `repo_name` and sort exactly as ready rows do.
pub fn attribute(issues: Vec<Issue>, prefix_map: &PrefixMap, fetched_at: SystemTime) -> Snapshot {
    // Attribute every issue to its source repo first (carrying the unique id
    // prefix), so a repo's display name can be made unique across the repos that
    // actually appear before it is stamped onto rows.
    let attributed: Vec<(Issue, Option<(&str, &Path)>)> = issues
        .into_iter()
        .map(|issue| {
            let repo = prefix_map
                .attribution(&issue.id)
                .map(|(prefix, e)| (prefix, e.path.as_path()));
            (issue, repo)
        })
        .collect();

    // basename → the distinct repo prefixes that share it. A basename claimed by
    // more than one repo is ambiguous; those rows get the prefix appended.
    let mut by_basename: HashMap<String, HashSet<&str>> = HashMap::new();
    for (_, repo) in &attributed {
        if let Some((prefix, path)) = repo {
            by_basename
                .entry(basename(path))
                .or_default()
                .insert(prefix);
        }
    }

    let mut rows: Vec<Row> = attributed
        .into_iter()
        .map(|(issue, repo)| {
            let repo_name = match repo {
                Some((prefix, path)) => display_name(prefix, path, &by_basename),
                None => UNKNOWN_REPO.to_string(),
            };
            Row { issue, repo_name }
        })
        .collect();

    rows.sort_by(|a, b| {
        a.issue
            .priority
            .cmp(&b.issue.priority)
            // `updated_at` is bd's RFC3339 UTC-`Z`, whole-second timestamp, and
            // every row in one `bd ready` call shares that format, so a lexical
            // string compare orders them chronologically. Reversed operands =>
            // newest first; `None` (omitted) sorts last.
            .then_with(|| b.issue.updated_at.cmp(&a.issue.updated_at))
            .then_with(|| a.issue.id.cmp(&b.issue.id))
    });

    Snapshot { rows, fetched_at }
}

/// A path's final component as a string, falling back to the full path string
/// for a path with no final component (not expected for real repos).
fn basename(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_string_lossy().into_owned())
}

/// A roster-unique, non-sensitive display label for an attributed repo: its
/// basename when only one repo carries that basename, else `basename (prefix)`.
/// The prefix is the repo's unique, short id (`ra`, `rb`, …), so two distinct
/// repos that share a directory name (e.g. `/work/a/api` and `/work/b/api`) get
/// distinct labels — `api (ra)`, `api (rb)` — without emitting a filesystem path
/// that would leak local layout through the serialized snapshot.
fn display_name(prefix: &str, path: &Path, by_basename: &HashMap<String, HashSet<&str>>) -> String {
    let bn = basename(path);
    match by_basename.get(&bn) {
        Some(prefixes) if prefixes.len() > 1 => format!("{bn} ({prefix})"),
        _ => bn,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bd::FakeBdClient;
    use crate::bd::{BdError, BdErrorKind};
    use crate::config::RepoEntry;
    use std::collections::BTreeMap;
    use std::path::{Path, PathBuf};
    use std::time::{Duration, UNIX_EPOCH};

    const READY: &str = include_str!("../tests/fixtures/ready.json");

    /// The checked-in `bd ready` fixture parsed into issues (ids `ra-z70`,
    /// `ra-shr`).
    fn ready_fixture() -> Vec<Issue> {
        serde_json::from_str(READY).expect("ready.json parses")
    }

    /// A `PrefixMap` from `(prefix, repo_path)` pairs.
    fn prefix_map(pairs: &[(&str, &str)]) -> PrefixMap {
        PrefixMap::from_pairs(
            pairs
                .iter()
                .map(|(prefix, path)| {
                    (
                        (*prefix).to_string(),
                        RepoEntry {
                            path: PathBuf::from(path),
                        },
                    )
                })
                .collect(),
        )
    }

    /// A minimal issue with a chosen id, priority, and `updated_at`.
    fn issue(id: &str, priority: i64, updated_at: Option<&str>) -> Issue {
        Issue {
            id: id.to_string(),
            title: format!("title {id}"),
            status: "open".into(),
            priority,
            description: None,
            issue_type: None,
            owner: None,
            labels: Vec::new(),
            created_at: None,
            created_by: None,
            updated_at: updated_at.map(str::to_string),
            dependency_count: None,
            dependent_count: None,
            comment_count: None,
        }
    }

    fn at(secs: u64) -> SystemTime {
        UNIX_EPOCH + Duration::from_secs(secs)
    }

    #[test]
    fn merges_ready_with_attribution() {
        let bd = FakeBdClient::new().with_ready(ready_fixture());
        let map = prefix_map(&[("ra", "/dev/session-tui")]);
        let when = at(1_700_000_000);

        let snap = fetch(&bd, Path::new("/hub"), &map, when).expect("fetch ok");

        assert_eq!(snap.fetched_at, when, "fetch time is the injected instant");
        assert_eq!(snap.rows.len(), 2, "both ready issues become rows");
        let ids: Vec<&str> = snap.rows.iter().map(|r| r.issue.id.as_str()).collect();
        assert_eq!(ids, vec!["ra-z70", "ra-shr"], "fixture ids preserved");
        assert!(
            snap.rows.iter().all(|r| r.repo_name == "session-tui"),
            "every ra-* row is attributed to the repo basename: {:?}",
            snap.rows.iter().map(|r| &r.repo_name).collect::<Vec<_>>()
        );
    }

    #[test]
    fn sorts_by_priority_then_updated() {
        // Scrambled input the fixture can't express: a P0 between two P1s whose
        // updated_at differ, so both sort keys are exercised.
        let issues = vec![
            issue("rb-old", 1, Some("2026-07-11T12:41:25Z")),
            issue("rb-p0", 0, Some("2026-07-11T12:41:26Z")),
            issue("rb-new", 1, Some("2026-07-11T12:41:27Z")),
        ];
        let bd = FakeBdClient::new().with_ready(issues);
        let map = prefix_map(&[("rb", "/dev/repo-b")]);

        let snap = fetch(&bd, Path::new("/hub"), &map, at(0)).expect("fetch ok");

        let order: Vec<&str> = snap.rows.iter().map(|r| r.issue.id.as_str()).collect();
        assert_eq!(
            order,
            vec!["rb-p0", "rb-new", "rb-old"],
            "P0 first, then P1s newest-updated first"
        );
    }

    #[test]
    fn groups_by_repo() {
        let issues = vec![
            issue("ra-1", 1, Some("2026-07-11T00:00:01Z")),
            issue("rb-1", 1, Some("2026-07-11T00:00:02Z")),
            issue("ra-2", 2, Some("2026-07-11T00:00:03Z")),
        ];
        let bd = FakeBdClient::new().with_ready(issues);
        let map = prefix_map(&[("ra", "/dev/repo-a"), ("rb", "/dev/repo-b")]);

        let snap = fetch(&bd, Path::new("/hub"), &map, at(0)).expect("fetch ok");

        // Grouping is a view concern; the data to group by lives on each row.
        let mut groups: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
        for row in &snap.rows {
            groups
                .entry(row.repo_name.as_str())
                .or_default()
                .push(row.issue.id.as_str());
        }
        assert_eq!(groups.get("repo-a"), Some(&vec!["ra-1", "ra-2"]));
        assert_eq!(groups.get("repo-b"), Some(&vec!["rb-1"]));
        assert_eq!(groups.len(), 2, "exactly two repo groups: {groups:?}");
    }

    #[test]
    fn unattributed_goes_to_unknown_bucket() {
        let issues = vec![
            issue("ra-1", 1, Some("2026-07-11T00:00:01Z")),
            issue("zz-999", 1, Some("2026-07-11T00:00:02Z")),
        ];
        let bd = FakeBdClient::new().with_ready(issues);
        let map = prefix_map(&[("ra", "/dev/repo-a")]);

        let snap = fetch(&bd, Path::new("/hub"), &map, at(0)).expect("fetch ok");

        let unknown = snap
            .rows
            .iter()
            .find(|r| r.issue.id == "zz-999")
            .expect("unattributed row present");
        assert_eq!(
            unknown.repo_name, UNKNOWN_REPO,
            "no prefix -> unknown bucket"
        );
        let attributed = snap
            .rows
            .iter()
            .find(|r| r.issue.id == "ra-1")
            .expect("attributed row present");
        assert_eq!(attributed.repo_name, "repo-a", "attributed row unaffected");
    }

    #[test]
    fn disambiguates_duplicate_basenames() {
        // Two distinct repos that share the directory name `api`.
        let issues = vec![
            issue("ra-1", 1, Some("2026-07-11T00:00:01Z")),
            issue("rb-1", 1, Some("2026-07-11T00:00:02Z")),
        ];
        let bd = FakeBdClient::new().with_ready(issues);
        let map = prefix_map(&[("ra", "/work/a/api"), ("rb", "/work/b/api")]);

        let snap = fetch(&bd, Path::new("/hub"), &map, at(0)).expect("fetch ok");

        let name = |id: &str| {
            snap.rows
                .iter()
                .find(|r| r.issue.id == id)
                .map(|r| r.repo_name.as_str())
                .expect("row present")
        };
        // The shared basename is ambiguous, so each is disambiguated by its
        // unique prefix — no filesystem path leaks — and grouping by repo_name
        // keeps the two repos distinct.
        assert_eq!(name("ra-1"), "api (ra)");
        assert_eq!(name("rb-1"), "api (rb)");
        assert!(
            !name("ra-1").contains('/') && !name("rb-1").contains('/'),
            "display labels never contain a filesystem path"
        );
    }

    #[test]
    fn serializes_to_json() {
        let bd = FakeBdClient::new().with_ready(ready_fixture());
        let map = prefix_map(&[("ra", "/dev/session-tui")]);

        let snap = fetch(&bd, Path::new("/hub"), &map, at(1_700_000_000)).expect("fetch ok");
        let json = serde_json::to_value(&snap).expect("Snapshot serializes to JSON");

        let rows = json
            .get("rows")
            .and_then(|r| r.as_array())
            .expect("rows array");
        assert_eq!(rows.len(), 2);
        let first = &rows[0];
        assert_eq!(
            first.get("repo_name").and_then(|v| v.as_str()),
            Some("session-tui")
        );
        assert_eq!(
            first
                .get("issue")
                .and_then(|i| i.get("id"))
                .and_then(|v| v.as_str()),
            Some("ra-z70"),
            "the issue is nested under the row and exposes its id"
        );
        assert!(json.get("fetched_at").is_some(), "fetch time is serialized");
    }

    #[test]
    fn attribute_is_shared_by_fetch() {
        // The pure `attribute` core produces the same rows/order/attribution
        // `fetch` does, so cross-repo search (Slice 11) can reuse it directly.
        let issues = vec![
            issue("ra-2", 2, Some("2026-07-11T00:00:03Z")),
            issue("rb-1", 0, Some("2026-07-11T00:00:02Z")),
            issue("ra-1", 1, Some("2026-07-11T00:00:01Z")),
        ];
        let map = prefix_map(&[("ra", "/dev/repo-a"), ("rb", "/dev/repo-b")]);
        let when = at(1_700_000_000);

        // Direct call — no bd, purely from the issue list.
        let direct = attribute(issues.clone(), &map, when);
        // Via fetch (which reads the same issues from `bd ready`).
        let bd = FakeBdClient::new().with_ready(issues);
        let via_fetch = fetch(&bd, Path::new("/hub"), &map, when).expect("fetch ok");

        assert_eq!(direct, via_fetch, "fetch delegates to attribute");
        let order: Vec<&str> = direct.rows.iter().map(|r| r.issue.id.as_str()).collect();
        assert_eq!(
            order,
            vec!["rb-1", "ra-1", "ra-2"],
            "priority-then-updated sort"
        );
        assert_eq!(
            direct.rows[0].repo_name, "repo-b",
            "attribution carried through"
        );
    }

    #[test]
    fn ready_error_propagates() {
        let bd = FakeBdClient::new().with_ready_err(BdError {
            command: "bd -C /hub ready --json".into(),
            stderr: "boom".into(),
            kind: BdErrorKind::NonZeroExit { code: Some(1) },
        });
        let map = prefix_map(&[("ra", "/dev/repo-a")]);

        let err = fetch(&bd, Path::new("/hub"), &map, at(0)).expect_err("ready failure propagates");
        assert!(matches!(
            err.kind,
            BdErrorKind::NonZeroExit { code: Some(1) }
        ));
    }
}

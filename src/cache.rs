//! A local, on-disk cache of the last successful [`Snapshot`], so launch can
//! paint the ready list instantly from disk while a real refresh runs in the
//! background instead of sitting in `Loading` until `bd ready` returns.
//!
//! Freshness is judged by the snapshot's own embedded `fetched_at`, not the
//! file's mtime, so the cache and the "last refreshed" age the UI renders
//! always agree. A cache miss (missing file, corrupt JSON, or stale data) is
//! never an error to the caller — it just means the ordinary `Loading` boot
//! runs, exactly as if no cache module existed.

use std::fs;
use std::io;
use std::path::Path;
use std::time::{Duration, SystemTime};

use crate::snapshot::Snapshot;

/// A cached snapshot older than this is not loaded at startup — better to wait
/// for one real refresh than paint a half-day-stale ready list.
pub const MAX_AGE: Duration = Duration::from_secs(12 * 60 * 60);

/// Load the snapshot cached at `path` if it exists, parses, and its embedded
/// `fetched_at` is within [`MAX_AGE`] of `now`. Any failure — missing file,
/// corrupt JSON, or a `fetched_at` more than `MAX_AGE` before (or after,
/// guarding against clock skew) `now` — yields `None` silently.
pub fn load(path: &Path, now: SystemTime) -> Option<Snapshot> {
    let bytes = fs::read(path).ok()?;
    let snapshot: Snapshot = serde_json::from_slice(&bytes).ok()?;
    let age = now.duration_since(snapshot.fetched_at).ok()?;
    if age > MAX_AGE {
        return None;
    }
    Some(snapshot)
}

/// Persist `snapshot` to `path` as JSON, creating parent directories as
/// needed and overwriting any previous cache. Best-effort: the caller treats a
/// write failure (e.g. a read-only data dir) as non-fatal to the refresh cycle
/// that produced the snapshot.
pub fn save(path: &Path, snapshot: &Snapshot) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let bytes = serde_json::to_vec(snapshot)?;
    fs::write(path, bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::snapshot::Row;
    use std::time::UNIX_EPOCH;

    fn snapshot_at(fetched_at: SystemTime) -> Snapshot {
        Snapshot {
            rows: vec![Row {
                issue: crate::bd::Issue {
                    id: "ra-1".into(),
                    title: "t".into(),
                    status: "open".into(),
                    priority: 1,
                    description: None,
                    issue_type: None,
                    owner: None,
                    labels: Vec::new(),
                    created_at: None,
                    created_by: None,
                    updated_at: None,
                    dependency_count: None,
                    dependent_count: None,
                    comment_count: None,
                },
                repo_name: "repo-a".into(),
            }],
            fetched_at,
        }
    }

    fn at(secs: u64) -> SystemTime {
        UNIX_EPOCH + Duration::from_secs(secs)
    }

    #[test]
    fn round_trips_a_fresh_snapshot() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("snapshot_cache.json");
        let snapshot = snapshot_at(at(1_000_000));

        save(&path, &snapshot).expect("save ok");
        let loaded = load(&path, at(1_000_000) + Duration::from_secs(60)).expect("fresh load");

        assert_eq!(loaded, snapshot);
    }

    #[test]
    fn rejects_a_snapshot_older_than_max_age() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("snapshot_cache.json");
        let snapshot = snapshot_at(at(1_000_000));
        save(&path, &snapshot).expect("save ok");

        let just_stale = at(1_000_000) + MAX_AGE + Duration::from_secs(1);
        assert!(load(&path, just_stale).is_none(), "past MAX_AGE is a miss");

        let just_fresh = at(1_000_000) + MAX_AGE;
        assert!(
            load(&path, just_fresh).is_some(),
            "exactly MAX_AGE is a hit"
        );
    }

    #[test]
    fn rejects_a_fetched_at_in_the_future() {
        // Clock skew (or a corrupted timestamp) should not be trusted as fresh.
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("snapshot_cache.json");
        let snapshot = snapshot_at(at(1_000_000));
        save(&path, &snapshot).expect("save ok");

        assert!(load(&path, at(999_999)).is_none());
    }

    #[test]
    fn missing_file_is_a_silent_miss() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("does-not-exist.json");
        assert!(load(&path, at(0)).is_none());
    }

    #[test]
    fn corrupt_json_is_a_silent_miss() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("snapshot_cache.json");
        fs::write(&path, b"not json").unwrap();
        assert!(load(&path, at(0)).is_none());
    }

    #[test]
    fn save_creates_missing_parent_directories() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("nested").join("snapshot_cache.json");
        let snapshot = snapshot_at(at(0));

        save(&path, &snapshot).expect("save creates parent dirs");
        assert!(path.exists());
    }
}

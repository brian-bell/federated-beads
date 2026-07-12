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
use std::path::{Path, PathBuf};
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
/// needed. Best-effort: the caller treats a write failure (e.g. a read-only
/// data dir) as non-fatal to the refresh cycle that produced the snapshot.
///
/// Writes to a same-directory temp file and renames it over `path`, so a
/// reader (or a second fbd instance's own cache write racing this one) always
/// sees either the old or the new cache in full, never a partial/interleaved
/// write — the same atomic-replace pattern [`crate::config::Config::save`]
/// uses for `config.toml`. The pid keeps concurrent writers from colliding on
/// the temp name.
///
/// A no-op, `Ok(())` skip when an existing on-disk cache's `fetched_at` is
/// already at or after `snapshot`'s: two fbd instances can each hold the hub
/// lock only during their own sync (see `refresh::run`), so their later,
/// lock-free `bd ready` reads and cache writes can finish out of order. This
/// keeps the cache monotonic in `fetched_at` without serializing the writes
/// themselves.
pub fn save(path: &Path, snapshot: &Snapshot) -> io::Result<()> {
    if let Some(existing) = raw_fetched_at(path)
        && existing >= snapshot.fetched_at
    {
        return Ok(());
    }

    let parent = path.parent().filter(|p| !p.as_os_str().is_empty());
    if let Some(parent) = parent {
        fs::create_dir_all(parent)?;
    }
    let bytes = serde_json::to_vec(snapshot)?;

    let file_name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "snapshot_cache.json".to_string());
    let tmp_name = format!(".{}.tmp.{}", file_name, std::process::id());
    let tmp_path = match parent {
        Some(parent) => parent.join(tmp_name),
        None => PathBuf::from(tmp_name),
    };

    fs::write(&tmp_path, bytes)?;
    fs::rename(&tmp_path, path)
}

/// The `fetched_at` embedded in whatever is currently at `path`, ignoring
/// [`MAX_AGE`] (unlike [`load`]) — [`save`]'s monotonicity check needs the
/// raw timestamp regardless of staleness. `None` for a missing/corrupt file.
fn raw_fetched_at(path: &Path) -> Option<SystemTime> {
    let bytes = fs::read(path).ok()?;
    let snapshot: Snapshot = serde_json::from_slice(&bytes).ok()?;
    Some(snapshot.fetched_at)
}

/// Remove the cache file at `path`, if present. A missing file is not an
/// error — `reset` calls this unconditionally alongside deleting the hub, so
/// a launch just after `fbd reset` never paints rows from the discarded hub
/// (see [`crate::hub::reset`]).
pub fn clear(path: &Path) -> io::Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
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

    #[test]
    fn save_leaves_no_temp_file_behind() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("snapshot_cache.json");
        save(&path, &snapshot_at(at(0))).expect("save ok");

        let entries: Vec<_> = fs::read_dir(tmp.path())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(
            entries,
            vec![path.file_name().unwrap().to_os_string()],
            "only the final cache file remains, no leftover .tmp file"
        );
    }

    #[test]
    fn save_never_regresses_a_newer_cache() {
        // Simulates two fbd instances whose syncs finish in one order but
        // whose (lock-free) `bd ready` reads and cache writes land in the
        // other: the older snapshot's write must not clobber the newer one.
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("snapshot_cache.json");
        let newer = snapshot_at(at(2_000_000));
        let older = snapshot_at(at(1_000_000));

        save(&path, &newer).expect("save ok");
        save(&path, &older).expect("save ok (no-op)");

        let on_disk = load(&path, at(2_000_000)).expect("still a hit");
        assert_eq!(
            on_disk, newer,
            "the older write did not overwrite the newer cache"
        );
    }

    #[test]
    fn clear_removes_an_existing_cache_file() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("snapshot_cache.json");
        save(&path, &snapshot_at(at(0))).expect("save ok");

        clear(&path).expect("clear ok");
        assert!(!path.exists());
    }

    #[test]
    fn clear_is_a_silent_no_op_when_the_file_is_missing() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("does-not-exist.json");
        clear(&path).expect("missing file is not an error");
    }
}

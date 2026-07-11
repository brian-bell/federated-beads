//! Gated end-to-end tests against a real `bd` binary.
//!
//! Every test here first checks whether `bd` is installed; if not, it prints
//! `SKIP: bd not installed` and returns early so `cargo test --test
//! bd_integration` is always green regardless of environment. These tests are
//! the schema-drift tripwire: they drive the real `BdCli` against real repos.

mod helpers;

use fbd::bd::{BdCli, BdClient};
use fbd::config::{Config, Paths, RepoEntry};
use fbd::hub::{ensure_hub, hub_dir, read_hub_roster};
use helpers::{bd_available, build_ready_fixture_repo, build_ready_fixture_repo_with_prefix};

#[test]
fn bd_probe_skips_cleanly_when_absent() {
    if !bd_available() {
        eprintln!("SKIP: bd not installed");
    }
}

#[test]
fn version_and_ready_roundtrip() {
    if !bd_available() {
        eprintln!("SKIP: bd not installed");
        return;
    }

    let cli = BdCli::new();

    // Version gate parses and reports the expected schema.
    let v = cli.version().expect("bd version --json");
    assert_eq!(v.schema_version, 1, "unexpected bd schema_version");

    // Build a real fixture repo: 3 issues, the third blocked by the second.
    let tmp = tempfile::tempdir().expect("tempdir");
    let repo = tmp.path().join("ra");
    std::fs::create_dir_all(&repo).expect("mkdir repo");
    build_ready_fixture_repo(&repo);

    // `ready` reads the repo's own hydrated data; the blocked issue is excluded.
    let ready = cli.ready(&repo).expect("bd ready --json");
    assert_eq!(
        ready.len(),
        2,
        "expected 2 of 3 issues ready (blocked excluded), got: {:?}",
        ready.iter().map(|i| &i.id).collect::<Vec<_>>()
    );
    assert!(
        ready.iter().all(|i| i.id.starts_with("ra-")),
        "ids carry the configured prefix"
    );
}

#[test]
fn ensure_hub_end_to_end() {
    if !bd_available() {
        eprintln!("SKIP: bd not installed");
        return;
    }

    let tmp = tempfile::tempdir().expect("tempdir");
    // Two fixture repos with distinct prefixes.
    let ra = tmp.path().join("ra");
    let rb = tmp.path().join("rb");
    std::fs::create_dir_all(&ra).expect("mkdir ra");
    std::fs::create_dir_all(&rb).expect("mkdir rb");
    build_ready_fixture_repo_with_prefix(&ra, "ra");
    build_ready_fixture_repo_with_prefix(&rb, "rb");

    // Hub lives under the injected data dir; roster names both repos.
    let paths = Paths::with_base(tmp.path());
    let roster = Config {
        repos: vec![
            RepoEntry { path: ra.clone() },
            RepoEntry { path: rb.clone() },
        ],
    };

    let status = ensure_hub(&BdCli::new(), &paths, &roster).expect("ensure_hub");
    assert!(
        status.warnings.is_empty(),
        "both repos exist, so no warnings: {:?}",
        status.warnings
    );

    // The chosen roster-read path (config.yaml) reflects both repos, canonicalized
    // as bd stores them.
    let hub = hub_dir(&paths);
    let tracked = read_hub_roster(&hub).expect("read hub roster");
    let canon = |p: &std::path::Path| std::fs::canonicalize(p).unwrap();
    assert!(
        tracked.contains(&canon(&ra)),
        "hub roster lists ra: {tracked:?}"
    );
    assert!(
        tracked.contains(&canon(&rb)),
        "hub roster lists rb: {tracked:?}"
    );

    // Idempotent: a second ensure_hub adds nothing and stays clean.
    let again = ensure_hub(&BdCli::new(), &paths, &roster).expect("ensure_hub again");
    assert!(again.warnings.is_empty());
    let tracked_again = read_hub_roster(&hub).expect("read hub roster again");
    assert_eq!(
        tracked_again.len(),
        tracked.len(),
        "second ensure_hub must not duplicate repos"
    );
}

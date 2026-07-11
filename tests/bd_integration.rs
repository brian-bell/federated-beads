//! Gated end-to-end tests against a real `bd` binary.
//!
//! Every test here first checks whether `bd` is installed; if not, it prints
//! `SKIP: bd not installed` and returns early so `cargo test --test
//! bd_integration` is always green regardless of environment. These tests are
//! the schema-drift tripwire: they drive the real `BdCli` against real repos.

mod helpers;

use fbd::bd::{BdCli, BdClient};
use helpers::{bd_available, build_ready_fixture_repo};

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

//! Gated end-to-end tests against a real `bd` binary.
//!
//! Every test here first checks whether `bd` is installed; if not, it prints
//! `SKIP: bd not installed` and returns early so `cargo test --test
//! bd_integration` is always green regardless of environment. Real end-to-end
//! coverage lands in Slice 2 onward; Slice 0 only establishes this file so the
//! command is a stable part of the verification suite.

use std::process::Command;

/// Returns true if a `bd` binary is present and reports a version.
fn bd_available() -> bool {
    Command::new("bd")
        .args(["version", "--json"])
        .output()
        .map(|out| out.status.success())
        .unwrap_or(false)
}

#[test]
fn bd_probe_skips_cleanly_when_absent() {
    if bd_available() {
        // bd is present: nothing to assert yet in Slice 0. Real e2e arrives later.
    } else {
        eprintln!("SKIP: bd not installed");
    }
}

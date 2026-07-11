//! fbd — Federated Beads: a read-only TUI over a `bd` multi-repo hub.
//!
//! Library crate exposing the modules the binary and integration tests drive.
//! Only `main` resolves real XDG paths; everything here is I/O-injectable so it
//! is testable without touching the environment or a real `bd`.

pub mod bd;
pub mod config;

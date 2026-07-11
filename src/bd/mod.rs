//! The `bd` interface layer: serde domain types for every `bd --json` payload
//! fbd reads, the [`BdClient`] trait fbd calls, and its real ([`BdCli`]) and
//! fake ([`FakeBdClient`]) implementations.

pub mod cli;
pub mod fake;
pub mod types;

pub use cli::BdCli;
#[doc(hidden)]
pub use fake::{Call, FakeBdClient};
pub use types::{BdShapeError, BdVersion, Dependency, Issue, IssueDetail};

use std::path::Path;

/// Everything fbd asks of `bd`. All calls are blocking subprocess invocations in
/// the real impl; the fake makes them synchronous and programmable for tests.
///
/// `dir`/`hub`/`repo` are passed to `bd -C <dir>`; `hub` is fbd's aggregation
/// workspace, `repo`/`dir` a source beads repo.
pub trait BdClient {
    /// `bd version --json` — the startup gate.
    fn version(&self) -> Result<BdVersion, BdError>;
    /// `bd -C <dir> init --prefix <prefix>` — create a beads workspace.
    fn init(&self, dir: &Path, prefix: &str) -> Result<(), BdError>;
    /// `bd -C <hub> repo add <repo_path>` — register a source repo with the hub.
    fn repo_add(&self, hub: &Path, repo_path: &Path) -> Result<(), BdError>;
    /// `bd -C <hub> repo list --json` — the hub's registered repos. The shape is
    /// consumed in Slice 3; here it is returned as a tolerant JSON value.
    fn repo_list(&self, hub: &Path) -> Result<serde_json::Value, BdError>;
    /// `bd -C <repo> export -o <repo>/.beads/issues.jsonl` — refresh a repo's
    /// passive JSONL export (the only write fbd makes to a source repo).
    fn export(&self, repo: &Path) -> Result<(), BdError>;
    /// `bd -C <hub> repo sync` — hydrate the hub from registered repos' exports.
    fn repo_sync(&self, hub: &Path) -> Result<(), BdError>;
    /// `bd -C <hub> ready --json` — issues with no open blockers.
    fn ready(&self, hub: &Path) -> Result<Vec<Issue>, BdError>;
    /// `bd -C <hub> show <id> --json` — one issue with its dependencies.
    fn show(&self, hub: &Path, id: &str) -> Result<IssueDetail, BdError>;
    /// `bd -C <hub> search <query> --json` — cross-repo full-text search.
    fn search(&self, hub: &Path, query: &str) -> Result<Vec<Issue>, BdError>;
}

/// The most stderr we retain/show from a failed `bd` call.
const STDERR_LIMIT: usize = 2000;

/// A failed `bd` invocation, carrying the command line and captured stderr for
/// display, plus a machine-inspectable [`BdErrorKind`].
#[derive(Debug, Clone, thiserror::Error)]
pub struct BdError {
    /// Human-readable command line, e.g. `bd -C <hub> ready --json`.
    pub command: String,
    /// Captured stderr (may be empty); truncated for display.
    pub stderr: String,
    /// What went wrong.
    pub kind: BdErrorKind,
}

/// The category of a [`BdError`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BdErrorKind {
    /// The process could not be spawned at all (e.g. `bd` not on PATH).
    Spawn,
    /// The process ran but exited non-zero.
    NonZeroExit { code: Option<i32> },
    /// stdout was not valid JSON for the expected type.
    Parse,
    /// JSON parsed but violated an invariant (e.g. `show` returned ≠ 1 element).
    Shape,
}

impl std::fmt::Display for BdError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "`{}` failed ({:?})", self.command, self.kind)?;
        if !self.stderr.is_empty() {
            let trimmed = self.stderr.trim_end();
            if trimmed.len() > STDERR_LIMIT {
                write!(
                    f,
                    ": {}…(truncated)",
                    &trimmed[..char_boundary(trimmed, STDERR_LIMIT)]
                )?;
            } else {
                write!(f, ": {trimmed}")?;
            }
        }
        Ok(())
    }
}

/// Largest byte index ≤ `max` that lands on a UTF-8 char boundary of `s`.
fn char_boundary(s: &str, max: usize) -> usize {
    let mut end = max.min(s.len());
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    end
}

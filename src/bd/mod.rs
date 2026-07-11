//! The `bd` interface layer: serde domain types for every `bd --json` payload
//! fbd reads. Slice 1 defines only the types; the `BdClient` trait and the
//! subprocess client arrive in Slice 2.

pub mod types;

pub use types::{BdShapeError, BdVersion, Dependency, Issue, IssueDetail};

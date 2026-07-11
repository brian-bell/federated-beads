//! serde types for `bd --json` payloads.
//!
//! Forward-compatibility contract (see the master plan's risk table): every key
//! bd omits when empty is `Option`/`#[serde(default)]`, and **no type uses
//! `#[serde(deny_unknown_fields)]`** so a newer bd that adds keys still parses.

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// A single issue as summarized by `bd ready`/`bd search --json`, and the base
/// shape flattened into [`IssueDetail`]. `priority` is a plain int (0 = highest);
/// `i64` tolerates any integer bd emits without risking a parse panic.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct Issue {
    pub id: String,
    pub title: String,
    pub status: String,
    pub priority: i64,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub issue_type: Option<String>,
    #[serde(default)]
    pub owner: Option<String>,
    /// The issue's labels. `bd show`/`ready --json` omit the key entirely when an
    /// issue has none, so `#[serde(default)]` yields an empty vec then.
    #[serde(default)]
    pub labels: Vec<String>,
    #[serde(default)]
    pub created_at: Option<String>,
    #[serde(default)]
    pub created_by: Option<String>,
    #[serde(default)]
    pub updated_at: Option<String>,
    #[serde(default)]
    pub dependency_count: Option<i64>,
    #[serde(default)]
    pub dependent_count: Option<i64>,
    #[serde(default)]
    pub comment_count: Option<i64>,
}

/// An embedded dependency in a `bd show --json` payload. Carries the linked
/// issue's identity plus the `dependency_type` (e.g. `"blocks"`).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct Dependency {
    pub id: String,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub dependency_type: Option<String>,
}

/// A `bd show <id> --json` element: an [`Issue`] plus its `dependencies`.
/// bd returns these as an array-of-one; use [`IssueDetail::into_single`] to
/// collapse that array with a clear error for any other length.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct IssueDetail {
    #[serde(flatten)]
    pub issue: Issue,
    #[serde(default)]
    pub dependencies: Vec<Dependency>,
}

impl IssueDetail {
    /// Collapse the array-of-one returned by `bd show --json` into a single
    /// detail, erroring for 0 or N != 1 elements.
    pub fn into_single(mut v: Vec<IssueDetail>) -> Result<IssueDetail, BdShapeError> {
        match v.len() {
            1 => Ok(v.pop().expect("length checked to be 1")),
            got => Err(BdShapeError::ExpectedOne { got }),
        }
    }
}

/// `bd version --json`, used as the Slice 6 startup gate (`version` +
/// `schema_version`).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct BdVersion {
    pub version: String,
    pub schema_version: i64,
    #[serde(default)]
    pub build: Option<String>,
    #[serde(default)]
    pub commit: Option<String>,
    #[serde(default)]
    pub branch: Option<String>,
}

/// A `bd --json` payload whose shape did not match expectations.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum BdShapeError {
    #[error("expected exactly one issue from 'bd show', got {got}")]
    ExpectedOne { got: usize },
}

#[cfg(test)]
mod tests {
    use super::*;

    const READY: &str = include_str!("../../tests/fixtures/ready.json");
    const SHOW: &str = include_str!("../../tests/fixtures/show.json");
    const SEARCH: &str = include_str!("../../tests/fixtures/search.json");
    const VERSION: &str = include_str!("../../tests/fixtures/version.json");

    #[test]
    fn parses_version() {
        let v: BdVersion = serde_json::from_str(VERSION).expect("version.json parses");
        assert_eq!(v.version, "1.1.0");
        assert_eq!(v.schema_version, 1);
    }

    #[test]
    fn parses_ready_fixture() {
        let issues: Vec<Issue> = serde_json::from_str(READY).expect("ready.json parses");
        // Blocked issue is excluded by bd; two ready issues remain.
        assert_eq!(issues.len(), 2);
        // First row (P0 blocker) has no description key -> None (omitted-key tolerance).
        let blocker = &issues[0];
        assert_eq!(blocker.id, "ra-z70");
        assert!(blocker.id.starts_with("ra-"));
        assert_eq!(blocker.priority, 0);
        assert!(blocker.description.is_none(), "omitted description -> None");
        // Second row carries a description.
        assert_eq!(issues[1].id, "ra-shr");
        assert_eq!(issues[1].priority, 1);
        assert_eq!(
            issues[1].description.as_deref(),
            Some("First ready task with a description")
        );
    }

    #[test]
    fn parses_show_fixture_with_dependencies() {
        let details: Vec<IssueDetail> = serde_json::from_str(SHOW).expect("show.json parses");
        assert_eq!(details.len(), 1, "show returns an array of one");
        let d = &details[0];
        assert_eq!(d.issue.id, "ra-4zf");
        assert!(d.issue.description.is_some());
        assert_eq!(d.dependencies.len(), 1);
        assert_eq!(d.dependencies[0].id, "ra-z70");
        assert_eq!(d.dependencies[0].dependency_type.as_deref(), Some("blocks"));
    }

    #[test]
    fn parses_search_fixture() {
        let issues: Vec<Issue> = serde_json::from_str(SEARCH).expect("search.json parses");
        assert!(!issues.is_empty());
        assert!(issues.iter().all(|i| !i.id.is_empty()));
    }

    #[test]
    fn parses_labels_and_defaults_when_absent() {
        // bd emits `labels` only when an issue has some; absent ⇒ empty vec.
        let labeled: Issue = serde_json::from_str(
            r#"{"id":"ra-1","title":"t","status":"open","priority":1,
                "labels":["urgent","backend"]}"#,
        )
        .expect("labeled issue parses");
        assert_eq!(labeled.labels, vec!["urgent", "backend"]);

        let unlabeled: Issue =
            serde_json::from_str(r#"{"id":"ra-2","title":"t","status":"open","priority":1}"#)
                .expect("unlabeled issue parses");
        assert!(unlabeled.labels.is_empty(), "omitted labels -> empty");
    }

    #[test]
    fn tolerates_unknown_future_keys() {
        // Newer bd may add keys; parsing must not fail (no deny_unknown_fields).
        let issue_json = r#"{
            "id": "ra-xyz",
            "title": "Future",
            "status": "open",
            "priority": 1,
            "future_field": 42
        }"#;
        let issue: Issue = serde_json::from_str(issue_json).expect("unknown key tolerated");
        assert_eq!(issue.id, "ra-xyz");

        let version_json = r#"{
            "version": "1.2.0",
            "schema_version": 1,
            "some_new_thing": true
        }"#;
        let v: BdVersion = serde_json::from_str(version_json).expect("unknown key tolerated");
        assert_eq!(v.version, "1.2.0");
    }

    #[test]
    fn into_single_ok() {
        let details: Vec<IssueDetail> = serde_json::from_str(SHOW).unwrap();
        let one = IssueDetail::into_single(details).expect("exactly one -> Ok");
        assert_eq!(one.issue.id, "ra-4zf");
    }

    #[test]
    fn into_single_rejects_zero_and_many() {
        let zero = IssueDetail::into_single(Vec::new());
        assert!(zero.is_err());
        assert!(zero.unwrap_err().to_string().contains("0"));

        let two: Vec<IssueDetail> = {
            let mut v: Vec<IssueDetail> = serde_json::from_str(SHOW).unwrap();
            v.push(v[0].clone());
            v
        };
        let many = IssueDetail::into_single(two);
        assert!(many.is_err());
        assert!(many.unwrap_err().to_string().contains("2"));
    }
}

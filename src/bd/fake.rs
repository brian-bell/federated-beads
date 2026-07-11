//! `FakeBdClient`: a programmable [`BdClient`] test double.
//!
//! Exposure decision: this is ordinary `pub` (but `#[doc(hidden)]`) library
//! code rather than `#[cfg(test)]`-gated, so later slices' unit tests for the
//! `hub`, `refresh`, and `snapshot` modules — which take a `&impl BdClient` —
//! can drive it. It is a test double, not part of fbd's supported API.

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

use super::{BdClient, BdError, BdVersion, Issue, IssueDetail};

/// One recorded invocation, so tests can assert call ordering/count (e.g.
/// "export A, export B, then sync once").
#[doc(hidden)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Call {
    Version,
    Init(PathBuf, String),
    RepoAdd(PathBuf, PathBuf),
    RepoList(PathBuf),
    Export(PathBuf),
    RepoSync(PathBuf),
    Ready(PathBuf),
    Show(PathBuf, String),
    Search(PathBuf, String),
}

/// A programmable [`BdClient`] test double.
///
/// **Every** method of the trait is programmable with a success value or a
/// [`BdError`], so downstream modules can drive their error paths (hub init
/// failure, roster-list failure, sync failure, per-repo export failure, …)
/// without writing another fake. Each `with_*` response is **reused** across
/// calls (not consumed). Unset slots default to something benign: the
/// value-returning calls yield an empty list / bd 1.1.0 version / an empty show
/// error, and the unit-returning calls (`init`/`repo_add`/`repo_sync`) yield
/// `Ok(())`. `export` is keyed **per repo path** so one repo can fail while the
/// rest succeed. Every call is recorded and retrievable via
/// [`FakeBdClient::calls`].
#[doc(hidden)]
#[derive(Debug, Default)]
pub struct FakeBdClient {
    calls: RefCell<Vec<Call>>,
    version: Option<Result<BdVersion, BdError>>,
    init: Option<Result<(), BdError>>,
    repo_add: Option<Result<(), BdError>>,
    repo_list: Option<Result<serde_json::Value, BdError>>,
    repo_sync: Option<Result<(), BdError>>,
    ready: Option<Result<Vec<Issue>, BdError>>,
    show: Option<Result<IssueDetail, BdError>>,
    search: Option<Result<Vec<Issue>, BdError>>,
    export_errs: HashMap<PathBuf, BdError>,
}

impl FakeBdClient {
    pub fn new() -> Self {
        FakeBdClient::default()
    }

    pub fn with_version(mut self, v: BdVersion) -> Self {
        self.version = Some(Ok(v));
        self
    }

    pub fn with_version_err(mut self, err: BdError) -> Self {
        self.version = Some(Err(err));
        self
    }

    pub fn with_init_err(mut self, err: BdError) -> Self {
        self.init = Some(Err(err));
        self
    }

    pub fn with_repo_add_err(mut self, err: BdError) -> Self {
        self.repo_add = Some(Err(err));
        self
    }

    pub fn with_repo_list(mut self, value: serde_json::Value) -> Self {
        self.repo_list = Some(Ok(value));
        self
    }

    pub fn with_repo_list_err(mut self, err: BdError) -> Self {
        self.repo_list = Some(Err(err));
        self
    }

    pub fn with_repo_sync_err(mut self, err: BdError) -> Self {
        self.repo_sync = Some(Err(err));
        self
    }

    pub fn with_ready(mut self, issues: Vec<Issue>) -> Self {
        self.ready = Some(Ok(issues));
        self
    }

    pub fn with_ready_err(mut self, err: BdError) -> Self {
        self.ready = Some(Err(err));
        self
    }

    pub fn with_show(mut self, detail: IssueDetail) -> Self {
        self.show = Some(Ok(detail));
        self
    }

    pub fn with_show_err(mut self, err: BdError) -> Self {
        self.show = Some(Err(err));
        self
    }

    pub fn with_search(mut self, issues: Vec<Issue>) -> Self {
        self.search = Some(Ok(issues));
        self
    }

    pub fn with_search_err(mut self, err: BdError) -> Self {
        self.search = Some(Err(err));
        self
    }

    /// Program `export(repo)` to fail for exactly this path; other paths still
    /// export `Ok`. Lets a refresh test fail one repo while the rest proceed.
    pub fn with_export_err(mut self, repo: impl Into<PathBuf>, err: BdError) -> Self {
        self.export_errs.insert(repo.into(), err);
        self
    }

    /// The invocations recorded so far, in order.
    pub fn calls(&self) -> Vec<Call> {
        self.calls.borrow().clone()
    }

    fn record(&self, call: Call) {
        self.calls.borrow_mut().push(call);
    }
}

/// Return a programmed response clone, or `Ok(default())` when unset.
fn resolve<T: Clone>(
    slot: &Option<Result<T, BdError>>,
    default: impl FnOnce() -> T,
) -> Result<T, BdError> {
    match slot {
        Some(r) => r.clone(),
        None => Ok(default()),
    }
}

impl BdClient for FakeBdClient {
    fn version(&self) -> Result<BdVersion, BdError> {
        self.record(Call::Version);
        resolve(&self.version, || BdVersion {
            version: "1.1.0".into(),
            schema_version: 1,
            build: None,
            commit: None,
            branch: None,
        })
    }

    fn init(&self, dir: &Path, prefix: &str) -> Result<(), BdError> {
        self.record(Call::Init(dir.to_path_buf(), prefix.to_string()));
        resolve(&self.init, || ())
    }

    fn repo_add(&self, hub: &Path, repo_path: &Path) -> Result<(), BdError> {
        self.record(Call::RepoAdd(hub.to_path_buf(), repo_path.to_path_buf()));
        resolve(&self.repo_add, || ())
    }

    fn repo_list(&self, hub: &Path) -> Result<serde_json::Value, BdError> {
        self.record(Call::RepoList(hub.to_path_buf()));
        resolve(&self.repo_list, || serde_json::Value::Array(Vec::new()))
    }

    fn export(&self, repo: &Path) -> Result<(), BdError> {
        self.record(Call::Export(repo.to_path_buf()));
        match self.export_errs.get(repo) {
            Some(err) => Err(err.clone()),
            None => Ok(()),
        }
    }

    fn repo_sync(&self, hub: &Path) -> Result<(), BdError> {
        self.record(Call::RepoSync(hub.to_path_buf()));
        resolve(&self.repo_sync, || ())
    }

    fn ready(&self, hub: &Path) -> Result<Vec<Issue>, BdError> {
        self.record(Call::Ready(hub.to_path_buf()));
        resolve(&self.ready, Vec::new)
    }

    fn show(&self, hub: &Path, id: &str) -> Result<IssueDetail, BdError> {
        self.record(Call::Show(hub.to_path_buf(), id.to_string()));
        match &self.show {
            Some(r) => r.clone(),
            None => Err(BdError {
                command: format!("bd -C {} show {id} --json", hub.display()),
                stderr: "FakeBdClient: no show response programmed".into(),
                kind: super::BdErrorKind::Shape,
            }),
        }
    }

    fn search(&self, hub: &Path, query: &str) -> Result<Vec<Issue>, BdError> {
        self.record(Call::Search(hub.to_path_buf(), query.to_string()));
        resolve(&self.search, Vec::new)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bd::types::Issue;
    use crate::bd::{BdClient, BdErrorKind};
    use std::path::Path;

    fn sample_issue(id: &str) -> Issue {
        Issue {
            id: id.to_string(),
            title: "t".into(),
            status: "open".into(),
            priority: 1,
            description: None,
            issue_type: None,
            owner: None,
            created_at: None,
            created_by: None,
            updated_at: None,
            dependency_count: None,
            dependent_count: None,
            comment_count: None,
        }
    }

    #[test]
    fn returns_programmed_ready() {
        let fake = FakeBdClient::new().with_ready(vec![sample_issue("ra-1")]);
        let got = fake.ready(Path::new("/tmp/hub")).expect("programmed ok");
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].id, "ra-1");
        // Stored response is reused on a second call.
        assert_eq!(fake.ready(Path::new("/tmp/hub")).unwrap().len(), 1);
    }

    #[test]
    fn returns_programmed_error() {
        let fake = FakeBdClient::new().with_ready_err(BdError {
            command: "bd -C /tmp/hub ready --json".into(),
            stderr: "boom".into(),
            kind: BdErrorKind::NonZeroExit { code: Some(2) },
        });
        let err = fake
            .ready(Path::new("/tmp/hub"))
            .expect_err("programmed err");
        assert!(matches!(
            err.kind,
            BdErrorKind::NonZeroExit { code: Some(2) }
        ));
    }

    #[test]
    fn export_fails_only_for_programmed_path() {
        let boom = BdError {
            command: "bd -C /tmp/b export ...".into(),
            stderr: "disk full".into(),
            kind: BdErrorKind::NonZeroExit { code: Some(1) },
        };
        let fake = FakeBdClient::new().with_export_err("/tmp/b", boom);

        // Programmed path fails; a different path still exports Ok.
        assert!(fake.export(Path::new("/tmp/a")).is_ok());
        assert!(fake.export(Path::new("/tmp/b")).is_err());
    }

    #[test]
    fn every_method_is_error_programmable() {
        let err = || BdError {
            command: "bd ...".into(),
            stderr: "nope".into(),
            kind: BdErrorKind::NonZeroExit { code: Some(1) },
        };
        let hub = Path::new("/tmp/hub");

        assert!(
            FakeBdClient::new()
                .with_version_err(err())
                .version()
                .is_err()
        );
        assert!(
            FakeBdClient::new()
                .with_init_err(err())
                .init(hub, "hub")
                .is_err()
        );
        assert!(
            FakeBdClient::new()
                .with_repo_add_err(err())
                .repo_add(hub, Path::new("/tmp/ra"))
                .is_err()
        );
        assert!(
            FakeBdClient::new()
                .with_repo_list_err(err())
                .repo_list(hub)
                .is_err()
        );
        assert!(
            FakeBdClient::new()
                .with_repo_sync_err(err())
                .repo_sync(hub)
                .is_err()
        );
        assert!(
            FakeBdClient::new()
                .with_show_err(err())
                .show(hub, "ra-1")
                .is_err()
        );
        assert!(
            FakeBdClient::new()
                .with_search_err(err())
                .search(hub, "q")
                .is_err()
        );
    }

    #[test]
    fn records_calls() {
        let fake = FakeBdClient::new();
        let _ = fake.export(Path::new("/tmp/a"));
        let _ = fake.export(Path::new("/tmp/b"));
        let _ = fake.repo_sync(Path::new("/tmp/hub"));

        let calls = fake.calls();
        assert_eq!(calls.len(), 3);
        assert!(matches!(&calls[0], Call::Export(p) if p == Path::new("/tmp/a")));
        assert!(matches!(&calls[1], Call::Export(p) if p == Path::new("/tmp/b")));
        assert!(matches!(&calls[2], Call::RepoSync(p) if p == Path::new("/tmp/hub")));
    }
}

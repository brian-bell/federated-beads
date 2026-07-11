//! `BdCli`: the real [`BdClient`] backed by spawning the `bd` binary.

use std::path::Path;
use std::process::Command;

use serde::de::DeserializeOwned;

use super::{BdClient, BdError, BdErrorKind, BdVersion, Issue, IssueDetail};

/// The real [`BdClient`]: every method shells out to the `bd` binary on PATH.
#[derive(Debug, Clone)]
pub struct BdCli {
    /// The program to spawn; `bd` by default. A field (not a const) so a future
    /// caller could point at an absolute path without touching call sites.
    program: String,
}

impl Default for BdCli {
    fn default() -> Self {
        // Delegate to `new()` so `default()` yields a usable `bd` client rather
        // than an empty program string that would fail every spawn.
        Self::new()
    }
}

impl BdCli {
    /// A client that spawns `bd` from PATH.
    pub fn new() -> Self {
        BdCli {
            program: "bd".to_string(),
        }
    }

    /// Render the command line for display/errors: `bd <args...>`.
    fn command_line(&self, args: &[String]) -> String {
        let mut s = self.program.clone();
        for a in args {
            s.push(' ');
            s.push_str(a);
        }
        s
    }

    /// Spawn `bd <args>`, require exit success, and deserialize stdout as JSON.
    fn run_json<T: DeserializeOwned>(&self, args: Vec<String>) -> Result<T, BdError> {
        let stdout = self.run_capture(&args, None)?;
        serde_json::from_slice::<T>(&stdout).map_err(|_| BdError {
            command: self.command_line(&args),
            stderr: String::from_utf8_lossy(&stdout).into_owned(),
            kind: BdErrorKind::Parse,
        })
    }

    /// Spawn `bd <args>` and require exit success, discarding stdout. For calls
    /// (repo add/export/repo sync) that print status text, not JSON.
    fn run_ok(&self, args: Vec<String>) -> Result<(), BdError> {
        self.run_capture(&args, None).map(|_| ())
    }

    /// Spawn `bd <args>` (optionally with `cwd` as the working directory),
    /// mapping spawn failure and non-zero exit into [`BdError`]; returns raw
    /// stdout on success.
    ///
    /// `cwd` exists because `bd init` targets the current directory and rejects
    /// the global `-C` flag (which pre-checks for an existing beads project);
    /// every other call uses `-C` and leaves `cwd` `None`.
    fn run_capture(&self, args: &[String], cwd: Option<&Path>) -> Result<Vec<u8>, BdError> {
        let mut cmd = Command::new(&self.program);
        cmd.args(args);
        if let Some(dir) = cwd {
            cmd.current_dir(dir);
        }
        let output = cmd.output().map_err(|e| BdError {
            command: self.command_line(args),
            stderr: e.to_string(),
            kind: BdErrorKind::Spawn,
        })?;
        if !output.status.success() {
            return Err(BdError {
                command: self.command_line(args),
                stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
                kind: BdErrorKind::NonZeroExit {
                    code: output.status.code(),
                },
            });
        }
        Ok(output.stdout)
    }
}

/// Render a path as an argv element (lossy for non-UTF-8, which `bd` paths are
/// not in practice).
fn arg(p: &Path) -> String {
    p.to_string_lossy().into_owned()
}

fn argv_version() -> Vec<String> {
    vec!["version".into(), "--json".into()]
}

/// `init`'s argv carries no `-C`: bd rejects `-C` here (it pre-checks for an
/// existing project), so [`BdClient::init`] runs `bd` with `dir` as its cwd.
fn argv_init(prefix: &str) -> Vec<String> {
    vec!["init".into(), "--prefix".into(), prefix.into()]
}

fn argv_repo_add(hub: &Path, repo_path: &Path) -> Vec<String> {
    vec![
        "-C".into(),
        arg(hub),
        "repo".into(),
        "add".into(),
        arg(repo_path),
    ]
}

fn argv_repo_list(hub: &Path) -> Vec<String> {
    vec![
        "-C".into(),
        arg(hub),
        "repo".into(),
        "list".into(),
        "--json".into(),
    ]
}

fn argv_export(repo: &Path) -> Vec<String> {
    vec![
        "-C".into(),
        arg(repo),
        "export".into(),
        "-o".into(),
        arg(&repo.join(".beads/issues.jsonl")),
    ]
}

fn argv_repo_sync(hub: &Path) -> Vec<String> {
    vec!["-C".into(), arg(hub), "repo".into(), "sync".into()]
}

fn argv_ready(hub: &Path) -> Vec<String> {
    vec!["-C".into(), arg(hub), "ready".into(), "--json".into()]
}

fn argv_show(hub: &Path, id: &str) -> Vec<String> {
    vec![
        "-C".into(),
        arg(hub),
        "show".into(),
        id.into(),
        "--json".into(),
    ]
}

fn argv_search(hub: &Path, query: &str) -> Vec<String> {
    vec![
        "-C".into(),
        arg(hub),
        "search".into(),
        query.into(),
        "--json".into(),
    ]
}

impl BdClient for BdCli {
    fn version(&self) -> Result<BdVersion, BdError> {
        self.run_json(argv_version())
    }

    fn init(&self, dir: &Path, prefix: &str) -> Result<(), BdError> {
        self.run_capture(&argv_init(prefix), Some(dir)).map(|_| ())
    }

    fn repo_add(&self, hub: &Path, repo_path: &Path) -> Result<(), BdError> {
        self.run_ok(argv_repo_add(hub, repo_path))
    }

    fn repo_list(&self, hub: &Path) -> Result<serde_json::Value, BdError> {
        self.run_json(argv_repo_list(hub))
    }

    fn export(&self, repo: &Path) -> Result<(), BdError> {
        self.run_ok(argv_export(repo))
    }

    fn repo_sync(&self, hub: &Path) -> Result<(), BdError> {
        self.run_ok(argv_repo_sync(hub))
    }

    fn ready(&self, hub: &Path) -> Result<Vec<Issue>, BdError> {
        self.run_json(argv_ready(hub))
    }

    fn show(&self, hub: &Path, id: &str) -> Result<IssueDetail, BdError> {
        let details: Vec<IssueDetail> = self.run_json(argv_show(hub, id))?;
        IssueDetail::into_single(details).map_err(|e| BdError {
            command: self.command_line(&argv_show(hub, id)),
            stderr: e.to_string(),
            kind: BdErrorKind::Shape,
        })
    }

    fn search(&self, hub: &Path, query: &str) -> Result<Vec<Issue>, BdError> {
        self.run_json(argv_search(hub, query))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn builds_correct_argv() {
        assert_eq!(argv_version(), vec!["version", "--json"]);

        // `init` carries no `-C`; it runs with the target dir as cwd because bd
        // rejects `-C` before an existing project is created.
        assert_eq!(argv_init("ra"), vec!["init", "--prefix", "ra"]);

        assert_eq!(
            argv_repo_add(Path::new("/tmp/hub"), Path::new("/tmp/ra")),
            vec!["-C", "/tmp/hub", "repo", "add", "/tmp/ra"]
        );

        assert_eq!(
            argv_repo_list(Path::new("/tmp/hub")),
            vec!["-C", "/tmp/hub", "repo", "list", "--json"]
        );

        assert_eq!(
            argv_export(Path::new("/tmp/ra")),
            vec![
                "-C",
                "/tmp/ra",
                "export",
                "-o",
                "/tmp/ra/.beads/issues.jsonl"
            ]
        );

        assert_eq!(
            argv_repo_sync(Path::new("/tmp/hub")),
            vec!["-C", "/tmp/hub", "repo", "sync"]
        );

        assert_eq!(
            argv_ready(Path::new("/tmp/hub")),
            vec!["-C", "/tmp/hub", "ready", "--json"]
        );

        assert_eq!(
            argv_show(Path::new("/tmp/hub"), "ra-2hc"),
            vec!["-C", "/tmp/hub", "show", "ra-2hc", "--json"]
        );

        assert_eq!(
            argv_search(Path::new("/tmp/hub"), "needle"),
            vec!["-C", "/tmp/hub", "search", "needle", "--json"]
        );
    }

    #[test]
    fn argv_preserves_paths_with_spaces() {
        // No shell is involved, so a spaced/unicode path is a single argv element.
        let dir = Path::new("/tmp/my repos/rä");
        assert_eq!(
            argv_ready(dir),
            vec!["-C", "/tmp/my repos/rä", "ready", "--json"]
        );
    }

    #[test]
    fn bderror_display_truncates_stderr() {
        let huge = "x".repeat(5000);
        let err = BdError {
            command: "bd -C /tmp/hub ready --json".to_string(),
            stderr: huge,
            kind: BdErrorKind::NonZeroExit { code: Some(1) },
        };
        let shown = err.to_string();
        assert!(shown.contains("bd -C /tmp/hub ready --json"));
        assert!(
            shown.len() < 5000,
            "stderr must be truncated, got {} chars",
            shown.len()
        );
        assert!(
            shown.contains("(truncated)"),
            "expected truncation marker in: {shown}"
        );
    }
}

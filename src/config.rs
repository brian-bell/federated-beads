use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

/// A single beads source repository in the roster.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RepoEntry {
    pub path: PathBuf,
}

/// The roster of beads repositories fbd federates. Source of truth is
/// `config.toml`; this is its in-memory form.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub repos: Vec<RepoEntry>,
}

impl Config {
    /// Load a roster from a TOML file. Errors if the file is missing or invalid
    /// (never silently returns a default).
    pub fn load(path: &Path) -> Result<Config> {
        let text = fs::read_to_string(path)
            .with_context(|| format!("reading config file {}", path.display()))?;
        let config: Config = toml::from_str(&text)
            .with_context(|| format!("parsing config file {}", path.display()))?;
        Ok(config)
    }

    /// Save the roster to a TOML file, creating parent directories as needed.
    ///
    /// Because this file is the roster's source of truth, the write is atomic:
    /// the serialized config is written to a temporary file in the same
    /// directory and then renamed over the destination, so an interrupted or
    /// failed write can never leave `config.toml` truncated or partial.
    pub fn save(&self, path: &Path) -> Result<()> {
        let parent = path.parent().filter(|p| !p.as_os_str().is_empty());
        if let Some(parent) = parent {
            fs::create_dir_all(parent)
                .with_context(|| format!("creating config directory {}", parent.display()))?;
        }
        let text = toml::to_string_pretty(self).context("serializing config to TOML")?;

        // Same-directory temp file so the final rename is an atomic replace on
        // the same filesystem. The pid keeps concurrent writers from colliding.
        let file_name = path
            .file_name()
            .context("config path has no file name")?
            .to_string_lossy();
        let tmp_name = format!(".{}.tmp.{}", file_name, std::process::id());
        let tmp_path = match parent {
            Some(parent) => parent.join(tmp_name),
            None => PathBuf::from(tmp_name),
        };

        fs::write(&tmp_path, text)
            .with_context(|| format!("writing temp config file {}", tmp_path.display()))?;
        fs::rename(&tmp_path, path).with_context(|| {
            format!(
                "replacing config file {} with {}",
                path.display(),
                tmp_path.display()
            )
        })?;
        Ok(())
    }
}

/// The application's subdirectory / file name under the XDG roots.
const APP_DIR: &str = "federated-beads";
const CONFIG_FILE_NAME: &str = "config.toml";
const CACHE_FILE_NAME: &str = "snapshot_cache.json";

/// Resolved filesystem locations fbd uses. Constructed either from real XDG
/// roots (`resolve`, only at the process edge) or from an injected base
/// (`with_base`, for env-independent tests). The join logic lives in one place
/// (`from_roots`) so both paths share tested behavior.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Paths {
    config_file: PathBuf,
    data_dir: PathBuf,
    cache_file: PathBuf,
}

impl Paths {
    /// Path to the roster config file (`<config_root>/federated-beads/config.toml`).
    pub fn config_file(&self) -> &Path {
        &self.config_file
    }

    /// Path to the hub data directory (`<data_root>/federated-beads`).
    pub fn data_dir(&self) -> &Path {
        &self.data_dir
    }

    /// Path to the cached [`crate::snapshot::Snapshot`] JSON file
    /// (`<data_root>/federated-beads/snapshot_cache.json`), read at launch by
    /// [`crate::cache::load`] and written after every successful refresh by
    /// [`crate::cache::save`].
    pub fn cache_file(&self) -> &Path {
        &self.cache_file
    }

    /// Derive paths from explicit config and data roots. Single source of the
    /// app-dir / file-name join convention.
    fn from_roots(config_root: &Path, data_root: &Path) -> Paths {
        Paths {
            config_file: config_root.join(APP_DIR).join(CONFIG_FILE_NAME),
            data_dir: data_root.join(APP_DIR),
            cache_file: data_root.join(APP_DIR).join(CACHE_FILE_NAME),
        }
    }

    /// Construct paths under a single injected base (tests). Both roots are the
    /// base, so all files land beneath it without touching real XDG dirs.
    pub fn with_base(base: &Path) -> Paths {
        Paths::from_roots(base, base)
    }

    /// Resolve real XDG locations. Only called from `main`; never in tests.
    pub fn resolve() -> Result<Paths> {
        let config_root = dirs::config_dir().context("resolving XDG config dir")?;
        let data_root = dirs::data_local_dir().context("resolving XDG data dir")?;
        Ok(Paths::from_roots(&config_root, &data_root))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn roundtrip_roster() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");

        let original = Config {
            repos: vec![
                RepoEntry {
                    path: PathBuf::from("/a"),
                },
                RepoEntry {
                    path: PathBuf::from("/b/c"),
                },
            ],
        };

        original.save(&path).unwrap();
        let loaded = Config::load(&path).unwrap();

        assert_eq!(loaded, original);
    }

    #[test]
    fn load_missing_file_errors() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("does-not-exist.toml");

        assert!(Config::load(&path).is_err());
    }

    #[test]
    fn save_creates_parent_dirs() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested/does/not/exist/config.toml");

        let original = Config {
            repos: vec![RepoEntry {
                path: PathBuf::from("/x"),
            }],
        };

        original.save(&path).unwrap();
        assert!(path.exists());
        assert_eq!(Config::load(&path).unwrap(), original);
    }

    #[test]
    fn paths_uses_injected_base() {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path();

        let paths = Paths::with_base(base);

        assert_eq!(
            paths.config_file(),
            base.join("federated-beads").join("config.toml")
        );
        assert_eq!(paths.data_dir(), base.join("federated-beads"));
    }

    #[test]
    fn save_overwrites_existing_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");

        Config {
            repos: vec![RepoEntry {
                path: PathBuf::from("/first"),
            }],
        }
        .save(&path)
        .unwrap();

        let second = Config {
            repos: vec![RepoEntry {
                path: PathBuf::from("/second"),
            }],
        };
        second.save(&path).unwrap();

        assert_eq!(Config::load(&path).unwrap(), second);
    }

    #[test]
    fn empty_roster_roundtrips() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");

        let original = Config::default();
        original.save(&path).unwrap();

        assert_eq!(Config::load(&path).unwrap(), original);
    }
}

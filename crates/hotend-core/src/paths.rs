//! Where Hotend keeps its data (PROTOCOL.md section 1).
//!
//! Every path that contains an id is built here, after the id has been checked to be a
//! canonical UUID, so no caller-supplied text can name a folder outside the data directory.

use std::path::{Path, PathBuf};

use crate::util::is_valid_id;

/// Environment variable that overrides the data directory.
pub const DATA_DIR_ENV: &str = "HOTEND_DATA_DIR";

/// Why a data path could not be produced.
#[derive(Debug, thiserror::Error)]
pub enum PathError {
    /// The platform reports no home directory; set `HOTEND_DATA_DIR`.
    #[error("no data directory can be determined on this system; set {DATA_DIR_ENV}")]
    NoDataDir,
    /// An id is not a canonical UUID.
    #[error("`{0}` is not a valid id")]
    InvalidId(String),
    /// A folder could not be created or resolved.
    #[error("cannot prepare {path}: {source}")]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
}

/// The root of Hotend's data on disk and the layout below it.
#[derive(Debug, Clone)]
pub struct DataDir {
    root: PathBuf,
}

impl DataDir {
    /// Resolves the data directory: `explicit` if given, else `HOTEND_DATA_DIR`, else the
    /// platform's per-user data folder for "Hotend". The folder is created and its path
    /// made absolute (the sandbox needs absolute paths).
    pub fn resolve(explicit: Option<&Path>) -> Result<Self, PathError> {
        let root = match explicit {
            Some(path) => path.to_path_buf(),
            None => match std::env::var_os(DATA_DIR_ENV).filter(|v| !v.is_empty()) {
                Some(path) => PathBuf::from(path),
                None => directories::ProjectDirs::from("", "", "Hotend")
                    .ok_or(PathError::NoDataDir)?
                    .data_dir()
                    .to_path_buf(),
            },
        };
        let io = |path: &Path| {
            let path = path.to_path_buf();
            move |source| PathError::Io { path, source }
        };
        std::fs::create_dir_all(&root).map_err(io(&root))?;
        let root = root.canonicalize().map_err(io(&root))?;
        for sub in ["projects", "jobs", "calibration"] {
            let dir = root.join(sub);
            std::fs::create_dir_all(&dir).map_err(io(&dir))?;
        }
        Ok(Self { root })
    }

    /// The data directory itself.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// `hotend.db`.
    pub fn db_path(&self) -> PathBuf {
        self.root.join("hotend.db")
    }

    /// `projects/<project-id>/`.
    pub fn project_dir(&self, project_id: &str) -> Result<PathBuf, PathError> {
        Ok(self.root.join("projects").join(checked(project_id)?))
    }

    /// `projects/<project-id>/versions/`.
    pub fn versions_dir(&self, project_id: &str) -> Result<PathBuf, PathError> {
        Ok(self.project_dir(project_id)?.join("versions"))
    }

    /// `projects/<project-id>/versions/<n>/`.
    pub fn version_dir(&self, project_id: &str, number: u32) -> Result<PathBuf, PathError> {
        Ok(self.versions_dir(project_id)?.join(number.to_string()))
    }

    /// `jobs/`.
    pub fn jobs_dir(&self) -> PathBuf {
        self.root.join("jobs")
    }

    /// `jobs/<job-id>/`.
    pub fn job_dir(&self, job_id: &str) -> Result<PathBuf, PathError> {
        Ok(self.jobs_dir().join(checked(job_id)?))
    }
}

fn checked(id: &str) -> Result<&str, PathError> {
    if is_valid_id(id) {
        Ok(id)
    } else {
        Err(PathError::InvalidId(crate::util::truncate(id, 64)))
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[test]
    fn layout_matches_the_protocol_and_ids_cannot_escape() {
        let tmp = tempfile::tempdir().unwrap();
        let data = DataDir::resolve(Some(&tmp.path().join("d"))).unwrap();
        assert!(data.root().is_absolute());
        assert!(data.jobs_dir().is_dir() && data.root().join("projects").is_dir());
        let id = crate::util::new_id();
        let version = data.version_dir(&id, 3).unwrap();
        assert!(version.ends_with(format!("projects/{id}/versions/3")));
        assert!(version.starts_with(data.root()));
        for bad in ["..", "../../etc", "", "a/b"] {
            assert!(matches!(
                data.project_dir(bad),
                Err(PathError::InvalidId(_))
            ));
            assert!(data.job_dir(bad).is_err());
        }
    }
}

//! Cleaning up after a core that did not get to: one that was killed, or lost power, while
//! jobs were running.
//!
//! Guarantees:
//! - Folders are removed only by a core that is alone in the data directory. Every core
//!   holds a shared lock on `hotend.lock` for as long as it is open; the sweep runs only
//!   while that lock can be taken exclusively, so the folders of another running core
//!   (the desktop application next to a `hotend` command) are never touched.
//! - Only job folders (`jobs/*`) and staging folders of unfinished builds
//!   (`projects/*/versions/.staging-*`) are removed. Links are removed, not followed.
//! - A sweep that removed something leaves one audit entry with the counts.
//! - A print job that such a core left `starting` or `printing` is ended as `failed`, with
//!   the reason, by the next core that is alone: the simulated printer lives inside the
//!   core's process, so the job can never report an end. A job is not touched while
//!   another core is open, because that core may be running it.

use std::fs::File;
use std::path::Path;

use serde_json::json;

use crate::paths::DataDir;
use crate::store::{NewAudit, Store};

/// The name of the lock file in the data directory.
pub const LOCK_FILE: &str = "hotend.lock";

/// The shared lock a core holds on its data directory. Dropping it releases the lock.
#[derive(Debug)]
pub struct DataLock {
    _file: File,
}

/// What a sweep removed.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Swept {
    pub jobs: u32,
    pub staging: u32,
}

/// Takes this core's lock on the data directory and, when no other core is using the
/// directory, removes what earlier cores left behind.
///
/// Never fails: a data directory whose lock file cannot be opened or locked is used
/// without a sweep (`None`), exactly as before.
pub fn lock_and_sweep(data: &DataDir, store: &Store) -> Option<DataLock> {
    let file = File::options()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(data.root().join(LOCK_FILE))
        .ok()?;
    if file.try_lock().is_ok() {
        let swept = sweep(data);
        if swept != Swept::default() {
            let _ = store.audit(NewAudit {
                project_id: None,
                run_id: None,
                kind: "job",
                name: "sweep".to_owned(),
                args_sha256: None,
                outcome: "ok".to_owned(),
                detail: json!({"job_folders": swept.jobs, "staging_folders": swept.staging,
                    "note": "left behind by a core that did not shut down"}),
            });
        }
        close_interrupted_prints(store);
        // The exclusive lock is given up before the shared one is taken. A core that
        // starts in between finds nothing of this one to remove: no job has run yet.
        let _ = file.unlock();
    }
    file.try_lock_shared().ok()?;
    Some(DataLock { _file: file })
}

/// Why a job that was running when its core went away is recorded as failed.
const INTERRUPTED: &str = "Hotend closed while the simulated printer was running this job. The \
    simulated printer does not outlive the program, so the job did not finish; propose it again \
    to run it again";

/// Ends the print jobs an earlier core left running, each with an audit entry.
fn close_interrupted_prints(store: &Store) {
    for job in store
        .close_interrupted_prints(INTERRUPTED)
        .unwrap_or_default()
    {
        let _ = store.audit(NewAudit {
            project_id: Some(job.project_id.clone()),
            run_id: None,
            kind: "print",
            name: job.status.clone(),
            args_sha256: Some(job.slice_sha256.clone()),
            outcome: "ok".to_owned(),
            detail: json!({"job": job.id, "delivery": job.delivery,
                "simulated": job.detail["simulated"], "message": INTERRUPTED,
                "note": "left running by a core that did not shut down"}),
        });
    }
}

/// Removes every job folder and every staging folder below the data directory.
fn sweep(data: &DataDir) -> Swept {
    let mut swept = Swept {
        jobs: remove_entries(&data.jobs_dir(), |_| true),
        staging: 0,
    };
    let Ok(projects) = std::fs::read_dir(data.root().join("projects")) else {
        return swept;
    };
    for project in projects.flatten() {
        swept.staging += remove_entries(&project.path().join("versions"), |name| {
            name.starts_with(".staging-")
        });
    }
    swept
}

/// Removes the entries of `dir` whose name `wanted` accepts and returns how many went.
fn remove_entries(dir: &Path, wanted: impl Fn(&str) -> bool) -> u32 {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return 0;
    };
    let mut removed = 0;
    for entry in entries.flatten() {
        if !wanted(&entry.file_name().to_string_lossy()) {
            continue;
        }
        let path = entry.path();
        // `file_type` does not follow links, and neither does `remove_dir_all`.
        let gone = match entry.file_type() {
            Ok(kind) if kind.is_dir() => std::fs::remove_dir_all(&path),
            _ => std::fs::remove_file(&path),
        };
        removed += u32::from(gone.is_ok());
    }
    removed
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    fn left_behind(root: &Path) -> (std::path::PathBuf, std::path::PathBuf, std::path::PathBuf) {
        let job = root.join("jobs/0190aaaa-0000-7000-8000-000000000001");
        std::fs::create_dir_all(job.join("out")).unwrap();
        std::fs::write(job.join("out/model.step"), b"half a model").unwrap();
        let versions = root.join("projects/0190aaaa-0000-7000-8000-000000000002/versions");
        let staging = versions.join(".staging-0190aaaa-0000-7000-8000-000000000003");
        std::fs::create_dir_all(staging.join("views")).unwrap();
        let version = versions.join("1");
        std::fs::create_dir_all(&version).unwrap();
        std::fs::write(version.join("model.py"), b"kept").unwrap();
        (job, staging, version)
    }

    #[test]
    fn a_core_alone_in_the_data_directory_removes_what_was_left_behind() {
        let tmp = tempfile::tempdir().unwrap();
        let data = DataDir::resolve(Some(tmp.path())).unwrap();
        let store = Store::open(&data.db_path()).unwrap();
        let (job, staging, version) = left_behind(data.root());
        // A link in the jobs folder is removed, not followed.
        let outside = tmp.path().join("outside");
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("keep.txt"), b"keep").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(&outside, data.jobs_dir().join("link")).unwrap();

        let lock = lock_and_sweep(&data, &store).unwrap();
        assert!(!job.exists() && !staging.exists());
        assert!(version.join("model.py").exists());
        assert!(outside.join("keep.txt").exists());
        assert_eq!(std::fs::read_dir(data.jobs_dir()).unwrap().count(), 0);
        let audit = store.list_audit(None, 10).unwrap();
        assert_eq!(audit.len(), 1);
        assert_eq!(
            (audit[0].kind.as_str(), audit[0].name.as_str()),
            ("job", "sweep")
        );
        assert_eq!(audit[0].detail["staging_folders"], 1);
        assert_eq!(
            audit[0].detail["job_folders"],
            if cfg!(unix) { 2 } else { 1 }
        );

        // While that core is open, a second one leaves its folders alone.
        let (job, staging, _) = left_behind(data.root());
        let second = lock_and_sweep(&data, &store).unwrap();
        assert!(job.exists() && staging.exists());
        drop(second);
        drop(lock);

        // With every core gone, the next one cleans up, and a clean start is not audited.
        // A child that another test thread forked while a lock above was held shares that
        // lock until it calls exec, which closes the descriptor; until then the next core
        // rightly sees someone else in the directory. That is the test process racing
        // itself, so wait for such children to let go (a skipped sweep is not audited).
        let mut attempt = 0;
        let third = loop {
            let third = lock_and_sweep(&data, &store).unwrap();
            if !job.exists() || attempt == 50 {
                break third;
            }
            drop(third);
            attempt += 1;
            std::thread::sleep(std::time::Duration::from_millis(20));
        };
        assert!(!job.exists() && !staging.exists());
        drop(third);
        drop(lock_and_sweep(&data, &store).unwrap());
        assert_eq!(store.list_audit(None, 10).unwrap().len(), 2);
    }
}

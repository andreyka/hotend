//! How much the job folder holds, measured by the launcher from outside the sandbox.
//!
//! The per-file size limit says nothing about the number of files, so the supervisor adds
//! up the whole folder. The tree it walks was written by the confined program: the walk
//! never follows a symbolic link, never trusts a path string (every step is relative to an
//! open directory), and stops as soon as a cap is passed, so a hostile tree cannot make it
//! slow or lead it elsewhere.

use std::ffi::CStr;
use std::os::fd::{AsRawFd, BorrowedFd};

/// Files and folders a job may hold. Beyond this the walk itself would become the cost.
pub(crate) const MAX_ENTRIES: u64 = 100_000;
/// Folder levels below the job folder. Each level holds one descriptor open during a walk.
const MAX_DEPTH: usize = 64;

/// What a walk found.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Usage {
    /// The folder is within its caps and holds this many bytes.
    Within(u64),
    /// More bytes than the cap.
    TooManyBytes,
    /// More than [`MAX_ENTRIES`] files and folders.
    TooManyEntries,
    /// Folders nested deeper than the walk goes.
    TooDeep,
}

struct Dir(*mut libc::DIR);

impl Dir {
    /// Opens the folder `name` below the open folder `parent`, refusing symbolic links.
    fn open_at(parent: libc::c_int, name: &CStr) -> Option<Self> {
        // SAFETY: `name` is a valid C string; the results are checked. On success
        // `fdopendir` owns the descriptor.
        unsafe {
            let fd = libc::openat(
                parent,
                name.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            );
            if fd < 0 {
                return None;
            }
            let dir = libc::fdopendir(fd);
            if dir.is_null() {
                libc::close(fd);
                return None;
            }
            Some(Self(dir))
        }
    }
}

impl Drop for Dir {
    fn drop(&mut self) {
        // SAFETY: the stream was opened by `open_at` and is closed exactly once.
        unsafe { libc::closedir(self.0) };
    }
}

/// Adds up the folder behind `root`. A file counts with its length or the space it
/// occupies, whichever is larger, so neither a sparse file nor preallocation hides bytes.
/// Entries that vanish during the walk are skipped.
pub(crate) fn usage(root: BorrowedFd<'_>, byte_cap: u64) -> Usage {
    let Some(top) = Dir::open_at(root.as_raw_fd(), c".") else {
        return Usage::Within(0);
    };
    let mut stack = vec![top];
    let mut bytes: u64 = 0;
    let mut entries: u64 = 0;
    while let Some(dir) = stack.last().map(|dir| dir.0) {
        // SAFETY: `dir` is an open stream owned by `stack`; the entry it returns is valid
        // until the next call on the same stream, and is used only before that.
        let entry = unsafe { libc::readdir(dir) };
        if entry.is_null() {
            stack.pop();
            continue;
        }
        // SAFETY: `d_name` is a NUL-terminated string inside the entry.
        let name = unsafe { CStr::from_ptr((*entry).d_name.as_ptr()) };
        if matches!(name.to_bytes(), b"." | b"..") {
            continue;
        }
        entries += 1;
        if entries > MAX_ENTRIES {
            return Usage::TooManyEntries;
        }
        // SAFETY: `stat` is a plain buffer the kernel fills; the name is valid (see above).
        let mut stat: libc::stat = unsafe { std::mem::zeroed() };
        let found = unsafe {
            libc::fstatat(
                libc::dirfd(dir),
                name.as_ptr(),
                &mut stat,
                libc::AT_SYMLINK_NOFOLLOW,
            )
        };
        if found != 0 {
            continue;
        }
        let occupied = (stat.st_blocks.max(0) as u64).saturating_mul(512);
        match stat.st_mode & libc::S_IFMT {
            libc::S_IFREG => {
                bytes = bytes.saturating_add(occupied.max(stat.st_size.max(0) as u64));
            }
            libc::S_IFDIR => {
                bytes = bytes.saturating_add(occupied);
                if stack.len() >= MAX_DEPTH {
                    return Usage::TooDeep;
                }
                // SAFETY: `dir` is still open; `name` is copied by the kernel.
                if let Some(child) = Dir::open_at(unsafe { libc::dirfd(dir) }, name) {
                    stack.push(child);
                }
            }
            _ => {}
        }
        if bytes > byte_cap {
            return Usage::TooManyBytes;
        }
    }
    Usage::Within(bytes)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use std::os::fd::AsFd;

    use super::*;

    fn usage_of(path: &std::path::Path, cap: u64) -> Usage {
        let root = std::fs::File::open(path).unwrap();
        usage(root.as_fd(), cap)
    }

    #[test]
    fn files_in_nested_folders_are_added_up() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("a/b")).unwrap();
        std::fs::write(dir.path().join("one"), vec![0u8; 300_000]).unwrap();
        std::fs::write(dir.path().join("a/b/two"), vec![0u8; 500_000]).unwrap();
        let Usage::Within(bytes) = usage_of(dir.path(), u64::MAX) else {
            panic!("the folder is within an unlimited cap");
        };
        assert!((800_000..1_000_000).contains(&bytes), "{bytes}");
        assert_eq!(usage_of(dir.path(), 700_000), Usage::TooManyBytes);
    }

    #[test]
    fn sparse_files_count_with_their_length_and_links_are_not_followed() {
        let dir = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("big"), vec![0u8; 400_000]).unwrap();
        std::os::unix::fs::symlink(outside.path(), dir.path().join("link")).unwrap();
        let sparse = std::fs::File::create(dir.path().join("sparse")).unwrap();
        sparse.set_len(1 << 20).unwrap();
        let Usage::Within(bytes) = usage_of(dir.path(), u64::MAX) else {
            panic!("the folder is within an unlimited cap");
        };
        assert!(((1 << 20)..(1 << 20) + 100_000).contains(&bytes), "{bytes}");
    }

    #[test]
    fn a_tree_that_is_too_deep_is_reported() {
        let dir = tempfile::tempdir().unwrap();
        let mut path = dir.path().to_path_buf();
        for _ in 0..MAX_DEPTH + 2 {
            path.push("d");
        }
        std::fs::create_dir_all(&path).unwrap();
        assert_eq!(usage_of(dir.path(), u64::MAX), Usage::TooDeep);
    }
}

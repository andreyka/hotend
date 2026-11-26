//! Reading files a sandboxed child left in its job folder. Everything here treats the folder
//! as hostile: a path the child reported is only a claim.
//!
//! [`resolve_inside`] guarantees that the returned path is a regular file below the root,
//! reached without following any symbolic link, with a single hard link and at most the
//! given size. [`copy_capped`] and [`read_capped`] never read more than the cap even if the
//! file grows after it was checked.

use std::fs::File;
use std::io::Read;
use std::path::{Component, Path, PathBuf};

use sha2::{Digest, Sha256};

/// Why a file from a job folder was refused.
#[derive(Debug, thiserror::Error)]
pub enum ImportError {
    /// The reported path is not a plain relative path.
    #[error("the path `{path}` is not a plain relative path inside the job folder")]
    NotRelative { path: String },
    /// A component of the path is a symbolic link.
    #[error("`{path}` goes through a symbolic link")]
    Symlink { path: String },
    /// The path does not name a regular file.
    #[error("`{path}` is missing or not a regular file")]
    NotAFile { path: String },
    /// The file has more than one name; it may alias a file elsewhere.
    #[error("`{path}` has several hard links")]
    HardLinked { path: String },
    /// The resolved location is outside the root.
    #[error("`{path}` resolves to a location outside the job folder")]
    Escapes { path: String },
    /// The file is larger than the cap for its kind.
    #[error("`{path}` is {size} bytes; the limit is {limit}")]
    TooLarge { path: String, size: u64, limit: u64 },
    /// Reading or writing failed.
    #[error("cannot copy `{path}`: {source}")]
    Io {
        path: String,
        source: std::io::Error,
    },
}

fn shown(path: &str) -> String {
    crate::util::truncate(path, 120)
}

/// Resolves a child-reported relative path below `root` and checks the file it names.
pub fn resolve_inside(root: &Path, relative: &str, max_bytes: u64) -> Result<PathBuf, ImportError> {
    let not_relative = || ImportError::NotRelative {
        path: shown(relative),
    };
    if relative.is_empty() || relative.len() > 240 || relative.contains(['\\', '\0', ':']) {
        return Err(not_relative());
    }
    let mut current = root.to_path_buf();
    let mut components = Path::new(relative).components().peekable();
    if components.peek().is_none() {
        return Err(not_relative());
    }
    while let Some(component) = components.next() {
        let Component::Normal(name) = component else {
            return Err(not_relative());
        };
        current.push(name);
        let not_a_file = || ImportError::NotAFile {
            path: shown(relative),
        };
        let metadata = std::fs::symlink_metadata(&current).map_err(|_| not_a_file())?;
        if metadata.file_type().is_symlink() {
            return Err(ImportError::Symlink {
                path: shown(relative),
            });
        }
        let last = components.peek().is_none();
        if last {
            if !metadata.is_file() {
                return Err(not_a_file());
            }
            #[cfg(unix)]
            {
                use std::os::unix::fs::MetadataExt;
                if metadata.nlink() > 1 {
                    return Err(ImportError::HardLinked {
                        path: shown(relative),
                    });
                }
            }
            if metadata.len() > max_bytes {
                return Err(ImportError::TooLarge {
                    path: shown(relative),
                    size: metadata.len(),
                    limit: max_bytes,
                });
            }
        } else if !metadata.is_dir() {
            return Err(not_a_file());
        }
    }
    // Belt and braces: the fully resolved path must still be below the resolved root.
    let escapes = || ImportError::Escapes {
        path: shown(relative),
    };
    let real_root = root.canonicalize().map_err(|_| escapes())?;
    let real = current.canonicalize().map_err(|_| escapes())?;
    if !real.starts_with(&real_root) {
        return Err(escapes());
    }
    Ok(current)
}

/// Opens a file for reading without following a symbolic link at the last component.
fn open_no_follow(path: &Path) -> std::io::Result<File> {
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW);
    }
    options.open(path)
}

/// Reads a checked file into memory, refusing to read past `max_bytes`.
pub fn read_capped(root: &Path, relative: &str, max_bytes: u64) -> Result<Vec<u8>, ImportError> {
    let path = resolve_inside(root, relative, max_bytes)?;
    let io = |source| ImportError::Io {
        path: shown(relative),
        source,
    };
    let mut bytes = Vec::new();
    open_no_follow(&path)
        .map_err(io)?
        .take(max_bytes + 1)
        .read_to_end(&mut bytes)
        .map_err(io)?;
    if bytes.len() as u64 > max_bytes {
        return Err(ImportError::TooLarge {
            path: shown(relative),
            size: bytes.len() as u64,
            limit: max_bytes,
        });
    }
    Ok(bytes)
}

/// Copies a checked file to `destination` (created new, parents created) and returns the
/// number of bytes and the SHA-256 of what was copied.
pub fn copy_capped(
    root: &Path,
    relative: &str,
    destination: &Path,
    max_bytes: u64,
) -> Result<(u64, String), ImportError> {
    let path = resolve_inside(root, relative, max_bytes)?;
    let io = |source| ImportError::Io {
        path: shown(relative),
        source,
    };
    if let Some(parent) = destination.parent() {
        std::fs::create_dir_all(parent).map_err(io)?;
    }
    let mut source = open_no_follow(&path).map_err(io)?.take(max_bytes + 1);
    let mut target = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(destination)
        .map_err(io)?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; 64 * 1024];
    let mut total = 0u64;
    loop {
        let read = source.read(&mut buffer).map_err(io)?;
        if read == 0 {
            break;
        }
        total += read as u64;
        if total > max_bytes {
            drop(target);
            let _ = std::fs::remove_file(destination);
            return Err(ImportError::TooLarge {
                path: shown(relative),
                size: total,
                limit: max_bytes,
            });
        }
        hasher.update(&buffer[..read]);
        std::io::Write::write_all(&mut target, &buffer[..read]).map_err(io)?;
    }
    Ok((total, crate::util::hex(&hasher.finalize())))
}

/// SHA-256 of a file of ours, streamed.
pub fn sha256_file(path: &Path) -> std::io::Result<String> {
    let mut file = File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(crate::util::hex(&hasher.finalize()))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    fn job() -> (tempfile::TempDir, PathBuf, PathBuf) {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("job");
        std::fs::create_dir_all(root.join("views")).unwrap();
        std::fs::write(root.join("model.step"), b"ISO-10303-21;").unwrap();
        std::fs::write(root.join("views/iso.png"), b"png").unwrap();
        let secret = tmp.path().join("secret.txt");
        std::fs::write(&secret, b"top secret").unwrap();
        (tmp, root, secret)
    }

    #[test]
    fn plain_files_inside_the_folder_are_accepted() {
        let (_tmp, root, _) = job();
        assert_eq!(
            read_capped(&root, "model.step", 100).unwrap(),
            b"ISO-10303-21;"
        );
        assert_eq!(read_capped(&root, "views/iso.png", 100).unwrap(), b"png");
    }

    #[test]
    fn paths_that_are_not_plain_and_relative_are_refused() {
        let (_tmp, root, secret) = job();
        let absolute = secret.to_string_lossy().into_owned();
        for bad in [
            "",
            "../secret.txt",
            "views/../../secret.txt",
            "./model.step",
            "views//../model.step",
            absolute.as_str(),
            "views\\iso.png",
            "C:model.step",
            "model.step\0",
        ] {
            assert!(
                matches!(
                    resolve_inside(&root, bad, 100),
                    Err(ImportError::NotRelative { .. })
                ),
                "{bad:?} was not refused as non-relative"
            );
        }
        assert!(matches!(
            resolve_inside(&root, "views", 100),
            Err(ImportError::NotAFile { .. })
        ));
        assert!(matches!(
            resolve_inside(&root, "missing.step", 100),
            Err(ImportError::NotAFile { .. })
        ));
        assert!(matches!(
            resolve_inside(&root, "model.step/x", 100),
            Err(ImportError::NotAFile { .. })
        ));
    }

    #[cfg(unix)]
    #[test]
    fn symlinks_and_hard_links_are_refused() {
        let (tmp, root, secret) = job();
        std::os::unix::fs::symlink(&secret, root.join("link.step")).unwrap();
        std::os::unix::fs::symlink(tmp.path(), root.join("up")).unwrap();
        std::os::unix::fs::symlink(root.join("model.step"), root.join("inner.step")).unwrap();
        for bad in ["link.step", "up/secret.txt", "inner.step"] {
            assert!(
                matches!(
                    resolve_inside(&root, bad, 100),
                    Err(ImportError::Symlink { .. })
                ),
                "{bad} was not refused as a symlink"
            );
        }
        std::fs::hard_link(&secret, root.join("hard.step")).unwrap();
        assert!(matches!(
            resolve_inside(&root, "hard.step", 100),
            Err(ImportError::HardLinked { .. })
        ));
        let out = tmp.path().join("out.step");
        assert!(copy_capped(&root, "link.step", &out, 100).is_err());
        assert!(!out.exists());
    }

    #[test]
    fn size_caps_hold_and_copies_report_their_hash() {
        let (tmp, root, _) = job();
        assert!(matches!(
            resolve_inside(&root, "model.step", 5),
            Err(ImportError::TooLarge {
                size: 13,
                limit: 5,
                ..
            })
        ));
        let out = tmp.path().join("deep/out.step");
        let (bytes, hash) = copy_capped(&root, "model.step", &out, 13).unwrap();
        assert_eq!(bytes, 13);
        assert_eq!(hash, crate::util::sha256_hex(b"ISO-10303-21;"));
        assert_eq!(hash, sha256_file(&out).unwrap());
        // An existing destination is never overwritten.
        assert!(copy_capped(&root, "model.step", &out, 13).is_err());
    }
}

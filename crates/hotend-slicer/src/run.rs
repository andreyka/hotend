//! Runs a slicer executable inside the sandbox.
//!
//! Guarantee: every program this crate starts goes through [`hotend_sandbox::command`] with
//! the `slicer` profile, so it has no network, writes only below its job folder, is killed at
//! the wall-clock limit and gets a `HOME` inside the job folder. Its stdout and stderr are
//! drained into bounded buffers, so a talkative or hostile program cannot exhaust memory.

use std::ffi::OsString;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Stdio;

use hotend_sandbox::{Limits, Policy, Profile};

use crate::error::SlicerError;

/// Bytes kept of each output stream: the last ones, where errors are.
const STREAM_TAIL_BYTES: usize = 64 * 1024;

/// What one sandboxed run left behind.
#[derive(Debug)]
pub(crate) struct RunOutput {
    /// Exit code of the launcher; `None` if a signal ended it.
    pub code: Option<i32>,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

impl RunOutput {
    /// Both streams as text, stdout first.
    pub fn text(&self) -> String {
        let mut text = String::from_utf8_lossy(&self.stdout).into_owned();
        text.push('\n');
        text.push_str(&String::from_utf8_lossy(&self.stderr));
        text
    }
}

/// Runs `program` with `args` under the slicer profile and waits for it.
pub(crate) fn run_sandboxed(
    job_dir: &Path,
    read_only: &[PathBuf],
    limits: Limits,
    program: &Path,
    args: &[OsString],
) -> Result<RunOutput, SlicerError> {
    let mut policy = Policy::new(Profile::Slicer, job_dir).limits(limits);
    for path in read_only {
        if path.exists() {
            policy = policy.read_only(path);
        }
    }
    let mut command = hotend_sandbox::command(&policy, program, args)?;
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command
        .spawn()
        .map_err(|e| SlicerError::io("start the sandbox launcher for", program, e))?;
    let stdout = child.stdout.take().map(drain);
    let stderr = child.stderr.take().map(drain);
    let status = child
        .wait()
        .map_err(|e| SlicerError::io("wait for", program, e))?;
    let collect = |handle: Option<std::thread::JoinHandle<Vec<u8>>>| {
        handle.and_then(|h| h.join().ok()).unwrap_or_default()
    };
    Ok(RunOutput {
        code: status.code(),
        stdout: collect(stdout),
        stderr: collect(stderr),
    })
}

/// Reads a stream to its end on a thread and keeps only its last [`STREAM_TAIL_BYTES`].
fn drain(mut stream: impl Read + Send + 'static) -> std::thread::JoinHandle<Vec<u8>> {
    std::thread::spawn(move || {
        let mut kept: Vec<u8> = Vec::new();
        let mut chunk = [0u8; 8192];
        loop {
            match stream.read(&mut chunk) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    kept.extend_from_slice(&chunk[..n]);
                    if kept.len() > 2 * STREAM_TAIL_BYTES {
                        let cut = kept.len() - STREAM_TAIL_BYTES;
                        kept.drain(..cut);
                    }
                }
            }
        }
        if kept.len() > STREAM_TAIL_BYTES {
            let cut = kept.len() - STREAM_TAIL_BYTES;
            kept.drain(..cut);
        }
        kept
    })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[test]
    fn drain_keeps_only_the_tail() {
        let mut data = vec![b'a'; 5 * STREAM_TAIL_BYTES];
        data.extend_from_slice(b"the end");
        let kept = drain(std::io::Cursor::new(data)).join().unwrap();
        assert_eq!(kept.len(), STREAM_TAIL_BYTES);
        assert!(kept.ends_with(b"the end"));
        let small = drain(std::io::Cursor::new(b"short".to_vec()))
            .join()
            .unwrap();
        assert_eq!(small, b"short");
    }
}

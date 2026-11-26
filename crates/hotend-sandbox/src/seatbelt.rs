//! The macOS Seatbelt profile (`sandbox-exec`), as text.
//!
//! UNTESTED ON REAL HARDWARE. This profile was written from Apple's sandbox profile language
//! as publicly described and has never been loaded by a Mac. It is deny-by-default, so the
//! likely failure is a program that does not start, not one that escapes; but nothing here
//! is verified. The backend that uses it refuses to run unless explicitly enabled (see
//! `macos.rs` and docs/notes/sandbox.md).
//!
//! The module is compiled on every platform so the text generation is unit-tested on Linux.
//!
//! Paths never appear in the profile text. They are passed as `-D NAME=value` parameters and
//! referenced with `(param "NAME")`, so a path cannot inject rules.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

/// A profile and the parameters `sandbox-exec` needs with it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SeatbeltProfile {
    /// The profile source, for `sandbox-exec -p`.
    pub text: String,
    /// `(name, value)` pairs, each passed as `-D name=value`.
    pub parameters: Vec<(String, OsString)>,
}

const HEADER: &str = r#"(version 1)
; Everything that is not allowed below is denied: network, Mach services, IPC, other
; processes, and every file outside the listed paths.
(deny default)

; Run programs and threads; signal only processes inside this sandbox.
(allow process-exec)
(allow process-fork)
(allow signal (target same-sandbox))
(allow process-info* (target same-sandbox))
(allow sysctl-read)

; What a dynamically linked program needs in order to start.
(allow file-read-metadata)
(allow file-read*
    (subpath "/usr")
    (subpath "/bin")
    (subpath "/sbin")
    (subpath "/System")
    (subpath "/Library/Apple")
    (subpath "/private/var/db/dyld")
    (subpath "/private/var/db/timezone")
    (literal "/private/etc/localtime")
    (literal "/dev/null")
    (literal "/dev/zero")
    (literal "/dev/random")
    (literal "/dev/urandom"))
(allow file-write-data (literal "/dev/null"))

; The job folder is the only writable location.
(allow file-read* file-write* (subpath (param "JOB_DIR")))
"#;

const FOOTER: &str = r#"
; Stated again so that no later edit of the rules above can open the network by accident.
(deny network*)
"#;

/// Builds the profile for a job folder and its read-only paths. Both must be canonical
/// (on macOS `/tmp` is a symlink to `/private/tmp`, and rules match the real path).
pub fn profile(job: &Path, read_only: &[PathBuf]) -> SeatbeltProfile {
    let mut text = String::from(HEADER);
    let mut parameters = vec![("JOB_DIR".to_owned(), job.as_os_str().to_owned())];
    if !read_only.is_empty() {
        text.push_str("\n; Read-only runtime paths from the policy.\n");
    }
    for (index, path) in read_only.iter().enumerate() {
        let name = format!("READ_ONLY_{index}");
        text.push_str(&format!(
            "(allow file-read* (subpath (param \"{name}\")))\n"
        ));
        parameters.push((name, path.as_os_str().to_owned()));
    }
    text.push_str(FOOTER);
    SeatbeltProfile { text, parameters }
}

impl SeatbeltProfile {
    /// The arguments that follow `sandbox-exec` and precede the program.
    pub fn arguments(&self) -> Vec<OsString> {
        let mut arguments: Vec<OsString> = vec!["-p".into(), self.text.clone().into()];
        for (name, value) in &self.parameters {
            let mut definition = OsString::from(format!("{name}="));
            definition.push(value);
            arguments.push("-D".into());
            arguments.push(definition);
        }
        arguments
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn profile_denies_by_default_and_never_allows_network() {
        let profile = profile(Path::new("/jobs/1"), &[PathBuf::from("/opt/python")]);
        assert!(profile.text.starts_with("(version 1)"));
        assert!(profile.text.contains("(deny default)"));
        assert!(profile.text.contains("(deny network*)"));
        for line in profile
            .text
            .lines()
            .filter(|l| l.trim_start().starts_with("(allow"))
        {
            assert!(!line.contains("network"), "network allowed: {line}");
            assert!(!line.contains("mach"), "Mach service allowed: {line}");
            assert!(
                !line.contains("file-write*") || line.contains("JOB_DIR"),
                "{line}"
            );
        }
        let open = profile.text.matches('(').count();
        assert_eq!(
            open,
            profile.text.matches(')').count(),
            "unbalanced profile"
        );
    }

    #[test]
    fn paths_are_parameters_and_cannot_inject_rules() {
        let hostile = Path::new("/jobs/\")) (allow network*) (allow file-read* (subpath \"/");
        let profile = profile(hostile, &[PathBuf::from("/a b"), PathBuf::from("/c\"d")]);
        assert!(!profile.text.contains("/jobs/"));
        assert!(!profile.text.contains("allow network"));
        assert_eq!(profile.parameters.len(), 3);
        assert_eq!(profile.parameters[0].0, "JOB_DIR");
        assert_eq!(profile.parameters[0].1, hostile.as_os_str());
        assert!(profile.text.contains("(param \"READ_ONLY_1\")"));
        let arguments = profile.arguments();
        assert_eq!(arguments.len(), 2 + 2 * 3);
        assert_eq!(arguments[0], OsString::from("-p"));
        assert_eq!(arguments[2], OsString::from("-D"));
        assert_eq!(arguments[5], OsString::from("READ_ONLY_0=/a b"));
    }
}

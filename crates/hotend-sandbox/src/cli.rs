//! Command line of the `hotend-sandbox` launcher (PROTOCOL.md section 7).

use std::ffi::OsString;
use std::path::PathBuf;

use crate::backend::platform_backend;
use crate::error::{EXIT_SANDBOX_FAILED, SandboxError, report};
use crate::policy::{Policy, Profile};

pub const USAGE: &str = "\
usage: hotend-sandbox run --profile <engine|agent|slicer> --job <dir>
           [--ro <path>]... [--env <NAME>]...
           [--wall-seconds N] [--cpu-seconds N] [--memory-mb N]
           [--max-processes N] [--max-file-mb N] [--max-disk-mb N]
           [--max-open-files N] [--no-namespaces] -- <program> [args]...
       hotend-sandbox probe

exit codes: the program's own code; 128+N when it died from signal N;
124 wall-clock limit; 125 sandbox not applied (program not started);
126 program not executable; 127 program not found";

/// A parsed `run` command.
#[derive(Debug)]
pub struct RunCommand {
    pub policy: Policy,
    pub program: OsString,
    pub args: Vec<OsString>,
}

/// Parses the arguments that follow `run`.
pub fn parse(args: impl IntoIterator<Item = OsString>) -> Result<RunCommand, SandboxError> {
    let mut args = args.into_iter();
    let mut profile: Option<Profile> = None;
    let mut job: Option<PathBuf> = None;
    let mut read_only = Vec::new();
    let mut env_allowlist = Vec::new();
    let mut limits: Vec<(String, u64)> = Vec::new();
    let mut namespaces = true;
    let mut rest: Option<Vec<OsString>> = None;

    while let Some(arg) = args.next() {
        let flag = arg
            .to_str()
            .ok_or_else(|| invalid("options must be valid UTF-8"))?
            .to_owned();
        let mut value = |what: &str| {
            args.next()
                .ok_or_else(|| invalid(&format!("{flag} needs {what}")))
        };
        match flag.as_str() {
            "--profile" => profile = Some(utf8(value("a profile name")?)?.parse()?),
            "--job" => job = Some(PathBuf::from(value("a directory")?)),
            "--ro" => read_only.push(PathBuf::from(value("a path")?)),
            "--env" => env_allowlist.push(utf8(value("a variable name")?)?),
            "--wall-seconds" | "--cpu-seconds" | "--memory-mb" | "--max-processes"
            | "--max-file-mb" | "--max-disk-mb" | "--max-open-files" => {
                let text = utf8(value("a number")?)?;
                let number = text
                    .parse::<u64>()
                    .map_err(|_| invalid(&format!("{flag} needs a whole number, got `{text}`")))?;
                limits.push((flag.clone(), number));
            }
            "--no-namespaces" => namespaces = false,
            "--" => {
                rest = Some(args.collect());
                break;
            }
            other => return Err(invalid(&format!("unknown option `{other}`"))),
        }
    }

    let profile = profile.ok_or_else(|| invalid("--profile is required"))?;
    let job = job.ok_or_else(|| invalid("--job is required"))?;
    let mut rest = rest
        .ok_or_else(|| invalid("`--` followed by the program is required"))?
        .into_iter();
    let program = rest
        .next()
        .ok_or_else(|| invalid("no program given after `--`"))?;

    let mut policy = Policy::new(profile, job);
    policy.read_only = read_only;
    policy.env_allowlist = env_allowlist;
    policy.namespaces = namespaces;
    for (flag, number) in limits {
        let field = match flag.as_str() {
            "--wall-seconds" => &mut policy.limits.wall_seconds,
            "--cpu-seconds" => &mut policy.limits.cpu_seconds,
            "--memory-mb" => &mut policy.limits.memory_mb,
            "--max-processes" => &mut policy.limits.max_processes,
            "--max-file-mb" => &mut policy.limits.max_file_mb,
            "--max-disk-mb" => &mut policy.limits.max_disk_mb,
            _ => &mut policy.limits.max_open_files,
        };
        *field = number;
    }
    Ok(RunCommand {
        policy,
        program,
        args: rest.collect(),
    })
}

fn invalid(message: &str) -> SandboxError {
    SandboxError::InvalidPolicy(message.to_owned())
}

fn utf8(value: OsString) -> Result<String, SandboxError> {
    value
        .into_string()
        .map_err(|_| invalid("option values must be valid UTF-8"))
}

/// Entry point of the launcher binary. Returns the process exit code.
pub fn main(args: impl IntoIterator<Item = OsString>) -> i32 {
    use std::io::Write;
    let mut args = args.into_iter();
    let _argv0 = args.next();
    match args.next().as_deref().and_then(|s| s.to_str()) {
        Some("probe") => {
            let printed = serde_json::to_string_pretty(&platform_backend().probe())
                .map_err(|e| e.to_string())
                .and_then(|json| writeln!(std::io::stdout(), "{json}").map_err(|e| e.to_string()));
            match printed {
                Ok(()) => 0,
                Err(e) => {
                    report(format_args!("cannot print the probe report: {e}"));
                    1
                }
            }
        }
        Some("run") => {
            let result = parse(args).and_then(|command| {
                platform_backend().run(&command.policy, &command.program, &command.args)
            });
            match result {
                Ok(code) => code,
                Err(error) => {
                    report(&error);
                    if matches!(error, SandboxError::InvalidPolicy(_)) {
                        let _ = writeln!(std::io::stderr(), "{USAGE}");
                    }
                    error.exit_code()
                }
            }
        }
        Some("--help" | "-h" | "help") => {
            let _ = writeln!(std::io::stdout(), "{USAGE}");
            0
        }
        _ => {
            let _ = writeln!(std::io::stderr(), "{USAGE}");
            EXIT_SANDBOX_FAILED
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    fn parse_strs(args: &[&str]) -> Result<RunCommand, SandboxError> {
        parse(args.iter().map(OsString::from))
    }

    #[test]
    fn minimal_command_uses_profile_defaults() {
        let command = parse_strs(&["--profile", "engine", "--job", "/j", "--", "p", "a"]).unwrap();
        assert_eq!(command.policy.limits, Profile::Engine.default_limits());
        assert!(command.policy.namespaces);
        assert_eq!(command.program, OsString::from("p"));
    }

    #[test]
    fn malformed_commands_are_rejected() {
        for args in [
            &["--job", "/j", "--", "p"][..],
            &["--profile", "engine", "--", "p"],
            &["--profile", "engine", "--job", "/j"],
            &["--profile", "engine", "--job", "/j", "--"],
            &["--profile", "root", "--job", "/j", "--", "p"],
            &["--profile", "engine", "--job", "/j", "--network", "--", "p"],
            &[
                "--profile",
                "engine",
                "--job",
                "/j",
                "--memory-mb",
                "lots",
                "--",
                "p",
            ],
            &["--profile", "engine", "--job"],
        ] {
            assert!(parse_strs(args).is_err(), "{args:?} must be rejected");
        }
    }
}

//! `hotend snapshot`: names for versions of a project. Take the model as it is at any time,
//! keep iterating, export that state's STEP and 3MF files (and the exact file of its print)
//! later, and go back to it without losing the newer versions.
//!
//! Each subcommand is one call into the core (`snapshot_create`, `snapshots_list`,
//! `snapshot_export`, `snapshot_restore`, `snapshot_delete`).

use std::path::PathBuf;

use clap::Subcommand;
use hotend_core::{SecretBackend, Snapshot, SnapshotExport};

use crate::error::CliError;
use crate::{ProjectArg, app, render, table};

#[derive(Debug, Subcommand)]
pub enum SnapshotCommand {
    /// Name a version (default: the current one) so you can export it or go back to it
    /// later. Changes no version.
    Create {
        /// One plain line of at most 80 characters, unique in the project.
        name: String,
        /// The version to name (default: the current one).
        #[arg(long)]
        version: Option<u32>,
        /// One line about this state.
        #[arg(long, default_value = "")]
        note: String,
        #[command(flatten)]
        project: ProjectArg,
        #[arg(long)]
        json: bool,
    },
    /// List the project's snapshots, oldest first.
    List {
        #[command(flatten)]
        project: ProjectArg,
        #[arg(long)]
        json: bool,
    },
    /// Write a snapshot's STEP and 3MF files, the exact sliced file of its print when its
    /// version was printed, and snapshot.json into a new folder inside FOLDER.
    Export {
        name: String,
        /// An existing folder; the snapshot's own folder is made inside it.
        #[arg(long, value_name = "FOLDER")]
        to: PathBuf,
        #[command(flatten)]
        project: ProjectArg,
        #[arg(long)]
        json: bool,
    },
    /// Make the snapshot's version the current one again. Nothing is lost: the later
    /// versions stay, and the next change continues from this version.
    Restore {
        name: String,
        #[command(flatten)]
        project: ProjectArg,
    },
    /// Remove a snapshot's name. Its version and every file stay.
    Delete {
        name: String,
        #[command(flatten)]
        project: ProjectArg,
    },
}

fn print_json(value: &impl serde::Serialize) -> Result<(), CliError> {
    let text = serde_json::to_string_pretty(value).map_err(|e| CliError::failed(e.to_string()))?;
    println!("{text}");
    Ok(())
}

fn snapshots_table(snapshots: &[Snapshot], current: Option<u32>) -> String {
    let rows: Vec<Vec<String>> = snapshots
        .iter()
        .map(|snapshot| {
            let mark = if Some(snapshot.version) == current {
                " *"
            } else {
                ""
            };
            vec![
                render::clean(&snapshot.name),
                format!("{}{mark}", snapshot.version),
                render::shorten(&render::clean(&snapshot.note), 60),
            ]
        })
        .collect();
    table::render(&["snapshot", "version", "note"], &rows)
}

/// What an export says about the printed file: a line of the result, or a warning.
#[derive(Debug, PartialEq, Eq)]
enum Printed {
    Info(String),
    Warning(String),
}

fn printed_line(export: &SnapshotExport) -> Printed {
    if let Some(missing) = &export.printed_missing {
        return Printed::Warning(render::clean(&missing.message));
    }
    match &export.printed {
        Some(printed) => Printed::Info(format!(
            "the printed file is the one of print job {} ({}), byte for byte",
            printed.job_id,
            printed.delivery.as_deref().unwrap_or("delivered")
        )),
        None => Printed::Info(
            "this version had not been printed when the snapshot was made, so there is no \
             sliced file"
                .to_owned(),
        ),
    }
}

pub async fn run(command: SnapshotCommand, data_dir: Option<PathBuf>) -> Result<(), CliError> {
    let core = app::open_core(data_dir, SecretBackend::from_env()).await?;
    match command {
        SnapshotCommand::Create {
            name,
            version,
            note,
            project,
            json,
        } => {
            let project = app::find_project(&core, project.project.as_deref()).await?;
            let snapshot = core
                .snapshot_create(&project.id, &name, version, &note)
                .await?;
            if json {
                return print_json(&snapshot);
            }
            println!(
                "snapshot \"{}\" names version {} of {}; no version was changed",
                render::clean(&snapshot.name),
                snapshot.version,
                render::clean(&project.name)
            );
        }
        SnapshotCommand::List { project, json } => {
            let project = app::find_project(&core, project.project.as_deref()).await?;
            let snapshots = core.snapshots_list(&project.id).await?;
            if json {
                return print_json(&snapshots);
            }
            println!("project {}  {}", project.id, render::clean(&project.name));
            if snapshots.is_empty() {
                println!("no snapshots yet; name one with `hotend snapshot create <name>`");
            } else {
                println!("snapshots (* = the current version):");
                print!("{}", snapshots_table(&snapshots, project.current_version));
            }
        }
        SnapshotCommand::Export {
            name,
            to,
            project,
            json,
        } => {
            if !to.is_dir() {
                return Err(CliError::setup(format!(
                    "the folder {} does not exist; create it first",
                    to.display()
                )));
            }
            let folder = std::fs::canonicalize(&to)?;
            let project = app::find_project(&core, project.project.as_deref()).await?;
            let export = core.snapshot_export(&project.id, &name, &folder).await?;
            if json {
                return print_json(&export);
            }
            println!(
                "snapshot \"{}\" (version {}) written to {}",
                render::clean(&export.snapshot.name),
                export.snapshot.version,
                export.folder.display()
            );
            let rows: Vec<Vec<String>> = export
                .files
                .iter()
                .map(|file| {
                    vec![
                        file.path
                            .file_name()
                            .map(|name| name.to_string_lossy().into_owned())
                            .unwrap_or_default(),
                        file.kind.clone(),
                        file.source.clone(),
                        file.bytes.to_string(),
                        file.sha256.clone(),
                    ]
                })
                .collect();
            print!(
                "{}",
                table::render(&["file", "kind", "source", "bytes", "sha256"], &rows)
            );
            match printed_line(&export) {
                Printed::Info(line) => println!("{line}"),
                Printed::Warning(line) => eprintln!("warning: {line}"),
            }
        }
        SnapshotCommand::Restore { name, project } => {
            let project = app::find_project(&core, project.project.as_deref()).await?;
            let version = core.snapshot_restore(&project.id, &name).await?;
            println!(
                "version {} (snapshot \"{}\") is the current version of {} again; the later \
                 versions stay, and the next change continues from version {}",
                version.number,
                render::clean(name.trim()),
                render::clean(&project.name),
                version.number
            );
        }
        SnapshotCommand::Delete { name, project } => {
            let project = app::find_project(&core, project.project.as_deref()).await?;
            let snapshot = core.snapshot_delete(&project.id, &name).await?;
            println!(
                "snapshot \"{}\" removed; version {} and its files stay",
                render::clean(&snapshot.name),
                snapshot.version
            );
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn export(printed_missing: Option<hotend_core::MissingPrinted>) -> SnapshotExport {
        SnapshotExport {
            snapshot: Snapshot {
                id: "s".into(),
                project_id: "p".into(),
                name: "lid".into(),
                version: 1,
                note: String::new(),
                created_at_ms: 0,
                printed_job_id: Some("job-1".into()),
                printed_sha256: Some("ab".into()),
            },
            folder: PathBuf::from("/out/x"),
            files: Vec::new(),
            printed: None,
            printed_missing,
        }
    }

    #[test]
    fn a_missing_printed_file_is_a_warning() {
        let missing = hotend_core::MissingPrinted {
            job_id: "job-1".into(),
            sha256: "ab".into(),
            message: "printed file missing: version 1 was printed (print job job-1)".into(),
        };
        assert_eq!(
            printed_line(&export(Some(missing))),
            Printed::Warning(
                "printed file missing: version 1 was printed (print job job-1)".to_owned()
            )
        );
        assert!(matches!(printed_line(&export(None)), Printed::Info(_)));
    }
}

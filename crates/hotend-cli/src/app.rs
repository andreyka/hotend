//! Wiring shared by the commands: opening the core, choosing the secret store, installing
//! the slicer backend and finding the project a command names.
//!
//! Guarantee: a slice requested here runs through `hotend-slicer` (sandboxed, version
//! policy applied). The stand-in slicer is used only when the caller asks for it, and its
//! results carry the `fake_slicer` warning.

use std::path::PathBuf;

use hotend_core::{Core, CoreConfig, PrinterOptions, Project, ProviderKind, SecretBackend};
use hotend_slicer::Programs;

use crate::error::CliError;
use crate::{ProviderArg, SecretsArg};

/// The process profile of the built-in demo set (generic, untuned).
pub const DEMO_PROCESS_PROFILE: &str = "demo-0.20mm";

impl From<ProviderArg> for ProviderKind {
    fn from(provider: ProviderArg) -> Self {
        match provider {
            ProviderArg::Anthropic => Self::Anthropic,
            ProviderArg::Openai => Self::Openai,
            ProviderArg::Google => Self::Google,
            ProviderArg::Openrouter => Self::Openrouter,
            ProviderArg::Ollama => Self::Ollama,
        }
    }
}

/// The secret store a command uses: the `--secrets` flag, else `HOTEND_SECRETS`, else the
/// keychain.
pub fn secret_backend(choice: Option<SecretsArg>) -> SecretBackend {
    match choice {
        Some(SecretsArg::Keyring) => SecretBackend::Keyring,
        Some(SecretsArg::Env) => SecretBackend::Environment,
        Some(SecretsArg::File) => SecretBackend::File,
        None => SecretBackend::from_env(),
    }
}

/// Opens the core on the data directory in use.
pub async fn open_core(
    data_dir: Option<PathBuf>,
    secrets: SecretBackend,
) -> Result<Core, CliError> {
    let config = CoreConfig {
        data_dir,
        secrets,
        ..CoreConfig::default()
    };
    Ok(Core::open(config).await?)
}

/// Finds the project a command names: an id, the unique start of an id, or a name. Without
/// a reference, the most recently changed project.
pub async fn find_project(core: &Core, reference: Option<&str>) -> Result<Project, CliError> {
    let projects = core.projects_list().await?;
    let Some(reference) = reference else {
        return projects.into_iter().next().ok_or_else(|| {
            CliError::setup("there is no project yet; create one with `hotend project new <name>`")
        });
    };
    match lookup(&projects, reference) {
        Lookup::One(project) => Ok(project.clone()),
        Lookup::None => Err(CliError::setup(format!(
            "no project matches `{reference}`; `hotend project list` shows the projects"
        ))),
        Lookup::Ambiguous(ids) => Err(CliError::setup(format!(
            "`{reference}` matches several projects; name one by id: {}",
            ids.join(", ")
        ))),
    }
}

/// Like [`find_project`], but a reference that matches nothing becomes a new project of
/// that name. Returns whether it was created.
pub async fn find_or_create_project(
    core: &Core,
    reference: &str,
) -> Result<(Project, bool), CliError> {
    let projects = core.projects_list().await?;
    match lookup(&projects, reference) {
        Lookup::One(project) => Ok((project.clone(), false)),
        Lookup::None => Ok((core.project_create(reference).await?, true)),
        Lookup::Ambiguous(ids) => Err(CliError::setup(format!(
            "`{reference}` matches several projects; name one by id: {}",
            ids.join(", ")
        ))),
    }
}

enum Lookup<'a> {
    None,
    One(&'a Project),
    Ambiguous(Vec<String>),
}

fn lookup<'a>(projects: &'a [Project], reference: &str) -> Lookup<'a> {
    if let Some(exact) = projects.iter().find(|p| p.id == reference) {
        return Lookup::One(exact);
    }
    let by_name: Vec<&Project> = projects.iter().filter(|p| p.name == reference).collect();
    // An id prefix must be long enough that a short name is not mistaken for one.
    let by_prefix: Vec<&Project> = if reference.len() >= 8 {
        projects
            .iter()
            .filter(|p| p.id.starts_with(reference))
            .collect()
    } else {
        Vec::new()
    };
    let matches = if by_name.is_empty() {
        by_prefix
    } else {
        by_name
    };
    match matches.as_slice() {
        [] => Lookup::None,
        [one] => Lookup::One(one),
        many => Lookup::Ambiguous(many.iter().map(|p| p.id.clone()).collect()),
    }
}

/// Installs the slicer backend into the core: the installed vendor slicers, or Hotend's
/// stand-in (estimates only, output not printable) when `stand_in` is set.
pub fn install_slicer(core: &Core, stand_in: bool) -> Result<(), CliError> {
    let programs = if stand_in {
        Programs::StandIn
    } else {
        Programs::Installed
    };
    hotend_slicer::install(core, programs).map_err(|error| CliError::setup(error.to_string()))
}

/// Opens the core like [`open_core`], with the simulated printer's clock set.
pub async fn open_core_with_printer(
    data_dir: Option<PathBuf>,
    printer: PrinterOptions,
) -> Result<Core, CliError> {
    let config = CoreConfig {
        data_dir,
        secrets: SecretBackend::from_env(),
        printer,
        ..CoreConfig::default()
    };
    Ok(Core::open(config).await?)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn project(id: &str, name: &str) -> Project {
        Project {
            id: id.to_owned(),
            name: name.to_owned(),
            created_at_ms: 0,
            updated_at_ms: 0,
            current_version: None,
        }
    }

    #[test]
    fn a_project_is_found_by_id_name_or_a_long_id_prefix() {
        let projects = [
            project("0192f3a4-0000-7000-8000-000000000001", "Cable box"),
            project("0192f3a4-1111-7000-8000-000000000002", "Bracket"),
            project("0192ffff-2222-7000-8000-000000000003", "Bracket"),
        ];
        let id = |found: Lookup<'_>| match found {
            Lookup::One(p) => p.id.clone(),
            Lookup::None => "none".into(),
            Lookup::Ambiguous(ids) => format!("{} matches", ids.len()),
        };
        assert_eq!(id(lookup(&projects, "Cable box")), projects[0].id);
        assert_eq!(id(lookup(&projects, &projects[1].id)), projects[1].id);
        assert_eq!(id(lookup(&projects, "0192f3a4-1")), projects[1].id);
        assert_eq!(id(lookup(&projects, "0192ffff")), projects[2].id);
        // Two projects share the name and the short prefix; a 4-character prefix is a name.
        assert_eq!(id(lookup(&projects, "Bracket")), "2 matches");
        assert_eq!(id(lookup(&projects, "0192f3a4")), "2 matches");
        assert_eq!(id(lookup(&projects, "0192")), "none");
        assert_eq!(id(lookup(&projects, "cable box")), "none");
    }
}

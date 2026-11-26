//! The project, build, checks, export, slice, views and key commands: each reads its
//! arguments, makes one call into the core and prints the result.

use std::collections::BTreeMap;
use std::io::{IsTerminal, Read};
use std::path::{Path, PathBuf};

use hotend_core::engine::{EngineStatus, Section};
use hotend_core::gateway::render as report;
use hotend_core::modeling::MAX_CODE_BYTES;
use hotend_core::{ExportFormat, ProviderKind, SecretBackend};
use serde_json::json;

use crate::app;
use crate::error::CliError;
use crate::{
    BuildArgs, ChecksArgs, ExportArgs, FormatArg, KeyArg, KeyCommand, ProjectCommand, SliceArgs,
    ViewsArgs, render, table,
};
use hotend_core::SearchService;

/// Whose key a `hotend key set` names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum KeyOwner {
    Provider(ProviderKind),
    Search(SearchService),
}

impl From<KeyArg> for KeyOwner {
    fn from(arg: KeyArg) -> Self {
        match arg {
            KeyArg::Anthropic => Self::Provider(ProviderKind::Anthropic),
            KeyArg::Openai => Self::Provider(ProviderKind::Openai),
            KeyArg::Google => Self::Provider(ProviderKind::Google),
            KeyArg::Openrouter => Self::Provider(ProviderKind::Openrouter),
            KeyArg::Ollama => Self::Provider(ProviderKind::Ollama),
            KeyArg::Keenable => Self::Search(SearchService::Keenable),
            KeyArg::Tavily => Self::Search(SearchService::Tavily),
        }
    }
}

impl KeyOwner {
    fn name(self) -> &'static str {
        match self {
            Self::Provider(kind) => kind.as_str(),
            Self::Search(service) => service.as_str(),
        }
    }

    fn env_names(self) -> String {
        match self {
            Self::Provider(kind) => env_names(kind),
            Self::Search(service) => service.env_var().to_owned(),
        }
    }
}

fn print_json(value: &impl serde::Serialize) -> Result<(), CliError> {
    let text = serde_json::to_string_pretty(value).map_err(|e| CliError::failed(e.to_string()))?;
    println!("{text}");
    Ok(())
}

pub async fn project(command: ProjectCommand, data_dir: Option<PathBuf>) -> Result<(), CliError> {
    let core = app::open_core(data_dir, SecretBackend::from_env()).await?;
    match command {
        ProjectCommand::New { name } => {
            let project = core.project_create(&name).await?;
            println!(
                "created project {}  {}",
                project.id,
                render::clean(&project.name)
            );
        }
        ProjectCommand::List { json } => {
            let projects = core.projects_list().await?;
            if json {
                return print_json(&projects);
            }
            if projects.is_empty() {
                println!("no projects yet in {}", core.data_dir().display());
                println!("create one with `hotend project new <name>` or run `hotend demo`");
                return Ok(());
            }
            let rows: Vec<Vec<String>> = projects
                .iter()
                .map(|p| {
                    vec![
                        p.id.clone(),
                        render::shorten(&render::clean(&p.name), 40),
                        p.current_version
                            .map_or_else(|| "-".to_owned(), |n| n.to_string()),
                    ]
                })
                .collect();
            print!(
                "{}",
                table::render(&["id", "name", "current version"], &rows)
            );
        }
        ProjectCommand::Show { project, json } => {
            let found = app::find_project(&core, project.as_deref()).await?;
            let detail = core.project_get(&found.id).await?;
            let pending = core.print_jobs(&found.id).await?;
            if json {
                return print_json(&json!({"project": detail.project,
                    "versions": detail.versions, "transcript": detail.transcript,
                    "print_jobs": pending}));
            }
            println!("project {}  {}", found.id, render::clean(&found.name));
            println!(
                "folder  {}",
                core.data_dir().join("projects").join(&found.id).display()
            );
            if detail.versions.is_empty() {
                println!("no versions yet");
            } else {
                println!("\nversions (* = current):");
                print!(
                    "{}",
                    render::versions_table(&detail.versions, detail.project.current_version)
                );
            }
            if !pending.is_empty() {
                println!("\nprint jobs (a pending one waits for `hotend print confirm`):");
                print!("{}", render::pending_table(&pending));
            }
            if !detail.transcript.is_empty() {
                println!("\nconversation:");
                for entry in &detail.transcript {
                    let who = if entry.role == "user" {
                        "You:   "
                    } else {
                        "Agent: "
                    };
                    println!(
                        "{}",
                        render::indented(&entry.text, "       ").replacen("       ", who, 1)
                    );
                }
            }
        }
    }
    Ok(())
}

/// Parses `name=value` into a parameter.
fn parse_param(text: &str) -> Result<(String, f64), CliError> {
    let parsed = text.split_once('=').and_then(|(name, value)| {
        let value: f64 = value.trim().parse().ok()?;
        (value.is_finite() && !name.trim().is_empty()).then(|| (name.trim().to_owned(), value))
    });
    parsed.ok_or_else(|| {
        CliError::setup(format!(
            "`--param {text}` is not NAME=NUMBER, for example --param width=90"
        ))
    })
}

pub async fn build(args: BuildArgs, data_dir: Option<PathBuf>) -> Result<(), CliError> {
    let params: BTreeMap<String, f64> = args
        .params
        .iter()
        .map(|text| parse_param(text))
        .collect::<Result<_, _>>()?;
    let too_large = std::fs::metadata(&args.model).is_ok_and(|m| m.len() > MAX_CODE_BYTES as u64);
    if too_large {
        return Err(CliError::setup(format!(
            "{} is larger than {MAX_CODE_BYTES} bytes; a model script is one Python file",
            args.model.display()
        )));
    }
    let code = std::fs::read_to_string(&args.model).map_err(|error| {
        CliError::setup(format!("cannot read {}: {error}", args.model.display()))
    })?;
    let core = app::open_core(data_dir, SecretBackend::from_env()).await?;
    let name = match &args.project {
        Some(reference) => reference.clone(),
        None => args
            .model
            .file_stem()
            .and_then(|stem| stem.to_str())
            .unwrap_or("model")
            .to_owned(),
    };
    let (project, created) = app::find_or_create_project(&core, &name).await?;
    if created {
        println!(
            "created project {}  {}",
            project.id,
            render::clean(&project.name)
        );
    } else {
        println!("project {}  {}", project.id, render::clean(&project.name));
    }
    let outcome = core
        .model_build(&project.id, &code, params, &args.note)
        .await?;
    print!(
        "{}",
        render::clean(&report::build_report(&outcome, project.current_version))
    );
    match &outcome.version {
        Some(version) => {
            let detail = core.version_get(&project.id, version.number).await?;
            println!("folder: {}", detail.dir.display());
        }
        None => return Err(CliError::Reported),
    }
    if outcome.result.status == EngineStatus::Ok {
        Ok(())
    } else {
        Err(CliError::Reported)
    }
}

pub async fn checks(args: ChecksArgs, data_dir: Option<PathBuf>) -> Result<(), CliError> {
    let core = app::open_core(data_dir, SecretBackend::from_env()).await?;
    let project = app::find_project(&core, args.project.project.as_deref()).await?;
    let only = (!args.only.is_empty()).then_some(args.only);
    let result = core.checks_run(&project.id, only, false).await?;
    let version = project.current_version.unwrap_or(0);
    println!("project {}  {}", project.id, render::clean(&project.name));
    print!(
        "{}",
        render::clean(&report::checks_report(&result, version))
    );
    if result.status == EngineStatus::Ok {
        Ok(())
    } else {
        Err(CliError::Reported)
    }
}

fn must_be_dir(path: &Path) -> Result<(), CliError> {
    if path.is_dir() {
        Ok(())
    } else {
        Err(CliError::setup(format!(
            "the output folder {} does not exist; create it first",
            path.display()
        )))
    }
}

pub async fn export(args: ExportArgs, data_dir: Option<PathBuf>) -> Result<(), CliError> {
    must_be_dir(&args.out)?;
    let core = app::open_core(data_dir, SecretBackend::from_env()).await?;
    let project = app::find_project(&core, args.project.project.as_deref()).await?;
    let mut formats: Vec<ExportFormat> = Vec::new();
    for format in args.format {
        let format = match format {
            FormatArg::Step => ExportFormat::Step,
            FormatArg::ThreeMf => ExportFormat::ThreeMf,
            FormatArg::Stl => ExportFormat::Stl,
        };
        if !formats.contains(&format) {
            formats.push(format);
        }
    }
    let parts = (!args.parts.is_empty()).then_some(args.parts);
    let files = core
        .export_run(&project.id, args.version, formats, parts, &args.out)
        .await?;
    let rows: Vec<Vec<String>> = files
        .iter()
        .map(|file| {
            vec![
                file.path.display().to_string(),
                file.part.clone().unwrap_or_else(|| "(all)".to_owned()),
                file.bytes.to_string(),
                file.sha256.clone(),
            ]
        })
        .collect();
    print!(
        "{}",
        table::render(&["file", "part", "bytes", "sha256"], &rows)
    );
    Ok(())
}

pub async fn slice(args: SliceArgs, data_dir: Option<PathBuf>) -> Result<(), CliError> {
    let core = app::open_core(data_dir, SecretBackend::from_env()).await?;
    let project = app::find_project(&core, args.project.project.as_deref()).await?;
    app::install_slicer(&core, args.fake)?;
    let outcome = core
        .slice_run(&project.id, &args.profile, args.plate)
        .await?;
    if args.json {
        return print_json(&outcome);
    }
    println!("project {}  {}", project.id, render::clean(&project.name));
    print!("{}", render::clean(&report::slice_report(&outcome)));
    println!("plate: {}", outcome.preparation.dir.display());
    println!("sliced file: {}", outcome.output_path.display());
    println!("sha256: {}", outcome.summary.sha256);
    if args.fake {
        println!(
            "note: this came from Hotend's stand-in slicer. The numbers are rough estimates \
             and the file is not printable."
        );
    }
    Ok(())
}

/// Parses `PLANE` or `PLANE@OFFSET`.
fn parse_section(text: &str) -> Result<Section, CliError> {
    let (plane, offset) = text.split_once('@').unwrap_or((text, "0"));
    let section = offset.trim().parse::<f64>().ok().and_then(|offset| {
        serde_json::from_value(json!({"plane": plane.trim().to_uppercase(), "offset": offset})).ok()
    });
    section.ok_or_else(|| {
        CliError::setup(format!(
            "`--section {text}` is not a plane (XY, XZ or YZ) with an optional offset, for \
             example XZ or XY@12.5"
        ))
    })
}

pub async fn views(args: ViewsArgs, data_dir: Option<PathBuf>) -> Result<(), CliError> {
    must_be_dir(&args.out)?;
    let section = args.section.as_deref().map(parse_section).transpose()?;
    let core = app::open_core(data_dir, SecretBackend::from_env()).await?;
    let project = app::find_project(&core, args.project.project.as_deref()).await?;
    let rendered = core
        .views_render(&project.id, args.views, section, [args.size, args.size])
        .await?;
    for view in rendered {
        let name = match section {
            Some(section) => format!(
                "{}_section_{}.png",
                view.name,
                format!("{:?}", section.plane).to_lowercase()
            ),
            None => format!("{}.png", view.name),
        };
        let path = args.out.join(name);
        std::fs::write(&path, &view.png).map_err(|error| {
            CliError::failed(format!("cannot write {}: {error}", path.display()))
        })?;
        println!("{}", path.display());
    }
    Ok(())
}

// ---- keys ------------------------------------------------------------------------------

pub use hotend_core::doctor::KeyState;

/// The name of a secret backend as the `--secrets` flag spells it.
pub fn backend_name(backend: &SecretBackend) -> &'static str {
    match backend {
        SecretBackend::Keyring => "keyring",
        SecretBackend::Environment => "env",
        SecretBackend::File => "file",
        SecretBackend::Custom(_) => "custom",
    }
}

/// Where a store keeps the key named `secret`, for `key status`; `env` is the variable(s)
/// the key is read from with the environment store.
fn key_place(backend: &SecretBackend, secret: &str, env: String) -> String {
    match backend {
        SecretBackend::Environment => env,
        SecretBackend::File if hotend_core::secrets::environment_has(secret) => {
            format!("{env} (environment; wins over the file)")
        }
        SecretBackend::File => format!("secrets file entry {secret}"),
        _ => format!("keychain entry {secret}"),
    }
}

/// Where a key just stored went, in words.
fn stored_in(backend: &SecretBackend) -> String {
    match backend {
        SecretBackend::File => match hotend_core::secrets::default_secrets_file() {
            Ok(path) => format!("the secrets file {}", path.display()),
            Err(_) => "the secrets file".to_owned(),
        },
        _ => "the system keychain".to_owned(),
    }
}

/// Asks one secret store which provider keys are set (see
/// [`hotend_core::doctor::key_states`]), with this command line's way out added to a
/// keychain that cannot be asked.
pub async fn key_states(backend: &SecretBackend) -> Vec<(ProviderKind, KeyState)> {
    hotend_core::doctor::key_states(backend)
        .await
        .into_iter()
        .map(|(kind, state)| match state {
            KeyState::Unavailable(reason) => (kind, KeyState::Unavailable(with_cli_hint(&reason))),
            other => (kind, other),
        })
        .collect()
}

/// Adds how to switch to the environment store on this command line to a keychain error.
pub fn with_cli_hint(reason: &str) -> String {
    format!(
        "{}. With this command: add `--secrets file` to keep keys in a file of your own (or \
         set HOTEND_SECRETS=file), or `--secrets env` to read them from the environment",
        reason.trim_end_matches('.')
    )
}

/// The environment variable(s) a provider's key is read from with `--secrets env`.
pub fn env_names(kind: ProviderKind) -> String {
    kind.rule().env_vars.join(" or ")
}

/// Reads a key from standard input: the first line of a pipe, or a prompt without echo.
fn read_key(name: &str) -> Result<String, CliError> {
    let key = if std::io::stdin().is_terminal() {
        rpassword::prompt_password(format!(
            "API key for {name} (typing is hidden, then press Enter): "
        ))
        .map_err(|error| CliError::setup(format!("cannot read the key: {error}")))?
    } else {
        let mut text = String::new();
        std::io::stdin()
            .take(hotend_core::secrets::MAX_SECRET_BYTES as u64 + 2)
            .read_to_string(&mut text)
            .map_err(|_| CliError::setup("cannot read the key from standard input"))?;
        text.lines().next().unwrap_or("").to_owned()
    };
    let key = key.trim().to_owned();
    if key.is_empty() {
        return Err(CliError::setup("no key was given; nothing was stored"));
    }
    Ok(key)
}

pub async fn key(command: KeyCommand, data_dir: Option<PathBuf>) -> Result<(), CliError> {
    match command {
        KeyCommand::Set {
            provider,
            secrets,
            rejected,
        } => {
            let owner = KeyOwner::from(provider);
            if !rejected.is_empty() {
                // Deliberately not repeated: it may be the key.
                return Err(CliError::setup(format!(
                    "`hotend key set {0}` takes nothing after the provider. A key is never \
                     read from the command line (it would stay in your shell history): run \
                     `hotend key set {0}` and type the key at the prompt, or pipe it in. If \
                     you just typed a key here, treat it as exposed and replace it",
                    owner.name()
                )));
            }
            if let KeyOwner::Provider(kind) = owner
                && kind.rule().env_vars.is_empty()
            {
                return Err(CliError::setup(format!(
                    "{} runs on this machine and takes no key",
                    kind.as_str()
                )));
            }
            let backend = app::secret_backend(secrets.secrets);
            if matches!(backend, SecretBackend::Environment) {
                return Err(CliError::setup(format!(
                    "with `--secrets env` Hotend only reads keys: set {} in your environment \
                     instead, or use `--secrets file` (HOTEND_SECRETS=file) to store it in a \
                     file of your own",
                    owner.env_names()
                )));
            }
            let place = stored_in(&backend);
            let core = app::open_core(data_dir, backend).await?;
            let key = read_key(owner.name())?;
            let stored = match owner {
                KeyOwner::Provider(kind) => core.provider_key_set(kind, key).await,
                KeyOwner::Search(service) => core.search_key_set(service, key).await,
            };
            stored.map_err(|error| match error {
                hotend_core::CoreError::Secret(
                    unavailable @ hotend_core::secrets::SecretError::Unavailable { .. },
                ) => CliError::failed(with_cli_hint(&unavailable.to_string())),
                other => other.into(),
            })?;
            println!("stored the {} key in {place}", owner.name());
        }
        KeyCommand::Delete { provider, secrets } => {
            let owner = KeyOwner::from(provider);
            let backend = app::secret_backend(secrets.secrets);
            if matches!(backend, SecretBackend::Environment) {
                return Err(CliError::setup(format!(
                    "with `--secrets env` Hotend only reads keys: remove {} from your \
                     environment instead",
                    owner.env_names()
                )));
            }
            let place = stored_in(&backend);
            let secret = match owner {
                KeyOwner::Provider(kind) => hotend_core::secrets::provider_key_name(kind),
                KeyOwner::Search(service) => hotend_core::search::search_key_name(service),
            };
            // With the file store a key in the environment still wins: say it stays in use.
            let still_read = matches!(backend, SecretBackend::File)
                && hotend_core::secrets::environment_has(&secret);
            let core = app::open_core(data_dir, backend).await?;
            match owner {
                KeyOwner::Provider(kind) => core.provider_key_delete(kind).await?,
                KeyOwner::Search(service) => core.search_key_delete(service).await?,
            }
            if still_read {
                println!(
                    "removed the {} key from {place}; still read from {} (remove it there to \
                     stop using it)",
                    owner.name(),
                    owner.env_names()
                );
            } else {
                println!("removed the {} key from {place}", owner.name());
            }
        }
        KeyCommand::Status { secrets, json } => {
            let backend = app::secret_backend(secrets.secrets);
            let states = key_states(&backend).await;
            let search: Vec<(SearchService, KeyState)> =
                hotend_core::doctor::search_key_states(&backend)
                    .await
                    .into_iter()
                    .map(|(service, state)| match state {
                        KeyState::Unavailable(reason) => {
                            (service, KeyState::Unavailable(with_cli_hint(&reason)))
                        }
                        other => (service, other),
                    })
                    .collect();
            if json {
                let rows: Vec<_> = states
                    .iter()
                    .map(|(kind, state)| json!({"provider": kind.as_str(), "key": state}))
                    .collect();
                let services: Vec<_> = search
                    .iter()
                    .map(|(service, state)| json!({"service": service.as_str(), "key": state}))
                    .collect();
                return print_json(&json!({"store": backend_name(&backend), "providers": rows,
                    "search_services": services}));
            }
            println!("secret store: {}", backend_name(&backend));
            let mut rows: Vec<Vec<String>> = states
                .iter()
                .map(|(kind, state)| {
                    let source = key_place(
                        &backend,
                        &hotend_core::secrets::provider_key_name(*kind),
                        env_names(*kind),
                    );
                    let source = if *state == KeyState::NotNeeded {
                        String::new()
                    } else {
                        source
                    };
                    vec![kind.as_str().to_owned(), state.word().to_owned(), source]
                })
                .collect();
            rows.extend(search.iter().map(|(service, state)| {
                let source = key_place(
                    &backend,
                    &hotend_core::search::search_key_name(*service),
                    service.env_var().to_owned(),
                );
                let word = match (service.keyless(), state) {
                    (true, KeyState::NotSet) => "not set (keyless trial path)".to_owned(),
                    _ => state.word().to_owned(),
                };
                vec![format!("{} (search)", service.as_str()), word, source]
            }));
            print!("{}", table::render(&["provider", "key", "where"], &rows));
            if let Some((_, KeyState::Unavailable(reason))) = states
                .iter()
                .find(|(_, state)| matches!(state, KeyState::Unavailable(_)))
            {
                println!("\n{}", table::wrap(reason, render::WIDTH, ""));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use hotend_core::engine::SectionPlane;

    #[test]
    fn parameters_are_name_equals_number() {
        assert_eq!(
            parse_param("width=90").ok(),
            Some(("width".to_owned(), 90.0))
        );
        assert_eq!(
            parse_param(" wall = 2.5 ").ok(),
            Some(("wall".to_owned(), 2.5))
        );
        for bad in [
            "width",
            "width=",
            "=3",
            "width=wide",
            "width=inf",
            "width=NaN",
        ] {
            assert!(parse_param(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn a_section_is_a_plane_with_an_optional_offset() {
        let section = parse_section("xz").ok();
        assert_eq!(
            section,
            Some(Section {
                plane: SectionPlane::XZ,
                offset: 0.0
            })
        );
        assert_eq!(parse_section("XY@12.5").ok().map(|s| s.offset), Some(12.5));
        assert!(parse_section("AB").is_err());
        assert!(parse_section("XY@deep").is_err());
    }

    #[tokio::test]
    async fn the_environment_store_reports_keys_without_reading_them_into_the_output() {
        let states = key_states(&SecretBackend::Environment).await;
        assert_eq!(states.len(), ProviderKind::ALL.len());
        let ollama = states
            .iter()
            .find(|(kind, _)| *kind == ProviderKind::Ollama);
        assert_eq!(ollama.map(|(_, state)| state), Some(&KeyState::NotNeeded));
        // A state is a word, never a value.
        for (_, state) in &states {
            assert!(["set", "not set", "not needed (local)"].contains(&state.word()));
        }
    }
}

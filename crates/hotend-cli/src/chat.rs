//! `hotend chat`: one message to the real agent, with the user's own provider key.
//!
//! The key never passes through this program: the core reads it from the secret store and
//! adds it to the outbound request. Nothing here prints a key, and the agent process never
//! sees one.

use std::path::PathBuf;
use std::sync::Arc;

use hotend_core::{
    AgentCommand, Core, CoreConfig, CoreError, MemorySecretStore, ProviderKind, ProviderSettings,
    PythonRuntime, RunEvent, SecretBackend, StopReason,
};

use crate::commands::{KeyState, backend_name, env_names, key_states};
use crate::error::CliError;
use crate::render::{self, StreamPrinter};
use crate::{ChatArgs, app, research};

/// The provider slot used for a scripted run: the one provider that takes no key. The
/// scripted agent sends no request to it or to anyone.
const SCRIPT_PROVIDER: ProviderKind = ProviderKind::Ollama;

pub async fn run(args: ChatArgs, data_dir: Option<PathBuf>) -> Result<(), CliError> {
    let mut backend = app::secret_backend(args.secrets.secrets);
    let mut config = CoreConfig {
        data_dir,
        ..CoreConfig::default()
    };
    if let Some(script) = &args.script {
        // A scripted run needs no key and must not touch the keychain.
        backend = SecretBackend::Custom(Arc::new(MemorySecretStore::default()));
        let runtime = PythonRuntime::discover().await.map_err(CoreError::from)?;
        config.agent = Some(scripted_agent(&runtime, script)?);
        config.runtime = Some(runtime);
    }
    config.secrets = backend.clone();
    let core = Core::open(config).await?;

    let saved = core.settings_get().await?;
    let mut settings = saved.clone();
    if let (Some(provider), Some(model)) = (args.provider, args.model) {
        settings.provider = Some(ProviderSettings {
            kind: provider.into(),
            model,
        });
    }
    if args.script.is_some() {
        settings.provider = Some(ProviderSettings {
            kind: SCRIPT_PROVIDER,
            model: "scripted".to_owned(),
        });
    }
    if let Some(max_turns) = args.max_turns {
        settings.max_turns = max_turns;
    }
    let Some(provider) = settings.provider.clone() else {
        return Err(CliError::setup(
            "no AI provider is chosen yet: add --provider <anthropic|openai|google|openrouter|\
             ollama> --model <id> (they are remembered for the next message)",
        ));
    };
    core.settings_set(settings).await?;
    require_key(&backend, provider.kind).await?;

    let (project, created) = app::find_or_create_project(&core, &args.project).await?;
    let verb = if created {
        "created project"
    } else {
        "project"
    };
    eprintln!("{verb} {}  {}", project.id, render::clean(&project.name));
    match &args.script {
        Some(script) => eprintln!(
            "SCRIPTED RUN: the agent replays `{}`. No model is asked, no key is used, nothing \
             is sent.\n",
            render::clean(script)
        ),
        None if provider.kind.rule().env_vars.is_empty() => eprintln!(
            "provider {} / {} (local, no key); press Ctrl-C to stop\n",
            provider.kind.as_str(),
            render::clean(&provider.model)
        ),
        None => eprintln!(
            "provider {} / {}; key from the {} store; press Ctrl-C to stop\n",
            provider.kind.as_str(),
            render::clean(&provider.model),
            backend_name(&backend)
        ),
    }
    if args.script.is_none()
        && let Some(line) = research::chat_line(&core.research_status(&project.id).await?)
    {
        eprintln!("{line}\n");
    }
    // The slice tool works when a slicer is installed; without one it answers with the
    // reason, which the agent can pass on.
    app::install_slicer(&core, false)?;

    let before = project.current_version;
    let outcome = stream(&core, &project.id, &args.text).await;
    if args.script.is_some() {
        // The placeholder provider of a scripted run is not the user's choice.
        core.settings_set(saved).await?;
    }
    let outcome = outcome?;
    let detail = core.project_get(&project.id).await?;
    let made = detail
        .versions
        .iter()
        .filter(|v| before.is_none_or(|b| v.number > b))
        .count();
    eprintln!();
    if let Some(usage) = &outcome.usage {
        eprintln!("{}", render::usage_line(usage));
    }
    if let Some(event) = &outcome.research {
        eprint!("{}", research::run_summary(event));
    }
    match detail.project.current_version {
        Some(number) => {
            let dir = core.version_get(&project.id, number).await?.dir;
            eprintln!(
                "{made} new version(s); current version {number}: {}",
                dir.display()
            );
        }
        None => eprintln!("the project has no version yet"),
    }
    match outcome.end {
        RunEvent::Finished {
            stop_reason: StopReason::Done,
            ..
        } => Ok(()),
        RunEvent::Finished {
            stop_reason: StopReason::Budget,
            ..
        } => {
            eprintln!(
                "the agent used all its turns for this message; send another message to let \
                 it continue (or raise --max-turns)"
            );
            Ok(())
        }
        RunEvent::Finished {
            stop_reason: StopReason::Cancelled,
            ..
        } => Err(CliError::failed("stopped")),
        RunEvent::Finished { error, .. } => Err(CliError::failed(format!(
            "the run ended with an error: {}",
            render::clean(&error.unwrap_or_else(|| "no detail was given".to_owned()))
        ))),
        RunEvent::Failed { code, message } => Err(CliError::failed(format!(
            "the run failed ({code}): {}",
            render::clean(&message)
        ))),
        RunEvent::Agent { .. } => Err(CliError::failed("the run ended without a final event")),
    }
}

/// The real agent started in scripted mode (PROTOCOL.md section 5.2). A script file must be
/// readable inside the agent's sandbox, so its folder joins the read-only view.
fn scripted_agent(runtime: &PythonRuntime, script: &str) -> Result<AgentCommand, CliError> {
    let mut agent = AgentCommand::python_module(runtime);
    let source = if script.starts_with("builtin:") {
        script.to_owned()
    } else {
        let path = std::fs::canonicalize(script).map_err(|error| {
            CliError::setup(format!("cannot read the script {script}: {error}"))
        })?;
        if let Some(folder) = path.parent() {
            agent.read_only.push(folder.to_path_buf());
        }
        path.display().to_string()
    };
    agent.args.extend(["--script".to_owned(), source]);
    Ok(agent)
}

/// Fails early, with the fix, when the chosen provider has no key in the chosen store.
async fn require_key(backend: &SecretBackend, kind: ProviderKind) -> Result<(), CliError> {
    let states = key_states(backend).await;
    let state = states
        .into_iter()
        .find(|(provider, _)| *provider == kind)
        .map(|(_, state)| state);
    let name = kind.as_str();
    match state {
        Some(KeyState::Set | KeyState::NotNeeded) => Ok(()),
        Some(KeyState::Unavailable(reason)) => Err(CliError::setup(reason)),
        _ if matches!(backend, SecretBackend::Environment) => Err(CliError::setup(format!(
            "no {name} key in the environment: set {} and run the command again",
            env_names(kind)
        ))),
        _ => Err(CliError::setup(format!(
            "no {name} key is stored: run `hotend key set {name}`, or put the key in {} and \
             add `--secrets env`",
            env_names(kind)
        ))),
    }
}

/// How a streamed run ended.
pub struct Streamed {
    /// The final event: `Finished` or `Failed`.
    pub end: RunEvent,
    pub usage: Option<serde_json::Value>,
    /// What the run looked up on the web, when it did (the core's `research` event).
    pub research: Option<serde_json::Value>,
}

/// Sends the message and prints the agent's text and tool activity as it happens. Ctrl-C
/// asks the run to stop; the core stops the agent process if it does not.
async fn stream(core: &Core, project_id: &str, text: &str) -> Result<Streamed, CliError> {
    let mut run = core.message_send(project_id, text).await?;
    let mut printer = StreamPrinter::default();
    let mut cancelled = false;
    let end = loop {
        tokio::select! {
            event = run.next() => match event {
                Some(RunEvent::Agent { event }) => printer.event(&event),
                Some(end) => break end,
                None => break RunEvent::Failed {
                    code: "closed".to_owned(),
                    message: "the event stream closed before the run ended".to_owned(),
                },
            },
            signal = tokio::signal::ctrl_c(), if !cancelled => {
                cancelled = true;
                printer.finish();
                if signal.is_ok() {
                    eprintln!("stopping the agent...");
                    core.run_cancel(&run.run_id).await;
                }
            }
        }
    };
    printer.finish();
    Ok(Streamed {
        end,
        usage: printer.usage,
        research: printer.research,
    })
}

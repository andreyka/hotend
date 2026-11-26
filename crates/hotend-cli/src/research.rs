//! `hotend research`: web research for a project (docs/WEB_RESEARCH.md).
//!
//! The agent may look up real products, parts and standards through the AI provider's web
//! search and web fetch. These commands switch that on or off, narrow it, show what it
//! used and what it cost, and let the person confirm or reject a number it found.
//!
//! Guarantees:
//! - Web research is switched on for the first time only by a person who typed `yes` at a
//!   terminal after reading what leaves this computer. No argument stands in for that.
//! - Everything a page wrote (titles, addresses, quotes) is printed as plain text with
//!   control characters removed, and no address is opened.

use std::io::{IsTerminal, Write};
use std::path::PathBuf;

use hotend_core::{
    Core, ProjectResearch, ResearchLogEntry, ResearchNote, ResearchState, ResearchStatus,
    SearchSource, SecretBackend, WebNumber,
};
use serde_json::Value;

use crate::error::CliError;
use crate::{CacheArg, ProjectArg, ResearchCommand, SourceArg, Switch, app, render, table};

impl From<SourceArg> for SearchSource {
    fn from(source: SourceArg) -> Self {
        match source {
            SourceArg::BuiltIn => Self::BuiltIn,
            SourceArg::Keenable => Self::Keenable,
            SourceArg::Tavily => Self::Tavily,
            SourceArg::Off => Self::Off,
        }
    }
}

/// A source as the command line names it.
pub fn source_word(source: SearchSource) -> &'static str {
    match source {
        SearchSource::BuiltIn => "built-in (the AI provider's own web search)",
        SearchSource::Keenable => "keenable",
        SearchSource::Tavily => "tavily",
        SearchSource::Off => "off",
    }
}

fn print_json(value: &impl serde::Serialize) -> Result<(), CliError> {
    let text = serde_json::to_string_pretty(value).map_err(|e| CliError::failed(e.to_string()))?;
    println!("{text}");
    Ok(())
}

/// Millionths of a dollar as dollars and cents.
pub fn dollars(micro_usd: u64) -> String {
    let cents = micro_usd.saturating_add(5000) / 10_000;
    format!("${}.{:02}", cents / 100, cents % 100)
}

fn plural(count: u64, one: &str, many: &str) -> String {
    if count == 1 {
        format!("1 {one}")
    } else {
        format!("{count} {many}")
    }
}

/// What a state is called in a line of output.
fn state_word(state: ResearchState) -> &'static str {
    match state {
        ResearchState::On => "on",
        ResearchState::Off => "off",
        ResearchState::NeedsConsent => "waiting for your agreement",
        ResearchState::BudgetUsed => "on, allowance used up",
        ResearchState::Stopped => "stopped",
        ResearchState::UnsupportedProvider => "not available with this provider",
        ResearchState::ProviderRefused => "refused by the provider",
        ResearchState::NeedsKey => "waiting for the search service's key",
    }
}

/// How far Hotend could check a note's quote, in plain words.
pub fn evidence_words(evidence: &str) -> &'static str {
    match evidence {
        "page_text" => "Hotend found this sentence on the page",
        "citation" => {
            "the sentence matches a passage the search cited, a search excerpt; it has not \
             been checked against the page itself"
        }
        "unsourced" => {
            "no page: the assistant set this number while it was reading the web and \
             recorded none"
        }
        _ => "Hotend could not check this sentence against the page",
    }
}

/// The status of a project as text.
pub fn status_text(status: &ResearchStatus) -> String {
    let mut out = format!(
        "web research: {}\n{}\n",
        state_word(status.state),
        render::indented(&status.message, "  ")
    );
    let mut line = |text: String| {
        out.push_str("  ");
        out.push_str(&render::clean(&text));
        out.push('\n');
    };
    let fee = if status.billed_searches > 0 {
        format!(
            "; the provider billed {}, a search fee of {}",
            status.billed_searches,
            dollars(status.search_fee_micro_usd)
        )
    } else {
        String::new()
    };
    line(format!(
        "searches: {} of {} used in this project (at most {} per message){fee}",
        status.searches_used, status.searches_cap, status.searches_per_turn
    ));
    line(format!(
        "pages fetched: {} of {} (at most {} per message)",
        status.fetches_used, status.fetches_cap, status.fetches_per_turn
    ));
    line(format!(
        "web content in the conversation: {} of {} kB, about {} tokens that are read again \
         with every message",
        status.result_bytes / 1024,
        status.result_bytes_cap / 1024,
        status.result_tokens_estimate
    ));
    let project = &status.project;
    if !project.allowed_domains.is_empty() {
        line(format!(
            "limited to: {}",
            project.allowed_domains.join(", ")
        ));
    } else if !project.blocked_domains.is_empty() {
        line(format!(
            "kept away from: {}",
            project.blocked_domains.join(", ")
        ));
    } else {
        line("sites: no limit".to_owned());
    }
    line(format!(
        "links inside fetched pages: {}; fetched PDFs: {}",
        if project.follow_links {
            "may be followed"
        } else {
            "not followed"
        },
        if project.allow_pdf {
            "kept"
        } else {
            "not kept"
        }
    ));
    let key = match status.source.service() {
        None => String::new(),
        Some(service) if status.key_present => format!("; {} key: stored", service.label()),
        Some(service) if service.keyless() => format!(
            "; {} key: none, the keyless trial path is used (`hotend key set {}` for a free \
             key)",
            service.label(),
            service.as_str()
        ),
        Some(service) => format!(
            "; {} key: none (`hotend key set {}`)",
            service.label(),
            service.as_str()
        ),
    };
    line(format!("source: {}{key}", source_word(status.source)));
    let receiver = match status.source.service() {
        Some(service) => format!("{} ({})", service.label(), service.host()),
        None => "the AI provider".to_owned(),
    };
    line(format!(
        "agreement that queries go to {receiver}: {}",
        if status.acknowledged {
            "given"
        } else {
            "not given yet (`hotend research on`)"
        }
    ));
    out
}

/// One note as a few lines: the value, what it is, the page and the quote.
pub fn note_text(note: &ResearchNote) -> String {
    let mut out = String::new();
    let mut line = |text: String| {
        out.push_str(&render::clean(&text).replace('\n', " "));
        out.push('\n');
    };
    line(format!(
        "{}  {} = {} {}  [{}]",
        note.id, note.key, note.value, note.unit, note.state
    ));
    if !note.label.is_empty() {
        line(format!("    {}", note.label));
    }
    let title = if note.title.is_empty() {
        String::new()
    } else {
        format!("{} - ", render::shorten(&note.title, 60))
    };
    // A note Hotend made for a number without a page has neither.
    if !note.url.is_empty() {
        line(format!("    page: {title}{}", note.url));
        let place = if note.locator.is_empty() {
            String::new()
        } else {
            format!(" ({})", note.locator)
        };
        line(format!("    quote: \"{}\"{place}", note.quote));
    }
    line(format!("    {}", evidence_words(&note.evidence)));
    out
}

/// A web number as the lines shown under a question and above a print confirmation.
pub fn web_number_lines(number: &WebNumber, indent: &str) -> String {
    let mut out = String::new();
    let mut line = |text: String| {
        out.push_str(indent);
        out.push_str(&render::clean(&text).replace('\n', " "));
        out.push('\n');
    };
    if number.url.is_empty() {
        line(evidence_words(&number.evidence).to_owned());
    } else {
        line(format!("page: {}", number.url));
        line(format!(
            "quote: \"{}\" ({})",
            number.quote,
            evidence_words(&number.evidence)
        ));
    }
    if number.state == "unconfirmed" {
        line(format!(
            "after checking it against the real thing: `hotend research confirm {0}` or \
             `hotend research reject {0}`",
            number.note
        ));
    }
    out
}

/// The lines printed after a chat message that used the web: counts, the fee, what now
/// sits in the conversation, and the pages. `event` is the core's `research` event.
pub fn run_summary(event: &Value) -> String {
    let n = |key: &str| event[key].as_u64().unwrap_or(0);
    let mut parts = Vec::new();
    let searches = n("searches");
    if searches > 0 {
        let fee = match n("search_fee_micro_usd") {
            0 => String::new(),
            fee => format!(" ({} search fee)", dollars(fee)),
        };
        parts.push(format!(
            "{}{fee}",
            plural(searches, "web search", "web searches")
        ));
    }
    match n("fetches") {
        0 => {}
        fetches => parts.push(plural(fetches, "page fetched", "pages fetched")),
    }
    let tokens = event["project"]["result_tokens_estimate"]
        .as_u64()
        .unwrap_or_else(|| n("result_tokens_estimate"));
    let mut out = String::new();
    if !parts.is_empty() || tokens > 0 {
        parts.push(format!(
            "about {tokens} tokens of web content now in this project"
        ));
        out.push_str(&parts.join(", "));
        out.push('\n');
    }
    let sources: Vec<&Value> = event["sources"].as_array().into_iter().flatten().collect();
    if !sources.is_empty() {
        out.push_str("Sources:\n");
    }
    for source in sources {
        let url = render::clean(source["url"].as_str().unwrap_or("")).replace('\n', " ");
        let title = render::clean(source["title"].as_str().unwrap_or("")).replace('\n', " ");
        if title.is_empty() {
            out.push_str(&format!("  {url}\n"));
        } else {
            out.push_str(&format!("  {} - {url}\n", render::shorten(&title, 70)));
        }
    }
    // Anything but plain "on" needs saying: the allowance is used up, research stopped.
    if let Some(state) = event["project"]["state"].as_str()
        && state != "on"
        && let Some(message) = event["project"]["message"].as_str()
    {
        out.push_str(&render::indented(message, ""));
        out.push('\n');
    }
    out
}

/// One line before a chat message: whether this message may search.
pub fn chat_line(status: &ResearchStatus) -> Option<String> {
    match status.state {
        ResearchState::On => Some(format!(
            "web research: on ({} of {} searches used in this project; `hotend research off` \
             switches it off)",
            status.searches_used, status.searches_cap
        )),
        ResearchState::NeedsConsent => Some(
            "web research: off until you agree to it (`hotend research on` explains what it \
             sends and costs)"
                .to_owned(),
        ),
        ResearchState::BudgetUsed
        | ResearchState::Stopped
        | ResearchState::ProviderRefused
        | ResearchState::NeedsKey => {
            Some(format!("web research: {}", render::clean(&status.message)))
        }
        // Off by choice or not offered by the provider: nothing to say with every message.
        ResearchState::Off | ResearchState::UnsupportedProvider => None,
    }
}

/// Asks the person at the terminal to agree that queries leave this computer, for the
/// source in `status`. The explanation is the core's
/// (`hotend_core::research::consent_explanation`), the same words the application's
/// agreement dialog shows.
fn agree(status: &ResearchStatus) -> Result<bool, CliError> {
    if !std::io::stdin().is_terminal() {
        return Err(CliError::setup(
            "web research is agreed to by a person at a terminal, and standard input is not \
             one: nothing was switched on. Run `hotend research on` in a terminal and type \
             `yes`",
        ));
    }
    println!(
        "{}",
        hotend_core::research::consent_explanation(status.source, status.key_present)
    );
    print!("Type `yes` to agree, anything else to leave web research off: ");
    std::io::stdout().flush()?;
    let mut answer = String::new();
    std::io::stdin().read_line(&mut answer)?;
    Ok(answer.trim() == "yes")
}

async fn change(
    core: &Core,
    project: &ProjectArg,
    edit: impl FnOnce(&mut ProjectResearch),
) -> Result<ResearchStatus, CliError> {
    let project = app::find_project(core, project.project.as_deref()).await?;
    let mut settings = core.research_status(&project.id).await?.project;
    edit(&mut settings);
    let status = core.research_set(&project.id, settings).await?;
    println!("project {}  {}", project.id, render::clean(&project.name));
    print!("{}", status_text(&status));
    Ok(status)
}

async fn decide(
    core: &Core,
    project: &ProjectArg,
    note: &str,
    state: &str,
) -> Result<(), CliError> {
    let project = app::find_project(core, project.project.as_deref()).await?;
    let note = core.research_note_decide(&project.id, note, state).await?;
    print!("{}", note_text(&note));
    match state {
        "confirmed" => println!(
            "confirmed: Hotend stops asking about it. The agent is told with your next \
             message."
        ),
        "rejected" => println!(
            "rejected: the agent is told not to use it. A version built with it still has \
             it; ask the agent to change the model, or set the parameter yourself."
        ),
        _ => println!("the decision was taken back: the number is unconfirmed again"),
    }
    Ok(())
}

pub async fn run(command: ResearchCommand, data_dir: Option<PathBuf>) -> Result<(), CliError> {
    let core = app::open_core(data_dir, SecretBackend::from_env()).await?;
    match command {
        ResearchCommand::Status { project, json } => {
            let project = app::find_project(&core, project.project.as_deref()).await?;
            let status = core.research_status(&project.id).await?;
            if json {
                return print_json(&status);
            }
            println!("project {}  {}", project.id, render::clean(&project.name));
            print!("{}", status_text(&status));
            Ok(())
        }
        ResearchCommand::On { project } => {
            let found = app::find_project(&core, project.project.as_deref()).await?;
            let before = core.research_status(&found.id).await?;
            match (before.state, before.source) {
                (ResearchState::UnsupportedProvider, _) => {
                    return Err(CliError::setup(format!(
                        "{} (`hotend research source keenable`)",
                        render::clean(&before.message)
                    )));
                }
                (_, SearchSource::Off) | (ResearchState::NeedsKey, _) => {
                    return Err(CliError::setup(format!(
                        "{} Choose a source with `hotend research source built-in|keenable|tavily`",
                        render::clean(&before.message)
                    )));
                }
                _ => {}
            }
            if !before.acknowledged {
                if !agree(&before)? {
                    println!("web research stays off; nothing was changed");
                    return Ok(());
                }
                // For the source that was explained, and no other.
                core.research_consent_for(before.source, true).await?;
            }
            change(&core, &project, |settings| settings.enabled = Some(true)).await?;
            Ok(())
        }
        ResearchCommand::Source { source } => {
            let source = SearchSource::from(source);
            let settings = core.research_source_set(source).await?;
            println!("web research source: {}", source_word(source));
            let options = core.research_source_options().await?;
            let chosen = options.iter().find(|option| option.source == source);
            if let Some(option) = chosen.filter(|option| !option.available) {
                println!("{}", render::indented(&render::clean(&option.reason), "  "));
                return Ok(());
            }
            match source.service() {
                Some(service) if !service.keyless() && !chosen.is_some_and(|o| o.key_present) => {
                    println!(
                        "{} needs an API key: `hotend key set {}` (or {} with `--secrets env`), \
                         then `hotend research on`",
                        service.label(),
                        service.as_str(),
                        service.env_var()
                    );
                }
                _ if source == SearchSource::Off => {
                    println!("no project searches the web until another source is chosen");
                }
                _ if hotend_core::research::consent_covers(&settings.research, source) => {
                    println!("your agreement for this source is already given");
                }
                _ => println!(
                    "web research stays off until you agree to this source: `hotend research \
                     on` explains where the queries go and asks"
                ),
            }
            Ok(())
        }
        ResearchCommand::Off { project } => {
            change(&core, &project, |settings| settings.enabled = Some(false)).await?;
            Ok(())
        }
        ResearchCommand::Allow { hosts, project } => {
            change(&core, &project, |settings| {
                settings.allowed_domains = hosts;
                if !settings.allowed_domains.is_empty() {
                    settings.blocked_domains.clear();
                }
            })
            .await?;
            Ok(())
        }
        ResearchCommand::Block { hosts, project } => {
            change(&core, &project, |settings| {
                settings.blocked_domains = hosts;
                if !settings.blocked_domains.is_empty() {
                    settings.allowed_domains.clear();
                }
            })
            .await?;
            Ok(())
        }
        ResearchCommand::Pdf { switch, project } => {
            change(&core, &project, |settings| {
                settings.allow_pdf = switch == Switch::On;
            })
            .await?;
            Ok(())
        }
        ResearchCommand::Links { switch, project } => {
            change(&core, &project, |settings| {
                settings.follow_links = switch == Switch::On;
            })
            .await?;
            Ok(())
        }
        ResearchCommand::Limit {
            searches,
            fetches,
            project,
        } => {
            if searches.is_none() && fetches.is_none() {
                return Err(CliError::setup(
                    "say what to change: --searches <number> or --fetches <number>",
                ));
            }
            change(&core, &project, |settings| {
                if searches.is_some() {
                    settings.searches_per_project = searches;
                }
                if fetches.is_some() {
                    settings.fetches_per_project = fetches;
                }
            })
            .await?;
            Ok(())
        }
        ResearchCommand::Defaults {
            new_projects,
            searches_per_message,
            fetches_per_message,
            searches,
            fetches,
            page_tokens,
            cache,
            json,
        } => {
            let mut settings = core.settings_get().await?;
            let research = &mut settings.research;
            if let Some(switch) = new_projects {
                research.default_on = switch == Switch::On;
            }
            if let Some(value) = searches_per_message {
                research.searches_per_turn = value;
            }
            if let Some(value) = fetches_per_message {
                research.fetches_per_turn = value;
            }
            if let Some(value) = searches {
                research.searches_per_project = value;
            }
            if let Some(value) = fetches {
                research.fetches_per_project = value;
            }
            if let Some(value) = page_tokens {
                research.max_content_tokens = value;
            }
            if let Some(cache) = cache {
                research.cache_ttl = match cache {
                    CacheArg::FiveMinutes => "5m",
                    CacheArg::OneHour => "1h",
                }
                .to_owned();
            }
            core.settings_set(settings.clone()).await?;
            let research = &settings.research;
            if json {
                return print_json(research);
            }
            println!(
                "a project has web research unless it says otherwise: {}",
                if research.default_on { "yes" } else { "no" }
            );
            println!(
                "per message: {} searches, {} fetched pages",
                research.searches_per_turn, research.fetches_per_turn
            );
            println!(
                "per project: {} searches, {} fetched pages, {} kB of web content",
                research.searches_per_project,
                research.fetches_per_project,
                research.max_project_result_bytes / 1024
            );
            println!(
                "one fetched page: at most {} tokens; conversation cache: {}",
                research.max_content_tokens,
                render::clean(&research.cache_ttl)
            );
            Ok(())
        }
        ResearchCommand::Withdraw => {
            core.research_consent(false).await?;
            println!(
                "the agreement is withdrawn: no project searches the web until `hotend \
                 research on` is agreed to again. What was found so far is kept."
            );
            Ok(())
        }
        ResearchCommand::Notes { project, json } => {
            let project = app::find_project(&core, project.project.as_deref()).await?;
            let notes = core.research_notes(&project.id).await?;
            if json {
                return print_json(&notes);
            }
            if notes.is_empty() {
                println!("no number from the web is recorded for this project");
            }
            for note in &notes {
                print!("{}", note_text(note));
            }
            if notes.iter().any(|note| note.state == "unconfirmed") {
                println!(
                    "check an unconfirmed number against the real thing, then `hotend research \
                     confirm <id>` or `hotend research reject <id>`"
                );
            }
            Ok(())
        }
        ResearchCommand::Log { project, json } => {
            let project = app::find_project(&core, project.project.as_deref()).await?;
            let log = core.research_log(&project.id).await?;
            if json {
                return print_json(&log);
            }
            if log.is_empty() {
                println!("this project has not searched the web");
                return Ok(());
            }
            print!("{}", log_table(&log));
            Ok(())
        }
        ResearchCommand::Confirm { note, project } => {
            decide(&core, &project, &note, "confirmed").await
        }
        ResearchCommand::Reject { note, project } => {
            decide(&core, &project, &note, "rejected").await
        }
        ResearchCommand::Undecide { note, project } => {
            decide(&core, &project, &note, "unconfirmed").await
        }
    }
}

fn log_table(log: &[ResearchLogEntry]) -> String {
    let rows: Vec<Vec<String>> = log
        .iter()
        .map(|entry| {
            vec![
                render::clean(&entry.kind),
                render::clean(&entry.text).replace('\n', " "),
                render::clean(&entry.outcome).replace('\n', " "),
            ]
        })
        .collect();
    table::render_wrapped(&["what", "query or page", "result"], &rows, render::WIDTH)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use serde_json::json;

    #[test]
    fn money_is_shown_in_dollars_and_cents() {
        assert_eq!(dollars(0), "$0.00");
        assert_eq!(dollars(10_000), "$0.01");
        assert_eq!(dollars(20_000), "$0.02");
        assert_eq!(dollars(1_230_000), "$1.23");
        assert_eq!(dollars(u64::MAX).chars().next(), Some('$'));
    }

    fn event() -> Value {
        json!({
            "type": "research", "searches": 2, "fetches": 1, "billed_searches": 2,
            "search_fee_micro_usd": 20_000, "result_bytes": 9000, "result_tokens_estimate": 2250,
            "sources": [
                {"url": "https://maker.example/brenta-600/specs", "title": "Brenta 600 - data"},
                {"url": "https://reviews.example/brenta-600", "title": ""},
            ],
            "project": {"state": "on", "message": "Web research is on.",
                        "result_tokens_estimate": 4100},
        })
    }

    #[test]
    fn a_run_with_searches_is_summed_up_in_one_line_and_its_pages() {
        let text = run_summary(&event());
        assert_eq!(
            text,
            "2 web searches ($0.02 search fee), 1 page fetched, about 4100 tokens of web \
             content now in this project\n\
             Sources:\n  Brenta 600 - data - https://maker.example/brenta-600/specs\n  \
             https://reviews.example/brenta-600\n"
        );
        let mut one = event();
        one["searches"] = json!(1);
        one["fetches"] = json!(0);
        one["search_fee_micro_usd"] = json!(0);
        one["sources"] = json!([]);
        assert_eq!(
            run_summary(&one),
            "1 web search, about 4100 tokens of web content now in this project\n"
        );
    }

    #[test]
    fn a_used_up_allowance_is_said_after_the_run() {
        let mut used = event();
        used["project"]["state"] = json!("budget_used");
        used["project"]["message"] = json!("This project has used its web research allowance.");
        assert!(
            run_summary(&used).ends_with("This project has used its web research allowance.\n")
        );
    }

    #[test]
    fn a_number_without_a_page_is_shown_as_one() {
        let number = WebNumber {
            note: "r2".to_owned(),
            key: "bore".to_owned(),
            label: "Bearing bore".to_owned(),
            value: 22.0,
            unit: "mm".to_owned(),
            url: String::new(),
            host: String::new(),
            title: String::new(),
            quote: String::new(),
            locator: String::new(),
            evidence: "unsourced".to_owned(),
            state: "unconfirmed".to_owned(),
            params: vec!["bore".to_owned()],
            built: Some(22.0),
            text: String::new(),
        };
        let lines = web_number_lines(&number, "  ");
        assert!(
            lines.starts_with("  no page: the assistant set this number"),
            "{lines}"
        );
        assert!(
            !lines.contains("page: \n") && !lines.contains("quote:"),
            "{lines}"
        );
        assert!(lines.contains("`hotend research confirm r2`"), "{lines}");
    }

    #[test]
    fn what_a_page_wrote_cannot_move_the_cursor_or_add_lines() {
        let mut hostile = event();
        hostile["sources"] = json!([{
            "url": "https://evil.example/\u{1b}[2Jx",
            "title": "Specs\nSources:\n  https://bank.example\u{7}",
        }]);
        let text = run_summary(&hostile);
        assert!(!text.contains('\u{1b}') && !text.contains('\u{7}'));
        assert_eq!(text.lines().count(), 3, "{text}");

        let note = ResearchNote {
            id: "r1".to_owned(),
            key: "width".to_owned(),
            label: "Width\u{1b}[31m".to_owned(),
            value: 80.5,
            unit: "mm".to_owned(),
            url: "https://maker.example/a".to_owned(),
            title: "T\nrm -rf".to_owned(),
            quote: "Width: 80.5 mm\nconfirmed by Hotend".to_owned(),
            locator: String::new(),
            evidence: "none".to_owned(),
            state: "unconfirmed".to_owned(),
            created_at_ms: 0,
            updated_at_ms: 0,
        };
        let text = note_text(&note);
        assert!(!text.contains('\u{1b}'));
        assert_eq!(text.lines().count(), 5, "{text}");
        assert!(text.contains("Hotend could not check this sentence against the page"));
    }
}

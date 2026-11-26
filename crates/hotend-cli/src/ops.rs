//! The commands for what a version is and how it prints: parameters, measurements,
//! versions, questions, settings, the audit log, print preparation, gauges and
//! calibration. Each reads its arguments, makes one call into the core and prints the
//! result, as a table or as JSON.

use std::collections::BTreeMap;
use std::path::PathBuf;

use hotend_core::engine::EngineStatus;
use hotend_core::gateway::render as report;
use hotend_core::{
    CardRef, GaugeFeature, GaugeReading, PrepareOptions, ProviderKind, ProviderSettings,
    SecretBackend,
};
use serde_json::Value;

use crate::error::CliError;
use crate::{
    AuditArgs, CalibrateArgs, GaugeArgs, MeasureArgs, ParamCommand, PrepareArgs, ProjectArg,
    RevertArgs, SettingsCommand, app, render, research, table,
};

fn print_json(value: &impl serde::Serialize) -> Result<(), CliError> {
    let text = serde_json::to_string_pretty(value).map_err(|e| CliError::failed(e.to_string()))?;
    println!("{text}");
    Ok(())
}

async fn open(data_dir: Option<PathBuf>) -> Result<hotend_core::Core, CliError> {
    app::open_core(data_dir, SecretBackend::from_env()).await
}

/// Parses `name=value` with a value of type `T`.
fn pair<T: std::str::FromStr>(text: &str, what: &str) -> Result<(String, T), CliError> {
    text.split_once('=')
        .and_then(|(name, value)| Some((name.trim().to_owned(), value.trim().parse().ok()?)))
        .filter(|(name, _)| !name.is_empty())
        .ok_or_else(|| CliError::setup(format!("`{text}` is not {what}")))
}

pub async fn param(command: ParamCommand, data_dir: Option<PathBuf>) -> Result<(), CliError> {
    let ParamCommand::Set {
        name,
        value,
        project,
        json,
    } = command;
    let core = open(data_dir).await?;
    let project = app::find_project(&core, project.project.as_deref()).await?;
    let outcome = core.parameter_set(&project.id, &name, value).await?;
    if json {
        print_json(&outcome)?;
    } else {
        print!(
            "{}",
            render::clean(&report::build_report(&outcome, project.current_version))
        );
    }
    if outcome.version.is_some() && outcome.result.status == EngineStatus::Ok {
        Ok(())
    } else {
        Err(CliError::Reported)
    }
}

pub async fn measure(args: MeasureArgs, data_dir: Option<PathBuf>) -> Result<(), CliError> {
    let queries: Vec<Value> = args
        .queries
        .iter()
        .map(|text| {
            serde_json::from_str::<Value>(text)
                .ok()
                .filter(Value::is_object)
                .ok_or_else(|| {
                    CliError::setup(format!(
                        "`--query {text}` is not a JSON object, for example \
                         --query '{{\"kind\": \"volume\", \"part\": \"lid\"}}'"
                    ))
                })
        })
        .collect::<Result<_, _>>()?;
    let core = open(data_dir).await?;
    let project = app::find_project(&core, args.project.project.as_deref()).await?;
    let results = core.measure_run(&project.id, queries).await?;
    if args.json {
        return print_json(&results);
    }
    for result in &results {
        println!("{}", render::clean(&result.to_string()));
    }
    Ok(())
}

pub async fn versions(
    project: ProjectArg,
    json: bool,
    data_dir: Option<PathBuf>,
) -> Result<(), CliError> {
    let core = open(data_dir).await?;
    let project = app::find_project(&core, project.project.as_deref()).await?;
    let versions = core.versions_list(&project.id).await?;
    if json {
        return print_json(&versions);
    }
    println!("project {}  {}", project.id, render::clean(&project.name));
    if versions.is_empty() {
        println!("no versions yet");
    } else {
        println!("versions (* = current):");
        print!(
            "{}",
            render::versions_table(&versions, project.current_version)
        );
    }
    Ok(())
}

pub async fn revert(args: RevertArgs, data_dir: Option<PathBuf>) -> Result<(), CliError> {
    let core = open(data_dir).await?;
    let project = app::find_project(&core, args.project.project.as_deref()).await?;
    let version = core.version_revert(&project.id, args.version).await?;
    println!(
        "version {} is the current version of {} again; no version was changed or removed",
        version.number,
        render::clean(&project.name)
    );
    Ok(())
}

pub async fn questions(
    project: ProjectArg,
    json: bool,
    data_dir: Option<PathBuf>,
) -> Result<(), CliError> {
    let core = open(data_dir).await?;
    let project = app::find_project(&core, project.project.as_deref()).await?;
    let questions = core.questions_list(&project.id, None).await?;
    if json {
        return print_json(&questions);
    }
    if questions.is_empty() {
        println!("no open questions: nothing the contract requires rests on a guess");
    }
    for question in &questions {
        println!(
            "[{}] {}",
            render::clean(&question.requirement),
            render::clean(&question.text)
        );
        // A number from the web comes with its page and quote, as text.
        if let Some(number) = &question.web {
            print!("{}", research::web_number_lines(number, "    "));
        }
    }
    Ok(())
}

pub async fn settings(command: SettingsCommand, data_dir: Option<PathBuf>) -> Result<(), CliError> {
    let core = open(data_dir).await?;
    match command {
        SettingsCommand::Show { json } => {
            let settings = core.settings_get().await?;
            if json {
                return print_json(&settings);
            }
            let provider = settings.provider.as_ref().map_or_else(
                || "not chosen".to_owned(),
                |p| format!("{} / {}", p.kind.as_str(), render::clean(&p.model)),
            );
            println!("provider and model: {provider}");
            println!(
                "effort:             {} (sent only to Anthropic models documented to accept it)",
                settings.effort.as_str()
            );
            println!("printer:            {}", settings.printer_id);
            println!(
                "material:           {} ({})",
                render::clean(&settings.material.id),
                render::clean(&settings.material.family)
            );
            println!(
                "build plate:        {}",
                settings.plate_type.as_deref().map_or_else(
                    || "not chosen (`hotend settings set --plate-type ...`)".to_owned(),
                    render::clean
                )
            );
            println!(
                "nozzle:             {}",
                settings.nozzle_diameter_mm.map_or_else(
                    || "0.4 mm (assumed: `hotend settings set --nozzle <mm>`)".to_owned(),
                    |size| format!("{size} mm")
                )
            );
            println!(
                "spool:              slot {}, colour {}{}{}",
                render::clean(&settings.spool.slot),
                render::clean(settings.spool.colour_or_assumed()),
                if settings.spool.colour.is_none() {
                    " (assumed: `hotend settings set --spool-colour #rrggbb`)"
                } else {
                    ""
                },
                if settings.spool.silk { ", silk" } else { "" }
            );
            println!(
                "hand-off folder:    {}",
                settings.handoff_folder.as_deref().map_or_else(
                    || "not chosen: Hotend's own folder in the data folder (under WSL choose \
                        one on a Windows drive: `hotend settings set --handoff-folder ...`)"
                        .to_owned(),
                    |folder| render::clean(&folder.display().to_string())
                )
            );
            println!(
                "plate previews:     {}",
                match settings.previews {
                    hotend_core::PreviewsMode::Slicer =>
                        "the slicer's own (none from a headless Linux Bambu Studio; \
                         `hotend settings set --previews hotend-made`)",
                    hotend_core::PreviewsMode::HotendMade =>
                        "made by Hotend from its plate renders when the slicer drew none",
                }
            );
            println!("turns per message:  {}", settings.max_turns);
            let web = &settings.research;
            println!(
                "web research:       {}; {} searches and {} pages per message, {} and {} per \
                 project; source: {} (`hotend research status`)",
                match (
                    hotend_core::research::consent_covers(web, web.source),
                    web.default_on,
                ) {
                    _ if web.source == hotend_core::SearchSource::Off => "off",
                    (false, _) => "not agreed to yet",
                    (true, true) => "on for projects that do not say otherwise",
                    (true, false) => "off unless a project switches it on",
                },
                web.searches_per_turn,
                web.fetches_per_turn,
                web.searches_per_project,
                web.fetches_per_project,
                crate::research::source_word(web.source),
            );
            for (kind, path) in &settings.slicer_executables {
                println!("slicer {kind}: {}", path.display());
            }
            println!(
                "printer status:     {}",
                crate::printer::setup_line(&core.printer_lan_get().await?)
            );
            Ok(())
        }
        SettingsCommand::Set(set) => {
            let crate::SettingsSet {
                printer,
                material,
                family,
                density,
                plate_type,
                nozzle,
                spool_colour,
                spool_slot,
                provider,
                model,
                effort,
                max_turns,
                slicer,
                handoff_folder,
                clear_handoff_folder,
                previews,
                printer_lan_ip,
                printer_serial,
                clear_printer_lan,
                json,
            } = *set;
            // The printer's address is checked first too: one that cannot be used changes
            // nothing at all.
            let printer_lan = match (&printer_lan_ip, &printer_serial) {
                (Some(ip), Some(serial)) => Some(
                    hotend_core::PrinterLanAddress::new(ip, serial).map_err(|error| {
                        CliError::setup(format!("{error}; the settings were not changed"))
                    })?,
                ),
                _ => None,
            };
            // The settings as they were, read before anything is changed.
            let previous = core.settings_get().await?;
            // A hand-off folder is checked first, as a hand-off would use it on this host:
            // one that cannot be used changes nothing at all, the slicer included.
            if let Some(folder) = &handoff_folder {
                core.set_handoff_host(hotend_core::HostKind::detect());
                if let Err(error) = core.handoff_folder_check(folder).await {
                    return Err(CliError::setup(format!(
                        "{}; the settings were not changed",
                        render::clean(&error.to_string())
                    )));
                }
            }
            // The slicer next: choosing one may switch the printer to one it slices for.
            let slicer_chosen = slicer.is_some();
            if let Some(choice) = slicer {
                app::install_slicer(&core, false)?;
                let (kind, executable) = match choice.split_once('=') {
                    Some((kind, path)) => (kind.to_owned(), Some(PathBuf::from(path))),
                    None => (choice, None),
                };
                core.slicer_choose(&kind, executable).await?;
            }
            // The core checks a material against the slicer's table of profiles, which it
            // knows once the slicer backend is installed.
            if material.is_some() || printer.is_some() {
                app::install_slicer(&core, false)?;
            }
            let mut settings = core.settings_get().await?;
            if let Some(printer) = printer {
                settings.printer_id = printer;
            }
            if let Some(material) = material {
                settings.material.id = material;
            }
            if let Some(family) = family {
                settings.material.family = family;
            }
            if let Some(density) = density {
                settings.material.density_g_cm3 = Some(density);
            }
            if let Some(nozzle) = nozzle {
                settings.nozzle_diameter_mm = Some(nozzle);
            }
            if let Some(colour) = spool_colour {
                settings.spool.colour = Some(colour);
            }
            if let Some(slot) = spool_slot {
                // The command line spells the outside holders with dashes, the settings
                // with underscores.
                settings.spool.slot = match slot.as_str() {
                    "external-main" => "external_main".to_owned(),
                    "external-auxiliary" => "external_auxiliary".to_owned(),
                    _ => slot,
                };
            }
            if let Some(plate_type) = plate_type {
                settings.plate_type = Some(plate_type);
            }
            if let (Some(provider), Some(model)) = (provider, model) {
                settings.provider = Some(ProviderSettings {
                    kind: ProviderKind::from(provider),
                    model,
                });
            }
            if let Some(effort) = effort {
                settings.effort = effort.into();
            }
            if let Some(max_turns) = max_turns {
                settings.max_turns = max_turns;
            }
            if clear_handoff_folder {
                settings.handoff_folder = None;
            }
            if let Some(folder) = handoff_folder {
                settings.handoff_folder = Some(folder);
            }
            if let Some(previews) = previews {
                settings.previews = previews.into();
            }
            // Settings the core refuses leave them as they were before this command, the
            // slicer chosen above included.
            if let Err(error) = core.settings_set(settings.clone()).await {
                if slicer_chosen {
                    core.settings_set(previous).await?;
                }
                return Err(error.into());
            }
            if clear_printer_lan {
                core.printer_lan_clear().await?;
            }
            if let Some(address) = &printer_lan {
                core.printer_lan_set(&address.ip, &address.serial).await?;
            }
            if json {
                return print_json(&settings);
            }
            println!("settings saved");
            if printer_lan.is_some() {
                println!(
                    "printer status: `hotend printer status` reads it (read-only) and first \
                     asks you to confirm the printer's certificate"
                );
            }
            Ok(())
        }
        SettingsCommand::Slicers { json } => {
            app::install_slicer(&core, false)?;
            let slicers = core.slicers_list().await?;
            if json {
                return print_json(&slicers);
            }
            let printer = core.settings_get().await?.printer_id;
            let rows: Vec<Vec<String>> = slicers
                .iter()
                .map(|slicer| {
                    vec![
                        slicer.kind.clone(),
                        slicer.version.clone().unwrap_or_else(|| "-".to_owned()),
                        slicer.verdict.clone(),
                        slicer.printers.join(", "),
                        if slicer.printers.contains(&printer) {
                            "in use"
                        } else {
                            ""
                        }
                        .to_owned(),
                        slicer
                            .path
                            .as_ref()
                            .map_or_else(|| "not found".to_owned(), |p| p.display().to_string()),
                    ]
                })
                .collect();
            print!(
                "{}",
                table::render(
                    &["slicer", "version", "policy", "printers", "", "program"],
                    &rows
                )
            );
            println!(
                "choose one with `hotend settings set --slicer <slicer>[=<program>]`; the \
                 printer in the settings decides which slicer a slice runs"
            );
            Ok(())
        }
    }
}

pub async fn audit(args: AuditArgs, data_dir: Option<PathBuf>) -> Result<(), CliError> {
    let core = open(data_dir).await?;
    let project = match (&args.project.project, args.all) {
        (None, true) => None,
        (reference, _) => Some(app::find_project(&core, reference.as_deref()).await?.id),
    };
    let entries = core.audit_log(project.as_deref(), args.limit).await?;
    if args.json {
        return print_json(&entries);
    }
    let rows: Vec<Vec<String>> = entries
        .iter()
        .map(|entry| {
            vec![
                entry.id.to_string(),
                entry.kind.clone(),
                render::shorten(&render::clean(&entry.name), 28),
                render::shorten(&render::clean(&entry.outcome), 24),
                render::shorten(&render::clean(&entry.detail.to_string()), 60),
            ]
        })
        .collect();
    print!(
        "{}",
        table::render(&["id", "kind", "name", "outcome", "detail"], &rows)
    );
    Ok(())
}

pub async fn prepare(args: PrepareArgs, data_dir: Option<PathBuf>) -> Result<(), CliError> {
    let mut options = PrepareOptions::default();
    for text in &args.copies {
        let (part, copies) = pair::<u32>(text, "PART=COPIES, for example --copies lid=2")?;
        options.copies.insert(part, copies);
    }
    for text in &args.up {
        let (part, up) = pair::<String>(text, "PART=DIRECTION, for example --up lid=-z")?;
        options.orientations.insert(part, up);
    }
    let core = open(data_dir).await?;
    let project = app::find_project(&core, args.project.project.as_deref()).await?;
    let prepared = core.prepare_run(&project.id, args.version, options).await?;
    if args.json {
        print_json(&prepared)?;
    } else {
        println!(
            "version {} prepared for {} and {}: {} plate(s)",
            prepared.version,
            prepared.printer_id,
            render::clean(&prepared.material_id),
            prepared.plates.len()
        );
        for orientation in &prepared.orientations {
            println!(
                "- {} x{}: {}. {}",
                orientation.part,
                orientation.copies,
                render::clean(&orientation.turned),
                render::clean(&orientation.explanation)
            );
        }
        if let Some(clearance) = prepared.clearance_mm {
            println!(
                "laid out mid-bed with {clearance} mm of room round every piece for its brim, \
                 {} mm from the edge, clear of {} keep-out zone(s)",
                prepared.margin_mm.unwrap_or_default(),
                prepared.keepouts.len()
            );
        }
        for plate in &prepared.plates {
            let names: Vec<&str> = plate.items.iter().map(|item| item.name.as_str()).collect();
            println!("plate {}: {}", plate.index, names.join(", "));
            if let Some(file) = prepared.plate_file(plate.index) {
                println!("  {}", file.display());
            }
            if let Some(summary) = prepared.plans.iter().find(|p| p.plate == plate.index) {
                match &summary.blocked {
                    Some(blocked) => println!(
                        "  print settings: BLOCKED ({}): {}",
                        blocked.code,
                        render::clean(&blocked.sentence)
                    ),
                    None => println!("  print settings: {}", summary.settings_source),
                }
            }
        }
        if !prepared.plans.is_empty() {
            println!("`hotend plan explain` gives every print setting with its reason");
        }
        for piece in &prepared.unplaced {
            println!(
                "DID NOT FIT: {}: {}",
                piece.name,
                render::clean(&piece.reason)
            );
        }
        let mut checks = String::new();
        report::check_table(&mut checks, &prepared.checks);
        print!("{}", render::clean(&checks));
    }
    if prepared.unplaced.is_empty() {
        Ok(())
    } else {
        Err(CliError::Reported)
    }
}

pub async fn gauge(args: GaugeArgs, data_dir: Option<PathBuf>) -> Result<(), CliError> {
    let cards: Vec<CardRef> = args
        .cards
        .iter()
        .map(|text| {
            serde_json::from_str(text).map_err(|_| {
                CliError::setup(format!(
                    "`--card {text}` is not a card, for example --card '{{\"card\": \
                     \"peg_hole\", \"args\": {{\"diameter\": 6.0, \"length\": 6.0, \"fit\": \
                     \"slide\"}}}}'"
                ))
            })
        })
        .collect::<Result<_, _>>()?;
    if !args.out.is_dir() {
        return Err(CliError::setup(format!(
            "the output folder {} does not exist; create it first",
            args.out.display()
        )));
    }
    let core = open(data_dir).await?;
    let (project, cards) = if cards.is_empty() {
        let project = app::find_project(&core, args.project.project.as_deref()).await?;
        (Some(project.id), None)
    } else {
        (None, Some(cards))
    };
    let export = core
        .gauge_export(project.as_deref(), cards, args.nozzle, &args.out)
        .await?;
    if args.json {
        return print_json(&export);
    }
    for gauge in &export.gauges {
        println!("gauge {} ({})", gauge.name, gauge.card);
        for dimension in &gauge.dimensions {
            println!(
                "  measure {}: {} drawn {:.3} mm - {}",
                dimension.object,
                dimension.feature,
                dimension.drawn,
                render::clean(&dimension.what)
            );
        }
    }
    for file in &export.files {
        println!("{}", file.path.display());
    }
    println!(
        "print the plate, measure with a caliper and record the readings with \
         `hotend calibrate --reading hole:<drawn>=<measured>`"
    );
    Ok(())
}

/// Parses `hole:6.49=6.31`.
fn reading(text: &str) -> Result<GaugeReading, CliError> {
    let parsed = text.split_once(':').and_then(|(feature, sizes)| {
        let feature = match feature.trim() {
            "hole" => GaugeFeature::Hole,
            "peg" => GaugeFeature::Peg,
            _ => return None,
        };
        let (drawn, measured) = sizes.split_once('=')?;
        Some(GaugeReading {
            feature,
            drawn: drawn.trim().parse().ok()?,
            measured: measured.trim().parse().ok()?,
        })
    });
    parsed.ok_or_else(|| {
        CliError::setup(format!(
            "`--reading {text}` is not FEATURE:DRAWN=MEASURED with hole or peg and sizes in \
             mm, for example --reading hole:6.49=6.31"
        ))
    })
}

pub async fn calibrate(args: CalibrateArgs, data_dir: Option<PathBuf>) -> Result<(), CliError> {
    let readings: Vec<GaugeReading> = args
        .readings
        .iter()
        .map(|text| reading(text))
        .collect::<Result<_, _>>()?;
    let core = open(data_dir).await?;
    if readings.is_empty() {
        let calibration = core.calibration_get().await?;
        if args.json {
            return print_json(&calibration);
        }
        match &calibration.table {
            Some(table) => println!(
                "calibration of {}:\n{}",
                calibration.printer_id,
                serde_json::to_string_pretty(table).unwrap_or_default()
            ),
            None => println!(
                "nothing is measured for {} yet: every clearance is a guessed default. \
                 Export a gauge with `hotend gauge`, print it and record what you measure",
                calibration.printer_id
            ),
        }
        return Ok(());
    }
    let update = core.calibration_record(readings, args.nozzle).await?;
    if args.json {
        return print_json(&update);
    }
    println!(
        "recorded for {} and {} at {}:",
        update.printer_id,
        render::clean(&update.material_id),
        update.measured_at
    );
    let changes: BTreeMap<&str, String> = update
        .changes
        .iter()
        .map(|change| {
            (
                change.quantity.as_str(),
                format!(
                    "{:.3} -> {:.3} mm (from {} reading(s), sigma {:.3})",
                    change.before, change.after, change.readings, change.sigma
                ),
            )
        })
        .collect();
    for (quantity, change) in changes {
        println!("- {quantity}: {change}");
    }
    println!("the next build of every project uses the new table");
    Ok(())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[test]
    fn readings_and_pairs_are_parsed_strictly() {
        let parsed = reading("hole:6.49=6.31").unwrap();
        assert_eq!(parsed.feature, GaugeFeature::Hole);
        assert_eq!((parsed.drawn, parsed.measured), (6.49, 6.31));
        for bad in ["slot:6=6", "hole:6", "hole:a=b", "6=6"] {
            assert!(reading(bad).is_err(), "{bad}");
        }
        assert_eq!(pair::<u32>("lid=2", "x").unwrap(), ("lid".to_owned(), 2));
        assert!(pair::<u32>("lid=two", "x").is_err());
        assert!(pair::<u32>("=2", "x").is_err());
    }
}

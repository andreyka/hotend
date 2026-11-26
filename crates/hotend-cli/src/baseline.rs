//! `hotend baseline`: the proven baselines of the data folder. A proven baseline is the
//! set of sticking settings of a print the person reported as good, for one printer, plate
//! type and material; later plans of the same three are never less careful than it.
//!
//! Every value is a Hotend field, given explicitly and checked against its hard range by the
//! core; there are no defaults, so nothing is recorded that the person did not state. The
//! source line is the person's own label for the print and is only shown.

use std::path::PathBuf;

use clap::{Args, Subcommand};
use hotend_core::{BaselineFields, InvalidBaseline, ProvenBaseline, SecretBackend};

use crate::error::CliError;
use crate::{app, render};

/// Which printer, plate and material a baseline belongs to.
#[derive(Debug, Args)]
pub struct BaselineKey {
    /// The printer profile id, for example bambu-x2d.
    #[arg(long)]
    pub printer: String,
    /// The plate the print was made on: cool, textured-pei, high-temp or engineering.
    #[arg(long)]
    pub plate: String,
    /// The material id, for example pla-generic.
    #[arg(long)]
    pub material: String,
}

/// What `baseline set` records. Every value is required.
#[derive(Debug, Args)]
pub struct BaselineSet {
    #[command(flatten)]
    pub key: BaselineKey,
    /// The day the print was made, YYYY-MM-DD.
    #[arg(long, value_name = "YYYY-MM-DD")]
    pub printed_on: String,
    /// Your words for the print, one line of at most 120 characters, for example "holder
    /// print, reported good". Shown beside the plan; never read as a setting.
    #[arg(long, value_name = "TEXT")]
    pub from_print: String,
    /// adhesion.level: 0, 1 or 2.
    #[arg(long)]
    pub level: u8,
    /// adhesion.brim_kind: none or outer.
    #[arg(long, value_name = "KIND")]
    pub brim_kind: String,
    /// adhesion.brim_width_mm: 0 to 15 (0 exactly when the kind is none).
    #[arg(long, value_name = "MM")]
    pub brim_width: i64,
    /// first_layer.wall_speed_mm_s: 10 to 50.
    #[arg(long, value_name = "MM/S")]
    pub first_layer_wall_speed: i64,
    /// first_layer.infill_speed_mm_s: 15 to 105.
    #[arg(long, value_name = "MM/S")]
    pub first_layer_infill_speed: i64,
    /// first_layer.bed_c: 30 to 120.
    #[arg(long, value_name = "C")]
    pub first_layer_bed: i64,
    /// layers.bed_c: 30 to 120, never above the first layer's.
    #[arg(long, value_name = "C")]
    pub layers_bed: i64,
    /// cooling.part_fan_off_layers: 1 to 10.
    #[arg(long, value_name = "LAYERS")]
    pub part_fan_off_layers: i64,
    /// cooling.aux_fan_first_layers_pct: 0 to 40.
    #[arg(long, value_name = "PERCENT")]
    pub aux_fan_first_layers: i64,
    /// cooling.aux_fan_max_pct: 0 to 100.
    #[arg(long, value_name = "PERCENT")]
    pub aux_fan_max: i64,
    /// shell.wall_loops: 2 to 6.
    #[arg(long, value_name = "LOOPS")]
    pub wall_loops: i64,
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Subcommand)]
pub enum BaselineCommand {
    /// Record the sticking settings of a print you reported as good, as the floor for later
    /// plans on the same printer, plate and material. Replaces an earlier one of the three.
    Set(Box<BaselineSet>),
    /// List the proven baselines of this data folder.
    Show {
        #[arg(long)]
        json: bool,
    },
    /// Remove the proven baseline of a printer, plate and material (later plans are then
    /// decided by the rules alone), or with --invalid every record that cannot be used.
    Remove {
        /// The printer profile id, for example bambu-x2d.
        #[arg(long, required_unless_present = "invalid", conflicts_with = "invalid")]
        printer: Option<String>,
        /// The plate the print was made on: cool, textured-pei, high-temp or engineering.
        #[arg(long, required_unless_present = "invalid", conflicts_with = "invalid")]
        plate: Option<String>,
        /// The material id, for example pla-generic.
        #[arg(long, required_unless_present = "invalid", conflicts_with = "invalid")]
        material: Option<String>,
        /// Remove every record of the file that cannot be used on its own (the ones
        /// `baseline show` warns of, except a printer, plate and material recorded twice,
        /// which is settled by key); usable records are never touched.
        #[arg(long)]
        invalid: bool,
    },
}

/// The command that removes every record that cannot be used.
const REMOVE_INVALID: &str = "hotend baseline remove --invalid";

fn print_json(value: &impl serde::Serialize) -> Result<(), CliError> {
    let text = serde_json::to_string_pretty(value).map_err(|e| CliError::failed(e.to_string()))?;
    println!("{text}");
    Ok(())
}

/// A record that cannot be used, as one line.
fn invalid_text(entry: &InvalidBaseline) -> String {
    let or_unknown = |text: &str| {
        if text.is_empty() {
            "?".to_owned()
        } else {
            text.to_owned()
        }
    };
    let name = format!(
        "the proven baseline {} / {} / {} cannot be used and is ignored ({})",
        or_unknown(&entry.printer_id),
        or_unknown(&entry.plate_type),
        or_unknown(&entry.material_id),
        entry.problem
    );
    if entry.has_key() {
        format!(
            "{name}; plans on that printer, plate and material are refused until it is \
             corrected, recorded again with `hotend baseline set`, or removed with \
             `hotend baseline remove --printer {} --plate {} --material {}`",
            entry.printer_id, entry.plate_type, entry.material_id
        )
    } else {
        format!(
            "{name}; it does not name a printer, plate and material, so it applies to no \
             plan; remove it with `{REMOVE_INVALID}`"
        )
    }
}

/// One baseline as lines of text.
pub fn baseline_text(entry: &ProvenBaseline) -> String {
    let v = &entry.values;
    let mut out = format!(
        "{} / {} / {}: proven {} ({})\n",
        entry.printer_id, entry.plate_type, entry.material_id, entry.printed_on, entry.source
    );
    for (field, value) in [
        ("adhesion.level", v.level.to_string()),
        ("adhesion.brim_kind", v.brim_kind.clone()),
        ("adhesion.brim_width_mm", format!("{} mm", v.brim_width_mm)),
        (
            "first_layer.wall_speed_mm_s",
            format!("{} mm/s", v.first_layer_wall_speed_mm_s),
        ),
        (
            "first_layer.infill_speed_mm_s",
            format!("{} mm/s", v.first_layer_infill_speed_mm_s),
        ),
        ("first_layer.bed_c", format!("{} C", v.first_layer_bed_c)),
        ("layers.bed_c", format!("{} C", v.layers_bed_c)),
        (
            "cooling.part_fan_off_layers",
            format!("{} layers", v.part_fan_off_layers),
        ),
        (
            "cooling.aux_fan_first_layers_pct",
            format!("{} %", v.aux_fan_first_layers_pct),
        ),
        (
            "cooling.aux_fan_max_pct",
            format!("{} %", v.aux_fan_max_pct),
        ),
        ("shell.wall_loops", v.wall_loops.to_string()),
    ] {
        out.push_str(&format!("  {field:<34} {value}\n"));
    }
    out
}

pub async fn run(command: BaselineCommand, data_dir: Option<PathBuf>) -> Result<(), CliError> {
    let core = app::open_core(data_dir, SecretBackend::from_env()).await?;
    match command {
        BaselineCommand::Set(args) => {
            let args = *args;
            let entry = ProvenBaseline {
                printer_id: args.key.printer,
                plate_type: args.key.plate,
                material_id: args.key.material,
                printed_on: args.printed_on,
                source: args.from_print,
                values: BaselineFields {
                    level: args.level,
                    brim_kind: args.brim_kind,
                    brim_width_mm: args.brim_width,
                    first_layer_wall_speed_mm_s: args.first_layer_wall_speed,
                    first_layer_infill_speed_mm_s: args.first_layer_infill_speed,
                    first_layer_bed_c: args.first_layer_bed,
                    layers_bed_c: args.layers_bed,
                    part_fan_off_layers: args.part_fan_off_layers,
                    aux_fan_first_layers_pct: args.aux_fan_first_layers,
                    aux_fan_max_pct: args.aux_fan_max,
                    wall_loops: args.wall_loops,
                },
                recorded_at_ms: 0,
            };
            let key = (
                entry.printer_id.clone(),
                entry.plate_type.clone(),
                entry.material_id.clone(),
            );
            // A value outside its range is the person's argument: exit code 2.
            let replaced = core
                .baseline_set(entry)
                .await
                .map_err(|error| match error {
                    hotend_core::CoreError::Modeling(
                        hotend_core::modeling::ModelingError::Invalid(message),
                    ) => CliError::setup(message),
                    other => other.into(),
                })?;
            let stored = core
                .baseline_list()
                .await?
                .into_iter()
                .find(|entry| entry.is_for(&key.0, &key.1, &key.2))
                .ok_or_else(|| CliError::failed("the baseline was not stored"))?;
            if args.json {
                return print_json(&stored);
            }
            print!("{}", render::clean(&baseline_text(&stored)));
            println!(
                "{}",
                if replaced.is_some() {
                    "recorded; it replaces the earlier proven baseline of this printer, plate and material"
                } else {
                    "recorded"
                }
            );
            println!(
                "later plans on this printer, plate and material are never less careful than this; \
                 `hotend plan explain` shows which values it set"
            );
            Ok(())
        }
        BaselineCommand::Show { json } => {
            let all = core.baseline_list().await?;
            // Records that cannot be used are warnings on stderr, so that `--json` still
            // prints the usable records alone.
            for entry in core.baseline_invalid().await? {
                eprintln!("warning: {}", render::clean(&invalid_text(&entry)));
            }
            if json {
                return print_json(&all);
            }
            if all.is_empty() {
                println!(
                    "no proven baseline is recorded in this data folder: the rules decide alone"
                );
            }
            for entry in &all {
                print!("{}", render::clean(&baseline_text(entry)));
            }
            Ok(())
        }
        BaselineCommand::Remove { invalid: true, .. } => {
            let removed = core.baseline_remove_invalid().await?;
            let or_unknown = |text: &str| {
                if text.is_empty() {
                    "?".to_owned()
                } else {
                    text.to_owned()
                }
            };
            for entry in &removed {
                println!(
                    "{}",
                    render::clean(&format!(
                        "removed the record of {} / {} / {} that could not be used ({})",
                        or_unknown(&entry.printer_id),
                        or_unknown(&entry.plate_type),
                        or_unknown(&entry.material_id),
                        entry.problem
                    ))
                );
            }
            println!(
                "{}",
                match removed.len() {
                    0 => "every record can be used; nothing was removed".to_owned(),
                    1 => "removed 1 record that could not be used; the usable records were kept"
                        .to_owned(),
                    n => format!(
                        "removed {n} records that could not be used; the usable records were \
                         kept"
                    ),
                }
            );
            Ok(())
        }
        BaselineCommand::Remove {
            printer,
            plate,
            material,
            ..
        } => {
            let (Some(printer), Some(plate), Some(material)) = (printer, plate, material) else {
                return Err(CliError::setup(format!(
                    "name the record with --printer, --plate and --material, or remove every \
                     record that cannot be used with `{REMOVE_INVALID}`"
                )));
            };
            let key = BaselineKey {
                printer,
                plate,
                material,
            };
            let removal = core
                .baseline_remove(&key.printer, &key.plate, &key.material)
                .await?;
            if removal.is_empty() {
                return Err(CliError::setup(format!(
                    "no proven baseline is recorded for {} / {} / {}; `hotend baseline show` lists them",
                    key.printer, key.plate, key.material
                )));
            }
            if let Some(removed) = &removal.removed {
                print!("{}", render::clean(&baseline_text(removed)));
            }
            for entry in &removal.invalid {
                println!(
                    "{}",
                    render::clean(&format!(
                        "removed the record of {} / {} / {} that could not be used ({})",
                        key.printer, key.plate, key.material, entry.problem
                    ))
                );
            }
            println!(
                "removed; later plans on this printer, plate and material follow the rules alone"
            );
            Ok(())
        }
    }
}

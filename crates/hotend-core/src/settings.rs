//! User settings kept in the store. Nothing secret lives here: provider keys are in the
//! secret store and are never part of a settings value.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::egress::{Effort, ProviderKind, research};
use crate::engine::MaterialContext;
use crate::profiles::DEFAULT_PRINTER_ID;
use crate::search::SearchSource;

/// Settings key under which the whole document is stored.
pub(crate) const SETTINGS_KEY: &str = "app";

/// Which model the agent talks to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderSettings {
    pub kind: ProviderKind,
    /// The provider's model name, as the user typed or picked it.
    pub model: String,
}

/// How the assistant may use the provider's web search and web fetch. These are the limits
/// for every project; a project can switch research off or narrow it (see
/// [`crate::research::ProjectResearch`]).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct ResearchSettings {
    /// Whether a project that says nothing has research on.
    pub default_on: bool,
    /// When the person agreed that search queries and page addresses leave this computer
    /// for the provider. `None` until they do: no run gets research before that.
    pub acknowledged_at_ms: Option<i64>,
    /// Where research comes from: the provider's own search (`built_in`, the default),
    /// `keenable`, `tavily` or `off`. Changed with `Core::research_source_set`.
    pub source: SearchSource,
    /// The source the agreement of `acknowledged_at_ms` was given for. An agreement covers
    /// that source only; `None` (an agreement from before sources existed) means `built_in`.
    /// Set by `research_consent` only.
    pub acknowledged_source: Option<SearchSource>,
    /// Searches the assistant may run while answering one message.
    pub searches_per_turn: u32,
    /// Pages it may read while answering one message; 0 for search only.
    pub fetches_per_turn: u32,
    /// Searches per project, over all messages.
    pub searches_per_project: u32,
    /// Pages read per project, over all messages.
    pub fetches_per_project: u32,
    /// Most tokens of one fetched page that reach the model.
    pub max_content_tokens: u32,
    /// Largest single web result kept in the conversation, in bytes.
    pub max_result_bytes: usize,
    /// Most web result bytes a project's conversation may collect.
    pub max_project_result_bytes: u64,
    /// How long the provider keeps the prompt cache of a research conversation: `5m` or `1h`.
    pub cache_ttl: String,
}

impl Default for ResearchSettings {
    fn default() -> Self {
        Self {
            default_on: true,
            acknowledged_at_ms: None,
            source: SearchSource::BuiltIn,
            acknowledged_source: None,
            searches_per_turn: 3,
            fetches_per_turn: 2,
            searches_per_project: 8,
            fetches_per_project: 6,
            max_content_tokens: 6000,
            max_result_bytes: research::DEFAULT_RESULT_BYTES,
            max_project_result_bytes: 1536 * 1024,
            cache_ttl: "1h".to_owned(),
        }
    }
}

impl ResearchSettings {
    fn validate(&self) -> Result<(), String> {
        let per_turn = 1..=research::MAX_USES_PER_TURN;
        if !per_turn.contains(&self.searches_per_turn)
            || self.fetches_per_turn > research::MAX_USES_PER_TURN
        {
            return Err(format!(
                "research.searches_per_turn must be between 1 and {max} and \
                 research.fetches_per_turn between 0 and {max}",
                max = research::MAX_USES_PER_TURN
            ));
        }
        if !(1..=MAX_RESEARCH_PER_PROJECT).contains(&self.searches_per_project)
            || self.fetches_per_project > MAX_RESEARCH_PER_PROJECT
        {
            return Err(format!(
                "research.searches_per_project must be between 1 and \
                 {MAX_RESEARCH_PER_PROJECT} and research.fetches_per_project between 0 and \
                 {MAX_RESEARCH_PER_PROJECT}"
            ));
        }
        if !research::CONTENT_TOKENS.contains(&self.max_content_tokens) {
            return Err(format!(
                "research.max_content_tokens must be between {} and {}",
                research::CONTENT_TOKENS.start(),
                research::CONTENT_TOKENS.end()
            ));
        }
        if !research::RESULT_BYTES.contains(&self.max_result_bytes)
            || !(self.max_result_bytes as u64..=64 * 1024 * 1024)
                .contains(&self.max_project_result_bytes)
        {
            return Err(format!(
                "research.max_result_bytes must be between {} and {}, and \
                 research.max_project_result_bytes between that and 64 MB",
                research::RESULT_BYTES.start(),
                research::RESULT_BYTES.end()
            ));
        }
        if !["5m", "1h"].contains(&self.cache_ttl.as_str()) {
            return Err("research.cache_ttl must be `5m` or `1h`".into());
        }
        if self.acknowledged_source == Some(SearchSource::Off) {
            return Err("research.acknowledged_source cannot be `off`".into());
        }
        Ok(())
    }
}

/// Most searches or fetches a project may be allowed.
pub const MAX_RESEARCH_PER_PROJECT: u32 = 200;

/// The nozzle sizes a person may say are fitted, in mm.
pub const NOZZLE_DIAMETERS_MM: [f64; 4] = [0.2, 0.4, 0.6, 0.8];

/// The colour a spool is assumed to have until the person says (a neutral grey).
pub const ASSUMED_SPOOL_COLOUR: &str = "#898989";

/// The one spool the print settings plan with (`setup.spools`). Its
/// material is the material of the settings; version 1 has one spool.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct SpoolSettings {
    /// Where it sits: an AMS slot `1` to `16`, `external_main` or `external_auxiliary`.
    pub slot: String,
    /// Its colour as `#RRGGBB`; `None` until the person says, and then
    /// [`ASSUMED_SPOOL_COLOUR`] is used and shown as an assumption.
    pub colour: Option<String>,
    /// Whether it is a silk filament, which sticks less well.
    pub silk: bool,
}

impl Default for SpoolSettings {
    fn default() -> Self {
        Self {
            slot: "1".to_owned(),
            colour: None,
            silk: false,
        }
    }
}

impl SpoolSettings {
    /// The colour planned with: the person's, or the assumed grey.
    pub fn colour_or_assumed(&self) -> &str {
        self.colour.as_deref().unwrap_or(ASSUMED_SPOOL_COLOUR)
    }

    fn validate(&self) -> Result<(), String> {
        if hotend_settings::model::SpoolSlot::parse(&self.slot).is_none() {
            return Err(
                "spool.slot must be an AMS slot 1 to 16, `external_main` or `external_auxiliary`"
                    .into(),
            );
        }
        if self
            .colour
            .as_deref()
            .is_some_and(|colour| hotend_settings::model::Colour::parse(colour).is_none())
        {
            return Err("spool.colour must be a colour written #rrggbb".into());
        }
        Ok(())
    }
}

/// All user settings.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct Settings {
    /// The chosen provider and model; `None` until the user picks one.
    pub provider: Option<ProviderSettings>,
    /// Printer the models are checked against (an id from the printer profiles).
    pub printer_id: String,
    /// Material the models are checked against.
    pub material: MaterialContext,
    /// The build plate that is on the printer, as Hotend's id for it (`cool`,
    /// `textured-pei`, `high-temp`, `engineering`); `None` until the person says which.
    /// Only the person at the printer knows it, so there is no default: a slice is made for
    /// this plate, the slice is recorded with it, and a print job is checked against it.
    pub plate_type: Option<String>,
    /// How much the model may think and spend on one answer. Sent only to Anthropic models
    /// documented to accept it (see [`crate::egress::effort`]); other providers and unknown
    /// model names get no effort field. `high` when the saved settings name none.
    pub effort: Effort,
    /// Most model turns one message may use.
    pub max_turns: u32,
    /// Slicer programs the user chose, by slicer kind (`bambu-studio`, `prusa-slicer`). A
    /// kind without an entry is looked for in the usual places. Which slicer a slice uses
    /// follows from the printer: each printer profile names its slicer.
    pub slicer_executables: BTreeMap<String, PathBuf>,
    /// Web research through the provider.
    pub research: ResearchSettings,
    // ---- print settings: what the plan is computed from --------------------------------
    /// The diameter of the nozzle fitted to the printer, in mm: one of
    /// [`NOZZLE_DIAMETERS_MM`]. `None` until the person says; 0.4 mm is then assumed and
    /// shown as an assumption.
    pub nozzle_diameter_mm: Option<f64>,
    /// The spool the plates are printed from.
    pub spool: SpoolSettings,

    // ---- delivery: the Bambu Connect hand-off -------------------------------------------
    /// The folder the person chose for the Bambu Connect hand-off: the checked file is
    /// placed there and Bambu Connect's import link names it. Under WSL it is required for
    /// a hand-off and must be a folder on a Windows drive (`/mnt/<drive letter>/...`); on
    /// other systems `None` means `<data-root>/handoff`.
    pub handoff_folder: Option<PathBuf>,
    /// Where the plate previews of a sliced file come from ([`PreviewsMode`]). The
    /// slicer's own (none, from a headless Linux Bambu Studio) unless the person asks for
    /// previews made by Hotend.
    pub previews: PreviewsMode,
}

/// Where the plate previews (`Metadata/plate_<n>.png` and its companions) of a sliced
/// Bambu Studio file come from. Bambu Connect does not import a file without them, and
/// the Linux Bambu Studio cannot draw them headless.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PreviewsMode {
    /// Only what the slicer drew; nothing is added (the default).
    #[default]
    Slicer,
    /// When the slicer drew none, Hotend puts its own renders of the plate into the
    /// sliced file before it is checked, labelled "plate previews made by Hotend" in the
    /// slice's warnings, the review and the confirmation.
    HotendMade,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            provider: None,
            printer_id: DEFAULT_PRINTER_ID.to_owned(),
            // Generic PLA is the default: its stock presets suit PLA of any brand. Settings
            // saved with `pla-basic` keep it: nothing is migrated.
            material: MaterialContext {
                id: "pla-generic".to_owned(),
                family: "PLA".to_owned(),
                density_g_cm3: Some(1.24),
                max_overhang_deg: None,
            },
            plate_type: None,
            effort: Effort::default(),
            max_turns: 24,
            slicer_executables: BTreeMap::new(),
            research: ResearchSettings::default(),
            nozzle_diameter_mm: None,
            spool: SpoolSettings::default(),
            handoff_folder: None,
            previews: PreviewsMode::Slicer,
        }
    }
}

impl Settings {
    /// Checks the material against what the slicer can slice with for the printer
    /// (`catalogue`, from the slicer backend's table): an id it does not list is refused,
    /// with the ids it does list. Without a catalogue (no slicer backend, or a printer the
    /// backend has no table for) there is nothing to check against and the id is accepted.
    pub fn validate_material(
        &self,
        catalogue: Option<&crate::gateway::SlicerCatalogue>,
    ) -> Result<(), String> {
        let Some(catalogue) = catalogue else {
            return Ok(());
        };
        if catalogue.knows_material(&self.material.id) {
            return Ok(());
        }
        Err(format!(
            "material `{}` is not a material Hotend has a slicer profile for on {}; the \
             known materials are: {}",
            crate::util::truncate(&self.material.id, 64),
            self.printer_id,
            catalogue.material_ids()
        ))
    }

    /// Checks the values a user or a UI can get wrong.
    pub fn validate(&self) -> Result<(), String> {
        let slug = |s: &str| {
            !s.is_empty()
                && s.len() <= 64
                && s.chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        };
        if !slug(&self.printer_id) {
            return Err("printer_id must be 1 to 64 lower-case letters, digits or dashes".into());
        }
        if self.slicer_executables.len() > 8
            || self
                .slicer_executables
                .iter()
                .any(|(kind, path)| !slug(kind) || !path.is_absolute())
        {
            return Err("a slicer executable needs the slicer's kind and an absolute path".into());
        }
        if self.plate_type.as_deref().is_some_and(|plate| !slug(plate)) {
            return Err(
                "plate_type must be a plate id such as `cool` or `textured-pei` (lower-case \
                 letters, digits or dashes)"
                    .into(),
            );
        }
        if crate::profiles::printer_profile(&self.printer_id).is_none() {
            return Err(format!(
                "this build has no profile for printer `{}`",
                self.printer_id
            ));
        }
        if !slug(&self.material.id) || self.material.family.len() > 32 {
            return Err("material.id must be a short lower-case id".into());
        }
        let positive = |v: Option<f64>| v.is_none_or(|v| v.is_finite() && v > 0.0);
        if !positive(self.material.density_g_cm3)
            || !positive(self.material.max_overhang_deg)
            || self.material.max_overhang_deg.is_some_and(|v| v >= 90.0)
        {
            return Err("material density and overhang limit must be positive numbers".into());
        }
        if let Some(provider) = &self.provider {
            let model_ok = !provider.model.is_empty()
                && provider.model.len() <= 200
                && provider.model.chars().all(|c| c.is_ascii_graphic());
            if !model_ok {
                return Err("provider.model must be 1 to 200 printable characters".into());
            }
        }
        if !(1..=200).contains(&self.max_turns) {
            return Err("max_turns must be between 1 and 200".into());
        }
        if self.nozzle_diameter_mm.is_some_and(|size| {
            !NOZZLE_DIAMETERS_MM
                .iter()
                .any(|allowed| (allowed - size).abs() < 1e-9)
        }) {
            return Err("nozzle_diameter_mm must be 0.2, 0.4, 0.6 or 0.8".into());
        }
        self.spool.validate()?;
        if self
            .handoff_folder
            .as_ref()
            .is_some_and(|folder| !folder.is_absolute() || folder.as_os_str().len() > 1024)
        {
            return Err("handoff_folder must be an absolute path to a folder".into());
        }
        self.research.validate()
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[test]
    fn defaults_are_valid_and_bad_values_are_named() {
        let settings = Settings::default();
        settings.validate().unwrap();
        let mut bad = settings.clone();
        bad.printer_id = "../x".into();
        assert!(bad.validate().unwrap_err().contains("printer_id"));
        let mut bad = settings.clone();
        bad.printer_id = "prusa-unknown".into();
        assert!(bad.validate().unwrap_err().contains("no profile"));
        let mut bad = settings.clone();
        bad.material.density_g_cm3 = Some(-1.0);
        assert!(bad.validate().is_err());
        // No plate is chosen until the person says which one is on the printer.
        assert_eq!(settings.plate_type, None);
        let mut chosen = settings.clone();
        chosen.plate_type = Some("textured-pei".into());
        chosen.validate().unwrap();
        chosen.plate_type = Some("Textured PEI Plate".into());
        assert!(chosen.validate().unwrap_err().contains("plate_type"));
        let mut bad = settings;
        bad.provider = Some(ProviderSettings {
            kind: ProviderKind::Anthropic,
            model: "has space".into(),
        });
        assert!(bad.validate().is_err());
    }

    #[test]
    fn the_print_settings_have_assumed_defaults_and_checked_values() {
        let settings = Settings::default();
        assert_eq!(settings.material.id, "pla-generic");
        assert_eq!(settings.material.family, "PLA");
        assert_eq!(settings.material.density_g_cm3, Some(1.24));
        assert_eq!(settings.nozzle_diameter_mm, None);
        assert_eq!(settings.spool.slot, "1");
        assert_eq!(settings.spool.colour, None);
        assert_eq!(settings.spool.colour_or_assumed(), "#898989");
        assert!(!settings.spool.silk);
        for size in NOZZLE_DIAMETERS_MM {
            let mut set = settings.clone();
            set.nozzle_diameter_mm = Some(size);
            set.validate().unwrap();
        }
        let bad = |change: &dyn Fn(&mut Settings)| {
            let mut settings = Settings::default();
            change(&mut settings);
            settings.validate().unwrap_err()
        };
        assert!(bad(&|s| s.nozzle_diameter_mm = Some(0.5)).contains("nozzle_diameter_mm"));
        assert!(bad(&|s| s.nozzle_diameter_mm = Some(f64::NAN)).contains("nozzle_diameter_mm"));
        assert!(bad(&|s| s.spool.slot = "17".into()).contains("spool.slot"));
        assert!(bad(&|s| s.spool.slot = "external-main".into()).contains("spool.slot"));
        assert!(bad(&|s| s.spool.colour = Some("grey".into())).contains("spool.colour"));
        let mut outside = settings.clone();
        outside.spool.slot = "external_main".into();
        outside.spool.colour = Some("#ff8800".into());
        outside.validate().unwrap();
        // Settings saved before this block, and with the earlier default material, load.
        let old: Settings = serde_json::from_str(
            r#"{"material": {"id": "pla-basic", "family": "PLA", "density_g_cm3": 1.24}}"#,
        )
        .unwrap();
        assert_eq!(old.material.id, "pla-basic");
        assert_eq!(old.spool, SpoolSettings::default());
        old.validate().unwrap();
        assert!(serde_json::from_str::<Settings>(r#"{"spool": {"brand": "x"}}"#).is_err());
    }

    #[test]
    fn research_limits_have_small_defaults_and_fixed_ranges() {
        let research = ResearchSettings::default();
        assert!(research.default_on && research.acknowledged_at_ms.is_none());
        assert_eq!(
            (research.searches_per_turn, research.fetches_per_turn),
            (3, 2)
        );
        assert_eq!(
            (research.searches_per_project, research.fetches_per_project),
            (8, 6)
        );
        let bad = |change: &dyn Fn(&mut ResearchSettings)| {
            let mut settings = Settings::default();
            change(&mut settings.research);
            settings.validate().unwrap_err()
        };
        assert!(bad(&|r| r.searches_per_turn = 0).contains("searches_per_turn"));
        assert!(bad(&|r| r.fetches_per_turn = 11).contains("fetches_per_turn"));
        assert!(bad(&|r| r.searches_per_project = 201).contains("searches_per_project"));
        assert!(bad(&|r| r.max_content_tokens = 500_000).contains("max_content_tokens"));
        assert!(bad(&|r| r.max_result_bytes = 1).contains("max_result_bytes"));
        assert!(bad(&|r| r.max_project_result_bytes = 10).contains("max_project_result_bytes"));
        assert!(bad(&|r| r.cache_ttl = "1d".into()).contains("cache_ttl"));
        // Settings written before research existed still load.
        let old: Settings = serde_json::from_str(r#"{"max_turns": 5}"#).unwrap();
        assert_eq!(old.research, ResearchSettings::default());
    }

    #[test]
    fn the_research_source_defaults_to_built_in_and_round_trips() {
        let defaults = ResearchSettings::default();
        assert_eq!(defaults.source, SearchSource::BuiltIn);
        assert_eq!(defaults.acknowledged_source, None);
        // Settings saved before sources existed: an agreement without a source.
        let old: Settings =
            serde_json::from_str(r#"{"research": {"acknowledged_at_ms": 5}}"#).unwrap();
        assert_eq!(old.research.source, SearchSource::BuiltIn);
        assert_eq!(old.research.acknowledged_source, None);
        for source in SearchSource::ALL {
            let mut settings = Settings::default();
            settings.research.source = source;
            settings.research.acknowledged_at_ms = Some(7);
            settings.research.acknowledged_source = (source != SearchSource::Off).then_some(source);
            settings.validate().unwrap();
            let text = serde_json::to_string(&settings).unwrap();
            assert!(
                text.contains(&format!(r#""source":"{}""#, source.as_str())),
                "{text}"
            );
            let back: Settings = serde_json::from_str(&text).unwrap();
            assert_eq!(back, settings);
            // Nothing secret is part of a settings value.
            assert!(
                !text.contains("key\":") && !text.contains("api_key"),
                "{text}"
            );
        }
        assert!(serde_json::from_str::<Settings>(r#"{"research": {"source": "google"}}"#).is_err());
        assert!(
            serde_json::from_str::<Settings>(r#"{"research": {"source": "built-in"}}"#).is_err()
        );
        let mut bad = Settings::default();
        bad.research.acknowledged_source = Some(SearchSource::Off);
        assert!(bad.validate().unwrap_err().contains("acknowledged_source"));
    }

    #[test]
    fn unknown_settings_fields_are_rejected_and_missing_ones_default() {
        assert!(serde_json::from_str::<Settings>(r#"{"api_key": "x"}"#).is_err());
        let partial: Settings = serde_json::from_str(r#"{"max_turns": 5}"#).unwrap();
        assert_eq!(partial.max_turns, 5);
        assert_eq!(partial.printer_id, DEFAULT_PRINTER_ID);
        assert_eq!(partial.handoff_folder, None);
    }

    #[test]
    fn a_hand_off_folder_is_an_absolute_path() {
        let mut settings = Settings {
            handoff_folder: Some("/mnt/d/Models/Hotend".into()),
            ..Settings::default()
        };
        settings.validate().unwrap();
        settings.handoff_folder = Some("Prints".into());
        assert!(settings.validate().unwrap_err().contains("handoff_folder"));
    }

    #[test]
    fn a_material_the_slicer_has_no_profile_for_is_refused_with_the_known_ones() {
        use crate::gateway::{CatalogueEntry, SlicerCatalogue};
        let entry = |id: &str| CatalogueEntry {
            id: id.to_owned(),
            aliases: Vec::new(),
            name: format!("{id} profile"),
            verified: true,
        };
        let catalogue = SlicerCatalogue {
            printer_id: DEFAULT_PRINTER_ID.to_owned(),
            processes: vec![entry("standard-0.20mm")],
            materials: vec![
                entry("pla-generic"),
                entry("pla-basic"),
                entry("petg-basic"),
            ],
            plates: vec![entry("textured-pei")],
        };
        let mut settings = Settings::default();
        settings.validate_material(Some(&catalogue)).unwrap();
        settings.material.id = "pla-silk-unknown".into();
        let error = settings.validate_material(Some(&catalogue)).unwrap_err();
        assert!(error.contains("`pla-silk-unknown`"), "{error}");
        assert!(
            error.contains("known materials are: pla-generic, pla-basic, petg-basic"),
            "{error}"
        );
        // Nothing to check against: accepted (the id's form is checked by `validate`).
        settings.validate_material(None).unwrap();
        settings.validate().unwrap();
    }

    #[test]
    fn effort_defaults_to_high_and_round_trips() {
        assert_eq!(Settings::default().effort, Effort::High);
        // Settings saved before the field existed read as `high`.
        let old: Settings = serde_json::from_str(r#"{"max_turns": 5}"#).unwrap();
        assert_eq!(old.effort, Effort::High);
        let settings = Settings {
            effort: Effort::Xhigh,
            ..Settings::default()
        };
        let text = serde_json::to_string(&settings).unwrap();
        assert!(text.contains(r#""effort":"xhigh""#), "{text}");
        let back: Settings = serde_json::from_str(&text).unwrap();
        assert_eq!(back, settings);
        assert!(serde_json::from_str::<Settings>(r#"{"effort": "extreme"}"#).is_err());
    }
}

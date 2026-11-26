//! Facade errors as the interface receives them: a tagged enum of plain data, each with a
//! message for the user and the next step they can take. No variant ever holds a secret.

use serde::{Deserialize, Serialize};

use crate::core::CoreError;
use crate::gateway::SliceFailure;
use crate::jobs::JobError;
use crate::modeling::ModelingError;
use crate::printing::{LintFinding, NotImportableReason, PrintError};
use crate::store::StoreError;
use crate::util::truncate;

/// Why a facade command failed, for the UI.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ApiError {
    /// A project, version or job with that id does not exist.
    NotFound { message: String, next_step: String },
    /// An argument is not acceptable.
    InvalidInput { message: String, next_step: String },
    /// Something must be set up first.
    NotConfigured { message: String, next_step: String },
    /// The project already has a message in progress.
    Busy { message: String, next_step: String },
    /// The project has no model yet.
    NoModel { message: String, next_step: String },
    /// A sandboxed engine job failed. `code` is the job error's stable code.
    Engine {
        code: String,
        message: String,
        next_step: String,
    },
    /// Slicing failed. `code` is the slicer's stable code.
    Slicer {
        code: String,
        message: String,
        next_step: String,
    },
    /// A print command was refused. `findings` are the linter's, for `gcode_rejected`.
    /// `reason` is a stable sub-code of some codes: for `not_importable`,
    /// `slice_before_previews` (slicing again is all it takes) or `previews_off`.
    Print {
        code: String,
        message: String,
        next_step: String,
        findings: Vec<LintFinding>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
    },
    /// The agent or the model provider failed.
    Agent {
        code: String,
        message: String,
        next_step: String,
    },
    /// The key store could not be used.
    Secrets { message: String, next_step: String },
    /// The data folder or the database could not be used.
    Storage { message: String, next_step: String },
    /// Hotend itself could not start a component.
    Internal { message: String, next_step: String },
}

impl ApiError {
    /// The message for the user.
    pub fn message(&self) -> &str {
        match self {
            Self::NotFound { message, .. }
            | Self::InvalidInput { message, .. }
            | Self::NotConfigured { message, .. }
            | Self::Busy { message, .. }
            | Self::NoModel { message, .. }
            | Self::Engine { message, .. }
            | Self::Slicer { message, .. }
            | Self::Print { message, .. }
            | Self::Agent { message, .. }
            | Self::Secrets { message, .. }
            | Self::Storage { message, .. }
            | Self::Internal { message, .. } => message,
        }
    }

    /// What the user can do next.
    pub fn next_step(&self) -> &str {
        match self {
            Self::NotFound { next_step, .. }
            | Self::InvalidInput { next_step, .. }
            | Self::NotConfigured { next_step, .. }
            | Self::Busy { next_step, .. }
            | Self::NoModel { next_step, .. }
            | Self::Engine { next_step, .. }
            | Self::Slicer { next_step, .. }
            | Self::Print { next_step, .. }
            | Self::Agent { next_step, .. }
            | Self::Secrets { next_step, .. }
            | Self::Storage { next_step, .. }
            | Self::Internal { next_step, .. } => next_step,
        }
    }
}

const DOCTOR: &str = "Run the doctor report; it says what is missing on this machine and how \
                      to fix it.";
/// The other ways to print a file Bambu Connect cannot import: the short second line of
/// every `not_importable` next step.
const NOT_IMPORTABLE_OTHER_WAYS: &str = "Other ways: make the editable Bambu Studio project, \
                                         or save the file to a folder or a USB drive.";
const STORAGE: &str = "Check that Hotend's data folder exists, is writable and has free \
                       space, then try again.";

fn modeling(error: &ModelingError, message: String) -> ApiError {
    match error {
        ModelingError::Store(StoreError::NotFound { .. }) => ApiError::NotFound {
            message,
            next_step: "Reload the project list and pick an existing project or version."
                .to_owned(),
        },
        ModelingError::Store(_) | ModelingError::Path(_) | ModelingError::Io { .. } => {
            ApiError::Storage {
                message,
                next_step: STORAGE.to_owned(),
            }
        }
        ModelingError::NoCurrentVersion => ApiError::NoModel {
            message,
            next_step: "Describe the product to the agent, or build a model script, to get a \
                        first version."
                .to_owned(),
        },
        ModelingError::Invalid(_) => ApiError::InvalidInput {
            message,
            next_step: "Correct the value and try again.".to_owned(),
        },
        ModelingError::Job(job) => ApiError::Engine {
            code: job.code().to_owned(),
            message,
            next_step: match job {
                JobError::Cancelled => "Nothing to do: the job was cancelled.".to_owned(),
                JobError::Timeout { .. } => "Simplify the model or try again; a job is \
                                              stopped when it runs past its time limit."
                    .to_owned(),
                // The engine refuses a request for what is in it: a value out of range,
                // a name it does not know. Nothing is wrong with the machine.
                JobError::BadRequest { .. } => "Correct the value and try again.".to_owned(),
                _ => DOCTOR.to_owned(),
            },
        },
    }
}

impl From<&CoreError> for ApiError {
    fn from(error: &CoreError) -> Self {
        let message = truncate(&error.to_string(), 4000);
        match error {
            CoreError::Store(StoreError::NotFound { .. }) => Self::NotFound {
                message,
                next_step: "Reload the list and pick an existing item.".to_owned(),
            },
            CoreError::Store(_) | CoreError::Path(_) => Self::Storage {
                message,
                next_step: STORAGE.to_owned(),
            },
            CoreError::Modeling(inner) => modeling(inner, message),
            CoreError::Runtime(_) | CoreError::Setup(_) => Self::Internal {
                message,
                next_step: DOCTOR.to_owned(),
            },
            CoreError::Secret(
                crate::secrets::SecretError::Unsafe(_)
                | crate::secrets::SecretError::Corrupt { .. }
                | crate::secrets::SecretError::File(_),
            ) => Self::Secrets {
                message,
                next_step: "Put the secrets file right as the message says, then try again."
                    .to_owned(),
            },
            CoreError::Secret(_) => Self::Secrets {
                message,
                next_step: "Store the provider key again in the settings. On a machine \
                            without a keychain, start Hotend with HOTEND_SECRETS=file to \
                            keep keys in a file of your own, or with HOTEND_SECRETS=env and \
                            the key in the provider's environment variable."
                    .to_owned(),
            },
            CoreError::Egress(inner) => Self::Agent {
                code: inner.code().to_owned(),
                message,
                next_step: "Check the provider, the model and the key in the settings, then \
                            send the message again."
                    .to_owned(),
            },
            CoreError::Agent(inner) => Self::Agent {
                code: inner.code().to_owned(),
                message,
                next_step: "Send the message again; if it keeps failing, run the doctor \
                            report (its agent self-test shows whether the agent can start)."
                    .to_owned(),
            },
            CoreError::Slice(SliceFailure::Modeling(inner)) => modeling(inner, message),
            CoreError::Slice(inner) => Self::Slicer {
                code: truncate(inner.code(), 64),
                next_step: match inner {
                    SliceFailure::FailedChecks { .. } => "Run the checks, fix what they \
                                                          report and build a new version."
                        .to_owned(),
                    SliceFailure::NoSlice { .. } => "Slice the version first.".to_owned(),
                    SliceFailure::NoPlate { plates: 0, .. } => {
                        "Make the part smaller, split it, or choose a printer it fits; then \
                         build a new version."
                            .to_owned()
                    }
                    SliceFailure::NoPlate { .. } => "Prepare the version again and pick a \
                                                     plate that exists."
                        .to_owned(),
                    _ => "Open the slicer list in the settings: it shows which slicers were \
                          found and why one cannot be used. Hotend never downloads a slicer."
                        .to_owned(),
                },
                message,
            },
            CoreError::Print(inner) => Self::Print {
                code: inner.code().to_owned(),
                next_step: match inner {
                    PrintError::Rejected { .. } => "Slice the version again with a vetted \
                                                    profile; a file the G-code check refuses \
                                                    is never delivered."
                        .to_owned(),
                    PrintError::SliceChanged
                    | PrintError::LayoutChanged
                    | PrintError::PlanChanged => {
                        "Slice the version again and propose a new print.".to_owned()
                    }
                    PrintError::NotReviewed | PrintError::SummaryMismatch => {
                        "Open the print job's review and confirm what it shows.".to_owned()
                    }
                    PrintError::NotPending { .. } => {
                        "Propose a new print for this version.".to_owned()
                    }
                    PrintError::PlateNotChosen { .. } | PrintError::PlateChanged { .. } => {
                        "Choose the build plate that is on the printer in the settings, \
                         slice the version again and propose a new print."
                            .to_owned()
                    }
                    PrintError::NoSpool { .. } => "Load a matching spool, or change the \
                                                   material in the settings and slice again."
                        .to_owned(),
                    PrintError::Unsupported(_) => "Export the sliced file and print it with \
                                                   the printer's own software."
                        .to_owned(),
                    PrintError::HandOffFolder(_) => "Open Settings, Printer, and choose the \
                                                     Bambu Connect hand-off folder (under \
                                                     WSL, a folder on a Windows drive)."
                        .to_owned(),
                    PrintError::ProjectStale | PrintError::ProjectChanged => {
                        "Slice the plate again, let the G-code check pass, and make the \
                         editable project again."
                            .to_owned()
                    }
                    PrintError::NotImportable { reason, .. } => {
                        let first = match reason {
                            NotImportableReason::SliceBeforePreviews => {
                                "Slice again, then hand it to Bambu Connect. "
                            }
                            NotImportableReason::PreviewsOff => {
                                "Open Settings, choose Hotend-made plate previews, and slice \
                                 again. "
                            }
                            NotImportableReason::Other => "",
                        };
                        format!("{first}{NOT_IMPORTABLE_OTHER_WAYS}")
                    }
                    PrintError::Store(_) | PrintError::Delivery(_) => "Check the printer \
                                                                       list and the folder, \
                                                                       then review the job \
                                                                       again."
                        .to_owned(),
                },
                findings: match inner {
                    PrintError::Rejected { findings, .. } => findings.clone(),
                    _ => Vec::new(),
                },
                reason: match inner {
                    PrintError::NotImportable { reason, .. } => reason.code().map(str::to_owned),
                    _ => None,
                },
                message,
            },
            CoreError::NotConfigured(_) => Self::NotConfigured {
                message,
                next_step: "Open the settings and complete what the message names.".to_owned(),
            },
            CoreError::Busy => Self::Busy {
                message,
                next_step: "Wait for the running message to finish, or cancel it.".to_owned(),
            },
            CoreError::InvalidInput(_) => Self::InvalidInput {
                message,
                next_step: "Correct the value and try again.".to_owned(),
            },
        }
    }
}

impl From<CoreError> for ApiError {
    fn from(error: CoreError) -> Self {
        Self::from(&error)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[test]
    fn an_error_is_tagged_data_with_a_message_and_a_next_step() {
        let error = ApiError::from(CoreError::Busy);
        let json = serde_json::to_value(&error).unwrap();
        assert_eq!(json["kind"], "busy");
        assert!(json["message"].as_str().unwrap().contains("in progress"));
        assert!(json["next_step"].as_str().unwrap().contains("cancel"));
        assert_eq!(serde_json::from_value::<ApiError>(json).unwrap(), error);

        let refused = ApiError::from(CoreError::Print(PrintError::Rejected {
            first: "line 3: bed too hot".into(),
            findings: Vec::new(),
        }));
        assert!(matches!(&refused, ApiError::Print { code, .. } if code == "gcode_rejected"));
        assert!(refused.message().contains("line 3"));
        assert!(!refused.next_step().is_empty());

        let missing = ApiError::from(CoreError::Modeling(ModelingError::NoCurrentVersion));
        assert!(matches!(missing, ApiError::NoModel { .. }));
    }

    /// A Bambu Connect refusal of a file without the plate preview says what to do in the
    /// words of the settings now: a slice made before Hotend-made previews were switched
    /// on is sliced again; with previews off they are switched on first. The editable
    /// project and a folder are only the short second line.
    #[test]
    fn a_file_bambu_connect_cannot_import_is_refused_in_words_that_fit_the_settings() {
        use crate::printing::not_importable;
        use crate::settings::PreviewsMode;
        let plate = || vec!["Metadata/plate_1.png".to_owned()];

        let before = ApiError::from(CoreError::Print(not_importable(
            plate(),
            PreviewsMode::HotendMade,
            false,
        )));
        let ApiError::Print {
            code,
            message,
            next_step,
            reason,
            ..
        } = &before
        else {
            panic!("{before:?}");
        };
        assert_eq!(code, "not_importable");
        assert_eq!(reason.as_deref(), Some("slice_before_previews"));
        assert_eq!(
            message,
            "This slice was made before Hotend drew plate previews. Slice again, then hand it \
             off. Bambu Connect cannot import it as it is: it lacks the plate preview \
             `Metadata/plate_1.png`. Nothing was saved or sent."
        );
        assert_eq!(
            next_step,
            "Slice again, then hand it to Bambu Connect. Other ways: make the editable Bambu \
             Studio project, or save the file to a folder or a USB drive."
        );
        let json = serde_json::to_value(&before).unwrap();
        assert_eq!(json["reason"], "slice_before_previews");
        assert_eq!(serde_json::from_value::<ApiError>(json).unwrap(), before);

        let off = ApiError::from(CoreError::Print(not_importable(
            plate(),
            PreviewsMode::Slicer,
            false,
        )));
        let ApiError::Print {
            code,
            message,
            next_step,
            reason,
            ..
        } = &off
        else {
            panic!("{off:?}");
        };
        assert_eq!(code, "not_importable");
        assert_eq!(reason.as_deref(), Some("previews_off"));
        assert_eq!(
            message,
            "Bambu Connect cannot import this slice: it lacks the plate preview \
             `Metadata/plate_1.png`, which Bambu Studio on Linux cannot draw without a \
             display. Switch on Hotend-made plate previews in Settings and slice again, then \
             hand it off. Nothing was saved or sent."
        );
        assert_eq!(
            next_step,
            "Open Settings, choose Hotend-made plate previews, and slice again. Other ways: \
             make the editable Bambu Studio project, or save the file to a folder or a USB \
             drive."
        );
        for words in [message, next_step] {
            assert!(!words.starts_with("Make the editable"), "{words}");
        }

        // A slice made with Hotend-made previews that still has none, or a file that lacks
        // something else, is not "made before": slicing again would not help.
        for refusal in [
            not_importable(plate(), PreviewsMode::HotendMade, true),
            not_importable(
                vec!["Metadata/slice_info.config".to_owned()],
                PreviewsMode::HotendMade,
                false,
            ),
        ] {
            let error = ApiError::from(CoreError::Print(refusal));
            assert!(
                matches!(&error, ApiError::Print { code, reason: None, .. } if code == "not_importable"),
                "{error:?}"
            );
            assert!(!error.message().contains("made before"), "{error:?}");
            assert!(error.next_step().starts_with("Other ways"), "{error:?}");
            let json = serde_json::to_value(&error).unwrap();
            assert!(json.get("reason").is_none(), "{json}");
        }
    }

    /// Every argument and result of a facade command is plain data that a typed binding can
    /// be generated for. Events only leave the core, so they need to serialise only.
    #[test]
    fn every_facade_type_is_plain_serde_data() {
        fn data<T: Serialize + serde::de::DeserializeOwned>() {}
        fn outgoing<T: Serialize>() {}
        data::<crate::store::Project>();
        data::<crate::core::ProjectDetail>();
        data::<crate::store::Version>();
        data::<crate::modeling::VersionDetail>();
        data::<crate::engine::Question>();
        data::<crate::modeling::BuildReport>();
        data::<crate::engine::EngineResult>();
        data::<crate::engine::Section>();
        data::<crate::modeling::RenderedView>();
        data::<crate::engine::ExportFormat>();
        data::<crate::modeling::ExportedFile>();
        data::<crate::store::Snapshot>();
        data::<crate::snapshot::SnapshotExport>();
        data::<crate::modeling::PrepareOptions>();
        data::<crate::modeling::Preparation>();
        data::<crate::engine::CardRef>();
        data::<crate::engine::GaugeReading>();
        data::<crate::modeling::GaugeExport>();
        data::<crate::modeling::Calibration>();
        data::<crate::modeling::CalibrationUpdate>();
        data::<crate::gateway::SlicerInstallation>();
        data::<crate::gateway::SliceOutcome>();
        data::<crate::settings::Settings>();
        data::<crate::printing::PrinterInfo>();
        data::<crate::printing::PrintJob>();
        data::<crate::printing::DeliveryChoice>();
        data::<crate::printing::PrintReview>();
        data::<crate::store::AuditEntry>();
        data::<crate::egress::ProviderKind>();
        data::<crate::doctor::DoctorReport>();
        data::<ApiError>();
        outgoing::<crate::agent::RunEvent>();
        outgoing::<crate::printing::CoreEvent>();

        let view = crate::modeling::RenderedView {
            name: "iso".to_owned(),
            png: b"\x89PNG".to_vec(),
        };
        let json = serde_json::to_value(&view).unwrap();
        assert_eq!(json, serde_json::json!({"name": "iso", "png": "iVBORw=="}));
        assert_eq!(
            serde_json::from_value::<crate::modeling::RenderedView>(json).unwrap(),
            view
        );
    }
}

//! Hotend's slicer integration (PROTOCOL.md section 6, DESIGN.md "Slicing").
//!
//! Hotend does not slice. It finds a vendor slicer the user installed, runs its documented
//! command line inside the sandbox with settings files Hotend wrote, and reads the result
//! as hostile input. Start with [`Slicer`]; the application gives the core a
//! [`CoreSliceBackend`].
//!
//! | Module | What it guarantees |
//! | --- | --- |
//! | [`detect`] | Slicers are found, never downloaded or bundled; versions are read by running `--help` inside the sandbox only |
//! | [`version`] | A version runs only if the pinned-version table (data) allows it |
//! | [`printer`] | Printer facts are data with `verified` flags; a job that breaks a nozzle's reach or material rule is refused before slicing; the filament map is always explicit and by device extruder id |
//! | [`input`] | A 3MF model file that holds anything but the model (settings, per-layer G-code) is not handed to a slicer |
//! | [`system`] | A Bambu Studio is given the system profiles of its own installation, selected only through the ids of a table (data); a real slice needs a plate somebody chose; the profile reader sees the vendor folder and its own file and writes into `profiles/` only |
//! | [`keymap`] | Hotend's print settings reach a profile only through a data table checked against the fields' hard ranges and a constant deny list; a value never measured with the real program is refused unless the installed profile already holds it |
//! | [`proof`] | A sliced file made with print settings is judged against them from its settings block and its first-layer commands; a file that does not show enough is refused |
//! | [`project`] | An editable project gets only its list of changed settings filled, and is handed over only when its lone re-slice passes the proof |
//! | [`flatten`] | Installed profiles are read as hostile input by a helper program inside the sandbox: names stay inside the vendor's folder, links are never followed, depth, count and size are capped, circles are found, values are carried through untouched; only a preset of its role is used, G-code and scripts come from the machine profile only, and every override has a type and a range |
//! | [`settings`] | PrusaSlicer and the stand-in get settings chosen by id from vetted files; a job cannot carry setting values or G-code |
//! | [`command`] | Command lines are built from public documentation and the measured run by pure functions; what no real run has confirmed is listed with the command line |
//! | [`output`] | The slicer's result file and the sliced file are checked, hashed and parsed with size limits; an unstated number is `None` with a warning |
//! | [`slicer`] | After a run the file is judged against the job: the version it states, the plate and bed temperature, where it prints against the nozzle reach and the placed parts |
//! | [`backend`] | The adapter to `hotend_core::SliceBackend` |
//! | [`fake`] | A stand-in slicer for tests and the offline demo that behaves as the real program was measured to; its output is labelled and not printable |
//!
//! # What is proven and what is not
//!
//! Proven against output the real program wrote (Bambu Studio 02.09.01.70 for the X2D,
//! sliced by hand on Windows on 17 November 2026; `tests/real_reference.rs` reads the
//! recording): the layout of the sliced file and of `result.json`, the header, settings
//! block and commands of the G-code that [`output`] reads, the plate type and bed
//! temperature of the command line's default plate, and that the recorded run passes every
//! check that follows a run. PrusaSlicer 2.7.2 was run for real (`tests/real_prusaslicer.rs`).
//!
//! Proven by runs of the real Bambu Studio on Linux (`tests/real_bambu_studio.rs`, only where
//! one is installed): that the program starts inside the sandbox, that the profiles of its
//! installation are found and flattened, and that it accepts the command line with one
//! filament.
//!
//! Not proven: every profile name in `profiles/system-profiles.toml` marked
//! `verified = false`; a file with more than one filament; and every X2D fact in
//! `profiles/printers.toml` marked `verified = false`. `README.md` in this crate lists the
//! steps that settle each of them on a machine with the real program.

pub mod backend;
pub mod command;
pub mod detect;
pub mod error;
pub mod fake;
pub mod flatten;
pub mod input;
pub mod job;
pub mod keymap;
pub mod output;
pub mod previews;
pub mod printer;
pub mod project;
pub mod proof;
mod run;
pub mod settings;
pub mod slicer;
pub mod system;
pub mod version;

pub use crate::backend::{CoreSliceBackend, PROFILE_DIR, Programs, SYSTEM_PROFILES_ENV, install};
pub use crate::detect::{Found, Installation};
pub use crate::error::SlicerError;
pub use crate::job::{
    BedReport, Extent, FilamentAssignment, FilamentUsage, PlatePreviews, PrintedExtent, SliceJob,
    SliceResult, SliceWarning, SlicerInfo,
};
pub use crate::keymap::Keymap;
pub use crate::output::OutputLimits;
pub use crate::printer::{Printer, PrinterFacts};
pub use crate::project::{ProjectExport, ProjectKeys};
pub use crate::proof::{Expected, ProofReport};
pub use crate::settings::{ProfileRole, SettingsStore};
pub use crate::slicer::{Slicer, SlicerConfig};
pub use crate::system::{BedType, SystemProfiles};
pub use crate::version::{SlicerKind, SlicerVersion, Verdict, VersionPolicy};

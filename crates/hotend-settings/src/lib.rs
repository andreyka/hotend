//! Hotend's print settings.
//!
//! This crate decides how a plate is printed: brim, first layer, bed temperatures, cooling,
//! shell, supports and motion caps. It is pure: no I/O beyond its own embedded data files, no
//! async, and no dependency on another Hotend crate. It knows no slicer: a field is a
//! setting of Hotend, and only `hotend-slicer` knows which profile key carries it.
//!
//! - [`model`]: the closed list of fields, their values, [`model::PrintSettings`],
//!   [`model::Decision`] and [`model::PrintPlan`];
//! - [`bounds`]: the constant hard range of every field;
//! - [`schema`]: the JSON Schema of a proposal, generated from the field table;
//! - [`facts`]: the plate facts the engine sends, checked before any rule reads them;
//! - [`inputs`]: everything else a plan is computed from;
//! - [`rules`]: the rule engine, [`RuleBook::plan`];
//! - [`envelope`]: the values each field may take on a plate, and the check every finished
//!   plan passes, [`RuleBook::verify`];
//! - [`propose`]: typed changes from the agent or the person and their answers,
//!   [`RuleBook::propose`];
//! - [`ruledata`], [`cards`], [`explain`]: the data files (thresholds and intents, material
//!   cards, sentences) and their strict loaders;
//! - [`checklist`]: the before-you-print list with its priority classes;
//! - [`signature`]: the fit signature and canonical hashing.
//!
//! ```
//! # use hotend_settings::RuleBook;
//! let book = RuleBook::builtin().expect("the embedded data files load");
//! assert!(book.cards().all().len() >= 5);
//! ```
//!
//! `docs/PRINT_SETTINGS.md` describes the model and the rules for a consumer of the crate.

pub mod bounds;
pub mod cards;
pub mod checklist;
pub mod envelope;
pub mod explain;
pub mod facts;
pub mod inputs;
mod level;
pub mod model;
pub mod propose;
pub mod ruledata;
pub mod rules;
pub mod schema;
pub mod signature;

pub use explain::{DataError, PlanError};
pub use inputs::PlanInputs;
pub use model::{Field, PrintPlan, PrintSettings, Value};
pub use propose::{Changes, Proposal, ProposalOutcome};
pub use rules::RuleBook;

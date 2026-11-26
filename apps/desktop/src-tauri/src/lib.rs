//! Hotend's desktop shell: the layer between the window (a webview that holds no secret
//! and reaches no network) and `hotend-core`.
//!
//! | Module | What it guarantees |
//! | --- | --- |
//! | [`ipc`] | The window's commands are exactly one table; the Tauri commands, the TypeScript binding and the bridge are generated from it |
//! | [`shell`] | Each command is a thin call into the core; paths and links from the window are checked first; no command returns a secret |
//! | [`confirm`] | A print job leaves `pending`, and an editable Bambu Studio project is handed over, and a printer's certificate is pinned, only after a person confirmed in a dialog the page cannot reach; the confirmation authority never leaves that module |
//! | [`dialog`] | The confirmation dialog: Cancel is its default and focused button, its confirming button does nothing for the first second, and its text is shown as written |
//! | [`grants`] | A path from the window (a folder to write into, a slicer program, a new hand-off folder) is accepted only if the person picked it in a system dialog |
//! | [`links`] | The system browser is opened only for a fixed list of https addresses |
//! | [`events`] | The core's events (agent runs, print jobs, a followed printer's reports) are passed on in order, unchanged |
//! | [`wire`] | The TypeScript types describe what the core serialises |
//! | [`app`] | The window: no navigation away from the application, no new windows, native dialogs from Rust only |
//! | [`bridge`] | Development only (`dev-bridge`, debug builds): the same commands over loopback HTTP behind a random token; it never shows or answers a confirmation (a print, an editable project, a printer certificate pin or a search agreement) unless started with the test switch |

pub mod app;
#[cfg(feature = "dev-bridge")]
pub mod bridge;
pub mod confirm;
pub mod dialog;
pub mod events;
pub mod grants;
pub mod ipc;
pub mod links;
pub mod shell;
pub mod wire;

// The bridge gives any local program that learns its token every command of the window.
// It must not exist in a build that is shipped; build.rs refuses the same combination
// earlier, with a longer message.
#[cfg(all(feature = "dev-bridge", not(debug_assertions)))]
compile_error!(
    "the `dev-bridge` feature cannot be built without debug assertions (a release build): \
     the development bridge is never shipped"
);

//! The Hotend desktop application.

// No console window next to the application on Windows.
#![cfg_attr(
    all(not(debug_assertions), target_os = "windows"),
    windows_subsystem = "windows"
)]

fn main() {
    hotend_desktop::app::run();
}

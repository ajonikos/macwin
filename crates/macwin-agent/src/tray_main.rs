//! `macwin-tray`: macwin in the notification area. On Windows it is a GUI
//! subsystem binary, so starting it at login opens no console window.
#![cfg_attr(windows, windows_subsystem = "windows")]
#![forbid(unsafe_code)]

use std::process::ExitCode;

fn main() -> ExitCode {
    match macwin_agent::commands::tray() {
        Ok(()) => ExitCode::SUCCESS,
        // Already logged and shown to the user.
        Err(_) => ExitCode::FAILURE,
    }
}

//! the live replay-history bridge patches Rocket League's game package with
//! Win32 file replacement and an elevated relaunch, so it is Windows only.
//! These no-ops keep the callers in app.rs compiling on Linux.

use std::path::{Path, PathBuf};

pub fn busy() -> bool {
    false
}

pub fn message() -> String {
    String::new()
}

pub fn installed(_game: &Path, _base: &Path) -> bool {
    false
}

pub fn resume_operation() -> Option<bool> {
    None
}

pub fn admin_restart_canceled() -> bool {
    false
}

pub fn show_admin_restart_canceled() {}

pub fn restart_as_admin(_remove: bool) -> Result<(), String> {
    Err("The live replay bridge is only available on Windows.".into())
}

pub fn start_operation(
    _game: PathBuf,
    _base: PathBuf,
    _remove: bool,
    _ctx: eframe::egui::Context,
) {
}

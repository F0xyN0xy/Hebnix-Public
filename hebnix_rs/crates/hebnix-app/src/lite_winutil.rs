//! linux winutil for the `hebnix-lite` binary -- same rationale as
//! `winutil.rs` (no client-side Wayland window management), trimmed to what
//! lite_app.rs needs.

use std::sync::atomic::{AtomicBool, Ordering};

pub type WindowHandle = ();

static HIDDEN: AtomicBool = AtomicBool::new(false);

fn runtime_dir() -> std::path::PathBuf {
    std::env::var("XDG_RUNTIME_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| std::env::temp_dir())
}

pub struct SingleInstanceLock {
    _file: std::fs::File,
}

pub fn acquire_single_instance() -> Option<SingleInstanceLock> {
    use std::os::fd::AsRawFd;
    let path = runtime_dir().join("hebnix_lite_single_instance.lock");
    let file = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .open(&path)
        .ok()?;
    match nix::fcntl::flock(file.as_raw_fd(), nix::fcntl::FlockArg::LockExclusiveNonblock) {
        Ok(()) => Some(SingleInstanceLock { _file: file }),
        Err(_) => None,
    }
}

pub fn focus_existing_instance() {
    tracing::info!("hebnix-lite is already running");
}

pub fn main_window_hwnd() -> Option<WindowHandle> {
    None
}

pub fn foreground_window_is_ours() -> bool {
    false
}

pub fn note_foreground() {}

pub fn set_main_window_topmost(_topmost: bool) {}

pub fn focus_main_window() {}

pub fn focus_rocket_league() {}

pub fn install_minimize_hook(_: WindowHandle, _: &eframe::egui::Context) {}

pub fn main_window_hidden() -> bool {
    HIDDEN.load(Ordering::Relaxed)
}

pub fn set_main_window_invisible(invisible: bool) {
    HIDDEN.store(invisible, Ordering::Relaxed);
}

pub fn restart_rocket_league(_game_path: &std::path::Path) -> std::io::Result<()> {
    use sysinfo::{ProcessRefreshKind, ProcessesToUpdate, Signal, System};
    let mut sys = System::new();
    sys.refresh_processes_specifics(ProcessesToUpdate::All, true, ProcessRefreshKind::nothing());
    for (_, proc_) in sys.processes() {
        if proc_
            .name()
            .to_string_lossy()
            .to_lowercase()
            .contains("rocketleague")
        {
            proc_.kill_with(Signal::Kill);
        }
    }
    for _ in 0..60 {
        if !hebnix_sdk::process::is_rocket_league_running() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(250));
    }
    if std::process::Command::new("steam")
        .arg("steam://rungameid/252950")
        .spawn()
        .is_ok()
    {
        return Ok(());
    }
    std::process::Command::new("xdg-open")
        .arg("steam://rungameid/252950")
        .spawn()
        .map(|_| ())
}

fn autostart_path() -> std::path::PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("."))
        .join("autostart")
        .join("hebnix-lite.desktop")
}

pub fn is_startup_enabled() -> bool {
    autostart_path().is_file()
}

pub fn set_startup_enabled(enabled: bool) -> std::io::Result<()> {
    let path = autostart_path();
    if enabled {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let exe = std::env::current_exe()?;
        let contents = format!(
            "[Desktop Entry]\n\
             Type=Application\n\
             Name=Hebnix Lite\n\
             Exec=\"{}\"\n\
             Icon=hebnix\n\
             Terminal=false\n\
             X-GNOME-Autostart-enabled=true\n",
            exe.display()
        );
        std::fs::write(&path, contents)
    } else {
        match std::fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e),
        }
    }
}

//! Windows retires its old WebSocket recovery boot tasks here. Linux never
//! had boot tasks: older builds' leftover hosts redirects were cleared on the
//! next start, and the one-time WebSocket cleanup below finishes the job.
use std::path::{Path, PathBuf};

pub fn session_marker(base_dir: &Path) -> PathBuf {
    base_dir.join("state").join("proxy_session.dirty")
}

/// A durable migration, retried on the next launch if cleanup cannot finish.
/// Shared certificates are retained for HTTP. Older Linux builds redirected
/// hosts through /etc/hosts, so leftover Hebnix redirects are cleared once.
pub fn cleanup_websocket_once(base_dir: &Path) -> Result<(), String> {
    run_cleanup_once(base_dir, || cleanup_legacy_state(base_dir))
}

fn run_cleanup_once(
    base_dir: &Path,
    cleanup: impl FnOnce() -> Result<(), String>,
) -> Result<(), String> {
    let complete = base_dir.join("state/websocket_cleanup_2.2.6.done");
    if complete.is_file() {
        return Ok(());
    }
    cleanup()?;
    std::fs::create_dir_all(complete.parent().unwrap()).map_err(|e| e.to_string())?;
    std::fs::write(
        complete,
        b"WebSocket bypass retired; Hebnix hosts redirects cleared",
    )
    .map_err(|e| e.to_string())
}

fn cleanup_legacy_state(base_dir: &Path) -> Result<(), String> {
    if crate::spoofer::hosts::has_redirects() {
        crate::spoofer::clear_hosts_privileged(base_dir)?;
    }
    if hebnix_sdk::process::is_rocket_league_running() {
        return Err(
            "WebSocket cache cleanup will run at the next Hebnix start with Rocket League closed."
                .into(),
        );
    }
    crate::winutil::clear_rocket_league_web_cache().map_err(|e| e.to_string())?;
    for path in [
        session_marker(base_dir),
        base_dir.join("rank_spoofer_frames.log"),
    ] {
        match std::fs::remove_file(path) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.to_string()),
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    struct TempDir(PathBuf);
    impl TempDir {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "hebnix-websocket-retirement-{}",
                rand::random::<u64>()
            ));
            std::fs::create_dir_all(&path).unwrap();
            Self(path)
        }
    }
    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    #[test]
    fn completed_migration_never_runs_cleanup_again() {
        let base = TempDir::new();
        run_cleanup_once(&base.0, || Ok(())).unwrap();
        run_cleanup_once(&base.0, || panic!("cleanup repeated")).unwrap();
    }
    #[test]
    fn failed_migration_can_retry_without_losing_saved_preferences() {
        let base = TempDir::new();
        let preferences = base.0.join("spoofer_settings.json");
        std::fs::write(&preferences, b"saved names and titles").unwrap();
        assert!(run_cleanup_once(&base.0, || Err("cache locked".into())).is_err());
        assert!(!base.0.join("state/websocket_cleanup_2.2.6.done").exists());
        run_cleanup_once(&base.0, || Ok(())).unwrap();
        assert_eq!(
            std::fs::read(preferences).unwrap(),
            b"saved names and titles"
        );
    }
}

//! Shared Hebnix runtime and data paths.

use std::path::PathBuf;

/// Rocket League's Documents folder. On Linux that is the emulated
/// "My Documents" inside the game's Wine/Proton prefix (see
/// process::rocket_league_documents_dir), falling back to the host's own
/// Documents folder.
pub fn documents_dir() -> Option<PathBuf> {
    crate::process::rocket_league_documents_dir()
        .or_else(dirs::document_dir)
        .filter(|path| path.is_absolute())
        .or_else(|| dirs::home_dir().map(|home| home.join("Documents")))
        .filter(|path| path.is_absolute())
}

/// The root for all Hebnix-owned files: `~/.config/hebnix` (XDG), or
/// `HEBNIX_BASE_DIR` for isolated development and test runs. Must resolve to
/// the same directory as the app crate's `config::base_dir()`.
pub fn base_dir() -> PathBuf {
    let dir = std::env::var_os("HEBNIX_BASE_DIR")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .or_else(|| dirs::config_dir().map(|dir| dir.join("hebnix")))
        .unwrap_or_else(|| std::env::temp_dir().join("hebnix"));

    if let Err(error) = std::fs::create_dir_all(&dir) {
        eprintln!(
            "failed to create Hebnix data directory {}: {error}",
            dir.display()
        );
    }
    dir
}

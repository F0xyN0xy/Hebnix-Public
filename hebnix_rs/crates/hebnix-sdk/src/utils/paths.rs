//! Shared Hebnix runtime and data paths.

use std::path::PathBuf;

/// Windows' Documents known folder includes OneDrive and other folder redirects.
/// Use the traditional location only when the known folder is unavailable.
pub fn documents_dir() -> Option<PathBuf> {
    dirs::document_dir()
        .filter(|path| path.is_absolute())
        .or_else(|| dirs::home_dir().map(|home| home.join("Documents")))
        .filter(|path| path.is_absolute())
}

/// The root for all Hebnix-owned files.
///
/// On Windows this is `%AppData%\Hebnix`. `HEBNIX_BASE_DIR` remains available
/// for isolated development and test runs.
pub fn base_dir() -> PathBuf {
    let dir = std::env::var_os("HEBNIX_BASE_DIR")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .or_else(portable_base_dir)
        .or_else(|| dirs::data_dir().map(|dir| dir.join("Hebnix")))
        .unwrap_or_else(|| std::env::temp_dir().join("Hebnix"));

    if let Err(error) = std::fs::create_dir_all(&dir) {
        eprintln!(
            "failed to create Hebnix data directory {}: {error}",
            dir.display()
        );
    }
    dir
}

fn portable_base_dir() -> Option<PathBuf> {
    let dir = std::env::current_exe().ok()?.parent()?.to_path_buf();
    dir.join("portable.txt").exists().then_some(dir)
}

//! Case-insensitive lookup of game files.
//!
//! Package names in item catalogs and inside UPK streaming references do not
//! always match the on-disk casing (`park_sfx` vs `Park_SFX.upk`). Windows does
//! not care, but Linux filesystems are case-sensitive, so every lookup of a
//! file inside the game folder goes through [`resolve`].
use std::path::{Path, PathBuf};

/// Windows filesystems are case-insensitive, so a plain join is enough.
#[cfg(target_os = "windows")]
pub fn resolve(dir: &Path, name: impl AsRef<Path>) -> PathBuf {
    dir.join(name)
}

#[cfg(target_os = "linux")]
pub use linux::resolve;

#[cfg(target_os = "linux")]
mod linux {
    use std::collections::HashMap;
    use std::ffi::OsString;
    use std::fs;
    use std::path::{Component, Path, PathBuf};
    use std::sync::{Mutex, OnceLock};
    use std::time::SystemTime;

    struct DirIndex {
        modified: SystemTime,
        files: HashMap<String, OsString>,
    }

    static INDEX: OnceLock<Mutex<HashMap<PathBuf, DirIndex>>> = OnceLock::new();

    /// Return the existing file in `dir` whose name matches `name` ignoring ASCII
    /// case. An exact match is preferred; when nothing matches, `dir.join(name)`
    /// is returned so new files keep the requested name.
    pub fn resolve(dir: &Path, name: impl AsRef<Path>) -> PathBuf {
        let name = name.as_ref();
        let exact = dir.join(name);
        if exact.exists() {
            return exact;
        }
        let mut resolved = dir.to_path_buf();
        for component in name.components() {
            let Component::Normal(part) = component else {
                return exact;
            };
            resolved = match lookup(&resolved, part.to_str()) {
                Some(found) => resolved.join(found),
                None => resolved.join(part),
            };
        }
        resolved
    }

    fn lookup(dir: &Path, name: Option<&str>) -> Option<OsString> {
        let key = name?.to_ascii_lowercase();
        // The directory's mtime changes whenever an entry is added, removed or
        // renamed, so a cached listing stays valid until then.
        let modified = fs::metadata(dir).and_then(|m| m.modified()).ok()?;
        let mut cache = INDEX.get_or_init(Default::default).lock().ok()?;
        let stale = cache
            .get(dir)
            .is_none_or(|index| index.modified != modified);
        if stale {
            let mut files = HashMap::new();
            for entry in fs::read_dir(dir).ok()?.flatten() {
                let file_name = entry.file_name();
                if let Some(text) = file_name.to_str() {
                    files.entry(text.to_ascii_lowercase()).or_insert(file_name);
                }
            }
            cache.insert(dir.to_path_buf(), DirIndex { modified, files });
        }
        cache.get(dir)?.files.get(&key).cloned()
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        fn temp(name: &str) -> PathBuf {
            let dir = std::env::temp_dir().join(format!("hebnix_game_path_{name}_{}", std::process::id()));
            let _ = fs::remove_dir_all(&dir);
            fs::create_dir_all(&dir).unwrap();
            dir
        }

        #[test]
        fn resolves_mismatched_case() {
            let dir = temp("case");
            fs::write(dir.join("Park_SFX.upk"), b"x").unwrap();
            assert_eq!(resolve(&dir, "park_sfx.upk"), dir.join("Park_SFX.upk"));
            assert_eq!(resolve(&dir, "Park_SFX.upk"), dir.join("Park_SFX.upk"));
            let _ = fs::remove_dir_all(dir);
        }

        #[test]
        fn missing_file_keeps_requested_name() {
            let dir = temp("missing");
            assert_eq!(resolve(&dir, "New_SF.upk"), dir.join("New_SF.upk"));
            let _ = fs::remove_dir_all(dir);
        }

        #[test]
        fn sees_files_added_after_first_lookup() {
            let dir = temp("refresh");
            assert_eq!(resolve(&dir, "late.upk"), dir.join("late.upk"));
            std::thread::sleep(std::time::Duration::from_millis(20));
            fs::write(dir.join("Late.upk"), b"x").unwrap();
            assert_eq!(resolve(&dir, "late.upk"), dir.join("Late.upk"));
            let _ = fs::remove_dir_all(dir);
        }

        #[test]
        fn resolves_nested_components() {
            let dir = temp("nested");
            fs::create_dir_all(dir.join("Sub")).unwrap();
            fs::write(dir.join("Sub").join("Body_SF.upk"), b"x").unwrap();
            assert_eq!(
                resolve(&dir, "sub/body_sf.upk"),
                dir.join("Sub").join("Body_SF.upk")
            );
            let _ = fs::remove_dir_all(dir);
        }
    }
}

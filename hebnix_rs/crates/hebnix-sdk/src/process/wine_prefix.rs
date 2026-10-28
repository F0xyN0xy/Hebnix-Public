//! Locate Rocket League's Wine/Proton prefix(es) on Linux.
//!
//! RL has no native Linux client anymore (F2P/Epic dropped it) - everyone
//! runs it through some Wine prefix, and its `.save` files live under that
//! prefix's emulated "My Documents", not the host's real `~/Documents`.
//! There's no single fixed prefix location either: people commonly run it
//! as a non-Steam game through Steam Play (a numeric "shortcut" appid under
//! steamapps/compatdata, different per machine) or through Heroic (Epic
//! games get their own prefix under ~/Games/Heroic/Prefixes). So rather
//! than guessing one path, scan every prefix we can find and let the
//! caller pick whichever one actually has the newest save file.

use std::path::{Path, PathBuf};

/// every Steam library's steamapps dir (the default one, plus any extra
/// libraries listed in libraryfolders.vdf - e.g. a game installed on a
/// second drive still gets its compatdata under that library, not the
/// default one).
pub fn steam_library_steamapps_dirs() -> Vec<PathBuf> {
    let Some(home) = dirs::home_dir() else {
        return Vec::new();
    };
    let default_steam = home.join(".local/share/Steam");
    let mut roots = vec![default_steam.join("steamapps")];

    let vdf = default_steam.join("steamapps/libraryfolders.vdf");
    if let Ok(text) = std::fs::read_to_string(&vdf) {
        let re = regex::Regex::new(r#""path"\s*"([^"]+)""#).expect("valid libraryfolders regex");
        for capture in re.captures_iter(&text) {
            let path = PathBuf::from(&capture[1]).join("steamapps");
            if !roots.contains(&path) {
                roots.push(path);
            }
        }
    }
    roots
}

/// every Wine/Proton prefix that might be running Rocket League: Steam
/// Proton prefixes (any compatdata id - RL is usually added as a non-Steam
/// game, so the id is an arbitrary per-machine "shortcut" appid, not a
/// fixed one) and Heroic's per-game prefixes. `HEBNIX_WINE_PREFIX` (a
/// prefix root, i.e. the dir containing `drive_c`) can be set to add
/// anything unusual (Lutris, Bottles, a custom prefix, etc) without
/// needing code changes.
pub fn wine_prefixes() -> Vec<PathBuf> {
    let mut prefixes = Vec::new();

    for steamapps in steam_library_steamapps_dirs() {
        let compatdata = steamapps.join("compatdata");
        if let Ok(entries) = std::fs::read_dir(&compatdata) {
            for entry in entries.flatten() {
                push_prefix(&mut prefixes, &entry.path());
            }
        }
    }

    for prefix in heroic_prefix_dirs() {
        push_prefix(&mut prefixes, &prefix);
    }

    if let Ok(custom) = std::env::var("HEBNIX_WINE_PREFIX") {
        push_prefix(&mut prefixes, Path::new(&custom));
    }

    prefixes
}

/// adds a prefix given either its root (the dir holding `drive_c`, what
/// Wine and Wine-GE use) or a Proton compat dir holding `pfx/drive_c`
fn push_prefix(prefixes: &mut Vec<PathBuf>, dir: &Path) {
    for candidate in [dir.to_path_buf(), dir.join("pfx")] {
        if candidate.join("drive_c").is_dir() && !prefixes.contains(&candidate) {
            prefixes.push(candidate);
        }
    }
}

/// Heroic's prefixes: the per-game `winePrefix` and the default prefix from
/// its config (native and Flatpak installs), plus everything under the
/// default ~/Games/Heroic/Prefixes, one level into each folder since the
/// default prefix holds a subfolder per game. Wine-GE prefixes hold
/// `drive_c` directly, Proton ones in `pfx/`; push_prefix takes either.
fn heroic_prefix_dirs() -> Vec<PathBuf> {
    let Some(home) = dirs::home_dir() else {
        return Vec::new();
    };
    let mut roots = vec![home.join("Games/Heroic/Prefixes")];
    let mut dirs = Vec::new();
    for config in [
        dirs::config_dir().map(|dir| dir.join("heroic")),
        Some(home.join(".var/app/com.heroicgameslauncher.hgl/config/heroic")),
    ]
    .into_iter()
    .flatten()
    {
        if let Some(default) = std::fs::read_to_string(config.join("config.json"))
            .ok()
            .and_then(|text| serde_json::from_str::<serde_json::Value>(&text).ok())
            .and_then(|json| find_string(&json, "defaultWinePrefix"))
        {
            roots.push(PathBuf::from(default));
        }
        if let Ok(entries) = std::fs::read_dir(config.join("GamesConfig")) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().is_none_or(|ext| ext != "json") {
                    continue;
                }
                if let Some(prefix) = std::fs::read_to_string(&path)
                    .ok()
                    .and_then(|text| serde_json::from_str::<serde_json::Value>(&text).ok())
                    .and_then(|json| find_string(&json, "winePrefix"))
                {
                    dirs.push(PathBuf::from(prefix));
                }
            }
        }
    }
    for root in roots {
        dirs.push(root.clone());
        if let Ok(entries) = std::fs::read_dir(&root) {
            for entry in entries.flatten() {
                let path = entry.path();
                if !path.is_dir() {
                    continue;
                }
                dirs.push(path.clone());
                if let Ok(games) = std::fs::read_dir(&path) {
                    dirs.extend(games.flatten().map(|game| game.path()).filter(|p| p.is_dir()));
                }
            }
        }
    }
    dirs
}

/// first string value under `key` anywhere in a Heroic config (game
/// configs nest it under the game's app name)
fn find_string(json: &serde_json::Value, key: &str) -> Option<String> {
    match json {
        serde_json::Value::Object(map) => map
            .get(key)
            .and_then(serde_json::Value::as_str)
            .filter(|value| !value.is_empty())
            .map(str::to_owned)
            .or_else(|| map.values().find_map(|value| find_string(value, key))),
        _ => None,
    }
}

/// every `drive_c/users/*/Documents` dir across every known prefix, in no
/// particular order - caller decides which one actually has what it wants.
pub fn candidate_documents_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    for prefix in wine_prefixes() {
        let users = prefix.join("drive_c/users");
        let Ok(entries) = std::fs::read_dir(&users) else {
            continue;
        };
        for entry in entries.flatten() {
            let docs = entry.path().join("Documents");
            if docs.is_dir() {
                dirs.push(docs);
            }
        }
    }
    dirs
}

/// the `drive_c/users/*/Documents` dir belonging to the same prefix a
/// running process's exe lives under, if any - used when we already know
/// exactly which prefix is live (a running RL process) so we don't have to
/// guess between multiple installed prefixes.
pub fn documents_dir_for_exe(exe_path: &Path) -> Option<PathBuf> {
    let drive_c = exe_path
        .ancestors()
        .find(|ancestor| ancestor.file_name().is_some_and(|name| name == "drive_c"))?;
    let users = drive_c.join("users");
    let entries = std::fs::read_dir(&users).ok()?;
    let mut candidates: Vec<PathBuf> = entries
        .flatten()
        .map(|entry| entry.path().join("Documents"))
        .filter(|docs| docs.is_dir())
        .collect();
    // Proton always uses "steamuser"; prefer it if present, otherwise
    // whichever single user profile exists (Heroic/Lutris/etc name theirs
    // after the real Linux username).
    let steamuser = users.join("steamuser").join("Documents");
    if let Some(pos) = candidates.iter().position(|c| *c == steamuser) {
        return Some(candidates.remove(pos));
    }
    candidates.into_iter().next()
}

// The one Documents folder Rocket League is actually using.
//
// A machine easily has several prefixes with a "My Games/Rocket League"
// folder in them: Heroic's per-game prefix, a Steam non-Steam-game prefix
// made by accident, Steam runtime prefixes. And the game itself is often
// installed outside any prefix (e.g. /mnt/games/rocketleague), so its exe
// path says nothing about which prefix it runs in. In order:
// 1. the folder picked in Settings (`set_documents_override`)
// 2. the prefix of the running game, from WINEPREFIX /
//    STEAM_COMPAT_DATA_PATH in the environment of the game process or its
//    wineserver
// 3. the candidate (every prefix plus the host's own Documents) whose
//    Rocket League folder has the most recently written real files: saves
//    and logs. Folders without any are ignored.

const RL_DIR: &str = "My Games/Rocket League";

static DOCUMENTS_OVERRIDE: std::sync::RwLock<Option<PathBuf>> = std::sync::RwLock::new(None);

/// set (or clear, with None) the Documents folder picked by the user. Takes
/// the Documents folder itself, its "My Games/Rocket League" folder, or the
/// "My Games" folder in between.
pub fn set_documents_override(path: Option<PathBuf>) {
    let documents = path.map(|path| normalize_documents(&path));
    if let Ok(mut slot) = DOCUMENTS_OVERRIDE.write() {
        *slot = documents;
    }
    clear_documents_cache();
}

fn normalize_documents(path: &Path) -> PathBuf {
    if path.join("TAGame").is_dir() || path.ends_with(RL_DIR) {
        if let Some(documents) = path.parent().and_then(Path::parent) {
            return documents.to_path_buf();
        }
    }
    if path.join("Rocket League").is_dir() && path.ends_with("My Games") {
        if let Some(documents) = path.parent() {
            return documents.to_path_buf();
        }
    }
    path.to_path_buf()
}

fn documents_override() -> Option<PathBuf> {
    DOCUMENTS_OVERRIDE.read().ok()?.clone()
}

type Cached = std::sync::Mutex<Option<(std::time::Instant, Option<PathBuf>)>>;
static DOCUMENTS_CACHE: Cached = std::sync::Mutex::new(None);

fn clear_documents_cache() {
    if let Ok(mut cache) = DOCUMENTS_CACHE.lock() {
        *cache = None;
    }
}

/// Rocket League's Documents folder (the one containing "My Games"), see
/// the comment above for how it is picked. Cached for a few seconds, since
/// save and log readers ask often.
pub fn rocket_league_documents_dir() -> Option<PathBuf> {
    if let Some(documents) = documents_override() {
        return Some(documents);
    }
    const TTL: std::time::Duration = std::time::Duration::from_secs(5);
    if let Ok(cache) = DOCUMENTS_CACHE.lock() {
        if let Some((at, value)) = cache.as_ref() {
            if at.elapsed() < TTL {
                return value.clone();
            }
        }
    }
    let found = running_game_documents().or_else(|| {
        let mut candidates = candidate_documents_dirs();
        if let Some(host) = dirs::document_dir() {
            candidates.push(host);
        }
        newest_rocket_league_documents(candidates)
    });
    if let Ok(mut cache) = DOCUMENTS_CACHE.lock() {
        *cache = Some((std::time::Instant::now(), found.clone()));
    }
    found
}

/// newest modification time of a real Rocket League file under
/// `documents`: any .save in the save folders, or a log. None when there
/// are none, so empty leftover folders never win.
pub fn rocket_league_activity(documents: &Path) -> Option<std::time::SystemTime> {
    let tagame = documents.join(RL_DIR).join("TAGame");
    let mut newest: Option<std::time::SystemTime> = None;
    let mut consider = |path: &Path| {
        if let Ok(modified) = std::fs::metadata(path).and_then(|meta| meta.modified()) {
            newest = Some(newest.map_or(modified, |current| current.max(modified)));
        }
    };
    for saves in ["SaveData/DBE_Production", "SaveDataEpic/DBE_Production"] {
        if let Ok(entries) = std::fs::read_dir(tagame.join(saves)) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path
                    .extension()
                    .is_some_and(|ext| ext.eq_ignore_ascii_case("save"))
                    && path.is_file()
                {
                    consider(&path);
                }
            }
        }
    }
    let launch_log = tagame.join("Logs/Launch.log");
    if launch_log.is_file() {
        consider(&launch_log);
    }
    newest
}

fn newest_rocket_league_documents(candidates: Vec<PathBuf>) -> Option<PathBuf> {
    candidates
        .into_iter()
        .filter_map(|documents| Some((rocket_league_activity(&documents)?, documents)))
        .max_by_key(|(modified, _)| *modified)
        .map(|(_, documents)| documents)
}

/// Documents folder of the prefix a running game uses, from the
/// environment of the game process (or, when that is unreadable, its
/// prefix's wineserver)
fn running_game_documents() -> Option<PathBuf> {
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return None;
    };
    let mut game_prefixes = Vec::new();
    let mut wineserver_prefixes = Vec::new();
    for entry in entries.flatten() {
        let pid = entry.file_name();
        if !pid.to_string_lossy().bytes().all(|byte| byte.is_ascii_digit()) {
            continue;
        }
        let Ok(comm) = std::fs::read_to_string(entry.path().join("comm")) else {
            continue;
        };
        let comm = comm.trim().to_ascii_lowercase();
        let game = comm.contains("rocketleague");
        if !game && comm != "wineserver" {
            continue;
        }
        let Some(prefix) = prefix_from_environ(&entry.path().join("environ")) else {
            continue;
        };
        if game {
            game_prefixes.push(prefix);
        } else {
            wineserver_prefixes.push(prefix);
        }
    }
    game_prefixes
        .into_iter()
        .chain(wineserver_prefixes)
        .filter_map(|prefix| prefix_documents(&prefix))
        .find(|documents| documents.join(RL_DIR).is_dir())
}

/// the prefix (the folder holding `drive_c` and `user.reg`) a Documents
/// folder belongs to, None for the host's own Documents
fn prefix_of_documents(documents: &Path) -> Option<PathBuf> {
    let drive_c = documents
        .ancestors()
        .find(|ancestor| ancestor.file_name().is_some_and(|name| name == "drive_c"))?;
    drive_c.parent().map(Path::to_path_buf)
}

static PREFIXES_CACHE: std::sync::Mutex<Option<(std::time::Instant, Vec<PathBuf>)>> =
    std::sync::Mutex::new(None);

/// every Wine prefix Rocket League has run in (one with a "My Games/Rocket
/// League" folder), plus the one picked in Settings and the prefixes of
/// running games. With Steam and Epic copies side by side there are
/// several, and until a game starts there is no telling which one comes
/// next. Cached for a few seconds, like rocket_league_documents_dir.
pub fn rocket_league_prefixes() -> Vec<PathBuf> {
    const TTL: std::time::Duration = std::time::Duration::from_secs(5);
    if let Ok(cache) = PREFIXES_CACHE.lock() {
        if let Some((at, value)) = cache.as_ref() {
            if at.elapsed() < TTL {
                return value.clone();
            }
        }
    }
    let mut found: Vec<PathBuf> = Vec::new();
    let mut push = |prefix: PathBuf| {
        let prefix = prefix.canonicalize().unwrap_or(prefix);
        if !found.contains(&prefix) {
            found.push(prefix);
        }
    };
    if let Some(prefix) = documents_override().as_deref().and_then(prefix_of_documents) {
        push(prefix);
    }
    for prefix in running_game_prefixes() {
        push(prefix);
    }
    for documents in candidate_documents_dirs() {
        if documents.join(RL_DIR).is_dir() {
            if let Some(prefix) = prefix_of_documents(&documents) {
                push(prefix);
            }
        }
    }
    if let Ok(mut cache) = PREFIXES_CACHE.lock() {
        *cache = Some((std::time::Instant::now(), found.clone()));
    }
    found
}

/// the prefixes running Rocket League processes use, from their
/// environment
pub fn running_game_prefixes() -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return Vec::new();
    };
    let mut prefixes = Vec::new();
    for entry in entries.flatten() {
        let pid = entry.file_name();
        if !pid.to_string_lossy().bytes().all(|byte| byte.is_ascii_digit()) {
            continue;
        }
        let Ok(comm) = std::fs::read_to_string(entry.path().join("comm")) else {
            continue;
        };
        if !comm.trim().to_ascii_lowercase().contains("rocketleague") {
            continue;
        }
        if let Some(prefix) = prefix_from_environ(&entry.path().join("environ")) {
            let prefix = prefix.canonicalize().unwrap_or(prefix);
            if !prefixes.contains(&prefix) {
                prefixes.push(prefix);
            }
        }
    }
    prefixes
}

/// is a wineserver (or the game) running in `prefix`? While one is, the
/// prefix's registry lives in that wineserver's memory and is written back
/// to user.reg when it exits, so user.reg must not be edited.
pub fn prefix_in_use(prefix: &Path) -> bool {
    let wanted = prefix.canonicalize().unwrap_or_else(|_| prefix.to_path_buf());
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return false;
    };
    entries.flatten().any(|entry| {
        let pid = entry.file_name();
        if !pid.to_string_lossy().bytes().all(|byte| byte.is_ascii_digit()) {
            return false;
        }
        let Ok(comm) = std::fs::read_to_string(entry.path().join("comm")) else {
            return false;
        };
        let comm = comm.trim().to_ascii_lowercase();
        if comm != "wineserver" && !comm.contains("rocketleague") {
            return false;
        }
        prefix_from_environ(&entry.path().join("environ")).is_some_and(|found| {
            found.canonicalize().unwrap_or(found) == wanted
        })
    })
}

fn prefix_from_environ(path: &Path) -> Option<PathBuf> {
    let environ = std::fs::read(path).ok()?;
    let mut wine_prefix = None;
    let mut compat_data = None;
    for variable in environ.split(|byte| *byte == 0) {
        let variable = String::from_utf8_lossy(variable);
        if let Some(value) = variable.strip_prefix("WINEPREFIX=") {
            wine_prefix = Some(PathBuf::from(value));
        } else if let Some(value) = variable.strip_prefix("STEAM_COMPAT_DATA_PATH=") {
            compat_data = Some(PathBuf::from(value).join("pfx"));
        }
    }
    wine_prefix.or(compat_data).filter(|prefix| prefix.join("drive_c").is_dir())
}

/// `drive_c/users/<user>/Documents` of a prefix, preferring Proton's
/// "steamuser"
fn prefix_documents(prefix: &Path) -> Option<PathBuf> {
    let users = prefix.join("drive_c/users");
    let steamuser = users.join("steamuser/Documents");
    if steamuser.is_dir() {
        return Some(steamuser);
    }
    std::fs::read_dir(&users)
        .ok()?
        .flatten()
        .map(|entry| entry.path().join("Documents"))
        .find(|documents| documents.join(RL_DIR).is_dir())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn touch(path: &Path) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, b"x").unwrap();
    }

    #[test]
    fn picks_the_documents_folder_with_real_files() {
        let root = std::env::temp_dir().join(format!("hebnix-docs-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let empty = root.join("empty/Documents");
        let stale = root.join("stale/Documents");
        let live = root.join("live/Documents");
        // folders exist but hold nothing real
        std::fs::create_dir_all(empty.join(RL_DIR).join("TAGame/SaveData/DBE_Production")).unwrap();
        touch(&stale.join(RL_DIR).join("TAGame/SaveDataEpic/DBE_Production/a.save"));
        std::thread::sleep(std::time::Duration::from_millis(20));
        touch(&live.join(RL_DIR).join("TAGame/Logs/Launch.log"));

        assert_eq!(rocket_league_activity(&empty), None);
        assert_eq!(
            newest_rocket_league_documents(vec![empty.clone(), stale.clone(), live.clone()]),
            Some(live.clone())
        );
        assert_eq!(newest_rocket_league_documents(vec![empty.clone()]), None);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn prefixes_are_found_with_and_without_pfx() {
        let root = std::env::temp_dir().join(format!("hebnix-prefix-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let wine = root.join("Rocket League");
        let proton = root.join("rocketleague");
        std::fs::create_dir_all(wine.join("drive_c")).unwrap();
        std::fs::create_dir_all(proton.join("pfx/drive_c")).unwrap();
        std::fs::create_dir_all(root.join("empty")).unwrap();
        let mut prefixes = Vec::new();
        for dir in [&wine, &proton, &root.join("empty"), &wine] {
            push_prefix(&mut prefixes, dir);
        }
        assert_eq!(prefixes, vec![wine.clone(), proton.join("pfx")]);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn heroic_config_values_are_found_when_nested() {
        let json = serde_json::json!({"Sugar": {"winePrefix": "/games/rl", "other": 1}});
        assert_eq!(find_string(&json, "winePrefix").as_deref(), Some("/games/rl"));
        assert_eq!(find_string(&json, "missing"), None);
    }

    #[test]
    fn override_accepts_documents_or_its_rocket_league_folder() {
        let documents = PathBuf::from("/x/drive_c/users/steamuser/Documents");
        assert_eq!(normalize_documents(&documents.join(RL_DIR)), documents);
        assert_eq!(normalize_documents(&documents), documents);
    }
}

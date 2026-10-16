//! Complete menu-arena replacement. Gameplay packages are only read; menu-only
//! copies contain the field, goals and recursively discovered streaming layers.
use super::{backup_guard, upk_package::UpkPackage};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{HashSet, VecDeque};
use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

const MENU: &str = "MENU_Main_p.upk";
const STATE: &str = ".hebnix-menu-map";
const MANIFEST: &str = "manifest.json";
const BACKUP: &str = "original.bin";

#[derive(Clone, Debug)]
pub struct Arena {
    pub package: String,
    pub label: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct MenuFile {
    name: String,
    sha256: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ActiveSwap {
    pub arena: String,
    build: String,
    original_sha256: String,
    patched_sha256: String,
    files: Vec<MenuFile>,
}

fn state_dir(cooked: &Path) -> PathBuf {
    cooked.join(STATE)
}

fn valid_package(name: &str) -> bool {
    !name.is_empty() && name.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'_')
}

fn arena_package(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    valid_package(name)
        && lower.ends_with("_p")
        && !lower.starts_with("bg_")
        && !lower.starts_with("menu_")
        && !["_oob", "_fx", "_vfx", "_signs", "woods_forest"]
            .iter()
            .any(|part| lower.contains(part))
}

pub fn installed_arenas(cooked: &Path) -> Result<Vec<Arena>, String> {
    let mut arenas = Vec::new();
    for entry in fs::read_dir(cooked).map_err(|e| format!("Could not read the game maps: {e}"))? {
        let entry = entry.map_err(|e| e.to_string())?;
        let path = entry.path();
        if !path
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("upk"))
        {
            continue;
        }
        let Some(package) = path.file_stem().and_then(|s| s.to_str()) else {
            continue;
        };
        // Filter names before asking for metadata. CookedPCConsole contains
        // thousands of unrelated packages, and per-file stat calls are slow.
        if !arena_package(package) {
            continue;
        }
        let file_type = entry.file_type().map_err(|e| e.to_string())?;
        if file_type.is_file() || (file_type.is_symlink() && path.is_file()) {
            arenas.push(Arena {
                label: crate::discord_presence::display_map_name(package),
                package: package.to_string(),
            });
        }
    }
    arenas.sort_by_key(|arena| (arena.label.to_lowercase(), arena.package.to_lowercase()));
    Ok(arenas)
}

fn hash(path: &Path) -> Result<String, String> {
    let mut file =
        fs::File::open(path).map_err(|e| format!("Could not read {}: {e}", path.display()))?;
    let mut digest = Sha256::new();
    let mut buffer = [0u8; 65536];
    loop {
        let size = file.read(&mut buffer).map_err(|e| e.to_string())?;
        if size == 0 {
            break;
        }
        digest.update(&buffer[..size]);
    }
    Ok(hex::encode(digest.finalize()))
}

fn owned_alias(name: &str) -> bool {
    name.strip_suffix(".upk")
        .and_then(|s| s.strip_prefix("HM"))
        .is_some_and(|tail| tail.len() >= 3 && tail.bytes().all(|c| c.is_ascii_hexdigit()))
}

pub fn active(cooked: &Path) -> Result<Option<ActiveSwap>, String> {
    let path = state_dir(cooked).join(MANIFEST);
    let data = match fs::read(&path) {
        Ok(data) => data,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("Could not read the menu map state: {e}")),
    };
    let state: ActiveSwap = serde_json::from_slice(&data)
        .map_err(|e| format!("The menu map state could not be read: {e}"))?;
    let mut seen = HashSet::new();
    if !valid_package(&state.arena)
        || state
            .files
            .iter()
            .any(|file| !owned_alias(&file.name) || !seen.insert(&file.name))
    {
        return Err("The menu map state contains invalid file names.".into());
    }
    Ok(Some(state))
}

fn ensure_closed() -> Result<(), String> {
    if hebnix_sdk::process::is_rocket_league_running() {
        Err("Close Rocket League before applying or restoring the menu map.".into())
    } else {
        Ok(())
    }
}

/// Discover every layer rather than swapping only the arena's distant scenery.
fn arena_layers(cooked: &Path, arena: &str) -> Result<Vec<PathBuf>, String> {
    let mut queue = VecDeque::from([arena.to_string()]);
    let mut seen = HashSet::new();
    let mut layers = Vec::new();
    while let Some(name) = queue.pop_front() {
        if !valid_package(&name) {
            return Err(format!("Invalid arena layer: {name}"));
        }
        if !seen.insert(name.to_ascii_lowercase()) {
            continue;
        }
        if seen.len() > 64 {
            return Err("This arena has too many streaming layers.".into());
        }
        let path = cooked.join(format!("{name}.upk"));
        let package = UpkPackage::load(&path).map_err(|e| format!("Could not load {name}: {e}"))?;
        for index in package.find_all_stream_name_indices() {
            queue.push_back(package.names[index].clone());
        }
        layers.push(path);
    }
    Ok(layers)
}

#[derive(Clone)]
struct StreamSlot {
    export: usize,
    name: usize,
    original: String,
}

fn arena_slots(menu: &UpkPackage) -> Result<Vec<StreamSlot>, String> {
    let mut slots = Vec::new();
    for e in &menu.exports {
        if !menu.class_of(e).starts_with("LevelStreamingKismet") {
            continue;
        }
        let props = menu.serialized_props(e)?.0;
        let Some(prop) = props.iter().find(|p| p.name == "PackageName") else {
            continue;
        };
        let index = menu.read_int(e.serial_offset + prop.value_offset)? as usize;
        let name = menu.names.get(index).ok_or("Invalid menu streaming name")?;
        // Preserve the actual menu, garage, cinematics and introduction sequences.
        if name.starts_with("bg_")
            || name.ends_with("_sfx")
            || name.ends_with("_vfx")
            || name.starts_with("outlaw_oasis_")
            || name.starts_with("neotokyo_hax_signs")
        {
            slots.push(StreamSlot {
                export: e.table_index,
                name: index,
                original: name.clone(),
            });
        }
    }
    let primary = slots
        .iter()
        .position(|s| s.original == "bg_stadium")
        .ok_or("This game version has no supported main menu stadium slot.")?;
    slots.swap(0, primary);
    Ok(slots)
}

/// Redirect arena streaming actions, including the default stadium's field and
/// goals. Editing only the seasonal background leaves the original grass loaded.
fn redirect_arena_actions(menu: &mut UpkPackage, slots: &[StreamSlot]) -> Result<usize, String> {
    let arena_names: HashSet<_> = slots.iter().map(|s| s.name).collect();
    let mut offsets = Vec::new();
    for e in &menu.exports {
        if !menu.class_of(e).starts_with("SeqAct_MultiLevelStreaming") {
            continue;
        }
        let props = menu.serialized_props(e)?.0;
        let Some(levels) = props.iter().find(|p| p.name == "Levels") else {
            continue;
        };
        let start = e.serial_offset + levels.value_offset;
        let end = start + levels.size;
        let count = menu.read_int(start)?;
        if !(0..=256).contains(&count) {
            return Err("Invalid menu arena list.".into());
        }
        let mut at = start + 4;
        for _ in 0..count {
            let (props, next) = menu.nested_props(at, end)?;
            for prop in props {
                if prop.tag_type == "NameProperty"
                    && arena_names.contains(&(menu.read_int(prop.value_offset)? as usize))
                {
                    offsets.push(prop.value_offset);
                }
            }
            at = next;
        }
        if at != end {
            return Err("Unexpected menu arena list layout.".into());
        }
    }
    if offsets.is_empty() {
        return Err("No main menu arena loading actions were found.".into());
    }
    for offset in &offsets {
        menu.patch_i32(*offset, slots[0].name as i32)?;
    }
    Ok(offsets.len())
}

fn alias(index: usize, length: usize) -> Result<String, String> {
    if length < 5 || index > 0xfff {
        return Err("Unsupported menu package name slot.".into());
    }
    Ok(format!("HM{index:0width$X}", width = length - 2))
}

/// Prepare the complete patch in an isolated directory. No installed files change.
fn prepare(cooked: &Path, arena: &str, output: &Path) -> Result<Vec<MenuFile>, String> {
    let layers = arena_layers(cooked, arena)?;
    let mut menu = UpkPackage::load(&cooked.join(MENU))?;
    let slots = arena_slots(&menu)?;
    if layers.len() > slots.len() {
        return Err(format!(
            "This arena needs {} layers; this menu supports {}.",
            layers.len(),
            slots.len()
        ));
    }
    let always_loaded = menu
        .exports
        .iter()
        .find(|e| menu.class_of(e).starts_with("LevelStreamingAlwaysLoaded"))
        .ok_or("No menu streaming template was found.")?
        .class_index;
    redirect_arena_actions(&mut menu, &slots)?;
    let mut files = Vec::new();
    for (i, (layer, slot)) in layers.iter().zip(&slots).enumerate() {
        let name = alias(i, slot.original.len())?;
        let filename = format!("{name}.upk");
        if cooked.join(&filename).exists() {
            return Err(format!(
                "{filename} already exists. Restore the previous menu map first."
            ));
        }
        menu.rename_name(slot.name, &name)?;
        if i != 0 {
            menu.patch_i32(menu.exports[slot.export].entry_offset, always_loaded)?;
        }
        let staged = output.join(format!("{i}.bin"));
        fs::copy(layer, &staged).map_err(|e| format!("Could not prepare the arena: {e}"))?;
        files.push(MenuFile {
            name: filename,
            sha256: hash(&staged)?,
        });
    }
    let path = output.join("patched.bin");
    menu.save(&path)?;
    let verified = UpkPackage::load(&path)?;
    if verified.image != menu.image {
        return Err("Menu package verification failed.".into());
    }
    Ok(files)
}

fn copy_new(source: &Path, destination: &Path) -> Result<(), String> {
    let mut input = fs::File::open(source).map_err(|e| e.to_string())?;
    let mut output = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(destination)
        .map_err(|e| format!("Could not create {}: {e}", destination.display()))?;
    let result = std::io::copy(&mut input, &mut output)
        .and_then(|_| output.flush())
        .and_then(|_| output.sync_all());
    drop(output);
    if result.is_err() {
        // create_new succeeded, so this is our incomplete copy, not another mod.
        let _ = fs::remove_file(destination);
    }
    result.map_err(|e| e.to_string())
}

fn replace_menu(source: &Path, cooked: &Path) -> Result<(), String> {
    let temporary = cooked.join(".hebnix-menu-map-replace.tmp");
    copy_new(source, &temporary)?;
    let result = fs::rename(&temporary, cooked.join(MENU))
        .map_err(|e| format!("Could not replace the menu: {e}"));
    if result.is_err() {
        let _ = fs::remove_file(temporary);
    }
    result
}

pub fn apply(cooked: &Path, arena: &str) -> Result<String, String> {
    ensure_closed()?;
    if active(cooked)?.is_some() {
        return Err("Restore the current menu map before applying another.".into());
    }
    if !installed_arenas(cooked)?
        .iter()
        .any(|entry| entry.package == arena)
    {
        return Err("Choose an installed arena from the menu map list.".into());
    }
    let state = state_dir(cooked);
    // Never erase unknown/orphaned backup files automatically.
    fs::create_dir(&state).map_err(|e| format!("Could not create the menu backup folder: {e}"))?;
    let prepared = (|| {
        let build = backup_guard::build_id(cooked)?;
        let original_sha256 = hash(&cooked.join(MENU))?;
        let files = prepare(cooked, arena, &state)?;
        fs::copy(cooked.join(MENU), state.join(BACKUP)).map_err(|e| e.to_string())?;
        if hash(&state.join(BACKUP))? != original_sha256 {
            return Err("The menu changed while preparing the backup.".into());
        }
        let manifest = ActiveSwap {
            arena: arena.to_string(),
            build,
            original_sha256,
            patched_sha256: hash(&state.join("patched.bin"))?,
            files,
        };
        // Persist the recovery record BEFORE copying anything into the game.
        fs::write(
            state.join(MANIFEST),
            serde_json::to_vec_pretty(&manifest).map_err(|e| e.to_string())?,
        )
        .map_err(|e| e.to_string())?;
        Ok::<_, String>(manifest)
    })();
    let manifest = match prepared {
        Ok(state) => state,
        Err(error) => {
            let _ = fs::remove_dir_all(&state); // exclusively created above; live files untouched
            return Err(error);
        }
    };
    let installed = (|| {
        ensure_closed()?;
        for (i, file) in manifest.files.iter().enumerate() {
            copy_new(&state.join(format!("{i}.bin")), &cooked.join(&file.name))?;
        }
        ensure_closed()?;
        if hash(&cooked.join(MENU))? != manifest.original_sha256 {
            return Err("The menu changed before the patch could be installed.".into());
        }
        replace_menu(&state.join("patched.bin"), cooked)?;
        if hash(&cooked.join(MENU))? != manifest.patched_sha256 {
            return Err("Installed menu verification failed.".into());
        }
        Ok::<_, String>(())
    })();
    if let Err(error) = installed {
        return match restore(cooked) {
            Ok(_) => Err(format!("{error} The original menu was kept.")),
            Err(rollback) => Err(format!(
                "{error} Close the game and restore the menu map to finish recovery: {rollback}"
            )),
        };
    }
    Ok(format!(
        "Main menu map changed to {}. Start Rocket League to see it.",
        crate::discord_presence::display_map_name(arena)
    ))
}

#[derive(Debug, PartialEq)]
enum RestoreAction {
    Restore,
    KeepCurrent,
}

fn restore_action(state: &ActiveSwap, build: &str, live: &str) -> Result<RestoreAction, String> {
    if live == state.original_sha256 {
        return Ok(RestoreAction::KeepCurrent);
    }
    if build != state.build {
        if live != state.patched_sha256 {
            return Ok(RestoreAction::KeepCurrent);
        }
        return Err("Rocket League was updated. Verify its game files in the launcher, then use Restore to clear the old menu map.".into());
    }
    if live != state.patched_sha256 {
        return Err("The menu was changed by another tool. Verify its game files before restoring this backup.".into());
    }
    Ok(RestoreAction::Restore)
}

pub fn restore(cooked: &Path) -> Result<String, String> {
    ensure_closed()?;
    let Some(manifest) = active(cooked)? else {
        return Err("No menu map change is active.".into());
    };
    let state = state_dir(cooked);
    let action = restore_action(
        &manifest,
        &backup_guard::build_id(cooked)?,
        &hash(&cooked.join(MENU))?,
    )?;
    // Check every owned file before restoring or removing anything. Missing
    // aliases are allowed so an interrupted installation can always be restored.
    for file in &manifest.files {
        let path = cooked.join(&file.name);
        if path.exists() && hash(&path)? != file.sha256 {
            return Err(format!(
                "{} changed outside Hebnix; it was left untouched.",
                file.name
            ));
        }
    }
    if action == RestoreAction::Restore {
        if hash(&state.join(BACKUP))? != manifest.original_sha256 {
            return Err("The original menu backup failed verification.".into());
        }
        replace_menu(&state.join(BACKUP), cooked)?;
        if hash(&cooked.join(MENU))? != manifest.original_sha256 {
            return Err("Restored menu verification failed.".into());
        }
    }
    for file in &manifest.files {
        let path = cooked.join(&file.name);
        if path.exists() {
            fs::remove_file(path).map_err(|e| e.to_string())?;
        }
    }
    fs::remove_dir_all(&state)
        .map_err(|e| format!("Menu restored, but the backup folder could not be cleared: {e}"))?;
    Ok("Original main menu map restored.".into())
}

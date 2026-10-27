//! Version-guarded staging, installation and removal of the live history bridge.
use crate::patcher::upk_package::UpkPackage;
use sha2::{Digest, Sha256};
use std::path::Path;
use std::sync::{
    Mutex, OnceLock,
    atomic::{AtomicBool, Ordering},
};

static BUSY: AtomicBool = AtomicBool::new(false);
static MESSAGE: Mutex<String> = Mutex::new(String::new());
pub fn busy() -> bool {
    BUSY.load(Ordering::Acquire)
}
pub fn message() -> String {
    MESSAGE.lock().unwrap().clone()
}
fn hash(path: &Path) -> Result<String, String> {
    Ok(format!(
        "{:x}",
        Sha256::digest(std::fs::read(path).map_err(|e| e.to_string())?)
    ))
}
pub fn installed(game: &Path, base: &Path) -> bool {
    type Cache = Option<(String, bool)>;
    static CACHE: OnceLock<Mutex<Cache>> = OnceLock::new();
    let source = game.join("TAGame/CookedPCConsole/TAGame.upk");
    let receipt = base.join("live-history-install.json");
    let key = format!(
        "{:?}:{:?}:{:?}",
        source,
        std::fs::metadata(&source).map(|m| (m.len(), m.modified().ok())),
        std::fs::metadata(&receipt).map(|m| (m.len(), m.modified().ok()))
    );
    let mut cache = CACHE.get_or_init(|| Mutex::new(None)).lock().unwrap();
    if let Some((old, value)) = cache.as_ref() {
        if old == &key {
            return *value;
        }
    }
    let value = (|| -> Option<bool> {
        let receipt: serde_json::Value =
            serde_json::from_slice(&std::fs::read(receipt).ok()?).ok()?;
        if Path::new(receipt["source"].as_str()?) != source {
            return Some(false);
        }
        if let Some(expected) = receipt["installed_sha256"].as_str() {
            if hash(&source).ok()? == expected {
                return Some(true);
            }
        }
        // Recognize the previous receipt format without trusting its presence
        // alone: check the actual bridge expressions in both game functions.
        let package = UpkPackage::load(&source).ok()?;
        let (_, init) = payload(&package, INIT).ok()?;
        let (_, command) = payload(&package, COMMAND).ok()?;
        let b = Builder(&package);
        let logger = b.logger().ok()?;
        let request = b.command().ok()?;
        if init.len() < 48 {
            return Some(false);
        }
        let end = 48usize
            .checked_add(word(&init, 44) as usize)?
            .checked_sub(3)?;
        if end > init.len() {
            return Some(false);
        }
        Some(
            init.get(end.checked_sub(logger.bytes.len())?..end) == Some(logger.bytes.as_slice())
                && command.get(48..48 + request.bytes.len()) == Some(request.bytes.as_slice()),
        )
    })()
    .unwrap_or(false);
    *cache = Some((key, value));
    value
}

pub fn resume_operation() -> Option<bool> {
    std::env::args().find_map(|arg| match arg.as_str() {
        "--resume-live-history-install" => Some(false),
        "--resume-live-history-uninstall" => Some(true),
        _ => None,
    })
}

pub fn admin_restart_canceled() -> bool {
    std::env::args().any(|arg| arg == "--live-history-admin-canceled")
}

pub fn show_admin_restart_canceled() {
    *MESSAGE.lock().unwrap() =
        "Administrator approval was canceled. The bridge was not changed.".into();
}

pub fn restart_as_admin(remove: bool) -> Result<(), String> {
    use std::os::windows::process::CommandExt;
    let exe = std::env::current_exe()
        .map_err(|e| e.to_string())?
        .to_string_lossy()
        .replace(char::from(39), "''");
    let pid = std::process::id();
    let action = if remove {
        "--resume-live-history-uninstall"
    } else {
        "--resume-live-history-install"
    };
    let renderer = if crate::window_state::safe_rendering() {
        " --safe-rendering"
    } else {
        ""
    };
    let fallback = format!(
        "{} --live-history-admin-canceled{renderer}",
        crate::spoofer::SKIP_ELEVATE_ARG
    );
    let script = format!(
        "Wait-Process -Id {pid} -ErrorAction SilentlyContinue; try {{ Start-Process -FilePath '{exe}' -ArgumentList '{action}{renderer}' -Verb RunAs -ErrorAction Stop }} catch {{ Start-Process -FilePath '{exe}' -ArgumentList '{fallback}' }}"
    );
    std::process::Command::new("powershell.exe")
        .args(["-NoProfile", "-WindowStyle", "Hidden", "-Command", &script])
        .creation_flags(0x08000000)
        .spawn()
        .map_err(|e| format!("Could not restart Hebnix as administrator: {e}"))?;
    Ok(())
}

pub fn start_operation(
    game: std::path::PathBuf,
    base: std::path::PathBuf,
    remove: bool,
    ctx: eframe::egui::Context,
) {
    if BUSY.swap(true, Ordering::AcqRel) {
        return;
    }
    *MESSAGE.lock().unwrap() = if remove {
        "Uninstalling bridge…"
    } else {
        "Installing bridge…"
    }
    .into();
    std::thread::spawn(move || {
        let result = (|| -> Result<(), String> {
            if !crate::spoofer::is_admin() {
                return Err(
                    "Hebnix must restart as administrator before changing the bridge".into(),
                );
            }
            if remove {
                uninstall(&game, &base)
            } else {
                install(&game, &base)
            }
        })();
        *MESSAGE.lock().unwrap() = match result {
            Ok(()) => if remove {
                "Live bridge uninstalled"
            } else {
                "Live bridge installed"
            }
            .into(),
            Err(e) => e,
        };
        BUSY.store(false, Ordering::Release);
        ctx.request_repaint();
    });
}

pub fn uninstall(game: &Path, base: &Path) -> Result<(), String> {
    if !crate::spoofer::is_admin() {
        return Err("Administrator access is required to uninstall the bridge".into());
    }
    ensure_closed()?;
    let receipt_path = base.join("live-history-install.json");
    let receipt: serde_json::Value = serde_json::from_slice(
        &std::fs::read(&receipt_path).map_err(|_| "Bridge backup receipt unavailable")?,
    )
    .map_err(|e| e.to_string())?;
    let source = game.join("TAGame/CookedPCConsole/TAGame.upk");
    if Path::new(receipt["source"].as_str().ok_or("Invalid bridge receipt")?) != source {
        return Err("Bridge receipt belongs to another installation".into());
    }
    let backup = Path::new(
        receipt["backup"]
            .as_str()
            .ok_or("Bridge backup unavailable")?,
    );
    let expected = if let Some(value) = receipt["installed_sha256"].as_str() {
        value.to_owned()
    } else {
        let staged = base.join(format!(
            "history-uninstall-check-{}.upk",
            rand::random::<u64>()
        ));
        stage(backup, &staged)?;
        let digest = hash(&staged);
        let _ = std::fs::remove_file(staged);
        digest?
    };
    if hash(&source)? != expected {
        return Err("Game package changed after bridge installation. Refusing to overwrite subsequent edits or a game update".into());
    }
    if let Some(expected) = receipt["backup_sha256"].as_str() {
        if hash(backup)? != expected {
            return Err("Bridge backup verification failed".into());
        }
    }
    let sibling = source.with_extension("hebnix-history-restore");
    std::fs::copy(backup, &sibling).map_err(|e| e.to_string())?;
    if hash(&sibling)? != hash(backup)? {
        return Err("Bridge restore copy verification failed".into());
    }
    ensure_closed()?;
    let (config, original, restored) = prepare_launcher_config(|original| {
        Ok(original
            .lines()
            .map(|line| {
                receipt["launcher_lines"]
                    .as_array()
                    .and_then(|pairs| {
                        pairs
                            .iter()
                            .find(|pair| pair["after"].as_str() == Some(line))
                    })
                    .and_then(|pair| pair["before"].as_str())
                    .unwrap_or(line)
                    .to_owned()
            })
            .collect::<Vec<_>>()
            .join("\r\n")
            + "\r\n")
    })?;
    ensure_closed()?;
    if let Some(changed) = &restored {
        std::fs::write(&config, changed).map_err(|e| e.to_string())?;
    }
    if let Err(e) = replace(&sibling, &source) {
        if restored.is_some() {
            let _ = std::fs::write(&config, original);
        }
        return Err(e);
    }
    std::fs::remove_file(receipt_path).map_err(|e| e.to_string())?;
    Ok(())
}

const INIT: &str = "GFxData_MatchHistory_TA.Init";
const COMMAND: &str = "MatchStatsExporter_TA.ProcessSocketCommand";
const MANAGER: &str = "GFxData_MatchHistoryManager_TA";

/// Requires the game closed. Epic is stopped automatically only if its
/// cached launch arguments need changing.
pub fn install(game: &Path, base: &Path) -> Result<(), String> {
    if !crate::spoofer::is_admin() {
        return Err("Administrator access is required to install the bridge".into());
    }
    ensure_closed()?;
    if installed(game, base) {
        return Ok(());
    }
    install_closed(game, base)
}

fn ensure_closed() -> Result<(), String> {
    if hebnix_sdk::process::is_rocket_league_running() {
        return Err("Close Rocket League before changing the history bridge".into());
    }
    Ok(())
}

fn launcher_config_path() -> Result<std::path::PathBuf, String> {
    Ok(std::env::var_os("LOCALAPPDATA")
        .map(std::path::PathBuf::from)
        .ok_or("Local app data unavailable")?
        .join("EpicGamesLauncher/Saved/Config/WindowsEditor/GameUserSettings.ini"))
}

// Epic caches these settings and can write them back on exit. Only stop it
// when an argument actually changes, then reread after it has fully exited.
fn prepare_launcher_config(
    transform: impl Fn(&str) -> Result<String, String>,
) -> Result<(std::path::PathBuf, String, Option<String>), String> {
    let path = launcher_config_path()?;
    let original =
        std::fs::read_to_string(&path).map_err(|_| "Epic launcher settings unavailable")?;
    let changed = transform(&original)?;
    if original.lines().eq(changed.lines()) {
        return Ok((path, original, None));
    }
    ensure_closed()?;
    *MESSAGE.lock().unwrap() = "Closing Epic Games Launcher to update launch settings…".into();
    crate::winutil::stop_epic_launcher()?;
    ensure_closed()?;
    let original = std::fs::read_to_string(&path).map_err(|e| e.to_string())?;
    let changed = transform(&original)?;
    let changed = (!original.lines().eq(changed.lines())).then_some(changed);
    Ok((path, original, changed))
}

fn add_logging_argument(original: &str) -> Result<String, String> {
    let mut found_commands = false;
    let mut found_enabled = false;
    let changed = original
        .lines()
        .map(|line| {
            if let Some((key, value)) = line.split_once('=') {
                if key.ends_with(":Sugar_AdditionalCommandsEnabled") {
                    found_enabled = true;
                    return if value.trim().eq_ignore_ascii_case("true") {
                        line.to_owned()
                    } else {
                        format!("{key}=True")
                    };
                }
                if key.ends_with(":Sugar_AdditionalCommands") {
                    found_commands = true;
                    return if value
                        .split_whitespace()
                        .any(|arg| arg.eq_ignore_ascii_case("-FORCELOGFLUSH"))
                    {
                        line.to_owned()
                    } else {
                        format!("{key}={} -FORCELOGFLUSH", value.trim_end())
                    };
                }
            }
            line.to_owned()
        })
        .collect::<Vec<_>>()
        .join("\r\n")
        + "\r\n";
    if !found_commands || !found_enabled {
        return Err(
            "Enable Rocket League additional command-line arguments in Epic once before installing"
                .into(),
        );
    }
    Ok(changed)
}

fn install_closed(game: &Path, base: &Path) -> Result<(), String> {
    let source = game.join("TAGame/CookedPCConsole/TAGame.upk");
    let staged = base.join(format!("live-history-stage-{}.upk", rand::random::<u64>()));
    stage(&source, &staged)?;
    let backup = base.join(format!(
        "TAGame-before-history-{}.upk",
        rand::random::<u64>()
    ));
    std::fs::copy(&source, &backup).map_err(|_| "Could not back up game package")?;
    let sibling = source.with_extension("hebnix-history-new");
    std::fs::copy(&staged, &sibling)
        .map_err(|e| format!("Could not stage bridge beside game package: {e}"))?;
    if hash(&sibling)? != hash(&staged)? {
        return Err("Bridge copy verification failed; installed package unchanged".into());
    }
    let (config, original_config, changed_config) = prepare_launcher_config(add_logging_argument)?;
    let launcher_lines: Vec<_> = original_config
        .lines()
        .zip(
            changed_config
                .as_deref()
                .unwrap_or(&original_config)
                .lines(),
        )
        .filter(|(a, b)| a != b)
        .map(|(before, after)| serde_json::json!({"before":before,"after":after}))
        .collect();
    let receipt = serde_json::json!({"backup":backup,"source":source,"backup_sha256":hash(&backup)?,"installed_sha256":hash(&staged)?,"launcher_lines":launcher_lines});
    std::fs::write(base.join("live-history-install.json"), receipt.to_string())
        .map_err(|_| "Could not save bridge backup receipt")?;
    ensure_closed()?;
    if let Some(changed) = &changed_config {
        std::fs::write(&config, changed).map_err(|e| {
            format!("Could not enable live logging; installed package unchanged: {e}")
        })?;
    }
    if let Err(error) = replace(&sibling, &source) {
        if changed_config.is_some() {
            let _ = std::fs::write(&config, &original_config);
        }
        return Err(format!("Could not install bridge: {error}"));
    }
    let _ = std::fs::remove_file(&staged);
    Ok(())
}

fn replace(from: &Path, to: &Path) -> Result<(), String> {
    use std::os::windows::ffi::OsStrExt;
    use std::os::windows::fs::MetadataExt;
    use windows::Win32::Storage::FileSystem::{
        MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH, MoveFileExW, REPLACE_FILE_FLAGS,
        ReplaceFileW,
    };
    use windows::core::PCWSTR;
    let from =
        std::fs::canonicalize(from).map_err(|e| format!("Cannot locate staged package: {e}"))?;
    let to =
        std::fs::canonicalize(to).map_err(|e| format!("Cannot locate original package: {e}"))?;
    let expected = hash(&from)?;
    let original = hash(&to)?;
    let operation = rand::random::<u64>();
    tracing::info!(operation, elevated=crate::spoofer::is_admin(), pid=std::process::id(),
        executable=?std::env::current_exe(), staged=%from.display(), target=%to.display(),
        staged_metadata=?std::fs::metadata(&from).map(|m|(m.len(),m.file_attributes())),
        target_metadata=?std::fs::metadata(&to).map(|m|(m.len(),m.file_attributes())),
        "live bridge: replacing package");
    let backup = to.with_extension(format!("hebnix-rollback-{}", rand::random::<u64>()));
    let wide = |path: &Path| {
        path.as_os_str()
            .encode_wide()
            .chain(Some(0))
            .collect::<Vec<_>>()
    };
    let from_w = wide(&from);
    let to_w = wide(&to);
    let backup_w = wide(&backup);
    // ReplaceFile preserves the destination's permissions/attributes and keeps
    // a same-volume rollback copy, including on partial replacement failures.
    let result = unsafe {
        ReplaceFileW(
            PCWSTR(to_w.as_ptr()),
            PCWSTR(from_w.as_ptr()),
            PCWSTR(backup_w.as_ptr()),
            REPLACE_FILE_FLAGS(0),
            None,
            None,
        )
    };
    if hash(&to).ok().as_deref() == Some(expected.as_str()) {
        tracing::info!(operation, "live bridge: replacement hash verified");
        let _ = std::fs::remove_file(&backup);
        return Ok(());
    }
    tracing::error!(operation, error=?result.as_ref().err(),
        hresult=?result.as_ref().err().map(|e|e.code()),
        staged_exists=from.exists(), target_exists=to.exists(), backup_exists=backup.exists(),
        "live bridge: replacement failed or content did not match");
    let detail = match result {
        Ok(()) => "Replacement content verification failed".to_owned(),
        Err(e) => format!("Windows replacement failed: {e}"),
    };
    if hash(&to).ok().as_deref() != Some(original.as_str())
        && hash(&backup).ok().as_deref() == Some(original.as_str())
    {
        let recovery = unsafe {
            MoveFileExW(
                PCWSTR(backup_w.as_ptr()),
                PCWSTR(to_w.as_ptr()),
                MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
            )
        };
        if let Err(error) = recovery {
            tracing::error!(operation, %error, "live bridge: restoring original package failed");
        }
    }
    if hash(&to).ok().as_deref() == Some(original.as_str()) {
        tracing::info!(
            operation,
            "live bridge: original package hash verified after failure"
        );
        let _ = std::fs::remove_file(&backup);
        Err(format!(
            "{detail}. Original package verified unchanged. Details in hebnix.log (operation {operation}). Path: {}",
            to.display()
        ))
    } else {
        Err(format!(
            "{detail}. Could not verify recovery; details in hebnix.log (operation {operation}). Original backup retained at {}",
            backup.display()
        ))
    }
}

#[derive(Clone, Default)]
struct Code {
    bytes: Vec<u8>,
    memory: usize,
}
impl Code {
    fn raw(bytes: &[u8]) -> Self {
        Self {
            bytes: bytes.into(),
            memory: bytes.len(),
        }
    }
    fn op(byte: u8) -> Self {
        Self::raw(&[byte])
    }
    fn pointer(reference: i32) -> Self {
        Self {
            bytes: reference.to_le_bytes().into(),
            memory: 8,
        }
    }
    fn then(mut self, other: Self) -> Self {
        self.bytes.extend(other.bytes);
        self.memory += other.memory;
        self
    }
    fn string(text: &str) -> Self {
        Self::op(0x1f)
            .then(Self::raw(text.as_bytes()))
            .then(Self::op(0))
    }
    fn jump(condition: Self, destination: usize) -> Result<Self, String> {
        let offset =
            u16::try_from(destination).map_err(|_| "History bridge branch exceeds script limit")?;
        Ok(Self::op(7)
            .then(Self::raw(&offset.to_le_bytes()))
            .then(condition))
    }
}
struct Builder<'a>(&'a UpkPackage);
impl Builder<'_> {
    fn reference(&self, path: &str) -> Result<i32, String> {
        self.0
            .find_object(path)
            .ok_or_else(|| format!("Unsupported game package: missing {path}"))
    }
    fn field(&self, opcode: u8, path: &str) -> Result<Code, String> {
        Ok(Code::op(opcode).then(Code::pointer(self.reference(path)?)))
    }
    fn call(&self, path: &str, args: Vec<Code>) -> Result<Code, String> {
        Ok(args
            .into_iter()
            .fold(self.field(0x1c, path)?, Code::then)
            .then(Code::op(0x16)))
    }
    fn context(&self, target: Code, body: Code, property: Option<&str>) -> Result<Code, String> {
        let size =
            u16::try_from(body.memory).map_err(|_| "History context exceeds script limit")?;
        Ok(Code::raw(&[0x19, 0])
            .then(target)
            .then(Code::raw(&size.to_le_bytes()))
            .then(Code::pointer(
                property
                    .map(|p| self.reference(p))
                    .transpose()?
                    .unwrap_or(0),
            ))
            .then(Code::op(0))
            .then(body))
    }
    fn native(&self, opcode: u8, args: Vec<Code>) -> Code {
        args.into_iter()
            .fold(Code::op(opcode), Code::then)
            .then(Code::op(0x16))
    }
    fn tag(&self, name: &str) -> Result<Code, String> {
        let index = self
            .0
            .names
            .iter()
            .position(|n| n == name)
            .ok_or("Missing history log tag")?;
        Ok(Code::op(0x21)
            .then(Code::raw(&(index as u32).to_le_bytes()))
            .then(Code::raw(&[0; 4])))
    }
    fn manager(&self) -> Result<Code, String> {
        let get = self.context(
            self.store()?,
            self.call(
                "ProjectX.GFxDataStore_X.GetObject",
                vec![self.field(0x20, MANAGER)?, Code::op(0x48)],
            )?,
            Some("ProjectX.GFxDataStore_X.GetObject.ReturnValue"),
        )?;
        Ok(self.field(0x2e, MANAGER)?.then(get))
    }
    fn store(&self) -> Result<Code, String> {
        let player = self.context(
            self.field(1, "MatchStatsExporter_TA.PrimaryPC")?,
            self.call("PlayerControllerBase_TA.GetLocalPlayer", vec![])?,
            Some("PlayerControllerBase_TA.GetLocalPlayer.ReturnValue"),
        )?;
        self.context(
            self.call("GFxShell_TA.GetShell", vec![player])?,
            self.field(1, "ProjectX.GFxShell_X.DataStore")?,
            Some("ProjectX.GFxShell_X.DataStore"),
        )
    }
    fn logger(&self) -> Result<Code, String> {
        Ok(self.native(
            231,
            vec![
                self.context(
                    self.field(0x2b, &format!("{INIT}.Item"))?,
                    self.call("Core.Object.ToJson", vec![])?,
                    Some("Core.Object.ToJson.ReturnValue"),
                )?,
                self.tag("MatchHistory")?,
                Code::op(0x28),
            ],
        ))
    }
    fn command(&self) -> Result<Code, String> {
        let condition = self.native(
            204,
            vec![
                self.field(0x2b, &format!("{COMMAND}.CommandJson"))?,
                Code::string("{\"Command\":\"HebnixMatchHistory\"}"),
            ],
        );
        let is_none = self.native(207, vec![self.manager()?, Code::op(0x2a)]);
        let create = self.context(
            self.store()?,
            self.call(
                "ProjectX.GFxDataStore_X.CreateObject",
                vec![self.field(0x20, MANAGER)?, Code::op(0x48), Code::op(0x48)],
            )?,
            Some("ProjectX.GFxDataStore_X.CreateObject.ReturnValue"),
        )?;
        let first = 3 + condition.memory;
        let create_jump = Code::jump(is_none.clone(), first + 3 + is_none.memory + create.memory)?;
        let refresh = self.context(
            self.manager()?,
            self.call(&format!("{MANAGER}.RefreshCache"), vec![])?,
            None,
        )?;
        let sync = self.context(
            self.manager()?,
            self.call(&format!("{MANAGER}.SyncNewMatches"), vec![])?,
            None,
        )?;
        let marker = self.native(
            231,
            vec![
                Code::string("Hebnix live history requested"),
                self.tag("Log")?,
                Code::op(0x28),
            ],
        );
        let actions = create_jump
            .then(create)
            .then(refresh)
            .then(sync)
            .then(marker)
            .then(Code::raw(&[4, 11]));
        Ok(Code::jump(condition, first + actions.memory)?.then(actions))
    }
}

fn word(bytes: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap())
}
fn payload(package: &UpkPackage, path: &str) -> Result<(usize, Vec<u8>), String> {
    let reference = package
        .find_object(path)
        .filter(|r| *r > 0)
        .ok_or("History function missing")?;
    let entry = &package.exports[reference as usize - 1];
    Ok((
        entry.table_index,
        package.image[entry.serial_offset..entry.serial_offset + entry.serial_size].into(),
    ))
}
fn insert(before: &[u8], code: Code, at_end: bool, hash: &str) -> Result<Vec<u8>, String> {
    if format!("{:x}", Sha256::digest(before)) != hash {
        return Err("Unsupported game update: history function changed".into());
    }
    let size = word(before, 44) as usize;
    if before[48 + size - 3..48 + size] != [4, 11, 76] {
        return Err("Unexpected history function return".into());
    }
    let at = if at_end { 48 + size - 3 } else { 48 };
    let mut after = before[..at].to_vec();
    after.extend(&code.bytes);
    after.extend(&before[at..]);
    after[40..44].copy_from_slice(&(word(before, 40) + code.memory as u32).to_le_bytes());
    after[44..48].copy_from_slice(&(word(before, 44) + code.bytes.len() as u32).to_le_bytes());
    Ok(after)
}

pub fn stage(source: &Path, destination: &Path) -> Result<(), String> {
    if source == destination || destination.exists() {
        return Err("Bridge output must be a new staging file".into());
    }
    let mut package = UpkPackage::load(source)?;
    let (init_index, init) = payload(&package, INIT)?;
    let (command_index, command) = payload(&package, COMMAND)?;
    let builder = Builder(&package);
    let logger = builder.logger()?;
    let request = builder.command()?;
    if logger.bytes.len() != 32
        || logger.memory != 44
        || request.bytes.len() != 385
        || request.memory != 561
    {
        return Err("Unexpected history bridge serialization".into());
    }
    let init = insert(
        &init,
        logger,
        true,
        "c65e92e170e7e602fc6ea7e0d5b4df348ec62679e631cc266f67cc1ff5b2900e",
    )?;
    let command = insert(
        &command,
        request,
        false,
        "e3d45769fefb8656d806aacdd55d6923d9d1b76d86b8575c9df947c46fd7cd7d",
    )?;
    package.replace_export_payload(init_index, &init)?;
    package.replace_export_payload(command_index, &command)?;
    package.save(destination)?;
    let reopened = UpkPackage::load(destination)?;
    if reopened.names != package.names || reopened.exports.len() != package.exports.len() {
        return Err("Bridge package table validation failed".into());
    }
    for old in &package.exports {
        let new = &reopened.exports[old.table_index];
        if package.image[old.serial_offset..old.serial_offset + old.serial_size]
            != reopened.image[new.serial_offset..new.serial_offset + new.serial_size]
        {
            return Err(format!(
                "Bridge package verification failed for export {}",
                old.table_index
            ));
        }
    }
    Ok(())
}

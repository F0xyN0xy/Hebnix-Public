// crates/hebnix-app/src/spoofer/mod.rs
//! name spoofer. our own CA (ca), our own mitm proxy (proxy), spoofs live in rules.

pub mod ca;
pub mod crl;
pub mod dns;
pub mod hosts;
pub mod proxy;
pub mod rules;
pub mod skill_bridge;
pub mod socket;
pub use crate::recovery_tasks::session_marker;

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::{Arc, Mutex};

use crossbeam_channel::Sender;

use crate::messages::AppMsg;
use crate::spoofer::rules::{
    NameRule, OwnedProductsRule, Rule, TITLE_HOST, TitleRule, TitleSettings, TitleSpoofSettings,
};
use crate::spoofer::skill_bridge::SkillBridge;
use crate::spoofer::socket::SocketProxy;

pub const PROXY_HOST: &str = "127.0.0.1";
pub const PROXY_PORT: u16 = 8080;
pub const MAX_NAME_LENGTH: usize = 32;
// Intercept Epic's account API only for name/friends, and PsyNet config only
// for title/rank routing.  These must stay independent: redirecting the game
// config host for an account-only spoof forces Rocket League's startup/auth
// path through the local proxy when it has nothing to do there.
const ACCOUNT_HOST: &str = "api.epicgames.dev";

const INET_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Internet Settings";

pub fn is_admin() -> bool {
    type BOOL = i32;
    #[link(name = "shell32")]
    unsafe extern "system" {
        fn IsUserAnAdmin() -> BOOL;
    }
    unsafe { IsUserAnAdmin() != 0 }
}

pub const SKIP_ELEVATE_ARG: &str = "--no-elevate";
pub fn disable_saved_master(base_dir: &Path) -> std::io::Result<()> {
    let path = base_dir.join("spoofer_settings.json");
    if !path.is_file() {
        return Ok(());
    }
    let text = std::fs::read_to_string(&path)?;
    let mut settings: serde_json::Value = serde_json::from_str(&text)
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
    if let Some(object) = settings.as_object_mut() {
        object.insert("spoofer_master".to_string(), serde_json::Value::Bool(false));
    }
    let output = serde_json::to_vec_pretty(&settings)
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
    std::fs::write(path, output)
}
pub fn spawn_elevated_relaunch() -> bool {
    use std::os::windows::process::CommandExt;

    let Ok(exe) = std::env::current_exe() else {
        return false;
    };
    let exe = exe.to_string_lossy().replace('\'', "''"); // ps single quote escape
    let pid = std::process::id();
    let script = format!(
        "Wait-Process -Id {pid} -ErrorAction SilentlyContinue; \
         try {{ Start-Process -FilePath '{exe}' -Verb RunAs -ErrorAction Stop }} \
         catch {{ Start-Process -FilePath '{exe}' -ArgumentList '{SKIP_ELEVATE_ARG}' }}"
    );
    std::process::Command::new("powershell")
        .args(["-NoProfile", "-WindowStyle", "Hidden", "-Command", &script])
        .creation_flags(0x08000000) // CREATE_NO_WINDOW
        .spawn()
        .is_ok()
}

fn marker_path(base_dir: &Path) -> PathBuf {
    ca::dir(base_dir).join("proxy_backup.json")
}

#[derive(serde::Serialize, serde::Deserialize)]
struct ProxyBackup {
    proxy_enable: Option<u32>,
    proxy_server: Option<String>,
    #[serde(default)]
    proxy_override: Option<String>,
}

fn read_current_proxy() -> ProxyBackup {
    use winreg::RegKey;
    let hkcu = RegKey::predef(winreg::enums::HKEY_CURRENT_USER);
    match hkcu.open_subkey(INET_KEY) {
        Ok(k) => ProxyBackup {
            proxy_enable: k.get_value("ProxyEnable").ok(),
            proxy_server: k.get_value("ProxyServer").ok(),
            proxy_override: k.get_value("ProxyOverride").ok(),
        },
        Err(_) => ProxyBackup {
            proxy_enable: None,
            proxy_server: None,
            proxy_override: None,
        },
    }
}

fn apply_proxy(state: &ProxyBackup) -> Result<(), String> {
    use winreg::RegKey;
    use winreg::enums::*;

    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    let (settings, _) = hkcu
        .create_subkey(INET_KEY)
        .map_err(|e| format!("open inet key: {e}"))?;

    settings
        .set_value("ProxyEnable", &state.proxy_enable.unwrap_or(0))
        .map_err(|e| format!("set ProxyEnable: {e}"))?;

    for (name, value) in [
        ("ProxyServer", &state.proxy_server),
        ("ProxyOverride", &state.proxy_override),
    ] {
        match value {
            Some(v) => settings
                .set_value(name, v)
                .map_err(|e| format!("set {name}: {e}"))?,
            None => {
                let _ = settings.delete_value(name);
            }
        }
    }

    refresh_wininet();
    Ok(())
}

fn restore_legacy_hebnix_proxy(base_dir: &Path) {
    let marker = marker_path(base_dir);
    let current = read_current_proxy();
    let ours = format!("{PROXY_HOST}:{PROXY_PORT}");
    let legacy_hebnix_proxy = current.proxy_server.as_deref() == Some(&ours)
        && current.proxy_enable.unwrap_or_default() != 0;
    if legacy_hebnix_proxy {
        if let Some(backup) = std::fs::read(&marker)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<ProxyBackup>(&bytes).ok())
        {
            let _ = apply_proxy(&backup);
        }
    }
    let _ = std::fs::remove_file(&marker);
}

pub fn restore_if_crashed(base_dir: &Path) {
    if marker_path(base_dir).is_file() {
        tracing::warn!("stale spoofer proxy marker, restoring system proxy");
        restore_legacy_hebnix_proxy(base_dir);
    }
    if hosts::has_redirects() {
        tracing::warn!("stale hosts redirect, clearing it");
        let _ = hosts::clear();
    }
    if let Err(error) = clean_session_cache(base_dir) {
        tracing::warn!("Could not clear stale Rocket League proxy cache: {error}");
    }
}

fn refresh_wininet() {
    use windows::Win32::Networking::WinInet::{
        INTERNET_OPTION_REFRESH, INTERNET_OPTION_SETTINGS_CHANGED, InternetSetOptionW,
    };
    unsafe {
        let _ = InternetSetOptionW(None, INTERNET_OPTION_SETTINGS_CHANGED, None, 0);
        let _ = InternetSetOptionW(None, INTERNET_OPTION_REFRESH, None, 0);
    }
}

pub struct SpooferManager {
    base_dir: PathBuf,
    tx: Sender<AppMsg>,
    spoofed_name: Arc<Mutex<String>>,
    pub spoofed_friends: Arc<Mutex<HashMap<String, String>>>,
    pub discovered_friends: Arc<Mutex<HashMap<String, String>>>,
    pub spoofed_ranks: Arc<Mutex<HashMap<i32, (i32, f64)>>>,
    owned_products: Arc<Mutex<HashSet<i64>>>,
    reverse_proxy: Mutex<Option<SocketProxy>>,
    http_active: Arc<AtomicBool>,
    socket_active: Arc<AtomicBool>,
    game_present: Arc<AtomicBool>,
    title_settings: Arc<Mutex<TitleSettings>>,
    skill_bridge: Mutex<Option<SkillBridge>>,
    item_cleanup: crate::item_cleanup::Cleaner,
    crl: Mutex<Option<crl::CrlServer>>,
    rlapi_retained: Arc<AtomicBool>,
    account_retained: AtomicBool,
    // Serialize redirect publication with shutdown and the lifecycle worker.
    redirect_lock: Mutex<()>,
    certificate_blocked: AtomicBool,
    closing: AtomicBool,
    lifecycle_started: AtomicBool,
    requested_options: AtomicU8,
}

/// Consumers of the shared proxy. Transport switches are derived from features.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ProxyOptions {
    pub account: bool,
    pub psynet: bool,
    pub replays: bool,
}

impl ProxyOptions {
    fn bits(self) -> u8 {
        u8::from(self.account) | (u8::from(self.psynet) << 1) | (u8::from(self.replays) << 2)
    }

    fn from_bits(bits: u8) -> Self {
        Self {
            account: bits & 1 != 0,
            psynet: bits & 2 != 0,
            replays: bits & 4 != 0,
        }
    }
}

impl SpooferManager {
    pub fn game_running(&self) -> bool {
        self.game_present.load(Ordering::Acquire)
    }

    pub fn configure(&self, options: ProxyOptions) {
        self.requested_options
            .store(options.bits(), Ordering::Release);
    }

    // Only the lifecycle worker starts listeners. It also runs while the UI is hidden.
    fn sync_proxy_options(&self) -> Result<(), String> {
        let active = self.game_present.load(Ordering::Acquire)
            && !self.closing.load(Ordering::Acquire)
            && !self.certificate_blocked.load(Ordering::Acquire)
            && is_admin();
        let options = if active {
            ProxyOptions::from_bits(self.requested_options.load(Ordering::Acquire))
        } else {
            ProxyOptions::default()
        };
        // Disable consumers first; retained connections continue to relay without spoofs.
        if !options.replays && hebnix_sdk::rlapi::session::shared_game_session().enabled() {
            self.disable_rlapi();
        }
        if !options.account && self.http_active.load(Ordering::Acquire) {
            self.stop_http();
        }
        if !options.psynet && self.socket_active.load(Ordering::Acquire) {
            self.stop_socket();
        }
        if options.account && !self.http_running() {
            self.start_http()?;
        }
        if options.psynet && !self.socket_running() {
            self.start_socket()?;
        }
        if options.replays
            && (!hebnix_sdk::rlapi::session::shared_game_session().enabled()
                || !self
                    .reverse_proxy
                    .lock()
                    .map(|p| p.as_ref().is_some_and(SocketProxy::is_running))
                    .unwrap_or(false))
        {
            self.enable_rlapi()?;
        }
        self.cleanup_idle_rlapi();
        Ok(())
    }

    fn ensure_crl(&self, ca: &ca::Ca) -> Result<(), String> {
        let mut slot = self.crl.lock().map_err(|_| "CRL lock poisoned")?;
        if slot.is_some() {
            return Ok(());
        }
        *slot = Some(crl::CrlServer::start(ca.crl_der()?, self.tx.clone())?);
        Ok(())
    }

    fn maybe_stop_crl(&self) {
        let proxy_up = self
            .reverse_proxy
            .lock()
            .map(|p| p.is_some())
            .unwrap_or(false);
        if proxy_up {
            return;
        }
        if let Ok(mut slot) = self.crl.lock() {
            if let Some(s) = slot.take() {
                s.stop();
            }
        }
    }

    pub fn new(base_dir: PathBuf, tx: Sender<AppMsg>) -> Self {
        let owned_products = std::fs::read(base_dir.join("owned_products.json"))
            .ok()
            .and_then(|bytes| serde_json::from_slice::<Vec<i64>>(&bytes).ok())
            .unwrap_or_default()
            .into_iter()
            .collect();
        let item_cleanup = crate::item_cleanup::Cleaner::new(&base_dir);
        Self {
            base_dir,
            tx,
            spoofed_name: Arc::new(Mutex::new(String::new())),
            spoofed_friends: Arc::new(Mutex::new(HashMap::new())),
            discovered_friends: Arc::new(Mutex::new(HashMap::new())),
            spoofed_ranks: Arc::new(Mutex::new(HashMap::new())),
            owned_products: Arc::new(Mutex::new(owned_products)),
            reverse_proxy: Mutex::new(None),
            http_active: Arc::new(AtomicBool::new(false)),
            socket_active: Arc::new(AtomicBool::new(false)),
            game_present: Arc::new(AtomicBool::new(
                hebnix_sdk::process::is_rocket_league_running(),
            )),
            title_settings: Arc::new(Mutex::new(TitleSettings::default())),
            skill_bridge: Mutex::new(None),
            item_cleanup,
            crl: Mutex::new(None),
            rlapi_retained: Arc::new(AtomicBool::new(false)),
            account_retained: AtomicBool::new(false),
            redirect_lock: Mutex::new(()),
            certificate_blocked: AtomicBool::new(false),
            closing: AtomicBool::new(false),
            lifecycle_started: AtomicBool::new(false),
            requested_options: AtomicU8::new(0),
        }
    }

    pub fn rlapi_running(&self) -> bool {
        let session = hebnix_sdk::rlapi::session::shared_game_session();
        session.enabled()
            || session.has_connection()
            || (self.rlapi_retained.load(Ordering::Acquire)
                && hebnix_sdk::process::is_rocket_league_running())
    }

    /// A game may cache localhost DNS/config endpoints even between requests.
    /// Keep the original authenticated transport available until it exits.
    pub fn relay_running(&self) -> bool {
        self.rlapi_running()
            || (self.account_retained.load(Ordering::Acquire)
                && hebnix_sdk::process::is_rocket_league_running())
    }

    fn redirect_hosts(&self) -> Vec<&'static str> {
        if self.closing.load(Ordering::Acquire) || !self.game_present.load(Ordering::Acquire) {
            return Vec::new();
        }
        let mut hosts = Vec::with_capacity(2);
        if self.http_active.load(Ordering::Relaxed)
            || (self.account_retained.load(Ordering::Acquire)
                && self.game_present.load(Ordering::Acquire))
        {
            hosts.push(ACCOUNT_HOST);
        }
        if self.socket_active.load(Ordering::Relaxed) || self.rlapi_running() {
            hosts.push(TITLE_HOST);
        }
        hosts
    }

    pub fn enable_rlapi(&self) -> Result<(), String> {
        if !is_admin() || !ca::is_current_installed(&self.base_dir) {
            return Err("RLAPI capture requires administrator rights and the existing Hebnix certificate setup.".into());
        }
        let session = hebnix_sdk::rlapi::session::shared_game_session();
        session.set_enabled(true);
        let result = self
            .start_skill_bridge()
            .and_then(|_| self.ensure_reverse_proxy());
        if let Err(error) = result {
            session.set_enabled(false);
            self.stop_skill_bridge();
            self.stop_reverse_if_unused();
            return Err(error);
        }
        self.rlapi_retained.store(true, Ordering::Release);
        Ok(())
    }

    pub fn disable_rlapi(&self) {
        hebnix_sdk::rlapi::session::shared_game_session().set_enabled(false);
        // A connected game retains its relay until it disconnects. Disabling
        // API requests must not kick the game out of its current session.
        self.cleanup_idle_rlapi();
    }

    pub fn cleanup_idle_rlapi(&self) {
        if !self.http_active.load(Ordering::Relaxed)
            && !hebnix_sdk::process::is_rocket_league_running()
        {
            self.account_retained.store(false, Ordering::Release);
        }
        if !self.rlapi_running() {
            self.stop_reverse_if_unused();
            if !self.http_active.load(Ordering::Relaxed)
                && !self.socket_active.load(Ordering::Relaxed)
            {
                self.stop_skill_bridge();
            }
            self.maybe_stop_crl();
            // Cached localhost endpoints must not survive the final relay.
            // Retry on later monitor ticks if Windows still holds cache files.
            if self.rlapi_retained.load(Ordering::Acquire)
                && !self.socket_active.load(Ordering::Relaxed)
                && !hebnix_sdk::process::is_rocket_league_running()
                && crate::winutil::clear_rocket_league_web_cache().is_ok()
            {
                self.rlapi_retained.store(false, Ordering::Release);
            }
        }
    }
    pub fn owned_product_ids(&self) -> HashSet<i64> {
        self.owned_products
            .lock()
            .map(|owned| owned.clone())
            .unwrap_or_default()
    }

    pub fn set_username(&self, name: &str) {
        if let Ok(mut guard) = self.spoofed_name.lock() {
            let truncated: String = name.chars().take(MAX_NAME_LENGTH).collect();
            *guard = truncated;
        }
    }

    pub fn update_friends(&self, spoofs: HashMap<String, String>) {
        if let Ok(mut guard) = self.spoofed_friends.lock() {
            *guard = spoofs;
        }
    }

    pub fn update_ranks(&self, ranks: HashMap<i32, (i32, f64)>) {
        if let Ok(mut guard) = self.spoofed_ranks.lock() {
            *guard = ranks;
        }
    }

    fn start_skill_bridge(&self) -> Result<(), String> {
        let mut slot = self
            .skill_bridge
            .lock()
            .map_err(|_| "rank bridge lock poisoned")?;
        if slot.is_none() {
            *slot = Some(SkillBridge::start(
                Arc::clone(&self.spoofed_ranks),
                Arc::clone(&self.http_active),
                Arc::clone(&self.socket_active),
                Arc::clone(&self.game_present),
                self.tx.clone(),
                self.base_dir.join("rank_spoofer_frames.log"),
            )?);
        }
        Ok(())
    }

    fn stop_skill_bridge(&self) {
        if self.http_active.load(Ordering::Relaxed)
            || self.socket_active.load(Ordering::Relaxed)
            || self.relay_running()
        {
            return;
        }
        if let Ok(mut slot) = self.skill_bridge.lock() {
            if let Some(bridge) = slot.take() {
                bridge.stop();
            }
        }
    }

    pub fn http_running(&self) -> bool {
        self.http_active.load(Ordering::Relaxed)
            && self
                .reverse_proxy
                .lock()
                .map(|proxy| proxy.as_ref().is_some_and(SocketProxy::is_running))
                .unwrap_or(false)
    }

    pub fn socket_running(&self) -> bool {
        self.socket_active.load(Ordering::Relaxed)
            && self
                .reverse_proxy
                .lock()
                .map(|proxy| proxy.as_ref().is_some_and(SocketProxy::is_running))
                .unwrap_or(false)
    }

    pub fn start_http(&self) -> Result<(), String> {
        if self.http_running() {
            return self.reconcile_hosts();
        }
        self.http_active.store(true, Ordering::Relaxed);
        if let Err(error) = self
            .start_skill_bridge()
            .and_then(|_| self.ensure_reverse_proxy())
        {
            self.http_active.store(false, Ordering::Relaxed);
            self.stop_reverse_if_unused();
            self.stop_skill_bridge();
            self.maybe_stop_crl();
            return Err(error);
        }
        self.account_retained.store(true, Ordering::Release);
        Ok(())
    }

    pub fn stop_http(&self) {
        self.http_active.store(false, Ordering::Relaxed);
        if !self.socket_active.load(Ordering::Relaxed) {
            self.stop_skill_bridge();
        }
        self.stop_reverse_if_unused();
        self.maybe_stop_crl();
    }

    pub fn set_titles(&self, titles: Vec<TitleSpoofSettings>) {
        if let Ok(mut settings) = self.title_settings.lock() {
            let mut normalized: Vec<TitleSpoofSettings> = titles
                .into_iter()
                .filter_map(|mut title| {
                    title.text = title.text.trim().chars().take(64).collect();
                    (!title.text.is_empty()).then_some(title)
                })
                .collect();
            let mut seen = HashSet::new();
            normalized.retain(|title| seen.insert(title.target_id.clone()));
            settings.titles = normalized;
        }
    }

    pub fn set_title_enabled(&self, enabled: bool) {
        if let Ok(mut settings) = self.title_settings.lock() {
            settings.enabled = enabled;
        }
    }

    pub fn start_socket(&self) -> Result<(), String> {
        if self.socket_running() {
            return self.reconcile_hosts();
        }
        self.socket_active.store(true, Ordering::Relaxed);
        if let Err(error) = self
            .start_skill_bridge()
            .and_then(|_| self.ensure_reverse_proxy())
        {
            self.socket_active.store(false, Ordering::Relaxed);
            hebnix_sdk::rlapi::session::shared_game_session().set_websocket_enabled(false);
            self.stop_reverse_if_unused();
            self.stop_skill_bridge();
            self.maybe_stop_crl();
            return Err(error);
        }
        hebnix_sdk::rlapi::session::shared_game_session().set_websocket_enabled(true);
        self.rlapi_retained.store(true, Ordering::Release);
        Ok(())
    }

    pub fn item_status(&self) -> crate::item_cleanup::ItemStatus {
        self.item_cleanup.status()
    }
    pub fn begin_item_clear(&self) -> Result<(), String> {
        self.item_cleanup.begin_clear()
    }
    pub fn cancel_item_clear(&self) -> Result<(), String> {
        self.item_cleanup.cancel_clear()
    }
    pub fn finish_item_clear(&self) -> Result<bool, String> {
        self.item_cleanup.finish_clear()
    }

    pub fn stop_socket(&self) {
        self.socket_active.store(false, Ordering::Relaxed);
        hebnix_sdk::rlapi::session::shared_game_session().set_websocket_enabled(false);
        self.stop_reverse_if_unused();
        self.stop_skill_bridge();
        self.maybe_stop_crl();
    }

    fn ensure_reverse_proxy(&self) -> Result<(), String> {
        if !self.lifecycle_started.load(Ordering::Acquire) {
            return Err(
                "The proxy lifecycle monitor is unavailable; interception was left off.".into(),
            );
        }
        if self.certificate_blocked.load(Ordering::Acquire) {
            return Err(
                "Certificate removal disabled the proxy. Enable Spoofer or a replay option to set it up again."
                    .into(),
            );
        }
        if !ca::is_current_installed(&self.base_dir) {
            return Err(
                "The proxy certificate is not trusted. Enable Spoofer or a replay option to install it.".into(),
            );
        }
        let mut slot = self
            .reverse_proxy
            .lock()
            .map_err(|_| "reverse proxy lock poisoned")?;
        if slot.as_ref().is_some_and(|proxy| !proxy.is_running()) {
            if let Some(proxy) = slot.take() {
                proxy.stop();
            }
        }
        if slot.is_some() {
            drop(slot);
            crate::watchdog::ensure_protected(&self.base_dir)?;
            return self.reconcile_hosts();
        }
        let ca = Arc::new(ca::ensure(&self.base_dir)?);
        if !ca::is_current_installed(&self.base_dir) {
            return Err(
                "Certificate not installed. open Spoofer settings and click Install Certificate"
                    .into(),
            );
        }
        if !hosts::is_writable() {
            return Err("The hosts file needs administrator, restart Hebnix as admin".into());
        }
        let mut real_ips = HashMap::new();
        // The account proxy may start before Titles turn on. Pin both possible
        // redirect targets now so a later hosts-file change cannot make an
        // unpinned upstream request resolve back to our own listener.
        for host in [ACCOUNT_HOST, TITLE_HOST] {
            real_ips.insert(host.to_string(), dns::resolve_a(host)?);
        }
        real_ips.insert(
            "api.rlpp.psynet.gg".to_string(),
            dns::resolve_a("api.rlpp.psynet.gg")?,
        );
        let spoof_rules: Vec<Box<dyn Rule>> = vec![
            Box::new(NameRule::new(Arc::clone(&self.spoofed_name))),
            Box::new(crate::spoofer::rules::FriendsRule::new(
                Arc::clone(&self.spoofed_friends),
                Arc::clone(&self.discovered_friends),
            )),
            Box::new(OwnedProductsRule::new(
                Arc::clone(&self.owned_products),
                self.base_dir.join("owned_products.json"),
            )),
            Box::new(TitleRule::new(Arc::clone(&self.title_settings))),
            Box::new(crate::spoofer::rules::RankRule::new(Arc::clone(
                &self.spoofed_ranks,
            ))),
        ];
        let mut rules: Vec<Box<dyn Rule>> = spoof_rules
            .into_iter()
            .map(|rule| {
                Box::new(crate::spoofer::rules::EnabledRule {
                    inner: rule,
                    http: Arc::clone(&self.http_active),
                    socket: Arc::clone(&self.socket_active),
                    game: Arc::clone(&self.game_present),
                }) as Box<dyn Rule>
            })
            .collect();
        rules.push(Box::new(crate::spoofer::rules::RlApiRouteRule {
            retained: Arc::clone(&self.rlapi_retained),
            game: Arc::clone(&self.game_present),
        }));
        let rules = Arc::new(rules);
        self.ensure_crl(&ca)?;
        let proxy = match SocketProxy::start(ca, rules, self.tx.clone(), real_ips) {
            Ok(proxy) => proxy,
            Err(error) => {
                drop(slot);
                self.maybe_stop_crl();
                return Err(error);
            }
        };
        // Claim the listener before registering recovery ownership, so a
        // second Windows session cannot steal a live proxy's cleanup task.
        if let Err(error) = crate::watchdog::ensure_protected(&self.base_dir) {
            proxy.stop();
            drop(slot);
            self.maybe_stop_crl();
            return Err(error);
        }
        *slot = Some(proxy);
        drop(slot);
        if let Err(error) = self.reconcile_hosts() {
            self.http_active.store(false, Ordering::Release);
            self.socket_active.store(false, Ordering::Release);
            hebnix_sdk::rlapi::session::shared_game_session().set_websocket_enabled(false);
            self.stop_reverse_if_unused();
            self.maybe_stop_crl();
            return Err(error);
        }
        Ok(())
    }

    /// Reconcile hosts with the shared proxy's actual runtime state.
    pub fn reconcile_hosts(&self) -> Result<(), String> {
        let _guard = self
            .redirect_lock
            .lock()
            .map_err(|_| "redirect lock poisoned")?;
        if self.certificate_blocked.load(Ordering::Acquire) {
            hosts::clear()?;
            if self.http_active.load(Ordering::Acquire)
                || self.socket_active.load(Ordering::Acquire)
                || hebnix_sdk::rlapi::session::shared_game_session().enabled()
            {
                return Err("Certificate removal cancelled proxy startup.".into());
            }
            return Ok(());
        }
        if self.http_running() || self.socket_running() || self.relay_running() {
            let targets = self.redirect_hosts();
            if !targets.is_empty() {
                if !self
                    .reverse_proxy
                    .lock()
                    .map_err(|_| "proxy lock poisoned")?
                    .as_ref()
                    .is_some_and(SocketProxy::is_running)
                {
                    hosts::clear()?;
                    return Err("Proxy listener stopped; hosts redirects were removed.".into());
                }
                if !crate::watchdog::is_ready() {
                    hosts::clear()?;
                    return Err(
                        "The cleanup watchdog stopped; interception has been paused.".into(),
                    );
                }
                mark_session_dirty(&self.base_dir)?;
            }
            hosts::set_redirects(&targets)
        } else {
            hosts::clear()
        }
    }

    fn stop_reverse_if_unused(&self) {
        let Ok(_guard) = self.redirect_lock.lock() else {
            return;
        };
        if self.http_active.load(Ordering::Relaxed)
            || self.socket_active.load(Ordering::Relaxed)
            || self.relay_running()
        {
            return;
        }
        if let Err(error) = hosts::clear() {
            let _ = self.tx.send(AppMsg::Log(format!(
                "[Spoofer] Could not remove hosts redirects: {error}"
            )));
            // Leave a working listener available if Windows refused cleanup.
            return;
        }
        if let Ok(mut slot) = self.reverse_proxy.lock() {
            if let Some(proxy) = slot.take() {
                proxy.stop();
            }
        }
    }

    /// Disable mutations and API requests while preserving the game's relay.
    /// Saved spoof settings are left intact for the next Hebnix launch.
    pub fn disable_spoofing(&self) {
        // Preserve already-established/cached local game connections. New
        // DNS lookups go directly to Epic as soon as Hebnix's UI closes.
        self.closing.store(true, Ordering::Release);
        hebnix_sdk::rlapi::session::shared_game_session().set_enabled(false);
        self.stop_socket();
        self.stop_http();
        if let Err(error) = self.reconcile_hosts() {
            tracing::error!("Could not reconcile retained game relay: {error}");
        }
    }

    /// Stops only runtime interception. It deliberately does not modify saved
    /// spoof settings, so the user's enabled toggles survive the next launch.
    pub fn shutdown(&self) {
        self.configure(ProxyOptions::default());
        self.rlapi_retained.store(false, Ordering::Release);
        self.account_retained.store(false, Ordering::Release);
        hebnix_sdk::rlapi::session::shared_game_session().reset();
        self.stop_socket();
        self.stop_http();
        // Clear a redirect even if the socket failed to start or its state was lost.
        if let Err(error) = hosts::clear() {
            tracing::error!("Could not remove Hebnix hosts redirects on shutdown: {error}");
            let _ = self.tx.send(AppMsg::Log(format!(
                "[Spoofer] Could not remove hosts redirects on shutdown: {error}"
            )));
        }
        hosts::flush_dns();
    }

    pub fn block_for_certificate_removal(&self) {
        // Prevent an in-flight RLAPI/proxy start from publishing redirects
        // after the UI has shut down the runtime and removed trust.
        self.certificate_blocked.store(true, Ordering::Release);
        self.shutdown();
    }

    pub fn certificate_setup_ready(&self) {
        self.certificate_blocked.store(false, Ordering::Release);
    }

    /// Keep redirect lifetimes independent of UI repaints and StatsAPI probes.
    /// Enabled consumers start when the game runs and stop when it exits.
    pub fn start_lifecycle_monitor(self: &Arc<Self>) -> Result<(), String> {
        let manager = Arc::downgrade(self);
        std::thread::Builder::new()
            .name("proxy-lifecycle".into())
            .spawn(move || {
                let mut last_error = None;
                let mut trust_check = std::time::Instant::now();
                loop {
                    let Some(manager) = manager.upgrade() else {
                        break;
                    };
                    let game_present = hebnix_sdk::process::is_rocket_league_running();
                    manager.game_present.store(game_present, Ordering::Release);
                    if trust_check.elapsed() >= std::time::Duration::from_secs(5) {
                        trust_check = std::time::Instant::now();
                        if (manager.http_running()
                            || manager.socket_running()
                            || manager.relay_running())
                            && !ca::is_current_installed(&manager.base_dir)
                        {
                            manager.block_for_certificate_removal();
                            let _ = manager.tx.send(AppMsg::SpooferCertificateLost);
                        }
                    }
                    let result = manager
                        .sync_proxy_options()
                        .and_then(|_| manager.reconcile_hosts());
                    if let Err(error) = result {
                        if last_error.as_ref() != Some(&error) {
                            let _ = manager.tx.send(AppMsg::Log(format!("[Spoofer] {error}")));
                        }
                        last_error = Some(error);
                    } else {
                        last_error = None;
                    }
                    if !crate::watchdog::is_ready()
                        && (manager.http_running()
                            || manager.socket_running()
                            || manager.relay_running())
                    {
                        // Reconcile above removes redirects before replacing a
                        // killed watchdog. The next tick can safely resume.
                        let _ = crate::watchdog::spawn();
                    }
                    // A retained localhost config must not survive into the next
                    // game session. Keep the marker until cleanup succeeds.
                    if !game_present {
                        let _ = clean_session_cache(&manager.base_dir);
                    }
                    drop(manager);
                    std::thread::sleep(std::time::Duration::from_millis(250));
                }
            })
            .map_err(|error| format!("Could not start proxy lifecycle monitor: {error}"))?;
        self.lifecycle_started.store(true, Ordering::Release);
        Ok(())
    }
}

fn mark_session_dirty(base_dir: &Path) -> Result<(), String> {
    let marker = session_marker(base_dir);
    if !marker.exists() {
        std::fs::create_dir_all(marker.parent().unwrap()).map_err(|e| e.to_string())?;
        std::fs::write(marker, b"Rocket League may have cached localhost endpoints")
            .map_err(|e| format!("Could not save proxy recovery state: {e}"))?;
    }
    Ok(())
}

pub fn clean_session_cache(base_dir: &Path) -> std::io::Result<()> {
    let marker = session_marker(base_dir);
    if marker.exists() && !hebnix_sdk::process::is_rocket_league_running() {
        crate::winutil::clear_rocket_league_web_cache()?;
        std::fs::remove_file(marker)?;
    }
    Ok(())
}

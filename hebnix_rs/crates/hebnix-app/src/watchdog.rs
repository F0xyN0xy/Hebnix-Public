use std::sync::{Arc, Mutex, OnceLock};
use windows::Win32::Foundation::{CloseHandle, WAIT_TIMEOUT};

use crate::spoofer::SpooferManager;

// Keep the live listeners in this process after the UI closes. Recreating
// them in the cleanup child would drop Rocket League's active WebSocket.
static LIVE_SPOOFER: OnceLock<Arc<SpooferManager>> = OnceLock::new();

pub fn handoff_live_spoofer(spoofer: Arc<SpooferManager>) -> bool {
    if !hebnix_sdk::process::is_rocket_league_running()
        || !(spoofer.http_running() || spoofer.socket_running() || spoofer.relay_running())
    {
        return false;
    }
    spoofer.disable_spoofing();
    LIVE_SPOOFER.set(spoofer).is_ok() || LIVE_SPOOFER.get().is_some()
}

pub fn has_live_handoff() -> bool {
    LIVE_SPOOFER.get().is_some()
}

pub fn finish_live_handoff() {
    let Some(spoofer) = LIVE_SPOOFER.get() else {
        return;
    };
    tracing::info!("Hebnix UI closed; proxy watchdog remains active until Rocket League exits");
    while hebnix_sdk::process::is_rocket_league_running() {
        std::thread::sleep(std::time::Duration::from_secs(1));
    }
    spoofer.shutdown();
    for _ in 0..5 {
        if crate::winutil::clear_rocket_league_web_cache().is_ok() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_secs(1));
    }
    let _ = crate::winutil::clear_rocket_league_multihome();
    tracing::info!("Rocket League closed; proxy watchdog cleaned up");
}

use windows::Win32::System::Threading::{OpenProcess, PROCESS_SYNCHRONIZE, WaitForSingleObject};

pub const CLEANUP_WATCHDOG_ARG: &str = "--cleanup-watchdog";
static CHILD: Mutex<Option<std::process::Child>> = Mutex::new(None);
static RECOVERY_INSTALLED: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

pub fn is_ready() -> bool {
    CHILD.lock().ok().is_some_and(|mut child| {
        child
            .as_mut()
            .is_some_and(|child| matches!(child.try_wait(), Ok(None)))
    })
}

pub fn ensure_protected(base_dir: &std::path::Path) -> Result<(), String> {
    use std::sync::atomic::Ordering;
    if !RECOVERY_INSTALLED.load(Ordering::Acquire) {
        crate::recovery_tasks::install(base_dir)?;
        RECOVERY_INSTALLED.store(true, Ordering::Release);
    }
    if !spawn() {
        return Err(
            "Could not start the Epic connection cleanup watchdog. Proxy was left off.".into(),
        );
    }
    Ok(())
}

pub fn spawn() -> bool {
    use std::io::Read;
    use std::os::windows::process::CommandExt;

    let Ok(mut child_slot) = CHILD.lock() else {
        return false;
    };
    if child_slot
        .as_mut()
        .is_some_and(|child| matches!(child.try_wait(), Ok(None)))
    {
        return true;
    }
    *child_slot = None;

    if !crate::spoofer::is_admin() {
        return false;
    }
    let Ok(exe) = std::env::current_exe() else {
        return false;
    };
    let owner = std::process::id();
    if std::fs::create_dir_all(watchdog_owner_path().parent().unwrap()).is_err()
        || std::fs::write(watchdog_owner_path(), owner.to_string()).is_err()
    {
        return false;
    }
    let Ok(ready) = std::net::TcpListener::bind("127.0.0.1:0") else {
        return false;
    };
    let Ok(address) = ready.local_addr() else {
        return false;
    };
    if ready.set_nonblocking(true).is_err() {
        return false;
    }
    let token = rand::random::<u64>().to_string();
    // Escape a launcher job when Windows allows it; otherwise use a detached
    // child so cleanup can outlive the Hebnix window.
    let start = |flags| {
        std::process::Command::new(&exe)
            .args([
                CLEANUP_WATCHDOG_ARG,
                &owner.to_string(),
                &address.to_string(),
                &token,
            ])
            .creation_flags(flags)
            .spawn()
    };
    if let Ok(mut child) = start(0x09000208).or_else(|_| start(0x08000208)) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while std::time::Instant::now() < deadline {
            if let Ok((mut stream, _)) = ready.accept() {
                let _ = stream.set_read_timeout(Some(std::time::Duration::from_millis(200)));
                let mut received = vec![0; token.len()];
                if stream.read_exact(&mut received).is_ok()
                    && received == token.as_bytes()
                    && matches!(child.try_wait(), Ok(None))
                {
                    *child_slot = Some(child);
                    return true;
                }
            }
            if !matches!(child.try_wait(), Ok(None)) {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(25));
        }
        let _ = child.kill();
        let _ = child.wait();
    }
    let _ = std::fs::remove_file(watchdog_owner_path());
    false
}

pub fn parent_pid() -> Option<u32> {
    let mut args = std::env::args();
    while let Some(argument) = args.next() {
        if argument == CLEANUP_WATCHDOG_ARG {
            return args.next()?.parse().ok();
        }
    }
    None
}

pub fn run(parent_pid: u32) {
    if let Ok(parent) = unsafe { OpenProcess(PROCESS_SYNCHRONIZE, false, parent_pid) } {
        // Acknowledge only after acquiring a real process handle. This handle
        // remains valid even if Windows later reuses the parent's PID.
        let args: Vec<String> = std::env::args().collect();
        if let Some(index) = args.iter().position(|arg| arg == CLEANUP_WATCHDOG_ARG) {
            if let (Some(address), Some(token)) = (args.get(index + 2), args.get(index + 3)) {
                use std::io::Write;
                if let Ok(mut stream) = std::net::TcpStream::connect(address) {
                    let _ = stream.write_all(token.as_bytes());
                }
            }
        }
        unsafe {
            while WaitForSingleObject(parent, 250) == WAIT_TIMEOUT {
                // Also protect against a hidden/frozen UI leaving redirects
                // behind after the game exits.
                if !hebnix_sdk::process::is_rocket_league_running()
                    && !replacement_is_running(parent_pid)
                {
                    let _ = crate::spoofer::hosts::clear();
                    let _ = crate::spoofer::clean_session_cache(&crate::config::base_dir());
                }
            }
            let _ = CloseHandle(parent);
        }
    }
    if replacement_is_running(parent_pid) {
        return;
    }
    // Cleanup retries begin immediately, even while the game is still open.
    for _ in 0..20 {
        if replacement_is_running(parent_pid) {
            return;
        }
        if crate::spoofer::hosts::clear().is_ok() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(250));
    }
    while hebnix_sdk::process::is_rocket_league_running() {
        if replacement_is_running(parent_pid) {
            return;
        }
        std::thread::sleep(std::time::Duration::from_secs(1));
    }
    if replacement_is_running(parent_pid) {
        return;
    }
    // Game shutdown can briefly keep WebCache files locked. Retry cleanup
    // after the process has gone, including any redirect left by a failed exit.
    for _ in 0..5 {
        let hosts_clean = crate::spoofer::hosts::clear().is_ok();
        let cache_clean = crate::winutil::clear_rocket_league_web_cache().is_ok();
        if hosts_clean && cache_clean {
            break;
        }
        std::thread::sleep(std::time::Duration::from_secs(1));
    }
    for _ in 0..3 {
        let _ = crate::winutil::clear_rocket_league_multihome();
        std::thread::sleep(std::time::Duration::from_secs(1));
    }
    let _ = crate::multiplayer_lan::cleanup_system_state();
    crate::spoofer::hosts::flush_dns();
    let _ = crate::spoofer::clean_session_cache(&crate::config::base_dir());
    if watchdog_owner().is_some_and(|owner| owner == parent_pid) {
        let _ = std::fs::remove_file(watchdog_owner_path());
    }
}

fn watchdog_owner_path() -> std::path::PathBuf {
    crate::config::base_dir()
        .join("state")
        .join("watchdog_owner.pid")
}

fn watchdog_owner() -> Option<u32> {
    std::fs::read_to_string(watchdog_owner_path())
        .ok()?
        .trim()
        .parse()
        .ok()
}

fn replacement_is_running(parent_pid: u32) -> bool {
    let Some(owner) = watchdog_owner().filter(|owner| *owner != parent_pid) else {
        return false;
    };
    let Ok(process) = (unsafe { OpenProcess(PROCESS_SYNCHRONIZE, false, owner) }) else {
        return false;
    };
    let running = unsafe { WaitForSingleObject(process, 0) } == WAIT_TIMEOUT;
    unsafe {
        let _ = CloseHandle(process);
    }
    running
}

//! Cleans up what the old tailscaled-based Workshop multiplayer could leave
//! behind: the `HebnixTailscale` Windows service and its extracted files
//! (see remove_legacy_service), and a stale Tailscale NRPT (DNS policy) rule.
//!
//!
//! tailscaled's Windows DNS manager writes a fixed, hardcoded registry key
//! (the same one for any tailscaled build, ours or a real install) whenever
//! it's told to manage MagicDNS resolvers:
//!   HKLM\SOFTWARE\Policies\Microsoft\Windows NT\DNSClient\DnsPolicyConfig\{5abe529b-675b-4486-8459-25a634dacc23}
//! pointing every DNS query at 100.100.100.100. It normally removes this on
//! a clean shutdown, but a crash, a forced service stop, or Windows losing
//! power mid-session can leave it behind with nothing actually listening at
//! that address -- which silently breaks DNS for every app on the machine
//! (nslookup still works since it bypasses the Windows resolver, which is
//! why this is easy to miss).
//!
//! we ship with headscale's magic_dns turned off for Workshop multiplayer
//! (it never needed MagicDNS names, only raw tailnet IPs), so this rule
//! should never get written by our own sidecar going forward. This check is
//! a safety net for whatever's already stuck from before that, or from a
//! real Tailscale install that crashed the same way -- it only removes the
//! rule if nothing answers at 100.100.100.100, so a real, working MagicDNS
//! setup (ours or the user's own Tailscale) is left alone.

use std::net::{SocketAddr, TcpStream};
use std::os::windows::process::CommandExt;
use std::process::Command;
use std::time::Duration;

use winreg::RegKey;
use winreg::enums::HKEY_LOCAL_MACHINE;

const LEGACY_SERVICE: &str = "HebnixTailscale";
/// files older versions extracted to `%AppData%\Hebnix\multiplayer-lan`
const LEGACY_FILES: &[&str] = &["tailscaled.exe", "tailscale.exe", "wintun.dll"];
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

const NRPT_RULE_ID: &str = "{5abe529b-675b-4486-8459-25a634dacc23}";
const MAGIC_DNS_IP: &str = "100.100.100.100";
const PROBE_TIMEOUT: Duration = Duration::from_millis(300);

fn dns_policy_config_path() -> String {
    format!(r"SOFTWARE\Policies\Microsoft\Windows NT\DNSClient\DnsPolicyConfig\{NRPT_RULE_ID}")
}

/// true if something is actually listening at MagicDNS's address (port 53,
/// where the real DNS manager would be) -- a quick, best-effort check, not
/// a real DNS query, just "is anyone home".
fn magic_dns_responding() -> bool {
    let addr: SocketAddr = format!("{MAGIC_DNS_IP}:53").parse().expect("valid address");
    TcpStream::connect_timeout(&addr, PROBE_TIMEOUT).is_ok()
}

/// checks for the stale rule and removes it if nothing answers at
/// 100.100.100.100. call this once at Hebnix startup, before anything else
/// touches the network -- logs what it finds either way via `log`.
pub fn clean_stale_nrpt_rule(log: impl Fn(&str)) {
    let hklm = RegKey::predef(HKEY_LOCAL_MACHINE);
    let path = dns_policy_config_path();

    let rule_exists = hklm.open_subkey(&path).is_ok();
    if !rule_exists {
        return;
    }

    if magic_dns_responding() {
        // a real MagicDNS resolver (ours or the user's own Tailscale) is
        // actually up and serving this -- leave it alone.
        return;
    }

    log(&format!(
        "[Core] Found a stale Tailscale DNS policy rule ({NRPT_RULE_ID}) with nothing \
         listening at {MAGIC_DNS_IP} -- this breaks DNS system-wide if left in place. Removing it."
    ));

    match hklm.open_subkey_with_flags(
        r"SOFTWARE\Policies\Microsoft\Windows NT\DNSClient\DnsPolicyConfig",
        winreg::enums::KEY_SET_VALUE | winreg::enums::KEY_ENUMERATE_SUB_KEYS,
    ) {
        Ok(parent) => {
            if let Err(error) = parent.delete_subkey_all(NRPT_RULE_ID) {
                log(&format!(
                    "[Core] Could not remove the stale DNS policy rule: {error}. \
                     You can remove it yourself with: Remove-DnsClientNrptRule -DisplayName \"{NRPT_RULE_ID}\""
                ));
            } else {
                log(
                    "[Core] Removed the stale DNS policy rule. DNS resolution should be back to normal.",
                );
            }
        }
        Err(error) => {
            log(&format!(
                "[Core] Could not open the DNS policy registry key to remove the stale rule: {error}"
            ));
        }
    }
}

/// Stops and deletes the `HebnixTailscale` service older versions installed
/// (and deliberately left running between sessions), then deletes the
/// tailscaled files they extracted. Needs administrator rights; without
/// them this does nothing and is retried on a later elevated start.
pub fn remove_legacy_service(base_dir: &std::path::Path, log: impl Fn(&str)) {
    let installed = sc(&["query", LEGACY_SERVICE]).is_ok();
    if installed {
        let _ = sc(&["stop", LEGACY_SERVICE]);
        // the service needs a moment to stop before it can be deleted
        for _ in 0..50 {
            match sc(&["query", LEGACY_SERVICE]) {
                Ok(out) if out.contains("STOPPED") => break,
                Err(_) => break,
                _ => std::thread::sleep(Duration::from_millis(100)),
            }
        }
        match sc(&["delete", LEGACY_SERVICE]) {
            Ok(_) => log("[Core] Removed the old Workshop multiplayer network service (HebnixTailscale)."),
            Err(error) => {
                log(&format!(
                    "[Core] Could not remove the old HebnixTailscale service: {error}"
                ));
                return;
            }
        }
    }
    let dir = base_dir.join("multiplayer-lan");
    for file in LEGACY_FILES {
        let _ = std::fs::remove_file(dir.join(file));
    }
}

fn sc(args: &[&str]) -> Result<String, String> {
    let output = Command::new("sc.exe")
        .args(args)
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .map_err(|error| format!("failed to run sc.exe: {error}"))?;
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    if output.status.success() {
        Ok(stdout)
    } else {
        Err(stdout.trim().to_string())
    }
}

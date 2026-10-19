use std::os::windows::process::CommandExt;
use std::path::Path;
use std::process::Command;

const RULE_PREFIX: &str = "Hebnix Workshop LAN";
const CREATE_NO_WINDOW: u32 = 0x0800_0000;
const PROFILES: &str = "private,public";

/// The multiplayer sidecar (hebnix-tc.exe) talks to the other players over
/// UDP from whatever port it gets, and for a direct (not relayed) path they
/// have to be able to reach it, so the rules are scoped by executable rather
/// than port. Everything between it and Rocket League or Hebnix stays on
/// loopback, which Windows Firewall doesn't filter. Without administrator
/// rights the rules can't be added; Windows then asks the first time the
/// sidecar listens, and the relay path works either way.
pub fn ensure_sidecar_rule(executable: &Path) -> Result<(), String> {
    let inbound = format!("{RULE_PREFIX} v4 multiplayer sidecar inbound UDP");
    ensure_udp_rule(&inbound, executable, "in")?;
    let outbound = format!("{RULE_PREFIX} v4 multiplayer sidecar outbound UDP");
    ensure_udp_rule(&outbound, executable, "out")
}

/// removes every rule this and older versions added (theirs were for
/// tailscaled, the beacon relay, map sync and Rocket League itself)
pub fn remove_rules() -> Result<(), String> {
    let script = format!(
        "Get-NetFirewallRule -ErrorAction SilentlyContinue | Where-Object {{ $_.DisplayName -like '{}*' }} | Remove-NetFirewallRule -ErrorAction SilentlyContinue",
        RULE_PREFIX
    );
    let output = Command::new("powershell.exe")
        .args(["-NoProfile", "-NonInteractive", "-Command", &script])
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .map_err(|error| format!("could not remove Workshop firewall rules: {error}"))?;
    if output.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&output.stderr).trim().to_string())
    }
}

fn ensure_udp_rule(name: &str, executable: &Path, direction: &str) -> Result<(), String> {
    if rule_exists(name)? {
        return Ok(());
    }
    let program = executable
        .to_str()
        .ok_or_else(|| "executable path is not valid Unicode".to_string())?;
    run_netsh(&[
        "advfirewall".to_string(),
        "firewall".to_string(),
        "add".to_string(),
        "rule".to_string(),
        format!("name={name}"),
        format!("dir={direction}"),
        "action=allow".to_string(),
        "protocol=UDP".to_string(),
        format!("program={program}"),
        format!("profile={PROFILES}"),
        "enable=yes".to_string(),
    ])
}

fn rule_exists(name: &str) -> Result<bool, String> {
    let output = Command::new("netsh")
        .args([
            "advfirewall",
            "firewall",
            "show",
            "rule",
            &format!("name={name}"),
        ])
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .map_err(|error| format!("could not query Windows Firewall: {error}"))?;
    Ok(output.status.success()
        && !String::from_utf8_lossy(&output.stdout).contains("No rules match"))
}

fn run_netsh(args: &[String]) -> Result<(), String> {
    let output = Command::new("netsh")
        .args(args)
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .map_err(|error| format!("could not update Windows Firewall: {error}"))?;
    if output.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&output.stderr).trim().to_string())
    }
}

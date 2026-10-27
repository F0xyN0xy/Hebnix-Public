#[cfg(target_os = "windows")]
mod imp {
    use std::path::Path;

    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x08000000;

    pub fn ensure_rules(port: u16, enable: bool) -> std::io::Result<()> {
        let dir = std::env::current_exe()?
            .parent()
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| Path::new(".").to_path_buf());
        let ps1 = dir.join("firewall.ps1");
        if !ps1.exists() {
            return Ok(());
        }
        let action = if enable { "add" } else { "remove" };
        let mut cmd = std::process::Command::new("powershell");
        cmd.args([
            "-NoProfile",
            "-ExecutionPolicy",
            "Bypass",
            "-File",
            &ps1.to_string_lossy(),
            action,
            &port.to_string(),
        ])
        .creation_flags(CREATE_NO_WINDOW);
        let _ = cmd.status()?;
        Ok(())
    }

    pub fn add_rule(port: u16) -> std::io::Result<()> {
        ensure_rules(port, true)
    }

    pub fn remove_rule(port: u16) -> std::io::Result<()> {
        ensure_rules(port, false)
    }
}

#[cfg(target_os = "linux")]
mod imp {
    use std::path::Path;
    pub fn ensure_rules(_port: u16, _enable: bool) -> std::io::Result<()> { Ok(()) }
    pub fn add_rule(_port: u16) -> std::io::Result<()> { Ok(()) }
    pub fn remove_rule(_port: u16) -> std::io::Result<()> { Ok(()) }
}

pub use imp::*;

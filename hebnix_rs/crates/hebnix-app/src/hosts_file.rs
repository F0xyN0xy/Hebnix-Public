//! hosts file redirect, the game ignores the wininet proxy. needs admin, the
//! file is in System32.

use std::path::{Path, PathBuf};

// our lines end with this so we can find them later
pub const MARK: &str = "# hebnix spoofer";

// All Hebnix processes and the boot recovery task share this lock. Without
// it, a watchdog can overwrite a newer hosts snapshot during a restart.
pub(crate) struct HostsLock(windows::Win32::Foundation::HANDLE);

impl HostsLock {
    pub(crate) fn acquire() -> Result<Self, String> {
        use windows::Win32::Foundation::{CloseHandle, WAIT_ABANDONED, WAIT_OBJECT_0};
        use windows::Win32::System::Threading::{CreateMutexW, WaitForSingleObject};
        let name: Vec<u16> = "Global\\HebnixHostsCleanup\0".encode_utf16().collect();
        let handle = unsafe { CreateMutexW(None, false, windows::core::PCWSTR(name.as_ptr())) }
            .map_err(|e| format!("Could not acquire hosts cleanup lock: {e}"))?;
        let result = unsafe { WaitForSingleObject(handle, 5000) };
        if result != WAIT_OBJECT_0 && result != WAIT_ABANDONED {
            unsafe {
                let _ = CloseHandle(handle);
            }
            return Err("Hosts cleanup lock timed out".into());
        }
        Ok(Self(handle))
    }
}

impl Drop for HostsLock {
    fn drop(&mut self) {
        unsafe {
            let _ = windows::Win32::System::Threading::ReleaseMutex(self.0);
            let _ = windows::Win32::Foundation::CloseHandle(self.0);
        }
    }
}

pub fn hosts_path() -> PathBuf {
    let root = std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".into());
    Path::new(&root).join(r"System32\drivers\etc\hosts")
}

pub fn is_writable() -> bool {
    std::fs::OpenOptions::new()
        .append(true)
        .open(hosts_path())
        .is_ok()
}

fn line_for(host: &str) -> String {
    format!("127.0.0.1 {host} {MARK}")
}

/// Ensure exactly one Hebnix redirect per requested host. Leave the file
/// untouched when it already has the desired entries.
pub fn set_redirects(hosts: &[&str]) -> Result<(), String> {
    let _guard = HostsLock::acquire()?;
    let path = hosts_path();
    let content =
        std::fs::read_to_string(&path).map_err(|e| format!("cant read the hosts file: {e}"))?;
    if redirects_match(&content, hosts) {
        return Ok(());
    }

    let out = with_redirects(&content, hosts);
    write(&path, &out)?;
    let verified =
        std::fs::read_to_string(&path).map_err(|e| format!("cant verify hosts redirects: {e}"))?;
    if !redirects_match(&verified, hosts) {
        return Err("Hebnix hosts redirects did not match the requested hosts".into());
    }
    flush_dns();
    Ok(())
}

fn redirects_match(content: &str, hosts: &[&str]) -> bool {
    let ours: Vec<&str> = content.lines().filter(|line| is_ours(line)).collect();
    ours.len() == hosts.len()
        && hosts.iter().all(|host| {
            ours.iter()
                .filter(|line| line.trim().eq_ignore_ascii_case(&line_for(host)))
                .count()
                == 1
        })
}

pub fn has_redirects() -> bool {
    std::fs::read_to_string(hosts_path())
        .map(|content| content.lines().any(is_ours))
        .unwrap_or(false)
}

/// drops our lines, whatever host they were for
pub fn clear() -> Result<(), String> {
    let _guard = HostsLock::acquire()?;
    let path = hosts_path();
    let content =
        std::fs::read_to_string(&path).map_err(|e| format!("cant read the hosts file: {e}"))?;

    if !content.lines().any(is_ours) {
        return Ok(());
    }

    let out = without_redirects(&content);
    write(&path, &out)?;
    let verified =
        std::fs::read_to_string(&path).map_err(|e| format!("cant verify hosts cleanup: {e}"))?;
    if verified.lines().any(is_ours) {
        return Err("Hebnix redirects remain in the hosts file after cleanup".into());
    }
    flush_dns();
    Ok(())
}

fn is_ours(line: &str) -> bool {
    let Some((mapping, comment)) = line.rsplit_once('#') else {
        return false;
    };
    let mut fields = mapping.split_whitespace();
    if fields.next() != Some("127.0.0.1") || fields.next().is_none() {
        return false;
    }
    matches!(
        comment.trim().to_ascii_lowercase().as_str(),
        "hebnix spoofer" | "hebnix"
    )
}

fn without_redirects(content: &str) -> String {
    content
        .split_inclusive('\n')
        .filter(|line| !is_ours(line.trim_end_matches(['\r', '\n'])))
        .collect()
}

fn with_redirects(content: &str, hosts: &[&str]) -> String {
    let mut out = without_redirects(content);
    let newline = if content.contains("\r\n") {
        "\r\n"
    } else {
        "\n"
    };
    if !out.is_empty() && !out.ends_with('\n') {
        out.push_str(newline);
    }
    for host in hosts {
        out.push_str(&line_for(host));
        out.push_str(newline);
    }
    out
}

fn write(path: &Path, content: &str) -> Result<(), String> {
    use std::io::Write;
    use std::os::windows::ffi::OsStrExt;
    // Publish a complete file instead of truncating hosts in place. A killed
    // process must not erase the user's unrelated entries halfway through.
    let temporary = path.with_file_name(format!(
        ".hebnix-hosts-{}-{}.tmp",
        std::process::id(),
        rand::random::<u64>()
    ));
    let result = (|| -> std::io::Result<()> {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)?;
        file.write_all(content.as_bytes())?;
        file.sync_all()?;
        drop(file);
        use windows::Win32::Storage::FileSystem::{REPLACE_FILE_FLAGS, ReplaceFileW};
        let target: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
        let source: Vec<u16> = temporary.as_os_str().encode_wide().chain(Some(0)).collect();
        unsafe {
            ReplaceFileW(
                windows::core::PCWSTR(target.as_ptr()),
                windows::core::PCWSTR(source.as_ptr()),
                None,
                REPLACE_FILE_FLAGS(0),
                None,
                None,
            )
        }
        .map_err(std::io::Error::other)
    })();
    let _ = std::fs::remove_file(&temporary);
    result.map_err(|e| {
        if e.kind() == std::io::ErrorKind::PermissionDenied {
            "the hosts file needs administrator, restart hebnix as admin".to_string()
        } else {
            format!("cant write the hosts file: {e}")
        }
    })
}

pub fn flush_dns() {
    use std::os::windows::process::CommandExt;
    let _ = std::process::Command::new("ipconfig")
        .arg("/flushdns")
        .creation_flags(0x08000000)
        .output();
}

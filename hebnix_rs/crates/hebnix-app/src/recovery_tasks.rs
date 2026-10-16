//! Cleanup-only recovery survives power loss, updates, and a moved Hebnix exe.
//! SYSTEM runs only a fixed script through Windows' own PowerShell; it never
//! executes the user's Hebnix binary or a script from a writable directory.

use base64::Engine;
use std::path::Path;

const BOOT_TASK: &str = "Hebnix Epic Connection Recovery";
const OWNER_KEY: &str = r"SOFTWARE\Hebnix\ProxyRecovery";
const OWNER_GUARD: &str = r#"
$state = Get-ItemProperty -LiteralPath 'HKLM:\SOFTWARE\Hebnix\ProxyRecovery' -ErrorAction SilentlyContinue
if ($state.OwnerPid -and $state.OwnerStarted) {
    $owner = Get-Process -Id $state.OwnerPid -ErrorAction SilentlyContinue
    if ($owner -and $owner.StartTime.ToFileTimeUtc() -eq $state.OwnerStarted) { exit 0 }
}
"#;

pub fn session_marker(base_dir: &Path) -> std::path::PathBuf {
    base_dir.join("state").join("proxy_session.dirty")
}
const HOSTS_CLEANUP: &str = r#"
$ErrorActionPreference = 'Stop'
$mutex = New-Object System.Threading.Mutex($false, 'Global\HebnixHostsCleanup')
try {
    try { $locked = $mutex.WaitOne(30000) } catch [System.Threading.AbandonedMutexException] { $locked = $true }
    if (-not $locked) { throw 'Hosts cleanup lock timed out' }
    # CHECK_LIVE_OWNER
    $path = Join-Path $env:SystemRoot 'System32\drivers\etc\hosts'
    $text = [IO.File]::ReadAllText($path)
    $pattern = '(?im)^[\t ]*127\.0\.0\.1[\t ]+[^#\r\n]+#[\t ]*hebnix(?: spoofer)?[\t ]*(?:\r?\n|$)'
    $clean = [regex]::Replace($text, $pattern, '')
    if ($clean -cne $text) {
        $temporary = Join-Path ([IO.Path]::GetDirectoryName($path)) ('.hebnix-hosts-' + [Guid]::NewGuid().ToString() + '.tmp')
        try {
            [IO.File]::WriteAllText($temporary, $clean, (New-Object Text.UTF8Encoding($false)))
            [IO.File]::Replace($temporary, $path, [NullString]::Value)
        } finally {
            if (Test-Path -LiteralPath $temporary) { Remove-Item -LiteralPath $temporary -Force }
        }
        & "$env:SystemRoot\System32\ipconfig.exe" /flushdns | Out-Null
    }
} finally {
    if ($locked) { $mutex.ReleaseMutex() }
    $mutex.Dispose()
}
"#;

fn xml_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

fn encoded_command(script: &str) -> String {
    let bytes: Vec<u8> = script.encode_utf16().flat_map(u16::to_le_bytes).collect();
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

fn task_xml(trigger: &str, principal: &str, script: &str) -> String {
    let system_root = std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".into());
    let powershell =
        Path::new(&system_root).join(r"System32\WindowsPowerShell\v1.0\powershell.exe");
    format!(
        r#"<?xml version="1.0"?>
<Task version="1.2" xmlns="http://schemas.microsoft.com/windows/2004/02/mit/task">
 <RegistrationInfo><Description>Restore Epic connectivity after an interrupted Hebnix proxy session. Cleanup only.</Description></RegistrationInfo>
 <Triggers>{trigger}</Triggers>
 <Principals><Principal id="Recovery">{principal}<RunLevel>HighestAvailable</RunLevel></Principal></Principals>
 <Settings><MultipleInstancesPolicy>IgnoreNew</MultipleInstancesPolicy><DisallowStartIfOnBatteries>false</DisallowStartIfOnBatteries><StopIfGoingOnBatteries>false</StopIfGoingOnBatteries><StartWhenAvailable>true</StartWhenAvailable><ExecutionTimeLimit>PT2M</ExecutionTimeLimit></Settings>
 <Actions Context="Recovery"><Exec><Command>{}</Command><Arguments>-NoProfile -NonInteractive -WindowStyle Hidden -EncodedCommand {}</Arguments></Exec></Actions>
</Task>"#,
        xml_escape(&powershell.to_string_lossy()),
        encoded_command(script)
    )
}

fn cache_cleanup(base_dir: &Path) -> String {
    let marker = session_marker(base_dir)
        .to_string_lossy()
        .replace('\'', "''");
    format!(
        r#"
$ErrorActionPreference = 'Stop'
$marker = '{marker}'
if (-not (Test-Path -LiteralPath $marker)) {{ exit 0 }}
if (Get-Process -Name RocketLeague -ErrorAction SilentlyContinue) {{ exit 0 }}
$documents = [Environment]::GetFolderPath('MyDocuments')
if (-not [IO.Path]::IsPathRooted($documents)) {{ throw 'Documents folder unavailable' }}
$cache = Join-Path $documents 'My Games\Rocket League\TAGame\Cache\WebCache'
if (Test-Path -LiteralPath $cache) {{ Remove-Item -LiteralPath $cache -Recurse -Force }}
Remove-Item -LiteralPath $marker -Force
"#
    )
}

fn register(name: &str, xml: &str) -> Result<(), String> {
    use std::os::windows::process::CommandExt;
    let path = std::env::temp_dir().join(format!(
        "hebnix-recovery-{}-{}.xml",
        std::process::id(),
        rand::random::<u64>()
    ));
    std::fs::write(&path, xml)
        .map_err(|e| format!("Could not prepare connection recovery: {e}"))?;
    let result = std::process::Command::new("schtasks.exe")
        .args(["/Create", "/TN", name, "/XML"])
        .arg(&path)
        .arg("/F")
        .creation_flags(0x08000000)
        .output();
    let _ = std::fs::remove_file(&path);
    let output = result.map_err(|e| format!("Could not register connection recovery: {e}"))?;
    if !output.status.success() {
        return Err(format!(
            "Windows could not enable connection recovery: {} {}",
            String::from_utf8_lossy(&output.stdout).trim(),
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(())
}

pub fn install(base_dir: &Path) -> Result<(), String> {
    use windows::Win32::Foundation::FILETIME;
    use windows::Win32::System::Threading::{GetCurrentProcess, GetProcessTimes};
    let mut created = FILETIME::default();
    let mut exited = FILETIME::default();
    let mut kernel = FILETIME::default();
    let mut user_time = FILETIME::default();
    unsafe {
        GetProcessTimes(
            GetCurrentProcess(),
            &mut created,
            &mut exited,
            &mut kernel,
            &mut user_time,
        )
    }
    .map_err(|e| format!("Could not identify the proxy owner: {e}"))?;
    let started = (u64::from(created.dwHighDateTime) << 32) | u64::from(created.dwLowDateTime);
    // Publish numeric identity under the hosts lock. A recovery task launched
    // by a previous Hebnix must see the replacement owner after acquiring the
    // lock. Nothing in this registry key is ever executed by SYSTEM.
    {
        let _guard = crate::hosts_file::HostsLock::acquire()?;
        let (key, _) = winreg::RegKey::predef(winreg::enums::HKEY_LOCAL_MACHINE)
            .create_subkey(OWNER_KEY)
            .map_err(|e| format!("Could not publish proxy recovery owner: {e}"))?;
        key.set_value("OwnerPid", &std::process::id())
            .map_err(|e| e.to_string())?;
        key.set_value("OwnerStarted", &started)
            .map_err(|e| e.to_string())?;
    }
    register(
        BOOT_TASK,
        &task_xml(
            "<BootTrigger><Enabled>true</Enabled></BootTrigger>",
            "<UserId>S-1-5-18</UserId>",
            &HOSTS_CLEANUP.replace("# CHECK_LIVE_OWNER", OWNER_GUARD),
        ),
    )?;
    let user = format!(
        "{}\\{}",
        std::env::var("USERDOMAIN").map_err(|e| e.to_string())?,
        std::env::var("USERNAME").map_err(|e| e.to_string())?
    );
    let escaped = xml_escape(&user);
    let trigger = format!(
        "<LogonTrigger><Enabled>true</Enabled><UserId>{escaped}</UserId></LogonTrigger>"
    );
    // WindowStyle Hidden takes effect inside PowerShell; running in the
    // user's interactive desktop can still flash a console during startup.
    // S4U uses a separate non-interactive desktop without storing a password.
    let principal = cache_principal(&escaped);
    // Separate per-user cache recovery from system-wide hosts recovery.
    let task_user = user.replace('\\', "-");
    register(
        &format!("Hebnix Epic Cache Recovery {task_user}"),
        &task_xml(
            &trigger,
            &principal,
            &format!("{OWNER_GUARD}{}", cache_cleanup(base_dir)),
        ),
    )
}

fn cache_principal(escaped_user: &str) -> String {
    format!("<UserId>{escaped_user}</UserId><LogonType>S4U</LogonType>")
}

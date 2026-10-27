//! read-only process memory scanner (windows), port of hebnix.eos._memory
//!
//! finds the eg1~eyJ... bearer token inside EpicGamesLauncher.exe without
//! injection. toolhelp snapshot + VirtualQueryEx/ReadProcessMemory walk over
//! committed readable pages.

#[cfg(target_os = "windows")]
use windows::Win32::Foundation::{CloseHandle, FILETIME, HANDLE};
#[cfg(target_os = "windows")]
use windows::Win32::System::Diagnostics::Debug::ReadProcessMemory;
#[cfg(target_os = "windows")]
use windows::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW, TH32CS_SNAPPROCESS,
};
#[cfg(target_os = "windows")]
use windows::Win32::System::Memory::{MEM_COMMIT, MEMORY_BASIC_INFORMATION, VirtualQueryEx};
#[cfg(target_os = "windows")]
use windows::Win32::System::Threading::{
    GetProcessTimes, OpenProcess, PROCESS_QUERY_INFORMATION, PROCESS_QUERY_LIMITED_INFORMATION,
    PROCESS_VM_READ,
};

/// Process metadata only; no access to game memory is requested.
pub fn started_at(pid: u32) -> Option<std::time::SystemTime> {
    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
        let mut created = FILETIME::default();
        let mut exited = FILETIME::default();
        let mut kernel = FILETIME::default();
        let mut user = FILETIME::default();
        let result = GetProcessTimes(handle, &mut created, &mut exited, &mut kernel, &mut user);
        let _ = CloseHandle(handle);
        result.ok()?;
        let ticks = ((created.dwHighDateTime as u64) << 32) | created.dwLowDateTime as u64;
        let unix = ticks.checked_sub(116_444_736_000_000_000)?;
        std::time::UNIX_EPOCH.checked_add(std::time::Duration::from_nanos(unix.checked_mul(100)?))
    }
}

// readable page protections: READONLY|READWRITE|EXECUTE_READ|EXECUTE_READWRITE
// = 0x02|0x04|0x20|0x40 = 0x66
const READABLE_MASK: u32 = 0x66;

/// pid of the first process whose exe name contains `name` (case-insensitive).
pub fn find_process(name: &str) -> Option<u32> {
    let needle = name.to_ascii_lowercase();
    unsafe {
        let snap = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0).ok()?;
        let mut entry = PROCESSENTRY32W {
            dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32,
            ..Default::default()
        };
        let mut result = None;
        if Process32FirstW(snap, &mut entry).is_ok() {
            loop {
                let exe = String::from_utf16_lossy(&entry.szExeFile);
                let exe = exe.trim_end_matches('\0').to_ascii_lowercase();
                if exe == needle {
                    result = Some(entry.th32ProcessID);
                    break;
                }
                if Process32NextW(snap, &mut entry).is_err() {
                    break;
                }
            }
        }
        let _ = CloseHandle(snap);
        result
    }
}

/// scan pid's committed readable memory for `needle`, return the surrounding
/// tokens (deduped, len > 50). same token-boundary extraction as the py scanner.
pub fn scan_memory(pid: u32, needle: &[u8]) -> Vec<String> {
    let mut tokens: Vec<String> = Vec::new();
    unsafe {
        let Ok(handle) = OpenProcess(PROCESS_VM_READ | PROCESS_QUERY_INFORMATION, false, pid)
        else {
            return tokens;
        };

        let mut buf = vec![0u8; 65536];
        let mut mbi = MEMORY_BASIC_INFORMATION::default();
        let mut cur: usize = 0;

        while VirtualQueryEx(
            handle,
            Some(cur as *const _),
            &mut mbi,
            std::mem::size_of::<MEMORY_BASIC_INFORMATION>(),
        ) != 0
        {
            let base = mbi.BaseAddress as usize;
            let size = mbi.RegionSize;
            if size == 0 {
                break;
            }

            let committed = mbi.State == MEM_COMMIT;
            let readable = (mbi.Protect.0 & READABLE_MASK) != 0 && (mbi.Protect.0 & 0x100) == 0;
            if committed && readable {
                scan_region(handle, base, size, &mut buf, needle, &mut tokens);
            }

            cur = base.saturating_add(size);
        }

        let _ = CloseHandle(handle);
    }
    tokens
}

/// read one region in 64 KiB chunks, extract matching tokens.
unsafe fn scan_region(
    handle: HANDLE,
    base: usize,
    size: usize,
    buf: &mut [u8],
    needle: &[u8],
    tokens: &mut Vec<String>,
) {
    let mut scanned = 0usize;
    while scanned < size {
        let chunk = (size - scanned).min(buf.len());
        let mut read: usize = 0;
        let ok = unsafe {
            ReadProcessMemory(
                handle,
                (base + scanned) as *const _,
                buf.as_mut_ptr() as *mut _,
                chunk,
                Some(&mut read),
            )
        };
        if ok.is_ok() && read > 0 {
            extract_tokens(&buf[..read], needle, tokens);
        }
        // JWTs can straddle a read boundary. Re-read a small overlap.
        if chunk <= 4096 {
            break;
        }
        scanned += chunk - 4096;
    }
}

/// chars that make up a bearer/jwt token: base64url alphabet + the eg1~ marker's
/// ~, jwt separators ., and base64 +/=. only walking these bytes stops the scan
/// at surrounding binary so tokens come out clean ascii (no junk prefix, and no
/// panic on non-utf8 slices, which the py errors="ignore" decode quietly hid).
fn is_token_char(b: u8) -> bool {
    b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~' | b'+' | b'/' | b'=')
}

/// pull out token-char runs containing `needle`, dedupe into `tokens` (len > 50).
fn extract_tokens(data: &[u8], needle: &[u8], tokens: &mut Vec<String>) {
    if needle.is_empty() {
        return;
    }
    let mut off = 0usize;
    while off < data.len() {
        let Some(rel) = find_sub(&data[off..], needle) else {
            break;
        };
        let hit = off + rel;

        // widen to the full token-char run around the hit
        let start = hit;
        let mut end = hit;
        while end < data.len() && is_token_char(data[end]) {
            end += 1;
        }

        // The slice is guaranteed ASCII, so this never allocates a lossy char.
        let raw = String::from_utf8_lossy(&data[start..end]).into_owned();
        if raw.len() > 50 && !tokens.contains(&raw) {
            tokens.push(raw);
        }
        off = end.max(hit + 1);
    }
}

/// First index of `needle` within `haystack`, or `None`.
fn find_sub(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || haystack.len() < needle.len() {
        return None;
    }
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

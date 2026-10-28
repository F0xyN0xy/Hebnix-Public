//! Reading the Epic launcher's memory for a session token goes through Win32
//! process APIs and isn't ported. These report nothing found, so replay upload
//! sign-in falls back to its own error messages on Linux.

pub fn started_at(_pid: u32) -> Option<std::time::SystemTime> {
    None
}

pub fn find_process(_name: &str) -> Option<u32> {
    None
}

pub fn scan_memory(_pid: u32, _needle: &[u8]) -> Vec<String> {
    Vec::new()
}

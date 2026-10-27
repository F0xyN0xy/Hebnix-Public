#[cfg(target_os = "windows")]
#[path = "sys/windows.rs"]
mod imp;

#[cfg(target_os = "linux")]
#[path = "sys/linux.rs"]
mod imp;

pub use imp::*;

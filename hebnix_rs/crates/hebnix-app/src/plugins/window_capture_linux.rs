//! Per-plugin capture of user-selected top-level windows.
//!
//! linux-port: not implemented. Wayland gives no way to read another
//! window's pixels without the screencast portal's consent dialog, so
//! `windows()` lists nothing and `start()` always fails. The API matches the
//! Windows build so plugins that use it still load.

use std::sync::Arc;

#[derive(Clone, Debug)]
pub struct WindowInfo {
    pub id: String,
    pub title: String,
    pub process: String,
}

/// A top-down, tightly packed, opaque BGRA frame.
#[derive(Debug)]
#[allow(dead_code)]
pub struct CapturedFrame {
    pub serial: u64,
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u8>,
}

/// Owned by one HostCtx.
#[derive(Default)]
pub struct WindowCaptureRegistry;

impl WindowCaptureRegistry {
    pub fn windows(&self) -> Vec<WindowInfo> {
        Vec::new()
    }

    pub fn start(
        &self,
        _window_id: &str,
        _fps: u32,
        _cursor: bool,
        _plugin_name: &str,
    ) -> Option<u64> {
        None
    }

    pub fn frame(&self, _handle: u64) -> Option<Arc<CapturedFrame>> {
        None
    }

    pub fn stop(&self, _handle: u64) -> bool {
        false
    }
}

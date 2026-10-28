//! Window identity and taskbar handling only exist on Windows. Linux has no
//! DirectComposition swapchain to work around and no taskbar button to drive.

use raw_window_handle::HasWindowHandle;

pub fn safe_rendering() -> bool {
    false
}

pub fn init(_window: &impl HasWindowHandle) {}

pub fn wake_ui(ctx: &eframe::egui::Context) {
    ctx.request_repaint();
}

//! WM_DPICHANGED drag-jump workaround -- windows-only bug (winit #4041).
//!
//! On Wayland, scale factor is compositor-driven (wp_fractional_scale /
//! output scale) and delivered to winit as a normal scale-factor-changed
//! event with no analogous mid-drag rect-clobbering issue, so there's
//! nothing to patch here. Kept as no-ops (same names/signatures) so call
//! sites don't need `#[cfg]` guards.

/// no-op on Linux; kept so callers compile unchanged.
pub fn install<H>(_hwnd: H) {}

/// no-op on Linux.
pub fn install_on_all_windows() {}

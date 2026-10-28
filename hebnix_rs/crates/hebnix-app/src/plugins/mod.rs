//! lua plugin system.
//!
//! plugins live in plugins/ as a dir <slug>/ with a plugin.toml and the lua
//! file it names. a plugin returns a table of callbacks (on_load, on_unload,
//! on_game_event, on_settings(ui), on_window(ui), on_overlay(draw,w,h)).
//! see examples/plugins/.

pub mod cvar;
pub mod gamepad_icons;
pub mod lua_api;
pub mod manager;
pub mod manifest;
#[cfg(target_os = "windows")]
pub mod media_session;
#[cfg(target_os = "linux")]
#[path = "media_session_linux.rs"]
pub mod media_session;
mod rlapi;
pub mod store;
#[cfg(target_os = "windows")]
pub mod window_capture;
#[cfg(target_os = "linux")]
#[path = "window_capture_linux.rs"]
pub mod window_capture;

pub use manager::PluginManager;

//! global show/hide hotkey.
//!
//! linux-port: originally used the `global-hotkey` crate, whose Linux
//! backend is X11-only (XGrabKey via XWayland). Under Hyprland, XWayland's
//! global key grab never fires -- confirmed live: neither `ydotool` nor
//! `xdotool` key presses reached it. `hebnix_sdk::input::is_key_pressed`
//! (evdev, reads /dev/input directly) is already proven working for plugin
//! key binds, so the toggle hotkey now polls the same way.

use std::sync::{Arc, Mutex};

/// owns the currently bound key name, shared with the polling thread.
pub struct ToggleHotkey {
    current_name: Arc<Mutex<String>>,
}

impl ToggleHotkey {
    pub fn new() -> Option<Self> {
        Some(Self {
            current_name: Arc::new(Mutex::new(String::new())),
        })
    }

    /// (re)bind the toggle hotkey. false if the name doesn't map to a known
    /// key, in which case the old binding stays so the menu isn't lost.
    pub fn rebind(&mut self, key_name: &str) -> bool {
        if hebnix_sdk::input::keyboard::name_to_code(key_name).is_none() {
            tracing::warn!("cannot map '{key_name}' to a hotkey");
            return false;
        }
        *self.current_name.lock().unwrap() = key_name.to_string();
        tracing::info!("global hotkey bound to '{key_name}'");
        true
    }

    /// shared handle for the polling thread to read the live key name from.
    pub fn shared_key(&self) -> Arc<Mutex<String>> {
        Arc::clone(&self.current_name)
    }
}

/// spawn the polling thread. calls `on_press` on each rising edge of the
/// currently-bound key (re-read from `key_name` every tick, so rebinding
/// takes effect immediately without restarting the thread).
pub fn spawn_poller(key_name: Arc<Mutex<String>>, on_press: impl Fn() + Send + 'static) {
    std::thread::Builder::new()
        .name("hotkey-poller".into())
        .spawn(move || {
            let mut was_pressed = false;
            loop {
                std::thread::sleep(std::time::Duration::from_millis(35));
                let name = key_name.lock().unwrap().clone();
                if name.is_empty() {
                    was_pressed = false;
                    continue;
                }
                let pressed = hebnix_sdk::input::is_key_pressed(&name);
                if pressed && !was_pressed {
                    on_press();
                }
                was_pressed = pressed;
            }
        })
        .ok();
}

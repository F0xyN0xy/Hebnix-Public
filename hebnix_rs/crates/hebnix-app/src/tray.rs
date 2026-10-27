//! tray icon with open/close menu.

use std::path::Path;

use tray_icon::menu::{Menu, MenuItem};

use crate::i18n::t;
use crate::messages::AppMsg;
use tray_icon::{
    Icon, MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent, TrayIconId,
};

pub struct Tray {
    // keep this alive, dropping it removes the tray icon
    pub _icon: TrayIcon,
    visibility_item: MenuItem,
    quit_item: MenuItem,
    open_id: tray_icon::menu::MenuId,
    quit_id: tray_icon::menu::MenuId,
}

// logo baked into the exe, hebnix.ico next to the exe overrides it
pub const EMBEDDED_ICON: &[u8] = include_bytes!("../assets/hebnix.ico");

fn decode_icon(bytes: &[u8]) -> Option<Icon> {
    let img = image::load_from_memory(bytes).ok()?;
    let rgba = img.to_rgba8();
    let (w, h) = rgba.dimensions();
    Icon::from_rgba(rgba.into_raw(), w, h).ok()
}

fn load_icon(base_dir: &Path) -> Icon {
    if let Ok(bytes) = std::fs::read(base_dir.join("hebnix.ico")) {
        if let Some(icon) = decode_icon(&bytes) {
            return icon;
        }
    }
    if let Some(icon) = decode_icon(EMBEDDED_ICON) {
        return icon;
    }
    let size = 32u32;
    let mut rgba = Vec::with_capacity((size * size * 4) as usize);
    for _ in 0..size * size {
        rgba.extend_from_slice(&[30, 30, 30, 255]);
    }
    Icon::from_rgba(rgba, size, size).expect("fallback tray icon")
}

impl Tray {
    pub fn new(base_dir: &Path, tooltip: &str, hidden: bool) -> Option<Self> {
        // tray-icon's Linux backend goes through libayatana-appindicator/GTK,
        // which needs gtk::init() called once before any tray/menu object is
        // built (Windows' native backend has no such requirement).
        #[cfg(target_os = "linux")]
        {
            static GTK_INIT: std::sync::Once = std::sync::Once::new();
            GTK_INIT.call_once(|| {
                if let Err(e) = gtk::init() {
                    tracing::warn!("gtk::init() failed, tray icon will be unavailable: {e}");
                }
            });
        }

        let menu = Menu::new();
        let open_item = MenuItem::new(Self::visibility_text(hidden), true, None);
        let quit_item = MenuItem::new(t("tray-close"), true, None);
        menu.append(&open_item).ok()?;
        menu.append(&quit_item).ok()?;

        let icon = TrayIconBuilder::new()
            .with_menu(Box::new(menu))
            .with_menu_on_left_click(false)
            .with_tooltip(tooltip)
            .with_icon(load_icon(base_dir))
            .build()
            .ok()?;

        Some(Self {
            _icon: icon,
            visibility_item: open_item.clone(),
            quit_item: quit_item.clone(),
            open_id: open_item.id().clone(),
            quit_id: quit_item.id().clone(),
        })
    }

    pub fn listen(&self, tx: crossbeam_channel::Sender<AppMsg>, ctx: eframe::egui::Context) {
        let icon_id = self._icon.id().clone();
        let open_id = self.open_id.clone();
        let quit_id = self.quit_id.clone();
        std::thread::Builder::new()
            .name("tray-forwarder".into())
            .spawn(move || {
                let menu_events = tray_icon::menu::MenuEvent::receiver();
                let icon_events = TrayIconEvent::receiver();
                loop {
                    let message = crossbeam_channel::select! {
                        recv(menu_events) -> event => {
                            let Ok(event) = event else { break };
                            if event.id == open_id {
                                Some(AppMsg::TrayToggleVisibility)
                            } else if event.id == quit_id {
                                Some(AppMsg::TrayQuit)
                            } else {
                                None
                            }
                        }
                        recv(icon_events) -> event => {
                            let Ok(event) = event else { break };
                            is_toggle_click(&event, &icon_id)
                                .then_some(AppMsg::TrayToggleVisibility)
                        }
                    };
                    if let Some(message) = message {
                        tracing::info!(?message, "tray command requested");
                        let connected = if matches!(&message, AppMsg::TrayToggleVisibility) {
                            crate::ui_work::send_toggle_control(&tx, &ctx, message)
                        } else {
                            crate::ui_work::send_control(&tx, &ctx, message)
                        };
                        if !connected {
                            break;
                        }
                    }
                }
            })
            .ok();
    }

    fn visibility_text(hidden: bool) -> String {
        if hidden {
            t("tray-show")
        } else {
            t("tray-hide")
        }
    }

    pub fn set_hidden(&self, hidden: bool) {
        self.visibility_item.set_text(Self::visibility_text(hidden));
    }

    /// re-read the menu texts after a language change
    pub fn refresh_labels(&self, hidden: bool) {
        self.set_hidden(hidden);
        self.quit_item.set_text(t("tray-close"));
    }
}

fn is_toggle_click(event: &TrayIconEvent, icon_id: &TrayIconId) -> bool {
    matches!(event, TrayIconEvent::Click {
        id,
        button: MouseButton::Left,
        button_state: MouseButtonState::Up,
        ..
    } if id == icon_id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn left_click_toggles_once_on_release_and_only_for_our_icon() {
        let id = TrayIconId::new("hebnix");
        let mut event = TrayIconEvent::Click {
            id: id.clone(),
            position: Default::default(),
            rect: Default::default(),
            button: MouseButton::Left,
            button_state: MouseButtonState::Down,
        };
        assert!(!is_toggle_click(&event, &id));
        if let TrayIconEvent::Click { button_state, .. } = &mut event {
            *button_state = MouseButtonState::Up;
        }
        assert!(is_toggle_click(&event, &id));
        assert!(!is_toggle_click(&event, &TrayIconId::new("another-app")));
        for other_button in [MouseButton::Right, MouseButton::Middle] {
            if let TrayIconEvent::Click { button, .. } = &mut event {
                *button = other_button;
            }
            assert!(!is_toggle_click(&event, &id));
        }
        assert!(!is_toggle_click(
            &TrayIconEvent::DoubleClick {
                id: id.clone(),
                position: Default::default(),
                rect: Default::default(),
                button: MouseButton::Left,
            },
            &id
        ));
    }
}

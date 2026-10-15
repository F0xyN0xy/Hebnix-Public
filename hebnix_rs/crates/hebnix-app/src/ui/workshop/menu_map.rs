use crate::i18n::{t, t_args};
use crate::messages::AppMsg;
use crate::patcher::menu_map::{self, ActiveSwap, Arena};
use crossbeam_channel::{Receiver, Sender, TryRecvError};
use eframe::egui;
use std::path::Path;

struct MenuMapScan {
    active: Result<Option<ActiveSwap>, String>,
    arenas: Result<Vec<Arena>, String>,
}

#[derive(Default)]
pub struct MenuMapState {
    arenas: Vec<Arena>,
    selected: String,
    filter: String,
    active: Option<ActiveSwap>,
    last_path: Option<String>,
    scan: Option<Receiver<MenuMapScan>>,
    refresh_needed: bool,
    busy: bool,
    state_error: bool,
    status: String,
}

impl MenuMapState {
    fn refresh(&mut self, rl_path: &str, ctx: &egui::Context) {
        if self.busy {
            return;
        }
        if self.last_path.as_deref() != Some(rl_path) {
            self.arenas.clear();
            self.active = None;
            self.selected.clear();
            self.status.clear();
        }
        self.last_path = Some(rl_path.to_string());
        self.refresh_needed = false;
        let cooked = Path::new(rl_path).join("TAGame").join("CookedPCConsole");
        let (tx, rx) = crossbeam_channel::bounded(1);
        self.scan = Some(rx);
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let result = MenuMapScan {
                active: menu_map::active(&cooked),
                arenas: menu_map::installed_arenas(&cooked),
            };
            let _ = tx.send(result);
            ctx.request_repaint();
        });
    }

    fn ensure_loaded(&mut self, rl_path: &str, ctx: &egui::Context) {
        if self.last_path.as_deref() != Some(rl_path) || self.refresh_needed {
            self.refresh(rl_path, ctx);
        }
    }

    fn poll_scan(&mut self) {
        let Some(scan) = &self.scan else { return };
        let result = match scan.try_recv() {
            Ok(result) => result,
            Err(TryRecvError::Empty) => return,
            Err(TryRecvError::Disconnected) => {
                self.scan = None;
                self.state_error = true;
                self.status = t("menu-map-scan-failed");
                return;
            }
        };
        self.scan = None;
        self.apply_scan(result);
    }

    fn apply_scan(&mut self, result: MenuMapScan) {
        self.state_error = false;
        match result.active {
            Ok(active) => self.active = active,
            Err(error) => {
                self.active = None;
                self.state_error = true;
                self.status = error;
            }
        }
        match result.arenas {
            Ok(arenas) => self.arenas = arenas,
            Err(error) => {
                self.arenas.clear();
                self.status = error;
            }
        }
        if let Some(active) = &self.active {
            self.selected = active.arena.clone();
        } else if !self
            .arenas
            .iter()
            .any(|arena| arena.package == self.selected)
        {
            self.selected = self
                .arenas
                .first()
                .map(|arena| arena.package.clone())
                .unwrap_or_default();
        }
    }

    fn launch(&mut self, restore: bool, rl_path: &str, tx: &Sender<AppMsg>, ctx: &egui::Context) {
        if self.busy
            || self.scan.is_some()
            || self.refresh_needed
            || self.state_error
            || self.last_path.as_deref() != Some(rl_path)
            || (!restore && self.active.is_some())
        {
            return;
        }
        if crate::messages::block_item_action_if_game_running(tx) {
            return;
        }
        self.busy = true;
        self.status = t(if restore {
            "menu-map-restoring"
        } else {
            "menu-map-applying"
        });
        let cooked = Path::new(rl_path).join("TAGame").join("CookedPCConsole");
        let arena = self.selected.clone();
        let tx = tx.clone();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let result = if restore {
                menu_map::restore(&cooked)
            } else {
                menu_map::apply(&cooked, &arena)
            };
            let _ = tx.send(AppMsg::MenuMapDone(result));
            ctx.request_repaint();
        });
    }

    pub fn finish(&mut self, result: Result<String, String>) -> String {
        self.busy = false;
        self.refresh_needed = true;
        let message = match result {
            Ok(message) => message,
            Err(error) => error,
        };
        self.status = message.clone();
        format!("[Maps] {message}")
    }

    pub fn render(&mut self, ui: &mut egui::Ui, rl_path: &str, tx: &Sender<AppMsg>) {
        self.ensure_loaded(rl_path, ui.ctx());
        self.poll_scan();
        let loading = self.scan.is_some();
        ui.heading(t("menu-map-title"));
        ui.label(t("menu-map-description"));
        ui.small(t("menu-map-close-game"));
        ui.add_space(12.0);
        if loading {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(t("menu-map-loading"));
            });
        }
        let ready = !self.busy
            && !loading
            && !self.refresh_needed
            && !self.state_error
            && self.last_path.as_deref() == Some(rl_path);
        let editable = ready && self.active.is_none();
        ui.add_enabled_ui(editable, |ui| {
            egui::ComboBox::from_id_salt("menu_map_arena")
                .selected_text(if self.selected.is_empty() {
                    t("menu-map-choose")
                } else {
                    crate::discord_presence::display_map_name(&self.selected)
                })
                .width(360.0)
                .height(320.0)
                .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
                .show_ui(ui, |ui| {
                    ui.add(
                        egui::TextEdit::singleline(&mut self.filter)
                            .hint_text(t("render-search-maps")),
                    );
                    ui.separator();
                    let filter = self.filter.trim().to_lowercase();
                    for arena in &self.arenas {
                        if arena.label.to_lowercase().contains(&filter)
                            || arena.package.to_lowercase().contains(&filter)
                        {
                            if ui
                                .selectable_value(
                                    &mut self.selected,
                                    arena.package.clone(),
                                    &arena.label,
                                )
                                .clicked()
                            {
                                ui.close();
                            }
                        }
                    }
                });
        });
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            if ui
                .add_enabled(
                    editable && !self.selected.is_empty(),
                    egui::Button::new(t("menu-map-apply")),
                )
                .clicked()
            {
                self.launch(false, rl_path, tx, ui.ctx());
            }
            if ui
                .add_enabled(
                    ready && self.active.is_some(),
                    egui::Button::new(t("menu-map-restore")),
                )
                .clicked()
            {
                self.launch(true, rl_path, tx, ui.ctx());
            }
            if self.busy {
                ui.spinner();
            }
        });
        if let Some(active) = &self.active {
            ui.label(t_args(
                "menu-map-active",
                &[(
                    "map",
                    crate::discord_presence::display_map_name(&active.arena).into(),
                )],
            ));
            ui.small(t("menu-map-restore-first"));
        }
        if !loading && self.arenas.is_empty() {
            ui.label(t("menu-map-empty"));
        }
        if !self.status.is_empty() {
            ui.add_space(8.0);
            ui.label(&self.status);
        }
        if ui
            .add_enabled(
                !self.busy && !loading,
                egui::Button::new(t("render-scan-again")),
            )
            .clicked()
        {
            self.status.clear();
            self.refresh(rl_path, ui.ctx());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scanned(package: &str) -> MenuMapScan {
        MenuMapScan {
            active: Ok(None),
            arenas: Ok(vec![Arena {
                package: package.to_string(),
                label: package.to_string(),
            }]),
        }
    }

    #[test]
    fn loading_keeps_the_ui_available_and_installs_the_completed_list() {
        let (tx, rx) = crossbeam_channel::bounded(1);
        let mut state = MenuMapState {
            scan: Some(rx),
            ..Default::default()
        };
        state.poll_scan();
        assert!(state.scan.is_some());
        assert!(state.arenas.is_empty());
        tx.send(scanned("Beach_P")).unwrap();
        state.poll_scan();
        assert!(state.scan.is_none());
        assert_eq!(state.selected, "Beach_P");
        assert!(!state.state_error);
    }

    #[test]
    fn revisiting_the_same_install_reuses_the_loaded_list() {
        let mut state = MenuMapState {
            last_path: Some("cached-install".to_string()),
            ..Default::default()
        };
        state.apply_scan(scanned("Beach_P"));
        state.ensure_loaded("cached-install", &egui::Context::default());
        assert!(state.scan.is_none());
        assert_eq!(state.selected, "Beach_P");
    }

    #[test]
    fn changing_installs_discards_an_old_pending_scan() {
        let (tx, rx) = crossbeam_channel::bounded(1);
        let mut state = MenuMapState {
            last_path: Some("old-install".to_string()),
            scan: Some(rx),
            ..Default::default()
        };
        state.apply_scan(scanned("Beach_P"));
        state.ensure_loaded("new-install", &egui::Context::default());
        assert!(tx.send(scanned("Stadium_P")).is_err());
        assert_eq!(state.last_path.as_deref(), Some("new-install"));
        assert!(state.arenas.is_empty());
        assert!(state.selected.is_empty());
        assert!(state.scan.is_some());
    }

    #[test]
    fn apply_completion_defers_the_rescan_and_preserves_the_result_message() {
        let mut state = MenuMapState {
            last_path: Some("cached-install".to_string()),
            busy: true,
            ..Default::default()
        };
        assert_eq!(state.finish(Ok("Applied".to_string())), "[Maps] Applied");
        assert!(!state.busy);
        assert!(state.refresh_needed);
        assert!(state.scan.is_none());
        assert_eq!(state.status, "Applied");
        state.ensure_loaded("cached-install", &egui::Context::default());
        assert!(state.scan.is_some());
        assert!(!state.refresh_needed);
        assert_eq!(state.status, "Applied");
    }
}

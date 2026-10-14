use crate::i18n::{t, t_args};
use crate::messages::AppMsg;
use crate::patcher::menu_map::{self, ActiveSwap, Arena};
use crossbeam_channel::Sender;
use eframe::egui;
use std::path::Path;

#[derive(Default)]
pub struct MenuMapState {
    arenas: Vec<Arena>,
    selected: String,
    filter: String,
    active: Option<ActiveSwap>,
    last_path: Option<String>,
    busy: bool,
    state_error: bool,
    status: String,
}

impl MenuMapState {
    pub fn refresh(&mut self, rl_path: &str) {
        if self.busy {
            return;
        }
        self.last_path = Some(rl_path.to_string());
        let cooked = Path::new(rl_path).join("TAGame").join("CookedPCConsole");
        self.state_error = false;
        match menu_map::active(&cooked) {
            Ok(active) => self.active = active,
            Err(error) => {
                self.active = None;
                self.state_error = true;
                self.status = error;
            }
        }
        match menu_map::installed_arenas(&cooked) {
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
        if self.busy || self.state_error || (!restore && self.active.is_some()) {
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
        if let Some(path) = self.last_path.clone() {
            self.refresh(&path);
        }
        let message = match result {
            Ok(message) => message,
            Err(error) => error,
        };
        self.status = message.clone();
        format!("[Maps] {message}")
    }

    pub fn render(&mut self, ui: &mut egui::Ui, rl_path: &str, tx: &Sender<AppMsg>) {
        if self.last_path.as_deref() != Some(rl_path) {
            self.refresh(rl_path);
        }
        ui.heading(t("menu-map-title"));
        ui.label(t("menu-map-description"));
        ui.small(t("menu-map-close-game"));
        ui.add_space(12.0);
        let editable = !self.busy && !self.state_error && self.active.is_none();
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
                    !self.busy && !self.state_error && self.active.is_some(),
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
        if self.arenas.is_empty() {
            ui.label(t("menu-map-empty"));
        }
        if !self.status.is_empty() {
            ui.add_space(8.0);
            ui.label(&self.status);
        }
        if ui
            .add_enabled(!self.busy, egui::Button::new(t("render-scan-again")))
            .clicked()
        {
            self.refresh(rl_path);
        }
    }
}

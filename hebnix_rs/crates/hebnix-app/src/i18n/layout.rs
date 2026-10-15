//! layout helpers so rows don't clip when a translation runs longer than english.

use eframe::egui::{self, Color32};

/// Reserve one label column while keeping the text against its left edge.
pub fn settings_label(
    ui: &mut egui::Ui,
    width: f32,
    text: impl Into<egui::WidgetText>,
) -> egui::Response {
    ui.allocate_ui_with_layout(
        egui::vec2(width, 20.0),
        egui::Layout::left_to_right(egui::Align::Center),
        |ui| {
            ui.set_min_size(egui::vec2(width, 20.0));
            ui.add(egui::Label::new(text).truncate())
        },
    )
    .inner
}

/// width for a settings page's label column: the widest label on that page,
/// never narrower than `min` (the old fixed width), capped so one huge label
/// can't eat the row. keeps the inputs next to the labels lined up.
pub fn label_column_width(ui: &egui::Ui, labels: &[String], min: f32) -> f32 {
    let font = egui::TextStyle::Body.resolve(ui.style());
    let widest = labels
        .iter()
        .map(|label| {
            ui.painter()
                .layout_no_wrap(label.clone(), font.clone(), Color32::WHITE)
                .size()
                .x
        })
        .fold(0.0_f32, f32::max);
    (widest + 8.0).clamp(min, min.max(320.0))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_text_starts_at_the_left_edge_and_controls_share_a_column() {
        let context = egui::Context::default();
        let mut rows = Vec::new();
        let _ = context.run_ui(egui::RawInput::default(), |ui| {
            for text in ["Start in Tray:", "Limit Hotkey to Hebnix/Rocket League:"] {
                ui.horizontal(|ui| {
                    let left = ui.cursor().left();
                    let label = settings_label(ui, 300.0, text);
                    let checkbox = ui.checkbox(&mut false, "");
                    rows.push((left, label.rect.left(), checkbox.rect.left()));
                });
            }
        });
        assert_eq!(rows.len(), 2);
        for (left, label_x, _) in &rows {
            assert!((left - label_x).abs() < 0.1);
        }
        assert!((rows[0].2 - rows[1].2).abs() < 0.1);
        assert!(rows[0].2 >= rows[0].0 + 300.0);
    }
}

pub mod game;
pub mod settings;
pub mod console;

use egui::{RichText, Sense, Stroke, Ui};
use crate::theme::ThemeSpec;

pub fn themed_separator(ui: &mut Ui, t: &ThemeSpec) {
    let desired = egui::vec2(ui.available_width(), 8.0);
    let (rect, _response) = ui.allocate_at_least(desired, Sense::hover());
    if ui.is_rect_visible(rect) {
        ui.painter().hline(rect.x_range(), rect.center().y, Stroke::new(1.0_f32, t.border));
    }
}

pub fn section(ui: &mut Ui, t: &ThemeSpec, title: &str) {
    ui.add_space(10.0);
    ui.label(RichText::new(title).strong().monospace().color(t.accent));
    themed_separator(ui, t);
    ui.add_space(4.0);
}

pub fn button(ui: &mut Ui, t: &ThemeSpec, label: &str, primary: bool, enabled: bool, w: f32, h: f32) -> egui::Response {
    let base = RichText::new(label).monospace();
    let txt = if primary { base.strong() } else { base }
        .color(if primary { t.bg } else { t.text });
    let fill = if primary { t.accent } else { t.bg };
    let st = Stroke::new(1.0_f32, if primary { t.accent } else { t.border });
    let btn = egui::Button::new(txt).fill(fill).stroke(st).min_size(egui::vec2(w, h));
    ui.add_enabled(enabled, btn)
}
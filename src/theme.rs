use eframe::egui;
use egui::{Color32, Context, Rounding, Stroke};
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Debug)]
pub enum ThemeId { AtlasDark, Carbon, Amber }

pub struct ThemeSpec {
    pub name: &'static str,
    pub bg: Color32, pub panel: Color32, pub panel2: Color32,
    pub border: Color32, pub accent: Color32, pub text: Color32, pub weak: Color32,
}

const BG: Color32 = Color32::from_rgb(10, 10, 11);
const PANEL: Color32 = Color32::from_rgb(17, 17, 19);
const PANEL2: Color32 = Color32::from_rgb(24, 24, 27);
const TEXT: Color32 = Color32::from_rgb(205, 208, 212);
const WEAK: Color32 = Color32::from_rgb(118, 122, 128);

fn dim(c: Color32, f: f32) -> Color32 {
    Color32::from_rgb((c.r() as f32 * f) as u8, (c.g() as f32 * f) as u8, (c.b() as f32 * f) as u8)
}

pub fn theme_spec(id: ThemeId) -> ThemeSpec {
    let (name, accent) = match id {
        ThemeId::AtlasDark => ("ATLAS DARK", Color32::from_rgb(0, 212, 255)),
        ThemeId::Carbon => ("CARBON", Color32::from_rgb(225, 225, 225)),
        ThemeId::Amber => ("AMBER TERM", Color32::from_rgb(255, 176, 0)),
    };
    ThemeSpec { name, bg: BG, panel: PANEL, panel2: PANEL2, border: dim(accent, 0.45), accent, text: TEXT, weak: WEAK }
}

pub fn apply(ctx: &Context, id: ThemeId) -> ThemeSpec {
    let t = theme_spec(id);
    let mut v = egui::Visuals::dark();
    v.override_text_color = Some(t.text);
    v.panel_fill = t.panel; v.window_fill = t.panel;
    v.extreme_bg_color = t.bg; v.faint_bg_color = t.bg; v.code_bg_color = t.bg;
    v.warn_fg_color = t.accent; v.error_fg_color = Color32::from_rgb(255, 90, 90);
    v.widgets.noninteractive.bg_fill = t.panel;
    v.widgets.inactive.bg_fill = t.bg;
    v.widgets.hovered.bg_fill = t.panel2;
    v.widgets.active.bg_fill = t.accent;
    for w in [&mut v.widgets.noninteractive, &mut v.widgets.inactive,
              &mut v.widgets.hovered, &mut v.widgets.active] {
        w.rounding = Rounding::same(2.0);
        w.bg_stroke = Stroke::new(1.0_f32, t.border);
        w.fg_stroke = Stroke::new(1.0_f32, t.text);
    }
    v.widgets.hovered.bg_stroke = Stroke::new(1.0_f32, t.accent);
    v.widgets.hovered.fg_stroke = Stroke::new(1.0_f32, t.accent);
    v.widgets.active.bg_stroke = Stroke::new(1.0_f32, t.accent);
    v.widgets.active.fg_stroke = Stroke::new(1.0_f32, t.bg);
    v.selection.bg_fill = t.accent; v.selection.stroke = Stroke::new(1.0_f32, t.bg);
    v.window_shadow = Default::default(); v.popup_shadow = Default::default();
    v.window_rounding = Rounding::same(2.0); v.menu_rounding = Rounding::same(2.0);
    ctx.set_visuals(v);
    t
}
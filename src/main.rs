mod app;
mod config;
mod lobby;
mod maps;
mod moho;
mod theme;
mod ui;

use crate::app::{AtlasApp, View};
use eframe::egui;
use egui::{Color32, Frame, RichText, Stroke};

impl eframe::App for AtlasApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        if let Some(l) = &mut self.lobby {
            if l.players.len() == 1 && l.created_at.elapsed().as_secs() >= 4 {
                l.players.push("Raptor".into());
                l.chat.push(("СИСТЕМА".into(), "Игрок Raptor присоединился".into()));
            } else { ctx.request_repaint(); }
        }

        let t = theme::apply(ctx, self.cfg.theme);

        egui::TopBottomPanel::top("top")
            .frame(Frame::none().fill(t.panel).inner_margin(egui::Margin::same(10.0))
                .stroke(Stroke::new(1.0_f32, t.border)))
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new("ATLAS").strong().monospace().size(18.0).color(t.accent));
                    ui.label(RichText::new("ПРОТОТИП 0.0.3").monospace().color(t.weak));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let ok = config::validate(&self.cfg.game_path);
                        ui.label(RichText::new(if ok { "● ГОТОВ" } else { "● ПУТЬ НЕ ЗАДАН" })
                            .monospace().color(if ok { t.accent } else { Color32::from_rgb(255, 90, 90) }));
                    });
                });
            });

        egui::SidePanel::left("nav").exact_width(200.0)
            .frame(Frame::none().fill(t.bg).inner_margin(egui::Margin::same(10.0))
                .stroke(Stroke::new(1.0_f32, t.border)))
            .show(ctx, |ui| {
                ui.add_space(6.0);
                let nav = |ui: &mut egui::Ui, label: &str, active: bool| {
                    let txt = RichText::new(label).strong().monospace()
                        .color(if active { t.accent } else { t.text });
                    let fill = if active { t.panel } else { t.bg };
                    let st = Stroke::new(1.0_f32, if active { t.accent } else { t.border });
                    ui.add_sized([ui.available_width(), 36.0], egui::Button::new(txt).fill(fill).stroke(st))
                };
                if nav(ui, "ИГРА", self.view == View::Game).clicked() { self.view = View::Game; }
                ui.add_space(4.0);
                if nav(ui, "КОНСОЛЬ", self.view == View::Console).clicked() { self.view = View::Console; }
                ui.add_space(4.0);
                if nav(ui, "НАСТРОЙКИ", self.view == View::Settings).clicked() { self.view = View::Settings; }
                ui.with_layout(egui::Layout::bottom_up(egui::Align::LEFT), |ui| {
                    ui.label(RichText::new(format!("ТЕМА: {}", t.name)).monospace().color(t.weak));
                    ui.label(RichText::new(format!(".SCD: {}", self.scd_count)).monospace().color(t.weak));
                    ui.label(RichText::new("СБОРКА 0.0.3").monospace().color(t.weak));
                });
            });

        egui::CentralPanel::default()
            .frame(Frame::none().fill(t.bg).inner_margin(egui::Margin::same(16.0)))
            .show(ctx, |ui| match self.view {
                View::Game => ui::game::draw(self, ui, &t),
                View::Settings => ui::settings::draw(self, ui, &t),
                View::Console => ui::console::draw(self, ui, &t),
            });
    }
}

fn main() -> eframe::Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1060.0, 720.0])
            .with_title("ATLAS — 0.0.3"),
        ..Default::default()
    };
    eframe::run_native("ATLAS", options, Box::new(|_cc| Ok(Box::new(AtlasApp::new()))))
}
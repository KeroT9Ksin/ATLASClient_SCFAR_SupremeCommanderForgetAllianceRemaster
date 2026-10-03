use egui::{RichText, ScrollArea, Stroke, TextEdit};
use crate::app::{AtlasApp, Stage};
use crate::config;
use crate::lobby::Balance;
use crate::theme::ThemeSpec;
use crate::ui::{button, section};

pub fn draw(app: &mut AtlasApp, ui: &mut egui::Ui, t: &ThemeSpec) {
    ScrollArea::vertical()
        .id_salt("game_scroll")
        .show(ui, |ui| {
            if app.stage == Stage::Lobby { draw_lobby(app, ui, t); return; }

            ui.add_space(6.0);
            ui.label(RichText::new("БОЕВОЙ МОДУЛЬ").monospace().color(t.weak));
            ui.add_space(6.0);

            let path_ok = config::validate(&app.cfg.game_path);
            let big = egui::Button::new(RichText::new("ИГРА").strong().monospace().size(16.0)
                .color(if path_ok { t.accent } else { t.weak }))
                .fill(t.bg).stroke(Stroke::new(1.0_f32, if path_ok { t.accent } else { t.border }))
                .min_size(egui::vec2(ui.available_width(), 44.0));
            if ui.add_enabled(path_ok, big).clicked() {
                app.stage = if app.stage == Stage::Closed { Stage::Balance } else { Stage::Closed };
            }
            if !path_ok {
                ui.label(RichText::new("Укажите путь к игре в разделе НАСТРОЙКИ").monospace().color(t.weak));
            }

            if app.stage != Stage::Closed {
                section(ui, t, "ВЫБОР БАЛАНСА");
                ui.horizontal(|ui| {
                    for b in Balance::all() {
                        let sel = app.balance == Some(b);
                        let txt = RichText::new(b.label()).strong().monospace()
                            .color(if sel { t.bg } else { t.text });
                        let fill = if sel { t.accent } else { t.bg };
                        let st = Stroke::new(1.0_f32, if sel { t.accent } else { t.border });
                        if ui.add_sized([120.0, 34.0], egui::Button::new(txt).fill(fill).stroke(st)).clicked() {
                            app.balance = Some(b);
                            app.stage = Stage::Config;
                        }
                    }
                });
                if let Some(b) = app.balance {
                    ui.add_space(4.0);
                    ui.label(RichText::new(b.desc()).monospace().color(t.weak));
                }
            }

            if app.stage == Stage::Config {
                section(ui, t, "ПАРАМЕТРЫ СЕТЕВОЙ ИГРЫ");
                ui.label(RichText::new("КАРТА").monospace().color(t.weak));
                if app.maps.is_empty() {
                    ui.label(RichText::new("Карты не найдены: нажмите ИНИЦИАЛИЗИРОВАТЬ MOHO в НАСТРОЙКАХ.")
                        .monospace().color(t.weak));
                }
                egui::ComboBox::from_id_salt("map_select").width(340.0)
                    .selected_text(if app.map.is_empty() { "— нет карт —".into() } else { app.map.clone() })
                    .show_ui(ui, |ui| {
                        for m in &app.maps { ui.selectable_value(&mut app.map, m.clone(), m); }
                    });
                ui.add_space(4.0);
                ui.label(RichText::new("ПАРОЛЬ (НЕОБЯЗАТЕЛЬНО)").monospace().color(t.weak));
                ui.add_sized([340.0, 30.0], TextEdit::singleline(&mut app.password).password(true));
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    let can = !app.map.is_empty();
                    if button(ui, t, "ПОДТВЕРДИТЬ СОЗДАНИЕ ЛОББИ", true, can, 320.0, 40.0).clicked() {
                        app.create_lobby();
                    }
                    if button(ui, t, "НАЗАД", false, true, 120.0, 40.0).clicked() {
                        app.stage = Stage::Balance;
                    }
                });
            }
        });
}

fn draw_lobby(app: &mut AtlasApp, ui: &mut egui::Ui, t: &ThemeSpec) {
    let mut action = 0u8;
    let (lobby_opt, chat_input) = (&mut app.lobby, &mut app.chat_input);

    if let Some(l) = lobby_opt {
        ui.horizontal(|ui| {
            ui.label(RichText::new(format!("ЛОББИ #{}", l.id)).strong().monospace().color(t.accent));
            ui.label(RichText::new(format!("| БАЛАНС: {}", l.balance.label())).monospace().color(t.weak));
            if l.has_password {
                ui.label(RichText::new("| ЗАЩИЩЕНО ПАРОЛЕМ").monospace().color(t.weak));
            }
        });
        ui.label(RichText::new(format!("КАРТА: {}", l.map)).monospace().color(t.text));

        section(ui, t, "ИГРОКИ");
        egui::Grid::new("slots").num_columns(4).min_col_width(150.0).show(ui, |ui| {
            for i in 0..8 {
                let name = l.players.get(i);
                let txt = match name {
                    Some(n) if i == 0 => format!("[HOST] {}", n),
                    Some(n) => n.clone(),
                    None => "— СВОБОДНО —".to_string(),
                };
                ui.label(RichText::new(txt).monospace()
                    .color(if name.is_some() { t.text } else { t.weak }));
                if (i + 1) % 4 == 0 { ui.end_row(); }
            }
        });

        section(ui, t, "КАНАЛ СВЯЗИ");
        ScrollArea::vertical().id_salt("lobby_chat").max_height(150.0)
            .stick_to_bottom(true).show(ui, |ui| {
                for (who, msg) in &l.chat {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(format!("{} >", who)).monospace().color(t.accent));
                        ui.label(RichText::new(msg).monospace().color(t.text));
                    });
                }
            });
        ui.add_space(4.0);
        let enter = ui.input(|i| i.key_pressed(egui::Key::Enter));
        ui.horizontal(|ui| {
            ui.add_sized([ui.available_width() - 120.0, 30.0],
                TextEdit::singleline(chat_input).hint_text("Сообщение..."));
            if button(ui, t, "ОТПРАВИТЬ", false, true, 110.0, 30.0).clicked() || enter {
                let s = chat_input.trim().to_string();
                if !s.is_empty() { l.chat.push(("ВЫ".into(), s)); chat_input.clear(); }
            }
        });

        ui.add_space(10.0);
        ui.horizontal(|ui| {
            if button(ui, t, "НАЧАТЬ ИГРУ", true, true, 240.0, 40.0).clicked() { action = 2; }
            if button(ui, t, "ПОКИНУТЬ ЛОББИ", false, true, 240.0, 40.0).clicked() { action = 1; }
        });
        if action == 2 {
            l.chat.push(("СИСТЕМА".into(),
                "Прототип 0.0.2: запуск симуляции появится в 0.0.3.".into()));
        }
    }
    if action == 1 { app.lobby = None; app.stage = Stage::Closed; app.balance = None; }
}
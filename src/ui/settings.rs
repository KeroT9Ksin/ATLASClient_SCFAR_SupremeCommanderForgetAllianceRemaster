use egui::{Color32, RichText, TextEdit};
use crate::app::AtlasApp;
use crate::config;
use crate::theme::{theme_spec, ThemeId, ThemeSpec};
use crate::ui::{button, section};

pub fn draw(app: &mut AtlasApp, ui: &mut egui::Ui, t: &ThemeSpec) {
    egui::ScrollArea::vertical()
        .id_salt("settings_scroll")
        .show(ui, |ui| {
            section(ui, t, "ПУТЬ К ИГРЕ");
            ui.horizontal(|ui| {
                ui.add_sized([ui.available_width() - 120.0, 30.0],
                    TextEdit::singleline(&mut app.path_input).hint_text("C:\\Program Files (x86)\\..."));
                if button(ui, t, "ОБЗОР", false, true, 110.0, 30.0).clicked() {
                    if let Some(dir) = rfd::FileDialog::new().set_title("Выбор каталога игры").pick_folder() {
                        app.path_input = dir.to_string_lossy().to_string();
                    }
                }
            });
            let ok = config::validate(&app.path_input);
            ui.label(RichText::new(if ok { "● КАТАЛОГ КОРРЕКТЕН" } else { "● НЕ НАЙДЕНО: ForgedAlliance.exe / gamedata" })
                .monospace().color(if ok { t.accent } else { Color32::from_rgb(255, 90, 90) }));
            if button(ui, t, "ПРИМЕНИТЬ ПУТЬ", true, ok, 220.0, 34.0).clicked() {
                app.apply_game_path();
            }

            section(ui, t, "ПАПКИ МОДОВ И КАРТ");
            ui.label(RichText::new("По умолчанию — из пути SCFA. Можно указать любую папку.")
                .monospace().color(t.weak));
            ui.label(RichText::new("МОДЫ:").monospace().color(t.weak));
            ui.horizontal(|ui| {
                ui.add_sized([ui.available_width() - 120.0, 30.0], TextEdit::singleline(&mut app.cfg.mods_dir));
                if button(ui, t, "ОБЗОР", false, true, 110.0, 30.0).clicked() {
                    if let Some(dir) = rfd::FileDialog::new().set_title("Папка модов").pick_folder() {
                        app.cfg.mods_dir = dir.to_string_lossy().to_string();
                    }
                }
            });
            ui.label(RichText::new("КАРТЫ:").monospace().color(t.weak));
            ui.horizontal(|ui| {
                ui.add_sized([ui.available_width() - 120.0, 30.0], TextEdit::singleline(&mut app.cfg.maps_dir));
                if button(ui, t, "ОБЗОР", false, true, 110.0, 30.0).clicked() {
                    if let Some(dir) = rfd::FileDialog::new().set_title("Папка карт").pick_folder() {
                        app.cfg.maps_dir = dir.to_string_lossy().to_string();
                    }
                }
            });
            if button(ui, t, "ПРИМЕНИТЬ ПАПКИ", false, true, 200.0, 30.0).clicked() {
                config::save(&app.cfg);
                app.init_moho();
                app.auto_load_lua();
            }

            section(ui, t, "SUPCOMDATAPATH.LUA");
            ui.label(RichText::new(format!("Файл: {}", app.supcom_source)).monospace().color(t.weak));
            ui.horizontal(|ui| {
                if button(ui, t, "ПРОЧИТАТЬ", false, true, 140.0, 30.0).clicked() { app.load_supcom_data_path(); }
                if button(ui, t, "ВЫБРАТЬ ФАЙЛ", false, true, 160.0, 30.0).clicked() { app.pick_supcom_file(); }
            });

            section(ui, t, "ДВИЖОК MOHO");
            ui.label(RichText::new(&app.moho_status).monospace().color(t.weak));
            ui.horizontal(|ui| {
                let can_init = config::validate(&app.cfg.game_path);
                let can_test = app.rt.is_some();
                if button(ui, t, "ИНИЦИАЛИЗИРОВАТЬ MOHO", false, can_init, 240.0, 34.0).clicked() { app.init_moho(); }
                if button(ui, t, "ТЕСТ LUA 5.1 / LUAJIT", false, can_test, 240.0, 34.0).clicked() { app.test_lua(); }
            });

            section(ui, t, "ТЕМА ОФОРМЛЕНИЯ");
            for th in [ThemeId::AtlasDark, ThemeId::Carbon, ThemeId::Amber] {
                let sel = app.cfg.theme == th;
                let label = format!("{} {}", if sel { "▣" } else { "▢" }, theme_spec(th).name);
                if ui.selectable_label(sel, RichText::new(label).monospace()).clicked() {
                    app.cfg.theme = th;
                    config::save(&app.cfg);
                }
            }

            section(ui, t, "О ПРОТОТИПЕ");
            ui.label(RichText::new("0.0.3 — автозагрузка Lua + консоль | 0.1.0 — Bevy")
                .monospace().color(t.weak));
        });
}
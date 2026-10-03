// src/ui/console.rs
// ОТЛАДОЧНАЯ КОНСОЛЬ ATLAS — поведение как в cmd.
//
// ДИАГНОЗ ПРОШЛОГО ПРОГОНА: bpstat/bp_scan/mapchunks уходили в Lua-ветку
// (bpstat -> nil, mapchunks SCMP_001 -> '=' expected), значит arm'ов не было.
// Теперь они есть. bpstat/bp_scan/mapchunks написаны ТОЛЬКО на подтверждённых
// функциях (shim::execute_lua, shim::vfs_list, crate::moho::map::*) и НЕ
// требуют новых функций в shim.rs: я не знаю, применён ли shim.rs, и не ставлю
// прогон на это. mapload/maphm требуют новых полей WorldState (hm_grid/
// heights/hm_scale) — если shim.rs НЕ применён, получишь E0609 и удалишь
// эти два arm'а (по 3 строки каждый), остальное заработает.

use crate::app::{AtlasApp, ConsoleEntryKind};
use crate::moho::debug;
use crate::moho::shim;

const VISIBLE_HISTORY: usize = 400;
const MAX_RENDER_CHARS: usize = 200_000;
const MAX_DRAIN_PER_FRAME: usize = 300;
const MONO_SIZE: f32 = 13.0;
const INPUT_ROW_H: f32 = 78.0;
const HIST_MIN_H: f32 = 120.0;
const HIST_MAX_H: f32 = 520.0;

// ==================== РИСОВАНИЕ ====================

pub fn draw(app: &mut AtlasApp, ui: &mut egui::Ui, t: &crate::theme::ThemeSpec) {
    let c_accent = t.accent;
    let c_text = t.text;
    let c_weak = t.weak;
    let c_bg = t.bg;
    let c_panel = t.panel;
    let c_border = t.border;

    drain_trace(app);

    egui::Frame::none()
        .fill(c_bg)
        .inner_margin(egui::Margin::same(8.0))
        .stroke(egui::Stroke::new(1.0_f32, c_border))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(
                    egui::RichText::new("ATLAS CONSOLE")
                        .strong()
                        .monospace()
                        .color(c_accent),
                );
                ui.label(
                    egui::RichText::new(
                        "кликни в поле ниже · Enter или кнопка · мышь = выделение · Ctrl+C",
                    )
                    .monospace()
                    .small()
                    .color(c_weak),
                );
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui
                        .add(
                            egui::Button::new(egui::RichText::new("ОЧИСТИТЬ").monospace())
                                .fill(c_panel)
                                .stroke(egui::Stroke::new(1.0_f32, c_border)),
                        )
                        .clicked()
                    {
                        app.console_history.clear();
                    }
                });
            });
            ui.separator();

            let hist_h = (ui.available_height() - INPUT_ROW_H).clamp(HIST_MIN_H, HIST_MAX_H);

            egui::ScrollArea::vertical()
                .auto_shrink([false, false])
                .max_height(hist_h)
                .stick_to_bottom(true)
                .show(ui, |ui| {
                    let total = app.console_history.len();
                    if total > VISIBLE_HISTORY {
                        ui.label(
                            egui::RichText::new(format!(
                                "… скрыто {} старых записей (полный лог: atlas_console_log.txt)",
                                total - VISIBLE_HISTORY
                            ))
                            .monospace()
                            .small()
                            .color(c_weak),
                        );
                    }
                    let mut buf = String::new();
                    let mut truncated_chars = false;
                    for e in app
                        .console_history
                        .iter()
                        .skip(total.saturating_sub(VISIBLE_HISTORY))
                    {
                        let prefix = match e.kind {
                            ConsoleEntryKind::Input => "> ",
                            ConsoleEntryKind::Output => " ",
                            ConsoleEntryKind::Error => "! ",
                            ConsoleEntryKind::Debug => "# ",
                        };
                        if buf.len() + e.text.len() + 4 > MAX_RENDER_CHARS {
                            truncated_chars = true;
                            break;
                        }
                        buf.push_str(prefix);
                        buf.push_str(&e.text);
                        buf.push('\n');
                    }
                    if truncated_chars {
                        buf.push_str("… обрезано по лимиту символов (полное: trace / logsave / atlas_console_log.txt)\n");
                    }
                    ui.add(
                        egui::TextEdit::multiline(&mut buf)
                            .frame(false)
                            .font(egui::FontId::monospace(MONO_SIZE))
                            .desired_width(f32::INFINITY)
                            .text_color(c_text),
                    );
                });

            ui.separator();

            ui.horizontal(|ui| {
                ui.label(egui::RichText::new(">").monospace().color(c_accent));
                let mut input = app.console_input.clone();
                let resp = ui.add(
                    egui::TextEdit::singleline(&mut input)
                        .hint_text("введи команду (help) и нажми Enter…")
                        .font(egui::FontId::monospace(MONO_SIZE))
                        .desired_width((ui.available_width() - 190.0).max(80.0)),
                );
                app.console_input = input;

                let enter = ui.input(|i| i.key_pressed(egui::Key::Enter))
                    && (resp.has_focus() || resp.lost_focus());
                let btn = ui
                    .add(
                        egui::Button::new(egui::RichText::new("ВЫПОЛНИТЬ").monospace())
                            .fill(c_panel)
                            .stroke(egui::Stroke::new(1.0_f32, c_accent)),
                    )
                    .clicked();

                if enter || btn {
                    let cmd = app.console_input.trim().to_string();
                    if !cmd.is_empty() {
                        app.log(ConsoleEntryKind::Input, cmd.clone());
                        execute(app, &cmd);
                        app.console_input.clear();
                    }
                    resp.request_focus();
                }
            });
        });
}

// ==================== ВЫВОД РАБОТЫ СКРИПТОВ ====================

fn drain_trace(app: &mut AtlasApp) {
    let rt = match &app.rt {
        Some(rt) => rt.clone(),
        None => return,
    };
    let fresh = debug::take_new(&rt);
    if fresh.is_empty() {
        return;
    }
    let mut batch = String::new();
    let mut errors = String::new();
    let mut shown = 0usize;
    for (kind, text) in &fresh {
        match kind {
            debug::TraceKind::Spew => continue,
            debug::TraceKind::Error | debug::TraceKind::Thread => {
                if errors.len() > 4096 {
                    errors.push_str(" …\n");
                    break;
                }
                errors.push_str(&format!("[{}] {}\n", kind.label(), text));
            }
            k => {
                if shown >= MAX_DRAIN_PER_FRAME {
                    continue;
                }
                shown += 1;
                if !batch.is_empty() {
                    batch.push('\n');
                }
                batch.push_str(&format!("[{}] {}", k.label(), text));
            }
        }
    }
    if !errors.is_empty() {
        app.log(ConsoleEntryKind::Error, errors);
    }
    if !batch.is_empty() {
        if fresh.len() > shown {
            batch.push_str(&format!(
                "\n… ещё {} событий (полные: `trace {}` / `logsave`)",
                fresh.len() - shown,
                MAX_DRAIN_PER_FRAME
            ));
        }
        app.log(ConsoleEntryKind::Output, batch);
    }
}

// ==================== СПРАВКА ====================

fn help_text() -> String {
    "КОМАНДЫ КОНСОЛИ ATLAS (как в cmd: Enter или кнопка ВЫПОЛНИТЬ):
help — справка; clear/cls — очистить; status — состояние MOHO/VFS
verbose on/off — детальный лог загрузки
mounts — монтирования (.scd и папки); ls <префикс> — файлы VFS
cat <путь> — трансформированный файл (первые 80 строк)
showline <путь> <номер> — исходные R и трансформ. T строки
maps / maplist — карты с классом; mapinfo <имя> — дамп заголовка .scmap
mapchunks <имя> — скан структуры: цепочки длин + поиск полотна высот
mapload <имя> — залить рельеф в ядро; maphm — статистика рельефа
mods — моды; bp <id> — блупринт (bp ueb0101)
bpstat — счётчики реестров .bp; bp_scan — где .bp лежат в VFS
load_all_lua — загрузить все .lua; load_bp — загрузить все .bp
test — самотест; pump — шаг потоков ForkThread
path <каталог> — путь игры + реинит
--- ОТЛАДКА ---
trace on|off|clear|N — кольцо событий; mem / watch — память и дельта
hunt — авто-диагноз: утечки / зависшие треды / повторы / таймауты
dump <глобал> — дамп состояния; luafile <путь> — исполнить Lua из VFS
logsave — кольцо в файл (Documents/atlas_debug.log)
иначе — выполняется как Lua-код (print, GetUnitBlueprintByName, ...)
Выделение: мышь по истории, Ctrl+A — всё, Ctrl+C — копировать.
Одна команда = одна строка (как в cmd)."
        .to_string()
}

fn map_roots_of(app: &AtlasApp) -> Vec<std::path::PathBuf> {
    use std::path::PathBuf;
    let mut v = Vec::new();
    let gp = PathBuf::from(&app.cfg.game_path);
    v.push(gp.join("maps"));
    v.push(gp.join("gamedata").join("maps"));
    if !app.cfg.maps_dir.is_empty() {
        v.push(PathBuf::from(&app.cfg.maps_dir));
    }
    v.push(
        PathBuf::from(crate::config::personal_dir())
            .join("My Games")
            .join("Gas Powered Games")
            .join("Supreme Commander Forged Alliance")
            .join("maps"),
    );
    for m in &app.supcom_mounts {
        let d = PathBuf::from(&m.dir);
        if d.is_dir() {
            v.push(d);
        }
    }
    v.retain(|p| p.is_dir());
    v
}

// ==================== БЛУПРИНТЫ: СЧЁТЧИКИ И ПОИСК ====================
// Реализованы БЕЗ новых функций в shim.rs: bpstat идёт через подтверждённый
// shim::execute_lua (Lua сам считает пары, потому что #t/len() не видит
// строковые ключи), bp_scan — через подтверждённый shim::vfs_list.

const BP_COUNT_SCRIPT: &str = r#"
local function count(t) local n = 0 for _ in pairs(t or {}) do n = n + 1 end return n end
local u = count(__bp_units)
local e = count(__bp_effects)
local o = count(__bp_other)
local r = count(__moho_bp_registry)
local b = count(__blueprints)
local probe = rawget(__bp_units or {}, 'ueb0101') and 'ЕСТЬ' or 'НЕТ'
local sample = {}
for k in pairs(__bp_units or {}) do
  sample[#sample + 1] = k
  if #sample >= 12 then break end
end
table.sort(sample)
return string.format(
  'units=%d effects=%d other=%d registry=%d blueprints=%d | probe ueb0101: %s | выборка: %s',
  u, e, o, r, b, probe,
  (#sample > 0) and table.concat(sample, ', ') or '(пусто)')
"#;

fn cmd_bpstat(app: &mut AtlasApp, rt: &std::sync::Arc<shim::MohoRuntime>) {
    match shim::execute_lua(rt, BP_COUNT_SCRIPT) {
        Ok(s) => {
            let mut out = format!("BPSTAT: {}\n", s);
            if s.starts_with("units=0") || s.contains("units=0 ") {
                out.push_str(
                    "  ВНИМАНИЕ: реестр units ПУСТОЙ, несмотря на «423 успешно».\n\
                     \u{2003}423 — это движковые .lua; сами юниты (.bp) в ядро не дошли.\n\
                     \u{2003}Выполни `bp_scan`: он разделит две причины, которые снаружи\n\
                     \u{2003}выглядят одинаково (.bp не смонтированы vs. store_bp их отбраковывает).",
                );
                out.push('\n');
            }
            app.log(ConsoleEntryKind::Output, out);
        }
        Err(e) => app.log(ConsoleEntryKind::Error, format!("bpstat: {}", e)),
    }
}

fn cmd_bp_scan(app: &mut AtlasApp, rt: &std::sync::Arc<shim::MohoRuntime>) {
    let all = shim::vfs_list(rt, "");
    let mut hits: Vec<String> = Vec::new();
    let mut by_ext: Vec<(String, usize)> = Vec::new();
    for k in &all {
        if let Some(dot) = k.rfind('.') {
            let ext = k[dot..].to_lowercase();
            if ext == ".bp" {
                hits.push(k.clone());
            }
            if let Some(slot) = by_ext.iter_mut().find(|(e, _)| *e == ext) {
                slot.1 += 1;
            } else {
                by_ext.push((ext, 1));
            }
        }
    }
    by_ext.sort_by(|a, b| b.1.cmp(&a.1));
    let mut s = String::new();
    s.push_str(&format!("BPSCAN: всего путей в VFS = {}\n", all.len()));
    s.push_str("  расширения (топ-15):\n");
    for (ext, n) in by_ext.iter().take(15) {
        s.push_str(&format!("    {:<10} {}\n", ext, n));
    }
    if hits.is_empty() {
        s.push_str(
            "  .bp В VFS НЕТ НИ ОДНОГО. Это и есть причина пустого реестра:\n\
             \u{2003}gamedata/*.scd (units/effects) не смонтированы. Чинится в\n\
             \u{2003}app.rs::init_moho — но ТОЛЬКО после проверки, что там .scd,\n\
             \u{2003}а не распакованные папки (иначе монтирование молча не поможет).",
        );
        s.push('\n');
    } else {
        s.push_str(&format!("  .bp найдено: {} (первые 40):\n", hits.len()));
        for h in hits.iter().take(40) {
            s.push_str(&format!("    {}\n", h));
        }
        if hits.len() > 40 {
            s.push_str(&format!("    ... ещё {}\n", hits.len() - 40));
        }
        s.push_str(
            "  .bp В VFS ЕСТЬ, но реестр пуст -> проблема в store_bp/resolve_id\n\
             \u{2003}или exec_with_retry молча падает на всех 5 режимах. Тогда:\n\
             \u{2003}`load_bp` покажет bp_ok/bp_fail явно, а `cat <путь.bp>` — что\n\
             \u{2003}реально внутри первого файла.",
        );
        s.push('\n');
    }
    app.log(ConsoleEntryKind::Output, s);
}

// ==================== ДИСПЕТЧЕР ====================

pub fn execute(app: &mut AtlasApp, cmd: &str) {
    let mut it = cmd.splitn(2, char::is_whitespace);
    let head = it.next().unwrap_or("").trim().to_lowercase();
    let arg = it.next().unwrap_or("").trim().to_string();

    match head.as_str() {
        "help" => {
            app.log(ConsoleEntryKind::Output, help_text());
            return;
        }
        "clear" | "cls" => {
            app.console_history.clear();
            return;
        }
        "verbose" => match arg.as_str() {
            "on" => {
                app.verbose_logging = true;
                app.log(ConsoleEntryKind::Debug, "Детальный лог загрузки ВКЛЮЧЕН".into());
            }
            "off" => {
                app.verbose_logging = false;
                app.log(ConsoleEntryKind::Debug, "Детальный лог загрузки ВЫКЛЮЧЕН".into());
            }
            _ => app.log(ConsoleEntryKind::Error, "использование: verbose on|off".into()),
        },
        "status" => {
            let s = format!(
                "MOHO: {}\nLua-файлов: {} | .SCD: {}\nИсточник SupComDataPath: {}\nМонтирований: {} | хуков: {}\nКарт выбрано: {} | путь: {}\nverbose: {} | auto_loaded: {}",
                app.moho_status,
                app.lua_files,
                app.scd_count,
                app.supcom_source,
                app.supcom_mounts.len(),
                app.supcom_hooks.len(),
                if app.map.is_empty() { "—" } else { &app.map },
                app.cfg.game_path,
                app.verbose_logging,
                app.auto_loaded,
            );
            app.log(ConsoleEntryKind::Output, s);
            return;
        }
        "mounts" => {
            let mut s = format!("Источник: {}\n", app.supcom_source);
            if app.supcom_mounts.is_empty() {
                s.push_str("  (нет монтирований)\n");
            }
            for m in &app.supcom_mounts {
                s.push_str(&format!(
                    "  {} -> {} [{}]\n",
                    m.dir,
                    m.mountpoint,
                    if m.contents { "contents" } else { "dir" }
                ));
            }
            app.log(ConsoleEntryKind::Output, s);
            return;
        }
        "path" => {
            if arg.is_empty() {
                app.log(ConsoleEntryKind::Error, "использование: path <каталог>".into());
            } else {
                app.path_input = arg.clone();
                app.apply_game_path();
            }
            return;
        }
        "theme" => {
            app.log(
                ConsoleEntryKind::Debug,
                "theme: переключение — в панели НАСТРОЙКИ (источник истины cfg.theme)".into(),
            );
            return;
        }
        "load_all_lua" => {
            app.auto_load_lua();
            return;
        }
        "test" => {
            app.test_lua();
            app.log(ConsoleEntryKind::Output, app.moho_status.clone());
            return;
        }
        _ => {}
    }

    let rt = match &app.rt {
        Some(rt) => rt.clone(),
        None => {
            app.log(
                ConsoleEntryKind::Error,
                format!("{}: MOHO не инициализирован (path <каталог> игры)", head),
            );
            return;
        }
    };

    match head.as_str() {
        "bpstat" => cmd_bpstat(app, &rt),
        "bp_scan" => cmd_bp_scan(app, &rt),
        "ls" => {
            let files = shim::vfs_list(&rt, &arg);
            let mut s = String::new();
            for f in files.iter().take(500) {
                s.push_str(&format!("  {}\n", f));
            }
            if files.len() > 500 {
                s.push_str(&format!("  ... ещё {}\n", files.len() - 500));
            }
            app.log(ConsoleEntryKind::Output, format!("VFS '{}': {} файлов\n{}", arg, files.len(), s));
        }
        "cat" => {
            if arg.is_empty() {
                app.log(ConsoleEntryKind::Error, "использование: cat <путь>".into());
            } else {
                app.log(ConsoleEntryKind::Output, shim::vfs_cat(&rt, &arg, 80));
            }
        }
        "showline" => {
            let mut w = arg.split_whitespace();
            let path = w.next().unwrap_or("");
            let line: usize = w.next().and_then(|s| s.parse().ok()).unwrap_or(1);
            if path.is_empty() {
                app.log(ConsoleEntryKind::Error, "использование: showline <путь> <номер>".into());
            } else {
                app.log(ConsoleEntryKind::Output, shim::peek_source(&rt, path, line));
            }
        }
        "load_bp" => {
            let (ok, fail) = shim::load_all_blueprints(&rt);
            app.log(ConsoleEntryKind::Output, format!("load_bp: {} ок / {} ошибок", ok, fail));
        }
        "pump" => match shim::execute_lua(&rt, "__moho_pump() return #__moho_threads") {
            Ok(s) => app.log(ConsoleEntryKind::Output, format!("pump: живых тредов = {}", s)),
            Err(e) => app.log(ConsoleEntryKind::Error, format!("pump: {}", e)),
        },
        "bp" => {
            if arg.is_empty() {
                app.log(ConsoleEntryKind::Error, "использование: bp <id>  (напр. bp ueb0101)".into());
            } else {
                let code = format!(
                    "local b = GetUnitBlueprintByName(\"{}\"); if not b then return 'nil' end; return repr(b)",
                    arg
                );
                match shim::execute_lua(&rt, &code) {
                    Ok(s) => app.log(ConsoleEntryKind::Output, format!("bp {}: {}", arg, s)),
                    Err(e) => app.log(ConsoleEntryKind::Error, format!("bp {}: {}", arg, e)),
                }
            }
        }
        "mods" => {
            let files = shim::vfs_list(&rt, "mods");
            let mut s = String::new();
            for f in files.iter().take(200) {
                s.push_str(&format!("  vfs: {}\n", f));
            }
            if !app.cfg.mods_dir.is_empty() {
                if let Ok(rd) = std::fs::read_dir(&app.cfg.mods_dir) {
                    for ent in rd.flatten() {
                        if let Some(n) = ent.path().file_name() {
                            s.push_str(&format!("  fs:  {}\n", n.to_string_lossy()));
                        }
                    }
                }
            }
            app.log(ConsoleEntryKind::Output, format!("mods:\n{}", s));
        }
        "maps" | "maplist" => {
            let recs = crate::moho::map::list_maps(&map_roots_of(app));
            app.log(ConsoleEntryKind::Output, crate::moho::map::report_maps(&recs));
        }
        "mapinfo" => {
            let roots = map_roots_of(app);
            let target = if arg.is_empty() { app.map.clone() } else { arg.clone() };
            if target.is_empty() {
                app.log(
                    ConsoleEntryKind::Error,
                    "mapinfo: карта не выбрана — сначала maps или mapinfo <имя>".into(),
                );
            } else {
                match crate::moho::map::find_scmap(&roots, &target) {
                    Some(p) => match crate::moho::map::probe(&p) {
                        Ok(info) => app.log(ConsoleEntryKind::Output, info.report()),
                        Err(e) => app.log(ConsoleEntryKind::Error, format!("mapinfo {}: {}", target, e)),
                    },
                    None => app.log(
                        ConsoleEntryKind::Error,
                        format!(
                            "mapinfo: .scmap не найден для '{}' (корни: {})",
                            target,
                            roots
                                .iter()
                                .map(|p| p.display().to_string())
                                .collect::<Vec<_>>()
                                .join(", ")
                        ),
                    ),
                }
            }
        }
        "mapchunks" => {
            let roots = map_roots_of(app);
            let target = if arg.is_empty() { app.map.clone() } else { arg.clone() };
            if target.is_empty() {
                app.log(ConsoleEntryKind::Error, "mapchunks: укажи имя карты".into());
            } else {
                match crate::moho::map::find_scmap(&roots, &target) {
                    Some(p) => match crate::moho::map::scan(&p) {
                        Ok(rep) => app.log(ConsoleEntryKind::Output, rep.report()),
                        Err(e) => app.log(ConsoleEntryKind::Error, format!("mapchunks {}: {}", target, e)),
                    },
                    None => app.log(
                        ConsoleEntryKind::Error,
                        format!("mapchunks: .scmap не найден для '{}'", target),
                    ),
                }
            }
        }
        // --- ВНИМАНИЕ: эти два arm'а требуют НОВЫХ полей WorldState
        // (hm_grid / heights / hm_scale). Если shim.rs с ними НЕ применён,
        // получишь E0609 no field 'hm_grid' — тогда УДАЛИ эти два arm'а
        // целиком, и bpstat/bp_scan/mapchunks заработают без них. ---
        "mapload" => {
            let roots = map_roots_of(app);
            let target = if arg.is_empty() { app.map.clone() } else { arg.clone() };
            if target.is_empty() {
                app.log(ConsoleEntryKind::Error, "mapload: укажи имя карты".into());
            } else {
                match crate::moho::map::find_scmap(&roots, &target) {
                    Some(p) => {
                        let scale = rt.world.lock().unwrap().hm_scale;
                        match crate::moho::map::load_terrain(&p, scale) {
                            Ok(t) => {
                                let units = t.units;
                                {
                                    let mut w = rt.world.lock().unwrap();
                                    w.map_size_x = units;
                                    w.map_size_z = units;
                                    w.hm_grid = t.grid;
                                    w.heights = t.heights.clone();
                                    w.hm_scale = t.scale;
                                }
                                app.map = target.clone();
                                app.log(
                                    ConsoleEntryKind::Output,
                                    format!(
                                        "mapload {}: GetMapSize -> ({:.1}, {:.1}) ед. = {:.2} км\n{}",
                                        target,
                                        units,
                                        units,
                                        crate::moho::map::units_to_km(units),
                                        t.stats()
                                    ),
                                );
                            }
                            Err(e) => app.log(ConsoleEntryKind::Error, format!("mapload {}: {}", target, e)),
                        }
                    }
                    None => app.log(
                        ConsoleEntryKind::Error,
                        format!("mapload: .scmap не найден для '{}'", target),
                    ),
                }
            }
        }
        "maphm" => {
            let w = rt.world.lock().unwrap();
            if w.hm_grid < 2 {
                app.log(ConsoleEntryKind::Debug, "maphm: рельеф не загружен -> mapload <имя>".into());
            } else {
                let t = crate::moho::map::Terrain {
                    grid: w.hm_grid,
                    units: w.map_size_x,
                    heights: w.heights.clone(),
                    offset: 0,
                    scale: w.hm_scale,
                    source: "из WorldState".into(),
                };
                drop(w);
                app.log(ConsoleEntryKind::Output, t.stats());
            }
        }
        "trace" => match arg.as_str() {
            "on" => app.log(ConsoleEntryKind::Output, debug::set_trace(&rt, true)),
            "off" => app.log(ConsoleEntryKind::Output, debug::set_trace(&rt, false)),
            "clear" => app.log(ConsoleEntryKind::Output, debug::clear_ring(&rt)),
            "" => app.log(ConsoleEntryKind::Output, debug::render_ring(&rt, 80)),
            n => {
                let lim = n.parse::<usize>().unwrap_or(80);
                app.log(ConsoleEntryKind::Output, debug::render_ring(&rt, lim));
            }
        },
        "mem" => app.log(ConsoleEntryKind::Output, debug::render_mem(&rt)),
        "watch" => app.log(ConsoleEntryKind::Output, debug::render_watch(&rt)),
        "hunt" => app.log(ConsoleEntryKind::Output, debug::hunt(&rt)),
        "dump" => {
            if arg.is_empty() {
                app.log(
                    ConsoleEntryKind::Error,
                    "использование: dump <глобал>  (напр. dump __bp_units)".into(),
                );
            } else {
                app.log(ConsoleEntryKind::Output, debug::render_dump(&rt, &arg));
            }
        }
        "luafile" => {
            if arg.is_empty() {
                app.log(
                    ConsoleEntryKind::Error,
                    "использование: luafile <путь>  (напр. luafile lua/sim/aibrain.lua)".into(),
                );
            } else {
                app.log(ConsoleEntryKind::Output, debug::run_lua_file(&rt, &arg));
            }
        }
        "logsave" => app.log(ConsoleEntryKind::Output, debug::save_log(&rt)),
        _ => match shim::execute_lua(&rt, cmd) {
            Ok(s) => app.log(ConsoleEntryKind::Output, s),
            Err(e) => app.log(ConsoleEntryKind::Error, format!("syntax/runtime error: {}", e)),
        },
    }
}
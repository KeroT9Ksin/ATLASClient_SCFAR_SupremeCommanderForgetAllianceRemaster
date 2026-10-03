use std::path::PathBuf;
use std::sync::Arc;

use crate::config::{self, Config, MountSpec};
use crate::lobby::{Balance, Lobby};
use crate::maps;
use crate::moho::shim::{self, MohoRuntime};
use crate::moho::vfs::Vfs;

#[derive(Clone, Copy, PartialEq)]
pub enum View {
    Game,
    Settings,
    Console,
}

#[derive(Clone, Copy, PartialEq)]
pub enum Stage {
    Closed,
    Balance,
    Config,
    Lobby,
}

#[derive(Clone, Copy, PartialEq)]
pub enum ConsoleEntryKind {
    Input,
    Output,
    Error,
    Debug,
}

pub struct ConsoleEntry {
    pub kind: ConsoleEntryKind,
    pub text: String,
}

pub struct AtlasApp {
    pub view: View,
    pub cfg: Config,
    pub path_input: String,
    pub stage: Stage,
    pub balance: Option<Balance>,
    pub maps: Vec<String>,
    pub map: String,
    pub password: String,
    pub lobby: Option<Lobby>,
    pub chat_input: String,
    pub rt: Option<Arc<MohoRuntime>>,
    pub scd_count: usize,
    pub lua_files: usize,
    pub moho_status: String,
    pub supcom_source: String,
    pub supcom_mounts: Vec<MountSpec>,
    pub supcom_hooks: Vec<String>,
    pub console_input: String,
    pub console_history: Vec<ConsoleEntry>,
    pub auto_loaded: bool,
    pub verbose_logging: bool,
}

// Монтирует содержимое каталога (.scd/.zip/подпапки) в VFS под префиксом mp.
fn mount_contents(vfs: &mut Vfs, dir: &str, mp: &str) {
    let pb = PathBuf::from(dir);
    if let Ok(rd) = std::fs::read_dir(&pb) {
        for e in rd.flatten() {
            let p = e.path();
            let name = e.file_name().to_string_lossy().to_lowercase();
            let base = name
                .trim_end_matches(".scd")
                .trim_end_matches(".zip")
                .to_string();
            if p.is_dir() {
                vfs.mount_extra_dir(&p, &format!("{}{}/", mp, base));
            } else if p
                .extension()
                .map(|x| x.eq_ignore_ascii_case("scd") || x.eq_ignore_ascii_case("zip"))
                .unwrap_or(false)
            {
                vfs.mount_zip(&p, &format!("{}{}/", mp, base));
            }
        }
    }
}

impl AtlasApp {
    pub fn new() -> Self {
        let cfg = config::load();
        let mut app = AtlasApp {
            view: View::Game,
            cfg,
            path_input: String::new(),
            stage: Stage::Closed,
            balance: None,
            maps: Vec::new(),
            map: String::new(),
            password: String::new(),
            lobby: None,
            chat_input: String::new(),
            rt: None,
            scd_count: 0,
            lua_files: 0,
            moho_status: "MOHO не инициализирован".into(),
            supcom_source: "SupComDataPath.lua не читался".into(),
            supcom_mounts: Vec::new(),
            supcom_hooks: Vec::new(),
            console_input: String::new(),
            console_history: Vec::new(),
            auto_loaded: false,
            verbose_logging: false,
        };
        app.path_input = app.cfg.game_path.clone();
        if app.cfg.mods_dir.is_empty() || !PathBuf::from(&app.cfg.mods_dir).is_dir() {
            app.cfg.mods_dir = config::default_mods_dir(&app.cfg.game_path);
        }
        if app.cfg.maps_dir.is_empty() || !PathBuf::from(&app.cfg.maps_dir).is_dir() {
            app.cfg.maps_dir = config::default_maps_dir(&app.cfg.game_path);
        }
        if config::validate(&app.cfg.game_path) {
            app.load_supcom_data_path();
            app.init_moho();
            if !app.auto_loaded {
                app.auto_load_lua();
                app.auto_loaded = true;
            }
        }
        app
    }

    pub fn log(&mut self, kind: ConsoleEntryKind, text: String) {
        self.console_history.push(ConsoleEntry { kind, text });
        crate::app::save_console_log(&self.console_history);
    }

    pub fn auto_load_lua(&mut self) {
        let rt = self.rt.clone();
        let verbose = self.verbose_logging;
        if let Some(rt) = rt {
            self.log(
                ConsoleEntryKind::Output,
                "Запуск массовой загрузки Lua-файлов...".into(),
            );
            let (ok, fail, errs) = shim::load_all_lua(&rt, verbose);
            self.log(
                ConsoleEntryKind::Output,
                format!("ИТОГ: Загружено {} успешно, {} ошибок", ok, fail),
            );
            // ПОКАЗЫВАЕМ ВСЕ ОШИБКИ
            for e in errs {
                self.log(ConsoleEntryKind::Error, e);
            }
        }
    }

    pub fn apply_game_path(&mut self) {
        self.cfg.game_path = self.path_input.clone();
        self.cfg.mods_dir = config::default_mods_dir(&self.cfg.game_path);
        self.cfg.maps_dir = config::default_maps_dir(&self.cfg.game_path);
        config::save(&self.cfg);
        self.load_supcom_data_path();
        self.init_moho();
        self.auto_load_lua();
    }

    pub fn load_supcom_data_path(&mut self) {
        for p in config::supcom_data_path_candidates(&self.cfg.game_path) {
            if let Ok(content) = std::fs::read_to_string(&p) {
                let init_dir = p
                    .parent()
                    .map(|d| d.to_string_lossy().to_string())
                    .unwrap_or_default();
                let data = config::parse_supcom_data(&content, &config::personal_dir(), &init_dir);
                self.supcom_source = p.to_string_lossy().to_string();
                self.supcom_mounts = data.mounts;
                self.supcom_hooks = data.hooks;
                return;
            }
        }
        self.supcom_source = "SupComDataPath.lua не найден".into();
        self.supcom_mounts.clear();
        self.supcom_hooks.clear();
    }

    pub fn pick_supcom_file(&mut self) {
        if let Some(f) = rfd::FileDialog::new()
            .set_title("Выбрать SupComDataPath.lua")
            .pick_file()
        {
            if let Ok(content) = std::fs::read_to_string(&f) {
                let init_dir = f
                    .parent()
                    .map(|d| d.to_string_lossy().to_string())
                    .unwrap_or_default();
                let data = config::parse_supcom_data(&content, &config::personal_dir(), &init_dir);
                self.supcom_source = f.to_string_lossy().to_string();
                self.supcom_mounts = data.mounts;
                self.supcom_hooks = data.hooks;
            }
        }
    }

    pub fn init_moho(&mut self) {
        if !config::validate(&self.cfg.game_path) {
            self.moho_status = "Путь некорректен — инициализация отменена".into();
            return;
        }
        let mut vfs = Vfs::new(&self.cfg.game_path);
        vfs.mount_extra_dir(&PathBuf::from(&self.cfg.game_path), "");
        mount_contents(&mut vfs, &self.cfg.mods_dir, "mods/");
        mount_contents(&mut vfs, &self.cfg.maps_dir, "maps/");
        for m in self.supcom_mounts.clone() {
            if m.mountpoint == "/mods" || m.mountpoint == "/maps" {
                continue;
            }
            let pb = PathBuf::from(&m.dir);
            if m.contents {
                mount_contents(&mut vfs, &m.dir, m.mountpoint.trim_start_matches('/'));
            } else if pb.is_dir() {
                let mp = m.mountpoint.trim_start_matches('/');
                let pref = if mp.is_empty() {
                    String::new()
                } else {
                    format!("{}/", mp)
                };
                vfs.mount_extra_dir(&pb, &pref);
            }
        }
        for h in self.supcom_hooks.clone() {
            let rel = h.trim_start_matches('/').to_string();
            let pb = PathBuf::from(&self.cfg.game_path)
                .join("gamedata")
                .join(&rel);
            if pb.is_dir() {
                vfs.mount_extra_dir(&pb, "");
            }
        }
        self.lua_files = vfs.list("lua").len();
        let vfs = Arc::new(vfs);
        let rt = Arc::new(MohoRuntime::new(vfs.clone()));
        self.maps = maps::scan(&self.cfg.game_path, &vfs);
        if !self.maps.contains(&self.map) {
            self.map = self.maps.first().cloned().unwrap_or_default();
        }
        self.scd_count = vfs.scd_count();
        self.rt = Some(rt);
        self.moho_status = format!(
            "MOHO готов: .scd = {}, Lua = {}, карт = {}, хуки = {:?}",
            self.scd_count, self.lua_files, self.maps.len(), self.supcom_hooks
        );
    }

    pub fn test_lua(&mut self) {
        match &self.rt {
            Some(rt) => match shim::run_self_test(rt) {
                Ok(s) => self.moho_status = s,
                Err(e) => self.moho_status = format!("Ошибка Lua: {}", e),
            },
            None => self.moho_status = "Сначала инициализируйте MOHO".into(),
        }
    }

    pub fn create_lobby(&mut self) {
        let bal = self.balance.unwrap_or(Balance::Faf);
        self.lobby = Some(Lobby::new(
            bal,
            self.map.clone(),
            !self.password.is_empty(),
        ));
        self.stage = Stage::Lobby;
    }
}

// ВНИМАНИЕ (E0624, исправлено): здесь раньше стояли map_roots/cmd_maplist/
// cmd_mapinfo с параметром self, но ВНЕ impl AtlasApp -> компилятор падал.
// Они УДАЛЕНЫ, а не перенесены, потому что НИКТО их не вызывает: console.rs
// имеет собственную свободную map_roots_of(app) и обращается к
// crate::moho::map::* напрямую. Возвращать их сюда бессмысленно.

pub fn save_console_log(history: &[ConsoleEntry]) {
    let mut log_text = String::new();
    for entry in history {
        let prefix = match entry.kind {
            ConsoleEntryKind::Input => "> ",
            ConsoleEntryKind::Output => " ",
            ConsoleEntryKind::Error => "! ",
            ConsoleEntryKind::Debug => "# ",
        };
        log_text.push_str(&format!("{}{}\n", prefix, entry.text));
    }
    let _ = std::fs::write("atlas_console_log.txt", log_text);
}
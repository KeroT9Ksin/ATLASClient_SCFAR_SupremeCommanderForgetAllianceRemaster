use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use crate::theme::ThemeId;

#[derive(Serialize, Deserialize, Clone)]
#[serde(default)]
pub struct Config {
    pub game_path: String,
    pub theme: ThemeId,
    pub mods_dir: String,
    pub maps_dir: String,
}

impl Default for Config {
    fn default() -> Self {
        Config { game_path: String::new(), theme: ThemeId::AtlasDark,
            mods_dir: String::new(), maps_dir: String::new() }
    }
}

pub fn file() -> PathBuf { PathBuf::from("atlas_config.json") }

pub fn load() -> Config {
    std::fs::read_to_string(file()).ok()
        .and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default()
}

pub fn save(c: &Config) {
    let _ = std::fs::write(file(), serde_json::to_string_pretty(c).unwrap_or_default());
}

pub fn validate(p: &str) -> bool {
    let pb = PathBuf::from(p);
    pb.join("ForgedAlliance.exe").exists() || pb.join("gamedata").exists()
}

pub fn personal_dir() -> String {
    std::env::var("USERPROFILE")
        .map(|up| PathBuf::from(up).join("Documents").to_string_lossy().to_string())
        .unwrap_or_default()
}

// По умолчанию — из указанного пути SCFA; запасной вариант — Personal
pub fn default_mods_dir(game_path: &str) -> String {
    let gp = PathBuf::from(game_path);
    for c in [gp.join("mods"), gp.join("gamedata").join("mods")] {
        if c.is_dir() { return c.to_string_lossy().to_string(); }
    }
    PathBuf::from(personal_dir()).join("My Games").join("Gas Powered Games")
        .join("Supreme Commander Forged Alliance").join("mods").to_string_lossy().to_string()
}

pub fn default_maps_dir(game_path: &str) -> String {
    let gp = PathBuf::from(game_path);
    let c = gp.join("maps");
    if c.is_dir() { return c.to_string_lossy().to_string(); }
    PathBuf::from(personal_dir()).join("My Games").join("Gas Powered Games")
        .join("Supreme Commander Forged Alliance").join("maps").to_string_lossy().to_string()
}

#[derive(Clone)]
pub struct MountSpec { pub contents: bool, pub dir: String, pub mountpoint: String }
#[derive(Clone)]
pub struct SupComData { pub mounts: Vec<MountSpec>, pub hooks: Vec<String> }

pub fn supcom_data_path_candidates(game_path: &str) -> Vec<PathBuf> {
    let mut v = Vec::new();
    let gp = PathBuf::from(game_path);
    v.push(gp.join("bin").join("SupComDataPath.lua"));
    v.push(gp.join("SupComDataPath.lua"));
    v.push(gp.join("gamedata").join("SupComDataPath.lua"));
    if let Ok(local) = std::env::var("LOCALAPPDATA") {
        v.push(PathBuf::from(local).join("Gas Powered Games")
            .join("Supreme Commander Forged Alliance").join("SupComDataPath.lua"));
    }
    v
}

fn find_close_paren(s: &str, from: usize) -> Option<usize> {
    let mut depth = 1;
    for (i, c) in s[from..].char_indices() {
        match c {
            '(' => depth += 1,
            ')' => { depth -= 1; if depth == 0 { return Some(from + i); } }
            _ => {}
        }
    }
    None
}

fn split_top_comma(s: &str) -> Option<(String, String)> {
    let mut depth = 0; let mut in_str = false; let mut last = None;
    for (i, c) in s.char_indices() {
        match c {
            '\'' | '"' => in_str = !in_str,
            '(' => depth += 1,
            ')' => depth -= 1,
            ',' if depth == 0 && !in_str => last = Some(i),
            _ => {}
        }
    }
    let i = last?;
    Some((s[..i].trim().to_string(),
          s[i + 1..].trim().trim_matches(|c| c == '\'' || c == '"').to_string()))
}

fn normalize_path(s: &str) -> String {
    let mut stack: Vec<String> = Vec::new();
    for comp in s.replace('\\', "/").split('/') {
        match comp {
            "" | "." => {}
            ".." => { stack.pop(); }
            c => stack.push(c.to_string()),
        }
    }
    stack.join("/")
}

fn eval_expr(expr: &str, personal: &str, init_dir: &str) -> String {
    let mut out = String::new();
    for part in expr.split("..") {
        let p = part.trim();
        if p.is_empty() { continue; }
        let s = if p.starts_with('\'') || p.starts_with('"') {
            p.trim_matches(|c| c == '\'' || c == '"').to_string()
        } else if p.contains("SHGetFolderPath") {
            personal.to_string()
        } else if p.contains("InitFileDir") {
            init_dir.to_string()
        } else {
            continue;
        };
        // ИСПРАВЛЕНО: добавляем разделитель, если его нет на стыке частей
        if !out.is_empty() && !out.ends_with('/') && !out.ends_with('\\')
            && !s.is_empty() && !s.starts_with('/') && !s.starts_with('\\') {
            out.push('/');
        }
        out.push_str(&s);
    }
    normalize_path(&out)
}

pub fn parse_supcom_data(content: &str, personal: &str, init_dir: &str) -> SupComData {
    // ИСПРАВЛЕНО: вырезаем Lua-комментарии ДО разбора, чтобы не хватать "-- mount_dir(...)"
    let clean: String = content.lines()
        .map(|l| match l.find("--") { Some(i) => &l[..i], None => l })
        .collect::<Vec<_>>().join("\n");

    let mut out = SupComData { mounts: Vec::new(), hooks: Vec::new() };
    for (needle, contents) in [("mount_contents(", true), ("mount_dir(", false)] {
        let mut start = 0;
        while let Some(pos) = clean[start..].find(needle) {
            let abs = start + pos + needle.len();
            match find_close_paren(&clean, abs) {
                Some(close) => {
                    if let Some((dir_expr, mp)) = split_top_comma(&clean[abs..close]) {
                        let dir = eval_expr(&dir_expr, personal, init_dir);
                        // ИСПРАВЛЕНО: отбрасываем мусорные/пустые записи и дубликаты
                        if !dir.is_empty() && mp.starts_with('/')
                            && !out.mounts.iter().any(|m| m.dir == dir && m.mountpoint == mp) {
                            out.mounts.push(MountSpec { contents, dir, mountpoint: mp });
                        }
                    }
                    start = close + 1;
                }
                None => break,
            }
        }
    }
    if let Some(hpos) = clean.find("hook") {
        if let Some(ob) = clean[hpos..].find('{') {
            let ob_abs = hpos + ob;
            if let Some(cb) = clean[ob_abs..].find('}') {
                for tok in clean[ob_abs + 1..ob_abs + cb].split(',') {
                    let t = tok.trim().trim_matches(|c| c == '\'' || c == '"').to_string();
                    if !t.is_empty() && t.starts_with('/') && !out.hooks.contains(&t) {
                        out.hooks.push(t);
                    }
                }
            }
        }
    }
    out
}
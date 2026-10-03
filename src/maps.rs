// Карты: распакованная папка maps + записи внутри maps.scd через VFS.
use crate::moho::vfs::Vfs;

pub fn scan(game_path: &str, vfs: &Vfs) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let root = std::path::Path::new(game_path).join("maps");
    if let Ok(rd) = std::fs::read_dir(root) {
        for e in rd.flatten() {
            if e.path().is_dir() {
                out.push(e.file_name().to_string_lossy().to_string());
            }
        }
    }
    for key in vfs.list("maps/") {
        if key.ends_with(".scmap") {
            if let Some(dir) = key.trim_start_matches("maps/").split('/').next() {
                let d = dir.to_uppercase();
                if !d.is_empty() && !out.contains(&d) { out.push(d); }
            }
        }
    }
    out.sort();
    out.dedup();
    out
}
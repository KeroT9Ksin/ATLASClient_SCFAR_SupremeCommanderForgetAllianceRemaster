use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

pub struct ZipMount {
    pub name: String,
    pub prefix: String,
    arc: Mutex<zip::ZipArchive<std::fs::File>>,
    index: HashMap<String, usize>,
}

pub enum Mount {
    Dir { path: PathBuf, prefix: String },
    Zip(ZipMount),
}

pub struct Vfs { pub mounts: Vec<Mount> }

fn norm(s: &str) -> String {
    s.replace('\\', "/").trim_start_matches('/').to_lowercase()
}

/// Удаляет UTF-8 BOM (\u{feff}) из начала строки
fn strip_bom(s: String) -> String {
    if s.starts_with('\u{feff}') {
        s[3..].to_string()
    } else {
        s
    }
}

impl Vfs {
    pub fn new(game_path: &str) -> Self {
        let mut vfs = Vfs { mounts: Vec::new() };
        let gd = Path::new(game_path).join("gamedata");
        if let Ok(rd) = std::fs::read_dir(&gd) {
            let mut scds: Vec<PathBuf> = rd.flatten().map(|e| e.path())
                .filter(|p| p.extension().map(|x| x.eq_ignore_ascii_case("scd")).unwrap_or(false))
                .collect();
            scds.sort();
            for p in scds { vfs.mount_zip(&p, ""); }
        }
        vfs.mount_dir(&gd, "");
        vfs
    }

    pub fn scd_count(&self) -> usize {
        self.mounts.iter().filter(|m| matches!(m, Mount::Zip(_))).count()
    }

    pub fn mount_zip(&mut self, path: &Path, prefix: &str) {
        if !path.is_file() { return; }
        if let Ok(f) = std::fs::File::open(path) {
            if let Ok(mut z) = zip::ZipArchive::new(f) {
                let mut index = HashMap::new();
                for i in 0..z.len() {
                    if let Ok(e) = z.by_index(i) {
                        if !e.is_dir() { index.insert(norm(e.name()), i); }
                    }
                }
                if let Ok(f2) = std::fs::File::open(path) {
                    if let Ok(z2) = zip::ZipArchive::new(f2) {
                        self.mounts.push(Mount::Zip(ZipMount {
                            name: path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default(),
                            prefix: norm(prefix),
                            arc: Mutex::new(z2), index,
                        }));
                    }
                }
            }
        }
    }

    pub fn mount_dir(&mut self, p: &Path, prefix: &str) {
        if p.is_dir() {
            self.mounts.push(Mount::Dir { path: p.to_path_buf(), prefix: norm(prefix) });
        }
    }

    pub fn mount_extra_dir(&mut self, p: &Path, prefix: &str) { self.mount_dir(p, prefix); }

    pub fn read(&self, name: &str) -> Option<Vec<u8>> {
        let key = norm(name);
        for m in self.mounts.iter().rev() {
            match m {
                Mount::Dir { path, prefix } => {
                    if let Some(rel) = key.strip_prefix(prefix) {
                        let p = path.join(rel);
                        if p.is_file() { return std::fs::read(p).ok(); }
                    }
                }
                Mount::Zip(z) => {
                    if let Some(rel) = key.strip_prefix(&z.prefix) {
                        if let Some(&i) = z.index.get(rel) {
                            if let Ok(mut e) = z.arc.lock().unwrap().by_index(i) {
                                let mut buf = Vec::new();
                                if e.read_to_end(&mut buf).is_ok() { return Some(buf); }
                            }
                        }
                    }
                }
            }
        }
        None
    }

        pub fn read_string(&self, name: &str) -> Option<String> {
        self.read(name).map(|b| {
            let s = String::from_utf8_lossy(&b).to_string();
            if s.starts_with('\u{feff}') { s[3..].to_string() } else { s }
        })
    }

    pub fn exists(&self, name: &str) -> bool { self.read(name).is_some() }

    pub fn list(&self, prefix: &str) -> Vec<String> {
        let p = norm(prefix);
        let mut out: Vec<String> = Vec::new();
        for m in &self.mounts {
            match m {
                Mount::Dir { path, prefix: mp } => walk(path, path, mp, &p, &mut out),
                Mount::Zip(z) => for k in z.index.keys() {
                    let full = format!("{}{}", z.prefix, k);
                    if full.starts_with(&p) && !out.contains(&full) { out.push(full); }
                },
            }
        }
        out.sort();
        out
    }
}

fn walk(base: &Path, cur: &Path, mp: &str, want: &str, out: &mut Vec<String>) {
    if let Ok(rd) = std::fs::read_dir(cur) {
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() { walk(base, &p, mp, want, out); }
            else if let Ok(rel) = p.strip_prefix(base) {
                let key = format!("{}{}", mp, rel.to_string_lossy().replace('\\', "/")).to_lowercase();
                if key.starts_with(want) && !out.contains(&key) { out.push(key); }
            }
        }
    }
}
// src/moho/map.rs
// КАРТЫ: классификация, разбор заголовка .scmap, СКАН СТРУКТУРЫ и поиск
// полотна высот. Версия после первого реального дампа (perftest.scmap).
//
// ЧТО ТЕПЕРЬ ФАКТ, А ЧТО ГИПОТЕЗА (границы названы в каждом отчёте):
//  ФАКТЫ из дампа perftest.scmap:
//    0x00 magic "Map\x1A"; 0x04 u32=2; 0x08 маркер 0xBEEFEFED; 0x0C u32=2;
//    0x10/0x14 f32=1024.0; 0x18 f32=0.0; 0x1C u32=8388608; 0x20 u16=4;
//    0x22 magic "DDS " (dwSize=124, flags=0x1007, 256x256) — поля DDS сошлись
//    с эталоном, значит смещения прочитаны верно.
//  ФАКТЫ из документации формата:
//    1 игровая единица = 19.53125 м (5x5 км = 256 единиц) [FAF wiki,
//    GPG-Map-Editor]; heightmap = (width+1)x(height+1), 16 бит, single channel
//    [SupCom wiki, Creating mountains] -> для 1024 единиц сетка 1025x1025.
//  ГИПОТЕЗЫ (помечены в отчёте, проверяются mapchunks):
//    смысл 0x1C=8388608: A) 2048^2*2 (2 сэмпла/единицу) или B) 1024^2*8.
//    масштаб u16 -> метры.
//
// ДВА МЕТОДИЧЕСКИХ ФИКСА ПОСЛЕВЧЕРАШНЕЙ ИТЕРЦИИ:
//  * magic ищем явным "Map\x1A" и порогом >=3 буквенных: прошлый порог 4
//    ОТБРАСОВЫВАЛ НАСТОЯЩИЙ magic (тот же класс ошибки, что фильтр, прячущий
//    падения UI).
//  * чтение .lua -> from_utf8_lossy. read_to_string падает на не-UTF-8 и
//    заставлял отчёт врать ("_script.lua отсутствует" при script=Y) и терять
//    scenario_info.type у части карт.

use std::fs;
use std::path::{Path, PathBuf};

// ==================== МАСШТАБ (из документации) ====================

/// Игровых единиц в 1 км: 5 км = 256 единиц [FAF wiki GPG-Map-Editor].
pub const UNITS_PER_KM: f32 = 256.0 / 5.0;

pub fn units_to_km(u: f32) -> f32 {
    u / UNITS_PER_KM
}

/// Правило сетки: width+1 сэмплов на width единиц [SupCom wiki Creating mountains].
pub fn grid_for(units: f32) -> u32 {
    if units <= 0.0 {
        0
    } else {
        units.round() as u32 + 1
    }
}

// ==================== ЧТЕНИЕ ====================

fn read_lossy(path: &Path) -> Option<String> {
    let b = fs::read(path).ok()?;
    Some(String::from_utf8_lossy(&b).to_string())
}

fn file_size(path: &Path) -> Option<u64> {
    fs::metadata(path).ok().map(|m| m.len())
}

fn find_string_field(src: &str, key: &str) -> Option<String> {
    let bytes = src.as_bytes();
    let kb = key.as_bytes();
    let mut from = 0usize;
    while let Some(rel) = src[from..].find(key) {
        let start = from + rel;
        let ok_left = start == 0
            || !(bytes[start - 1].is_ascii_alphanumeric() || bytes[start - 1] == b'_');
        let after = start + kb.len();
        let ok_right = after >= bytes.len()
            || !(bytes[after].is_ascii_alphanumeric() || bytes[after] == b'_');
        if ok_left && ok_right {
            let mut i = after;
            while i < bytes.len() && (bytes[i] == b' ' || bytes[i] == b'\t') {
                i += 1;
            }
            if i < bytes.len() && bytes[i] == b'=' {
                i += 1;
                while i < bytes.len() && (bytes[i] == b' ' || bytes[i] == b'\t') {
                    i += 1;
                }
                if i < bytes.len() && (bytes[i] == b'\'' || bytes[i] == b'"') {
                    let q = bytes[i];
                    i += 1;
                    let vs = i;
                    while i < bytes.len() && bytes[i] != q && bytes[i] != b'\n' {
                        i += 1;
                    }
                    if i < bytes.len() && bytes[i] == q && i > vs {
                        return Some(src[vs..i].to_string());
                    }
                }
            }
        }
        from = start + 1;
    }
    None
}

fn has_word(src: &str, word: &str) -> bool {
    let wb = word.as_bytes();
    let bytes = src.as_bytes();
    let mut from = 0usize;
    while let Some(rel) = src[from..].find(word) {
        let s = from + rel;
        let e = s + wb.len();
        let ok_l = s == 0 || !(bytes[s - 1].is_ascii_alphanumeric() || bytes[s - 1] == b'_');
        let ok_r = e >= bytes.len() || !(bytes[e].is_ascii_alphanumeric() || bytes[e] == b'_');
        if ok_l && ok_r {
            return true;
        }
        from = s + 1;
    }
    false
}

// ==================== КЛАССИФИКАЦИЯ ====================

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum MapKind {
    Campaign,
    Coop,
    Skirmish,
    Adaptive,
    Tutorial,
    Multiplayer,
    Special,
    Unknown,
}

impl MapKind {
    pub fn label(&self) -> &'static str {
        match self {
            MapKind::Campaign => "КАМПАНИЯ",
            MapKind::Coop => "КООП",
            MapKind::Skirmish => "СХВАТКА",
            MapKind::Adaptive => "АДАПТИВНАЯ (схватка)",
            MapKind::Tutorial => "ТУТОРИАЛ",
            MapKind::Multiplayer => "МУЛЬТИПЛЕЕР",
            MapKind::Special => "СПЕЦ (perftest/внутр.)",
            MapKind::Unknown => "НЕ ИЗВЕСТНО",
        }
    }
    pub fn is_solo(&self) -> bool {
        matches!(self, MapKind::Campaign | MapKind::Coop | MapKind::Tutorial)
    }
    pub fn is_skirmish(&self) -> bool {
        matches!(
            self,
            MapKind::Skirmish | MapKind::Adaptive | MapKind::Multiplayer
        )
    }
}

#[derive(Clone, Debug)]
pub struct MapRecord {
    pub dir: String,
    pub path: PathBuf,
    pub kind: MapKind,
    pub reasons: Vec<String>,
    pub has_scmap: bool,
    pub scmap_bytes: Option<u64>,
    pub has_script: bool,
    pub has_save: bool,
    /// Отдельно от has_*: файл может существовать, но не читаться. Прошлая
    /// версия путала эти состояния и врала в reasons.
    pub scenario_readable: bool,
    pub script_readable: bool,
    pub scenario_name: Option<String>,
    pub scenario_type: Option<String>,
    pub scenario_version: Option<String>,
    pub map_size: Option<String>,
}

pub fn classify(map_dir: &Path) -> MapRecord {
    let dir = map_dir
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| map_dir.to_string_lossy().to_string());

    let mut scmap: Option<PathBuf> = None;
    let mut scenario: Option<PathBuf> = None;
    let mut script: Option<PathBuf> = None;
    let mut save: Option<PathBuf> = None;
    if let Ok(rd) = fs::read_dir(map_dir) {
        for ent in rd.flatten() {
            let p = ent.path();
            if !p.is_file() {
                continue;
            }
            let ext = p
                .extension()
                .map(|e| e.to_string_lossy().to_lowercase())
                .unwrap_or_default();
            let low = p
                .file_name()
                .map(|s| s.to_string_lossy().to_lowercase())
                .unwrap_or_default();
            if ext == "scmap" && scmap.is_none() {
                scmap = Some(p.clone());
            } else if low.ends_with("_scenario.lua") {
                scenario = Some(p.clone());
            } else if low.ends_with("_script.lua") {
                script = Some(p.clone());
            } else if low.ends_with("_save.lua") {
                save = Some(p.clone());
            }
        }
    }

    let has_scmap = scmap.is_some();
    let scmap_bytes = scmap.as_ref().and_then(|p| file_size(p));
    let has_script = script.is_some();
    let has_save = save.is_some();

    // lossy: не-UTF-8 больше не выглядит как «файла нет».
    let scen_text = scenario.as_ref().and_then(|p| read_lossy(p));
    let scr_text = script.as_ref().and_then(|p| read_lossy(p));
    let scenario_readable = scen_text.is_some();
    let script_readable = scr_text.is_some();

    let scenario_name = scen_text.as_deref().and_then(|s| find_string_field(s, "name"));
    let scenario_type = scen_text.as_deref().and_then(|s| find_string_field(s, "type"));
    let scenario_version = scen_text.as_deref().and_then(|s| find_string_field(s, "version"));
    let map_size = scen_text
        .as_deref()
        .and_then(|s| find_string_field(s, "Size"))
        .or_else(|| scen_text.as_deref().and_then(|s| find_string_field(s, "size")));

    let mut reasons: Vec<String> = Vec::new();
    let mut kind = MapKind::Unknown;

    if scenario.is_some() && !scenario_readable {
        reasons.push("_scenario.lua есть, но не читается (не-UTF-8?)".into());
    }

    // Приоритет 1: scenario_info.type — читает сама игра.
    if let Some(t) = scenario_type.as_deref() {
        match t.to_lowercase().as_str() {
            "campaign" => kind = MapKind::Campaign,
            "coop" => kind = MapKind::Coop,
            "tutorial" => kind = MapKind::Tutorial,
            "skirmish" => kind = MapKind::Skirmish,
            "adaptive" | "random" => kind = MapKind::Adaptive,
            "multiplayer" => kind = MapKind::Multiplayer,
            // Значение из реального прогона (PerfTest) — добавлено по факту,
            // а не по догадке.
            "special" => kind = MapKind::Special,
            other => {
                reasons.push(format!(
                    "scenario_info.type == '{}' (нераспознанное значение)",
                    other
                ));
            }
        }
        if kind != MapKind::Unknown {
            reasons.push(format!("scenario_info.type == '{}'", t));
        }
    }

    // Приоритет 2: маркеры SCAR/миссии в _script.lua.
    if let Some(s) = scr_text.as_deref() {
        let mut marks: Vec<&str> = Vec::new();
        for w in [
            "ImportSCAR",
            "ScenarioInit",
            "OnScenarioInit",
            "ScenarioOnStart",
            "CampaignManager",
            "briefing",
            "Briefing",
            "Operation",
        ] {
            if has_word(s, w) {
                marks.push(w);
            }
        }
        if !marks.is_empty() {
            reasons.push(format!("_script.lua: маркеры миссии ({})", marks.join(", ")));
            if kind == MapKind::Unknown {
                kind = MapKind::Campaign;
            }
        }
        if has_word(s, "Adaptive") || has_word(s, "RandomMap") {
            reasons.push("_script.lua: ссылка на генерацию (Adaptive/RandomMap)".into());
            if kind == MapKind::Unknown || kind == MapKind::Skirmish {
                kind = MapKind::Adaptive;
            }
        }
        if has_word(s, "Tutorial") && kind == MapKind::Unknown {
            reasons.push("_script.lua: ссылка на туториал".into());
            kind = MapKind::Tutorial;
        }
    } else if !has_script {
        reasons.push("_script.lua отсутствует".into());
        if kind == MapKind::Unknown {
            kind = MapKind::Skirmish;
        }
    }

    // Приоритет 3: имя папки — только намёк, помечен как намёк.
    let dl = dir.to_lowercase();
    let mut hints: Vec<&str> = Vec::new();
    for (pat, hint) in [
        ("campaign", "имя: 'campaign'"),
        ("operation", "имя: 'operation'"),
        ("mission", "имя: 'mission'"),
        ("coop", "имя: 'coop'"),
        ("tutorial", "имя: 'tutorial'"),
        ("adaptive", "имя: 'adaptive'"),
        ("perftest", "имя: 'perftest'"),
        ("scmp_", "имя в формате SCMP_###"),
        ("x1ca", "имя в формате X1CA_### (кампания)"),
        ("x1mp", "имя в формате X1MP_### (мультиплеер)"),
    ] {
        if dl.contains(pat) {
            hints.push(hint);
        }
    }
    if !hints.is_empty() {
        reasons.push(format!("намёки по имени: {}", hints.join("; ")));
    }

    if reasons.is_empty() {
        reasons.push("ни один признак не сработал".into());
    }

    MapRecord {
        dir,
        path: map_dir.to_path_buf(),
        kind,
        reasons,
        has_scmap,
        scmap_bytes,
        has_script,
        has_save,
        scenario_readable,
        script_readable,
        scenario_name,
        scenario_type,
        scenario_version,
        map_size,
    }
}

pub fn list_maps(roots: &[PathBuf]) -> Vec<MapRecord> {
    let mut seen: Vec<String> = Vec::new();
    let mut out: Vec<MapRecord> = Vec::new();
    for root in roots {
        if !root.is_dir() {
            continue;
        }
        let rd = match fs::read_dir(root) {
            Ok(r) => r,
            Err(_) => continue,
        };
        for ent in rd.flatten() {
            let p = ent.path();
            if !p.is_dir() {
                continue;
            }
            let looks_like_map = p
                .read_dir()
                .map(|rd2| {
                    rd2.flatten().any(|e| {
                        let n = e.file_name().to_string_lossy().to_lowercase();
                        n.ends_with(".scmap") || n.ends_with("_scenario.lua")
                    })
                })
                .unwrap_or(false);
            if !looks_like_map {
                continue;
            }
            let name = p
                .file_name()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_default();
            // Дедуп по lower: PERFTEST и PerfTest — одна карта (в прогоне
            // maps::scan отдал "PERFTEST", а скан папок — "PerfTest").
            let low = name.to_lowercase();
            if seen.iter().any(|s| *s == low) {
                continue;
            }
            seen.push(low);
            out.push(classify(&p));
        }
    }
    out.sort_by(|a, b| a.kind.label().cmp(b.kind.label()).then(a.dir.cmp(&b.dir)));
    out
}

pub fn report_maps(recs: &[MapRecord]) -> String {
    if recs.is_empty() {
        return "maplist: карт не найдено.\n\
                Причины по убыванию вероятности: (1) карты внутри .scd-архивов, а не\n\
                в папках — тогда нужен разбор через VFS; (2) неверные корни — покажи\n\
                `mounts` и `status`; (3) путь игры не задан — `path <каталог>`."
            .to_string();
    }
    let mut s = String::new();
    let groups: [(MapKind, &str); 8] = [
        (MapKind::Campaign, "=== КАМПАНИЯ / МИССИИ ==="),
        (MapKind::Coop, "=== КООП ==="),
        (MapKind::Tutorial, "=== ТУТОРИАЛЫ ==="),
        (MapKind::Skirmish, "=== СХВАТКИ ==="),
        (MapKind::Adaptive, "=== АДАПТИВНЫЕ (генерируемые схватки) ==="),
        (MapKind::Multiplayer, "=== МУЛЬТИПЛЕЕР ==="),
        (MapKind::Special, "=== СПЕЦ ==="),
        (MapKind::Unknown, "=== НЕ КЛАССИФИЦИРОВАНЫ ==="),
    ];
    let mut solo = 0usize;
    let mut skir = 0usize;
    for (k, header) in groups.iter() {
        let rows: Vec<&MapRecord> = recs.iter().filter(|r| r.kind == *k).collect();
        if rows.is_empty() {
            continue;
        }
        if k.is_solo() {
            solo += rows.len();
        }
        if k.is_skirmish() {
            skir += rows.len();
        }
        s.push_str(&format!("\n{}\n", header));
        for r in rows {
            s.push_str(&format!(
                "  {:<26} {:<22} scmap={} script={} save={}{}{}\n",
                r.dir,
                r.kind.label(),
                if r.has_scmap { "Y" } else { "N" },
                if r.has_script {
                    if r.script_readable { "Y" } else { "Y!" }
                } else {
                    "N"
                },
                if r.has_save { "Y" } else { "N" },
                r.scenario_name
                    .as_deref()
                    .map(|n| format!(" name=\"{}\"", n))
                    .unwrap_or_default(),
                r.map_size
                    .as_deref()
                    .map(|z| format!(" size={}", z))
                    .unwrap_or_default(),
            ));
            for reason in &r.reasons {
                s.push_str(&format!("      - {}\n", reason));
            }
        }
    }
    s.push_str(&format!(
        "\nИТОГО: карт={} | КАМПАНИИ/МИССИИ={} | СХВАТКИ={} | неясно={}\n\
         Легенда: script=Y! = файл есть, но не читается (не-UTF-8).\n\
         Приоритет имеет scenario_info.type (его читает сама игра); намёки по\n\
         имени подписаны как намёки и никогда его не перекрывают.\n",
        recs.len(),
        solo,
        skir,
        recs.iter().filter(|r| r.kind == MapKind::Unknown).count()
    ));
    s
}

// ==================== ЗАГОЛОВОК .scmap ====================

pub struct ProbeInfo {
    pub path: PathBuf,
    pub size: u64,
    pub head: Vec<u8>,
    pub facts: Vec<String>,
    pub hypotheses: Vec<String>,
    /// Измеренный размер карты в игровых единицах (0x10/0x14), если найден.
    pub units: Option<(f32, f32)>,
    pub grid: u32,
}

fn ascii_of(b: u8) -> char {
    if b.is_ascii_graphic() || b == b' ' {
        b as char
    } else {
        '.'
    }
}

fn hexdump(head: &[u8], rows: usize) -> String {
    let mut s = String::new();
    for (row, chunk) in head.chunks(16).take(rows).enumerate() {
        let hex: String = chunk.iter().map(|b| format!("{:02X} ", b)).collect();
        let asc: String = chunk.iter().map(|b| ascii_of(*b)).collect();
        s.push_str(&format!("    +{:#06X}  {:<48} |{}|\n", row * 16, hex, asc));
    }
    s
}

fn f32_at(b: &[u8], off: usize) -> Option<f32> {
    if off + 4 > b.len() {
        return None;
    }
    Some(f32::from_le_bytes([b[off], b[off + 1], b[off + 2], b[off + 3]]))
}

fn u32_at(b: &[u8], off: usize) -> Option<u32> {
    if off + 4 > b.len() {
        return None;
    }
    Some(u32::from_le_bytes([b[off], b[off + 1], b[off + 2], b[off + 3]]))
}

/// Разложение u32 на «сеткоподобные» произведения: ищем N^2*bpp и N*N*8.
fn factor_block(v: u64) -> Vec<String> {
    let mut out = Vec::new();
    for bpp in [1u64, 2, 3, 4, 8] {
        if v % bpp != 0 {
            continue;
        }
        let cells = v / bpp;
        let side = (cells as f64).sqrt();
        let isq = side.round() as u64;
        if isq * isq == cells && isq > 16 {
            out.push(format!("{} = {}^2 x {} (полотно {}x{}, {} байт/сэмпл)", v, isq, bpp, isq, isq, bpp));
        }
    }
    // variant: N x M с N=2*M или M=2*N (текстуры 2:1)
    for n in [512u64, 1024, 2048, 4096] {
        for bpp in [1u64, 2, 4] {
            if v % (n * bpp) == 0 {
                let m = v / (n * bpp);
                if m >= 16 && m <= 8192 && m != n {
                    out.push(format!("{} = {} x {} x {} байт", v, n, m, bpp));
                }
            }
        }
    }
    out.sort();
    out.dedup();
    out.truncate(6);
    out
}

pub fn probe(path: &Path) -> Result<ProbeInfo, String> {
    let bytes = fs::read(path).map_err(|e| format!("не читается: {}", e))?;
    if bytes.len() < 64 {
        return Err(format!("файл слишком мал ({} байт) — это не .scmap", bytes.len()));
    }
    let head: Vec<u8> = bytes[..64].to_vec();
    let mut facts: Vec<String> = Vec::new();
    let mut hyps: Vec<String> = Vec::new();
    let mut units: Option<(f32, f32)> = None;
    let mut grid = 0u32;

    // --- magic: явная проверка + порог 3 (прошлый порог 4 ОТБРОСИЛ настоящий) ---
    if head.len() >= 4 && &head[0..4] == b"Map\x1A" {
        facts.push("0x0000: magic \"Map\\x1A\" — сигнатура SCMAP подтверждена".into());
    } else {
        let alpha = head[..8].iter().filter(|b| b.is_ascii_alphabetic()).count();
        if alpha >= 3 {
            let a: String = head[..4].iter().map(|b| ascii_of(*b)).collect();
            facts.push(format!("0x0000: ASCII-префикс \"{}\" ({} буквенных) — вероятный magic", a.trim(), alpha));
        } else {
            facts.push("0x0000: буквенных байт < 3 — magic не найден, заголовок начинается с чисел".into());
        }
    }

    // --- известные смещения из реального дампа ---
    for (off, name) in [(4usize, "u32 @0x04"), (0xC, "u32 @0x0C")] {
        if let Some(v) = u32_at(&head, off) {
            facts.push(format!("{}: {} — кандидат на версию/счётчик", name, v));
        }
    }
    if let Some(v) = u32_at(&head, 8) {
        if v == 0xBEEF_EFED || v == 0xDEAD_BEEF || v == 0xFEED_FACE {
            facts.push(format!("0x0008: маркер {:#010X} — константа формата, не данные", v));
        }
    }

    // --- размер карты в игровых единицах ---
    let mut pair: Vec<(usize, f32)> = Vec::new();
    for off in [0x10usize, 0x14, 0x18, 0x1C] {
        if let Some(f) = f32_at(&head, off) {
            if f.is_finite() && f > 0.0 && (f - f.round()).abs() < 1e-6 && f <= 8192.0 {
                pair.push((off, f));
            }
        }
    }
    if pair.len() >= 2 {
        let (o1, v1) = pair[0];
        let (o2, v2) = pair[1];
        units = Some((v1, v2));
        grid = grid_for(v1);
        facts.push(format!(
            "0x{:04X}/0x{:04X}: f32 = {} / {} — РАЗМЕР КАРТЫ в игровых единицах",
            o1, o2, v1, v2
        ));
        facts.push(format!(
            "  -> {:.2} x {:.2} км (1 ед. = 19.53125 м: 5 км = 256 ед. [FAF wiki GPG-Map-Editor])",
            units_to_km(v1),
            units_to_km(v2)
        ));
        facts.push(format!(
            "  -> heightmap = {}x{} (правило width+1 [SupCom wiki Creating mountains]), 16 бит raw = {} байт",
            grid,
            grid,
            (grid as u64) * (grid as u64) * 2
        ));
    } else {
        hyps.push("пары целочисленных f32 в 0x10..0x1F не найдено — размер карты надо искать в другом месте".into());
    }

    // --- 0x1C: длина блока ---
    if let Some(v) = u32_at(&head, 0x1C) {
        if v > 1024 {
            let f = factor_block(v as u64);
            facts.push(format!("0x001C: u32 = {} — кандидат на ДЛИНУ БЛОКА", v));
            for line in f {
                facts.push(format!("  разложение: {}", line));
            }
            if let Some(g) = grid.checked_mul(grid).map(|x| x as u64 * 2) {
                if v as u64 == g {
                    facts.push(format!("  СОВПАЛО с heightmap {}x{} 16-бит ({} байт)", grid, grid, g));
                }
            }
            hyps.push(
                "смысл 0x1C не определён одним дампом: смотри `mapchunks` — обход цепочек \
                 длин покажет, какой блок реально имеет эту длину"
                    .into(),
            );
        }
    }

        // --- встроенный DDS ---
    if let Some(pos) = head.windows(4).position(|w| w == b"DDS ") {
        let off = pos;
        let dds = u32_at(&head, off + 4);
        let flags = u32_at(&head, off + 8);
        let h = u32_at(&head, off + 12);
        let w = u32_at(&head, off + 16);
        // Все Option разворачиваем В ПЕРЕМЕННЫЕ до format!: прошлая версия
        // сунула {} в Option<u32> (E0277) и {:#06X?} — спорный спецификатор.
        let dsz = dds.map(|v| v.to_string()).unwrap_or_else(|| "?".to_string());
        let fl = flags.map(|v| format!("{:#06X}", v)).unwrap_or_else(|| "?".to_string());
        let dims = match (w, h) {
            (Some(a), Some(b)) => format!("{}x{}", a, b),
            _ => "?".to_string(),
        };
        facts.push(format!(
            "0x{:04X}: magic \"DDS \" — встроенная текстура: dwSize={} (эталон 124), flags={}, {}",
            off, dsz, fl, dims
        ));
        if dds == Some(124) {
            facts.push("  dwSize=124 совпал с эталоном DDS -> смещения прочитаны ВЕРНО".into());
        }
    }

    // --- сверка размера файла с полотном ---
    let hm = (grid as u64) * (grid as u64) * 2;
    if grid > 0 && bytes.len() as u64 >= hm {
        facts.push(format!(
            "размер файла {} байт >= heightmap {} байт -> остаток {} байт на текстуры/пропсы/декали/маркеры",
            bytes.len(),
            hm,
            bytes.len() as u64 - hm
        ));
    }

    Ok(ProbeInfo {
        path: path.to_path_buf(),
        size: bytes.len() as u64,
        head,
        facts,
        hypotheses: hyps,
        units,
        grid,
    })
}

impl ProbeInfo {
    pub fn report(&self) -> String {
        let mut s = String::new();
        s.push_str(&format!(
            "MAP INFO {}\n  размер: {} байт\n  head[64] hex:\n",
            self.path.display(),
            self.size
        ));
        s.push_str(&hexdump(&self.head, 4));
        s.push_str("  ФАКТЫ (измерено в этом файле или подтверждено эталоном):\n");
        for f in &self.facts {
            s.push_str(&format!("    - {}\n", f));
        }
        if !self.hypotheses.is_empty() {
            s.push_str("  ГИПОТЕЗЫ (не данные — требует `mapchunks` / второго дампа):\n");
            for h in &self.hypotheses {
                s.push_str(&format!("    - {}\n", h));
            }
        }
        if let Some((a, b)) = self.units {
            s.push_str(&format!(
                "  ДЛЯ ЯДРА: GetMapSize -> ({:.1}, {:.1}) [игровые единицы], \
                 heightmap {}x{} 16-бит.\n",
                a, b, self.grid, self.grid
            ));
        }
        s.push_str(
            "  СЛЕДУЮЩИЙ ШАГ: `mapchunks <имя>` — обход цепочек длин + поиск полотна\n\
            \u{2003}по гладкости. Это измерение, а не догадка: блок, который реально\n\
            \u{2003}является heightmap, имеет низкие дельты между соседними сэмплами.\n",
        );
        s
    }
}

// ==================== СКАН СТРУКТУРЫ ====================

#[derive(Clone, Debug)]
pub struct SigHit {
    pub off: usize,
    pub what: String,
}

#[derive(Clone, Debug)]
pub struct Chain {
    pub start: usize,
    pub lens: Vec<(usize, u32)>,
    pub end: usize,
    pub exact_eof: bool,
}

#[derive(Clone, Debug)]
pub struct HmCandidate {
    pub off: usize,
    pub grid: u32,
    pub bpp: usize,
    pub mean: f64,
    pub min: u32,
    pub max: u32,
    pub zero_frac: f64,
    pub dh: f64,
    pub dv: f64,
    /// Чем выше, тем больше похоже на рельеф: гладкость при разумном диапазоне.
    pub score: f64,
}

/// Обход «u32 длина -> данные -> u32 длина ...» от стартового смещения.
/// Цепочка, доходящая ровно до EOF, есть структурное доказательство layout —
/// без единой догадки о смысле полей.
pub fn walk_chain(bytes: &[u8], start: usize, max_chunks: usize) -> Option<Chain> {
    let mut pos = start;
    let mut lens = Vec::new();
    while pos + 4 <= bytes.len() && lens.len() < max_chunks {
        let l = u32_at(bytes, pos)?;
        if l < 4 || l as usize > bytes.len() - pos - 4 {
            return None;
        }
        lens.push((pos, l));
        pos += 4 + l as usize;
        if pos == bytes.len() {
            return Some(Chain { start, lens, end: pos, exact_eof: true });
        }
        if pos > bytes.len() {
            return None;
        }
    }
    if lens.is_empty() {
        None
    } else {
        Some(Chain { start, lens, end: pos, exact_eof: false })
    }
}

fn find_all(bytes: &[u8], pat: &[u8]) -> Vec<usize> {
    let mut out = Vec::new();
    if pat.is_empty() || bytes.len() < pat.len() {
        return out;
    }
    let mut i = 0;
    while i + pat.len() <= bytes.len() {
        if &bytes[i..i + pat.len()] == pat {
            out.push(i);
            if out.len() >= 64 {
                break;
            }
        }
        i += 1;
    }
    out
}

/// Скоринг участка как полотна высот по СЕМКЕ, а не по всему блоку: 1025^2
/// сэмплов на каждый кандидата — это миллиарды операций, а平滑ость видна и
/// по 64x64 с шагом.
fn score_heightmap(bytes: &[u8], off: usize, grid: u32, bpp: usize) -> Option<HmCandidate> {
    let g = grid as usize;
    let need = g * g * bpp;
    if off + need > bytes.len() || g < 8 {
        return None;
    }
    let step = if g > 128 { g / 64 } else { 1 };
    let mut vals: Vec<u32> = Vec::new();
    let mut z = 0usize;
    let mut r = 0usize;
    while r < g {
        let mut c = 0usize;
        while c < g {
            let p = off + (r * g + c) * bpp;
            let v = match bpp {
                1 => bytes.get(p).copied().unwrap_or(0) as u32,
                2 => u32_at(bytes, p).map(|x| x & 0xFFFF)?,
                4 => u32_at(bytes, p)?,
                8 => u32_at(bytes, p)?,
                _ => return None,
            };
            if v == 0 {
                z += 1;
            }
            vals.push(v);
            c += step;
        }
        r += step;
    }
    if vals.len() < 16 {
        return None;
    }
    let mn = *vals.iter().min().unwrap() as u64;
    let mx = *vals.iter().max().unwrap() as u64;
    let sum: u64 = vals.iter().map(|v| *v as u64).sum();
    let mean = sum as f64 / vals.len() as f64;
    let zero_frac = z as f64 / vals.len() as f64;

    // Дельты соседей вдоль строки и вдоль столбца (в пределах сетки).
    let mut dh = 0f64;
    let mut nh = 0usize;
    let mut dv = 0f64;
    let mut nv = 0usize;
    let cols = (g + step - 1) / step;
    for i in 0..vals.len() {
        if (i + 1) % cols != 0 && i + 1 < vals.len() {
            dh += (vals[i + 1] as f64 - vals[i] as f64).abs();
            nh += 1;
        }
        if i + cols < vals.len() {
            dv += (vals[i + cols] as f64 - vals[i] as f64).abs();
            nv += 1;
        }
    }
    let dh = if nh > 0 { dh / nh as f64 } else { f64::MAX };
    let dv = if nv > 0 { dv / nv as f64 } else { f64::MAX };

    // Правила скоринга (каждое — против конкретного ложного срабатывания):
    //  * нули почти везде -> не рельеф (мусор/выравнивание);
    //  * диапазон < 16 -> плоский блок констант;
    //  * среднее |дельта| > 3000 -> шум (текстура/DXT/пропсы), не высота.
    if zero_frac > 0.9 {
        return None;
    }
    if mx - mn < 16 {
        return None;
    }
    let avg = (dh + dv) / 2.0;
    if avg > 3000.0 {
        return None;
    }
    let score = 1.0 / (1.0 + avg) * (1.0 - zero_frac);
    Some(HmCandidate {
        off,
        grid,
        bpp,
        mean,
        min: mn as u32,
        max: mx as u32,
        zero_frac,
        dh,
        dv,
        score,
    })
}

pub struct ScanReport {
    pub path: PathBuf,
    pub size: u64,
    pub grid: u32,
    pub sigs: Vec<SigHit>,
    pub strings: Vec<(usize, String)>,
    pub chains: Vec<Chain>,
    pub hm: Vec<HmCandidate>,
}

/// Полный структурный скан. Ограничения по объёму названы в отчёте: сканируем
/// сигнатуры по всему файлу (дёшево), строки — по всему файлу, но вывод режем,
/// цепочки — из ограниченного набора стартов, полотно — только на длинах,
/// совпадающих с grid^2*bpp.
pub fn scan(path: &Path) -> Result<ScanReport, String> {
    let bytes = fs::read(path).map_err(|e| format!("не читается: {}", e))?;
    let info = probe(path)?;
    let grid = info.grid;

    let mut sigs = Vec::new();
    for off in find_all(&bytes, b"DDS ") {
        let hdr = if off + 20 <= bytes.len() {
            let w = u32_at(&bytes, off + 16).unwrap_or(0);
            let h = u32_at(&bytes, off + 12).unwrap_or(0);
            let sz = u32_at(&bytes, off + 4).unwrap_or(0);
            format!(" (dwSize={}, {}x{})", sz, w, h)
        } else {
            String::new()
        };
        sigs.push(SigHit { off, what: format!("\"DDS \"{}", hdr) });
    }
    for off in find_all(&bytes, b"Map\x1A") {
        sigs.push(SigHit { off, what: "\"Map\\x1A\" (magic SCMAP)".into() });
    }
    for off in find_all(&bytes, &[0xED, 0xFE, 0xEF, 0xBE]) {
        sigs.push(SigHit { off, what: "маркер 0xBEEFEFED".into() });
    }
    for off in find_all(&bytes, b"<LOC") {
        sigs.push(SigHit { off, what: "\"<LOC\" (локализация внутри блока)".into() });
        break;
    }
    sigs.sort_by_key(|s| s.off);
    sigs.truncate(80);

    // Строки: прогоны >=6 печатных байт. Это бездопущенийный способ увидеть
    // имена секций, если они есть (HazardX: формат расшифрован частично [42]).
    let mut strings: Vec<(usize, String)> = Vec::new();
    let mut start: Option<usize> = None;
    for (i, b) in bytes.iter().enumerate() {
        let ok = b.is_ascii_alphanumeric() || matches!(b, b'_' | b'/' | b'.' | b'-' | b' ');
        match (start, ok) {
            (None, true) => start = Some(i),
            (Some(s), true) => {
                if i - s + 1 >= 6 && strings.len() < 400 {
                    let cand = &bytes[s..=i];
                    if let Some(last) = strings.last() {
                        if last.1 == String::from_utf8_lossy(cand).to_string() {
                            continue;
                        }
                    }
                    strings.push((s, String::from_utf8_lossy(cand).to_string()));
                }
            }
            (Some(_), false) => start = None,
            // Мы НЕ внутри строки, и байт не печатный -> делать нечего:
            // start и так None. Пустое действие, а не паника.
            (None, false) => {}
        }
    }
    strings.retain(|(_, t)| t.chars().filter(|c| !c.is_whitespace()).count() >= 5);
    strings.truncate(120);

    // Цепочки длин: старты — 0x00..0x80, плюс сразу после каждого DDS-магика
    // (там заведомо граница блока), плюс 0x1C+4 (поле длины из заголовка).
    let mut starts: Vec<usize> = (0usize..=0x80).collect();
    for s in &sigs {
        if s.what.starts_with("\"DDS \"") {
            starts.push(s.off + 4);
            starts.push(s.off + 128);
        }
    }
    starts.push(0x20);
    starts.sort_unstable();
    starts.dedup();
    let mut chains: Vec<Chain> = Vec::new();
    for st in starts.iter().take(400) {
        if let Some(c) = walk_chain(&bytes, *st, 64) {
            if c.exact_eof && c.lens.len() >= 2 {
                chains.push(c);
            }
        }
    }
    chains.sort_by(|a, b| b.lens.len().cmp(&a.lens.len()));
    chains.truncate(8);

    // Полотно высот: тестируем только длины из цепочек и из заголовка.
    let mut lens: Vec<(usize, u32)> = Vec::new();
    for c in &chains {
        lens.extend(c.lens.iter().copied());
    }
    if let Some(v) = u32_at(&bytes, 0x1C) {
        lens.push((0x20, v));
    }
    let mut hm: Vec<HmCandidate> = Vec::new();
    if grid >= 8 {
        let want = (grid as u64) * (grid as u64);
        for (pos, l) in lens.iter() {
            // данные идут ПОСЛЕ 4-байтной длины
            let data = pos + 4;
            for bpp in [2usize, 1, 4, 8] {
                if (*l as u64) == want * bpp as u64 {
                    if let Some(c) = score_heightmap(&bytes, data, grid, bpp) {
                        hm.push(c);
                    }
                }
            }
        }
        // Запасной проход: если цепочек нет, пробуем выровненные смещения.
        if hm.is_empty() {
            let mut off = 0x20usize;
            while off + (want as usize) * 2 <= bytes.len() && off < 0x40_0000 {
                if let Some(c) = score_heightmap(&bytes, off, grid, 2) {
                    hm.push(c);
                }
                off += 0x10000; // 64 KiB шаг — полотно не может начинаться где угодно
            }
        }
    }
    hm.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));
    hm.truncate(10);

    Ok(ScanReport {
        path: path.to_path_buf(),
        size: bytes.len() as u64,
        grid,
        sigs,
        strings,
        chains,
        hm,
    })
}

impl ScanReport {
    pub fn report(&self) -> String {
        let mut s = String::new();
        s.push_str(&format!(
            "MAP CHUNKS {} ({} байт, сетка {}x{})\n",
            self.path.display(),
            self.size,
            self.grid,
            self.grid
        ));
        s.push_str(&format!("  Сигнатуры ({} показанных из найденных):\n", self.sigs.len()));
        for h in self.sigs.iter().take(40) {
            s.push_str(&format!("    +{:#08X}  {}\n", h.off, h.what));
        }
        if self.chains.is_empty() {
            s.push_str("  ЦЕПОЧЕК ДЛИН, ДОХОДЯЩИХ ДО EOF, НЕ НАЙДЕНО.\n\
                        \u{2003}Это значимый результат, а не пустой вывод: значит файл\n\
                        \u{2003}НЕ является последовательностью u32-длина + данные, и\n\
                        \u{2003}layout надо читать из HazardX-псевдокода [gamereplays 176526],\n\
                        \u{2003}а не выводить из структуры.\n");
        } else {
            for (i, c) in self.chains.iter().take(4).enumerate() {
                s.push_str(&format!(
                    "  Цепочка {}: старт +{:#06X}, блоков {}, конец +{:#08X} (EOF {})\n",
                    i + 1,
                    c.start,
                    c.lens.len(),
                    c.end,
                    if c.exact_eof { "ТОЧНО" } else { "НЕТ" }
                ));
                for (pos, l) in c.lens.iter().take(12) {
                    let tag = if self.grid >= 8 && *l as u64 == (self.grid as u64) * (self.grid as u64) * 2 {
                        "  <-- РАВНА heightmap grid^2*2"
                    } else {
                        ""
                    };
                    s.push_str(&format!("      +{:#08X} len={:>9}{}\n", pos, l, tag));
                }
                if c.lens.len() > 12 {
                    s.push_str(&format!("      ... ещё {}\n", c.lens.len() - 12));
                }
            }
        }
        if self.hm.is_empty() {
            s.push_str("  Полотно высот: кандидатов не найдено.\n\
                        \u{2003}Оговорка: скоринг отбрасывает блоки с >90% нулей, диапазоном\n\
                        \u{2003}<16 и средней дельтой >3000. Если рельеф сжат или хранится\n\
                        \u{2003}иначе, он пройдёт мимо — тогда нужен разбор по псевдокоду.\n");
        } else {
            s.push_str("  Кандидаты на полотно высот (сортировка по гладкости):\n");
            for c in self.hm.iter().take(6) {
                s.push_str(&format!(
                    "    +{:#08X} {}x{} bpp={} | min={} max={} mean={:.0} нули={:.1}% \
                     Δгор={:.1} Δвер={:.1} score={:.5}\n",
                    c.off, c.grid, c.grid, c.bpp, c.min, c.max, c.mean,
                    c.zero_frac * 100.0, c.dh, c.dv, c.score
                ));
            }
            let b = &self.hm[0];
            s.push_str(&format!(
                "  ЛУЧШИЙ: +{:#08X} ({}x{}, {} байт/сэмпл). Калибровка u16->метры НЕ \
                 подтверждена: если mean≈16384, то scale=1/256 даёт {:.1} м, что близко к \
                 рекомендованному Initial Elev. 64 [quick map guide] — это проверямая \
                 гипотеза, а не факт.\n",
                b.off, b.grid, b.grid, b.bpp, b.mean as f64 / 256.0
            ));
        }
        s.push_str(&format!("  Печатные строки ({} из найденных, для имён секций):\n", self.strings.len().min(120)));
        for (off, t) in self.strings.iter().take(60) {
            s.push_str(&format!("    +{:#08X} {:?}\n", off, t));
        }
        if self.strings.len() > 60 {
            s.push_str(&format!("    ... ещё {}\n", self.strings.len() - 60));
        }
        s
    }
}

// ==================== ЗАГРУЗКА РЕЛЬЕФА ====================

#[derive(Clone, Debug)]
pub struct Terrain {
    pub grid: u32,
    pub units: f32,
    pub heights: Vec<u16>,
    pub offset: usize,
    /// u16 -> метры. НЕ подтверждено: см. калибровку в ScanReport::report.
    pub scale: f32,
    pub source: String,
}

impl Terrain {
    /// Линейная интерполяция по координате в игровых единицах.
    /// Индекс = координата, т.к. сетка = units+1 (1 сэмпл на единицу).
    pub fn height_at(&self, x: f32, z: f32) -> f32 {
        if self.grid < 2 || self.heights.is_empty() {
            return 0.0;
        }
        let g = self.grid as f32 - 1.0;
        let fx = (x.max(0.0)).min(g);
        let fz = (z.max(0.0)).min(g);
        let x0 = fx.floor() as usize;
        let z0 = fz.floor() as usize;
        let x1 = (x0 + 1).min(self.grid as usize - 1);
        let z1 = (z0 + 1).min(self.grid as usize - 1);
        let tx = fx - x0 as f32;
        let tz = fz - z0 as f32;
        let at = |c: usize, r: usize| -> f32 {
            self.heights[r * self.grid as usize + c] as f32 * self.scale
        };
        let a = at(x0, z0);
        let b = at(x1, z0);
        let c = at(x0, z1);
        let d = at(x1, z1);
        let top = a + (b - a) * tx;
        let bot = c + (d - c) * tx;
        top + (bot - top) * tz
    }

    pub fn stats(&self) -> String {
        if self.heights.is_empty() {
            return "TERRAIN: пусто".to_string();
        }
        let mut mn = u16::MAX;
        let mut mx = 0u16;
        let mut sum: u64 = 0;
        for h in &self.heights {
            if *h < mn {
                mn = *h;
            }
            if *h > mx {
                mx = *h;
            }
            sum += *h as u64;
        }
        let mean = sum as f64 / self.heights.len() as f64;
        format!(
            "TERRAIN {}x{} из {} (+{:#08X}), {} байт/сэмпл, scale={:.6}\n\
             u16: min={} max={} mean={:.0} | метры (при этом scale): min={:.2} max={:.2} mean={:.2}\n\
             Пробники (x,z)->высота: (0,0)={:.2}  центр={:.2}  ({:.0},{:.0})={:.2}\n\
             ЕСЛИ среднее в метрах абсурдно (>2000 или <0) — scale неверен, подбери\n\
             \u{2003}1/256, 1/100, 1/10 или 1.0 по правилу: quick map guide даёт\n\
             \u{2003}Initial Elev. ~64, т.е. среднее должно быть около 64 м.",
            self.grid,
            self.grid,
            self.source,
            self.offset,
            2,
            self.scale,
            mn,
            mx,
            mean,
            mn as f64 * self.scale as f64,
            mx as f64 * self.scale as f64,
            mean * self.scale as f64,
            self.height_at(0.0, 0.0),
            self.height_at(self.units / 2.0, self.units / 2.0),
            self.units * 0.25,
            self.units * 0.75,
            self.height_at(self.units * 0.25, self.units * 0.75),
        )
    }
}

/// Грузит рельеф: сначала по лучшему кандидату скана, иначе по правилу
/// grid^2*2 от конца заголовка. Возвращает Terrain или причину отказа.
pub fn load_terrain(path: &Path, scale: f32) -> Result<Terrain, String> {
    let bytes = fs::read(path).map_err(|e| format!("не читается: {}", e))?;
    let info = probe(path)?;
    let units = info
        .units
        .ok_or("в заголовке не найден размер карты (пара f32 @0x10/0x14)")?
        .0;
    let grid = info.grid;
    if grid < 8 {
        return Err(format!("неверная сетка {} (размер {} ед.)", grid, units));
    }
    let need = (grid as usize) * (grid as usize) * 2;
    if bytes.len() < need {
        return Err(format!(
            "файла не хватает на полотно {}x{} 16-бит: нужно {}, есть {}",
            grid,
            grid,
            need,
            bytes.len()
        ));
    }
    let rep = scan(path)?;
    let (off, source) = if let Some(best) = rep.hm.first() {
        (best.off, format!("скан по гладкости (score={:.5})", best.score))
    } else {
        // Запасной офсет: сразу за 0x20-полем (первый блок после заголовка).
        (0x20, "запасной: сразу за заголовком (цепочка не подтверждена)".to_string())
    };
    if off + need > bytes.len() {
        return Err(format!(
            "кандидат +{:#08X} не влезает: нужно {} байт, остаток файла {}",
            off,
            need,
            bytes.len() - off
        ));
    }
    let mut heights = Vec::with_capacity(grid as usize * grid as usize);
    let mut p = off;
    for _ in 0..(grid as usize * grid as usize) {
        heights.push(u16::from_le_bytes([bytes[p], bytes[p + 1]]));
        p += 2;
    }
    Ok(Terrain {
        grid,
        units,
        heights,
        offset: off,
        scale,
        source,
    })
}

pub fn find_scmap(roots: &[PathBuf], name: &str) -> Option<PathBuf> {
    let want = name.to_lowercase();
    for root in roots {
        let map_dir = root.join(name);
        if map_dir.is_dir() {
            if let Ok(rd) = fs::read_dir(&map_dir) {
                for ent in rd.flatten() {
                    let p = ent.path();
                    if p.extension()
                        .map(|e| e.to_string_lossy().eq_ignore_ascii_case("scmap"))
                        .unwrap_or(false)
                    {
                        return Some(p);
                    }
                }
            }
        }
        if let Ok(rd) = fs::read_dir(root) {
            for ent in rd.flatten() {
                let d = ent.path();
                if !d.is_dir() {
                    continue;
                }
                let dn = d
                    .file_name()
                    .map(|s| s.to_string_lossy().to_lowercase())
                    .unwrap_or_default();
                if dn != want {
                    continue;
                }
                if let Ok(rd2) = fs::read_dir(&d) {
                    for e2 in rd2.flatten() {
                        let p = e2.path();
                        if p.extension()
                            .map(|e| e.to_string_lossy().eq_ignore_ascii_case("scmap"))
                            .unwrap_or(false)
                        {
                            return Some(p);
                        }
                    }
                }
            }
        }
    }
    None
}
// src/moho/debug.rs
// ОТЛАДОЧНАЯ КОНСОЛЬ: кольцо событий, память, дамп состояния, исполнение
// Lua-файлов с наблюдением, автоматический диагноз, курсор выгрузки (take_new).
//
// Три сознательных проектных решения (каждое — из наших ошибок сессии):
//
// 1) КОЛЬЦО МОЛЧИТ ПО УМОЛЧАНИЮ (__moho_trace_on = false). Наблюдатель не
//    меняет наблюдаемое: 423/0 не двигаются, пока не включишь `trace on`.
// 2) ВСЕ ПОДСЧЁТЫ ТАБЛИЦ — В LUA, НЕ ЧЕРЕЗ Rust Table::len(): #t/len() не
//    считает строковые ключи, а __bp_units/__blueprints/__moho_modcache —
//    именно со строковыми ключами. Счёт через Rust врал бы (самообман в
//    цифрах). Заодно уход в Lua снимает риск повторить E0624/E0599.
// 3) ЗАВИСИМОСТЬ ОТ shim.rs МИНИМАЛЬНА: только публичные sim_state/MohoRuntime
//    и публичные поля rt.lua / rt.trace / rt.vfs. Ни has_state, ни read_source
//    не требуются.
//
// record/record_native принимают &Mutex<TraceRing> (не Arc): поле trace в
// MohoRuntime = Mutex<TraceRing>, и менять его незачем.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use crate::moho::preproc;
use crate::moho::shim::{self, MohoRuntime};

// ==================== КОЛЬЦО СОБЫТИЙ ====================

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TraceKind {
    Print,  // print / LOG
    Spew,   // SPEW (погрузка модулей)
    Warn,   // WARN
    Thread, // THREAD ERROR из __moho_pump
    Native, // нативный вызов (CreateUnitHPR, Damage, ...)
    Error,  // ошибка загрузки/исполнения
    Sys,    // служебные события самой консоли (mem/watch/dump/...)
}

impl TraceKind {
    pub fn label(&self) -> &'static str {
        match self {
            TraceKind::Print => "PRINT",
            TraceKind::Spew => "SPEW",
            TraceKind::Warn => "WARN",
            TraceKind::Thread => "THREAD",
            TraceKind::Native => "NATIVE",
            TraceKind::Error => "ERROR",
            TraceKind::Sys => "SYS",
        }
    }
    pub fn tag(&self) -> &'static str {
        match self {
            TraceKind::Print => "print",
            TraceKind::Spew => "spew",
            TraceKind::Warn => "warn",
            TraceKind::Thread => "thread",
            TraceKind::Native => "native",
            TraceKind::Error => "error",
            TraceKind::Sys => "sys",
        }
    }
}

#[derive(Clone, Debug)]
pub struct TraceEvent {
    pub ms: u128,
    pub kind: TraceKind,
    pub text: String,
}

pub struct TraceRing {
    /// Кольцевой буфер: переполнение выбрасывает старейшее. cap=4096 хватает
    /// на полный прогон load_all_lua (тысячи SPEW) плюс игровой цикл; ~1-2 МБ.
    pub events: VecDeque<TraceEvent>,
    pub cap: usize,
    /// Всего событий с момента включения — чтобы видеть «потеряно N».
    pub total: u64,
    /// Сколько событий УЖЕ прилито в историю консоли (drain_trace).
    /// Без курсора консоль либо льёт одни и те же строки каждый кадр,
    /// либо теряет события при переполнении кольца.
    pub drained: u64,
    pub started: Option<Instant>,
    /// Последний снимок памяти — база для `watch` (дельты).
    pub mem_last: Option<MemStats>,
    pub natives: Vec<(String, u64)>,
}

impl TraceRing {
    pub fn new() -> Self {
        Self {
            events: VecDeque::new(),
            cap: 4096,
            total: 0,
            drained: 0,
            started: Some(Instant::now()),
            mem_last: None,
            natives: Vec::new(),
        }
    }
}

impl Default for TraceRing {
    fn default() -> Self {
        Self::new()
    }
}

fn now_ms(r: &TraceRing) -> u128 {
    r.started.map(|s| s.elapsed().as_millis()).unwrap_or(0)
}

/// Записать событие. try_lock НАМЕРЕННО: диагностика ни при каких условиях
/// не должна становиться причиной дедлока. Молчаливый пропуск события лучше
/// зависшей игры.
pub fn record(ring: &Mutex<TraceRing>, kind: TraceKind, text: &str) -> bool {
    if let Ok(mut g) = ring.try_lock() {
        let ms = now_ms(&g);
        g.total += 1;
        if g.events.len() >= g.cap {
            g.events.pop_front();
        }
        g.events.push_back(TraceEvent { ms, kind, text: text.to_string() });
        true
    } else {
        false
    }
}

pub fn record_native(ring: &Mutex<TraceRing>, name: &str) {
    if let Ok(mut g) = ring.try_lock() {
        let ms = now_ms(&g);
        g.total += 1;
        if let Some(slot) = g.natives.iter_mut().find(|(n, _)| n == name) {
            slot.1 += 1;
        } else {
            g.natives.push((name.to_string(), 1));
        }
        if g.events.len() >= g.cap {
            g.events.pop_front();
        }
        g.events.push_back(TraceEvent { ms, kind: TraceKind::Native, text: name.to_string() });
    }
}

fn fmt_ms(ms: u128) -> String {
    format!("{:05}.{:03}", ms / 1000, ms % 1000)
}

/// Есть ли УЖЕ загруженное состояние, без создания нового. Поле rt.lua
/// публичное -> не зависим от shim::has_state (её может не быть).
fn state_ready(rt: &Arc<MohoRuntime>) -> bool {
    rt.lua.lock().map(|g| g.is_some()).unwrap_or(false)
}

/// Забрать события, которые ещё НЕ прилиты в историю консоли.
/// При переполнении кольца возвращает всё, что в нём осталось, —
/// честнее, чем молча потерять строки, которые консоль не успела показать.
pub fn take_new(rt: &Arc<MohoRuntime>) -> Vec<(TraceKind, String)> {
    let mut g = rt.trace.lock().unwrap();
    if g.total <= g.drained {
        return Vec::new();
    }
    let unseen = (g.total - g.drained) as usize;
    let skip = g.events.len().saturating_sub(unseen);
    let out: Vec<(TraceKind, String)> =
        g.events.iter().skip(skip).map(|e| (e.kind, e.text.clone())).collect();
    g.drained = g.total;
    out
}

pub fn render_ring(rt: &Arc<MohoRuntime>, limit: usize) -> String {
    let g = rt.trace.lock().unwrap();
    let dropped = g.total.saturating_sub(g.events.len() as u64);
    let mut s = String::new();
    s.push_str(&format!(
        "TRACE: всего {}, в буфере {}, потеряно (кольцо) {}{}, cap={}\n",
        g.total,
        g.events.len(),
        dropped,
        if dropped > 0 { " — увеличь cap или разбирай чаще" } else { "" },
        g.cap
    ));
    let start = if limit == 0 || g.events.len() <= limit { 0 } else { g.events.len() - limit };
    for e in g.events.iter().skip(start) {
        s.push_str(&format!("  {} [{}] {}\n", fmt_ms(e.ms), e.kind.label(), e.text));
    }
    if !g.natives.is_empty() {
        let mut n = g.natives.clone();
        n.sort_by(|a, b| b.1.cmp(&a.1));
        s.push_str("  NATIVE (топ):\n");
        for (name, cnt) in n.iter().take(20) {
            s.push_str(&format!("    {:<34} {}\n", name, cnt));
        }
    }
    s
}

// ==================== ФЛАГ ТРАСИРОВКИ ====================

pub fn set_trace(rt: &Arc<MohoRuntime>, on: bool) -> String {
    if !state_ready(rt) {
        return "trace: состояние не загружено -> сначала `load_all_lua`".to_string();
    }
    let lua = match shim::sim_state(rt) {
        Ok(l) => l,
        Err(e) => return format!("trace: sim_state: {}", e),
    };
    if let Err(e) = lua.globals().set("__moho_trace_on", on) {
        return format!("trace: не удалось установить флаг: {}", e);
    }
    {
        let mut g = rt.trace.lock().unwrap();
        if on && g.started.is_none() {
            g.started = Some(Instant::now());
        }
    }
    record(&rt.trace, TraceKind::Sys, if on { "трассировка ВКЛЮЧЕНА" } else { "трассировка ВЫКЛЮЧЕНА" });
    format!(
        "трассировка {}: события пишутся в кольцо (cap {}), разбирай `trace`, `hunt`, `logsave`",
        if on { "ВКЛЮЧЕНА" } else { "ВЫКЛЮЧЕНА" },
        rt.trace.lock().unwrap().cap
    )
}

// ==================== ПАМЯТЬ ====================

#[derive(Clone, Copy, Debug, Default)]
pub struct MemStats {
    pub lua_kb: f64,
    pub units: i64,
    pub effects: i64,
    pub other: i64,
    pub blueprints: i64,
    pub modcache: i64,
    pub threads: i64,
    pub registry_paths: i64,
}

impl MemStats {
    pub fn rows(&self) -> i64 {
        self.units + self.effects + self.other + self.blueprints + self.modcache + self.registry_paths
    }
}

pub fn mem_snapshot(rt: &Arc<MohoRuntime>) -> Result<MemStats, String> {
    if !state_ready(rt) {
        return Err("состояние не загружено -> сначала `load_all_lua`".to_string());
    }
    let lua = shim::sim_state(rt).map_err(|e| format!("sim_state: {}", e))?;
    let script = r#"
local function count(t) local n = 0 for _ in pairs(t or {}) do n = n + 1 end return n end
local gc = collectgarbage
return string.format(
  '%d %d %d %d %d %d %d %d',
  math.floor(gc('count')),
  count(__bp_units), count(__bp_effects), count(__bp_other),
  count(__blueprints), count(__moho_modcache),
  (__moho_threads and #__moho_threads) or 0,
  count(__moho_bp_registry))
"#;
    let raw = lua
        .load(script)
        .set_name("atlas_mem")
        .eval::<String>()
        .map_err(|e| format!("замер памяти: {}", e))?;
    let v: Vec<i64> = raw.split_whitespace().filter_map(|s| s.parse::<i64>().ok()).collect();
    if v.len() < 8 {
        return Err(format!("замер памяти: неожиданный ответ: {}", raw));
    }
    Ok(MemStats {
        lua_kb: v[0] as f64,
        units: v[1],
        effects: v[2],
        other: v[3],
        blueprints: v[4],
        modcache: v[5],
        threads: v[6],
        registry_paths: v[7],
    })
}

pub fn render_mem(rt: &Arc<MohoRuntime>) -> String {
    match mem_snapshot(rt) {
        Ok(s) => {
            record(&rt.trace, TraceKind::Sys, &format!("mem: lua_kb={} rows={}", s.lua_kb as i64, s.rows()));
            format!(
                "MEM: Lua-куча {:.1} КБ | реестр: units={} effects={} other={} blueprints={} modcache={} registry={}\n\
                 | треды(живые)={} | всего строк реестров={}\n\
                 Подсказка: `watch` покажет дельту; монотонный рост rows или threads = утечка.",
                s.lua_kb, s.units, s.effects, s.other, s.blueprints, s.modcache, s.registry_paths, s.threads, s.rows()
            )
        }
        Err(e) => format!("MEM: {}", e),
    }
}

/// Дельта к прошлому замеру. ПЕРВЫЙ вызов дельты не даёт и прямо об этом
/// говорит: иначе «дельта 0» выглядела бы как «утечки нет», чего мы не знаем.
pub fn render_watch(rt: &Arc<MohoRuntime>) -> String {
    let cur = match mem_snapshot(rt) {
        Ok(s) => s,
        Err(e) => return format!("WATCH: {}", e),
    };
    let base = {
        let mut g = rt.trace.lock().unwrap();
        g.mem_last.replace(cur)
    };
    match base {
        None => format!(
            "WATCH: базовый снимок запомнен (lua_kb={:.1} rows={} threads={}). \
             Повтори `watch` после действий, чтобы увидеть дельту.",
            cur.lua_kb, cur.rows(), cur.threads
        ),
        Some(b) => {
            let dkb = cur.lua_kb - b.lua_kb;
            let drows = cur.rows() - b.rows();
            let dthr = cur.threads - b.threads;
            let verdict = if dkb > 512.0 && drows > 0 {
                "ПОДОЗРЕНИЕ НА УТЕЧКУ: куча и строки растут вместе"
            } else if dthr > 0 && dkb > 128.0 {
                "ПОДОЗРЕНИЕ: треды не умирают и держат память"
            } else if dkb.abs() < 8.0 && drows == 0 {
                "стабильно"
            } else {
                "в пределах нормы"
            };
            let text = format!("WATCH: Δкуча {:+.1} КБ | Δстрок {:+} | Δтредов {:+} -> {}", dkb, drows, dthr, verdict);
            record(&rt.trace, TraceKind::Sys, &text);
            format!(
                "{}\n  было: kb={:.1} rows={} threads={}\n  стало: kb={:.1} rows={} threads={}",
                text, b.lua_kb, b.rows(), b.threads, cur.lua_kb, cur.rows(), cur.threads
            )
        }
    }
}

// ==================== ДАМП СОСТОЯНИЯ ====================

/// Имя глобала должно быть простым идентификатором: иначе в Lua-скрипт
/// улетит мусорный/вредоносный текст. Отклоняем честно, а не экранируем
/// «как-нибудь» — это отладчик, молчаливая подмена здесь недопустима.
fn is_plain_ident(s: &str) -> bool {
    !s.is_empty()
        && s.chars().enumerate().all(|(i, c)| {
            c == '_' || c.is_ascii_alphanumeric() && !(i == 0 && c.is_ascii_digit())
        })
}

pub fn render_dump(rt: &Arc<MohoRuntime>, name: &str) -> String {
    if !is_plain_ident(name) {
        return format!("dump: недопустимое имя глобала: {:?} (только буквы/цифры/_)", name);
    }
    if !state_ready(rt) {
        return "dump: состояние не загружено -> сначала `load_all_lua`".to_string();
    }
    let lua = match shim::sim_state(rt) {
        Ok(l) => l,
        Err(e) => return format!("dump: sim_state: {}", e),
    };
    // Считаем и форматируем в Lua: repr() из PRELUDE режет большие таблицы
    // за 5 элементов — ровно то, что нужно для дампа.
    let script = format!(
        r#"
local name = "{name}"
local v = _G[name]
local t = type(v)
local out = {{ string.format('DUMP %s : %s', name, t) }}
if t == 'table' then
  local n = 0 for _ in pairs(v) do n = n + 1 end
  local arr = #v
  out[#out+1] = string.format('  записей=%d (массивной части=%d)', n, arr)
  local keys = {{}}
  for k in pairs(v) do
    keys[#keys+1] = k
    if #keys >= 12 then break end
  end
  table.sort(keys, function(x,y) return tostring(x) < tostring(y) end)
  for _, k in ipairs(keys) do
    out[#out+1] = '  [' .. tostring(k) .. '] = ' .. repr(v[k])
  end
  if n > 12 then out[#out+1] = string.format('  ... ещё %d', n - 12) end
elseif t == 'function' then
  local info = debug and debug.getinfo and debug.getinfo(v, 'S')
  if info then
    out[#out+1] = string.format('  источник: %s:%s', tostring(info.source), tostring(info.linedefined))
  end
elseif t == 'string' then
  out[#out+1] = '  ' .. repr(v)
  out[#out+1] = string.format('  длина=%d', #v)
else
  out[#out+1] = '  ' .. tostring(v)
end
return table.concat(out, '\n')
"#,
        name = name,
    );
    match lua.load(&script).set_name("atlas_dump").eval::<String>() {
        Ok(s) => {
            record(&rt.trace, TraceKind::Sys, &format!("dump {}", name));
            s
        }
        Err(e) => format!("dump {}: {}", name, e),
    }
}

// ==================== ИСПОЛНЕНИЕ LUA-ФАЙЛА С НАБЛЮДЕНИЕМ ====================

/// Чтение из VFS с минимальным fallback по префиксам. Не зависим от
/// приватной shim::read_raw_source: поле rt.vfs публичное.
fn vfs_read(rt: &Arc<MohoRuntime>, path: &str) -> Option<String> {
    let mut tries: Vec<String> = Vec::new();
    let p = path.trim_start_matches('/').replace('\\', "/");
    tries.push(p.clone());
    tries.push(format!("/{}", p));
    for pre in ["lua/", "/lua/", "lua/sim/", "/lua/sim/", "lua/ai/", "/lua/ai/"] {
        tries.push(format!("{}{}", pre, p));
    }
    for t in &tries {
        if let Some(s) = rt.vfs.read_string(t) {
            return Some(s);
        }
    }
    None
}

pub fn run_lua_file(rt: &Arc<MohoRuntime>, path: &str) -> String {
    if !state_ready(rt) {
        return "luafile: состояние не загружено -> сначала `load_all_lua`".to_string();
    }
    let raw = match vfs_read(rt, path) {
        Some(s) => s,
        None => return format!("luafile: не найден: {}", path),
    };
    let before = mem_snapshot(rt).ok();
    // Замок освобождается до exec() ниже (drop в конце statement):
    // иначе логгер внутри исполняемого кода уперся бы в try_lock и молчал.
    let mark = rt.trace.lock().unwrap().total;

    let lua = match shim::sim_state(rt) {
        Ok(l) => l,
        Err(e) => return format!("luafile: sim_state: {}", e),
    };
    // Сброс бюджета инструкций И guard-счётчика: иначе watchdog срежет на
    // суммарном счётчике после предыдущих команд (мы уже видели ATLAS TIMEOUT
    // именно так).
    if let Ok(reset) = lua.globals().get::<mlua::Function>("__moho_reset_fuel") {
        let _ = reset.call::<()>(());
    }
    let _ = lua.globals().set("__moho_wait_guard", 0i64);

    let tr = preproc::moho_to_lua51(&raw);
    let mut s = String::new();
    s.push_str(&format!(
        "LUAFILE {} (raw={} строк, trans={} строк)\n",
        path,
        raw.lines().count(),
        tr.lines().count()
    ));
    match lua.load(&tr).set_name(path).exec() {
        Ok(_) => s.push_str("  исполнение: ОК\n"),
        Err(e) => {
            s.push_str(&format!("  исполнение: ОШИБКА\n    {}\n", e));
            record(&rt.trace, TraceKind::Error, &format!("{}: {}", path, e));
        }
    }
    if let (Some(b), Ok(a)) = (before, mem_snapshot(rt)) {
        s.push_str(&format!(
            "  память: {:+.1} КБ, строк реестров {:+}, тредов {:+}\n",
            a.lua_kb - b.lua_kb,
            a.rows() - b.rows(),
            a.threads - b.threads
        ));
    }
    let after = rt.trace.lock().unwrap().total;
    if after > mark {
        s.push_str(&format!("  событий за время исполнения: {}\n", after - mark));
        let g = rt.trace.lock().unwrap();
        let skip = g.events.len().saturating_sub((after - mark) as usize);
        for e in g.events.iter().skip(skip) {
            s.push_str(&format!("    {} [{}] {}\n", fmt_ms(e.ms), e.kind.label(), e.text));
        }
    }
    s
}

// ==================== АВТОМАТИЧЕСКИЙ ДИАГНОЗ ====================

/// `hunt` — явные правила с порогами и основаниями. Каждое заключение цитирует
/// число, из которого выросло. Пороги консервативные: ложная тревога стоит
/// дороже пропуска (десять прогонов мы убирали именно самообман).
pub fn hunt(rt: &Arc<MohoRuntime>) -> String {
    let mut s = String::new();
    let mut findings = 0usize;

    if !state_ready(rt) {
        return "hunt: состояние не загружено -> сначала `load_all_lua`".to_string();
    }
    let cur = match mem_snapshot(rt) {
        Ok(c) => c,
        Err(e) => return format!("hunt: {}", e),
    };

    // --- 1. Динамика памяти ---
    let base = rt.trace.lock().unwrap().mem_last;
    match base {
        Some(b) => {
            let dkb = cur.lua_kb - b.lua_kb;
            let drows = cur.rows() - b.rows();
            let dthr = cur.threads - b.threads;
            if dkb > 2048.0 && drows > 0 {
                findings += 1;
                s.push_str(&format!(
                    "[УТЕЧКА?] Lua-куча выросла на {:.1} КБ при росте строк реестров на {}.\n\
                     \u{2003}Основание: обычно повторное исполнение top-level поверх живых\n\
                     \u{2003}глобалов (load_all_lua без reset_state) или треды, держащие upvalue'ы.\n",
                    dkb, drows
                ));
            }
            if dthr > 50 {
                findings += 1;
                s.push_str(&format!(
                    "[ТРЕДЫ] Живых тредов прибавилось на {} ({} -> {}). Они не завершаются:\n\
                     \u{2003}WaitSeconds в бесконечном цикле или condition, который не наступает.\n",
                    dthr, b.threads, cur.threads
                ));
            }
        }
        None => s.push_str(&format!(
            "Память: базового снимка нет — выполни `mem` дважды с паузой,\n\
             \u{2003}чтобы hunt увидел динамику (сейчас: kb={:.1} rows={}).\n",
            cur.lua_kb, cur.rows()
        )),
    }
    if cur.threads > 2000 {
        findings += 1;
        s.push_str(&format!(
            "[ТРЕДЫ] {} живых тредов — аномально много; guard ATLAS DEADLOCK скоро сработает.\n",
            cur.threads
        ));
    }

    // --- 2. Кольцо: повторы, таймауты, дедлоки, падения тредов ---
    {
        let g = rt.trace.lock().unwrap();
        if g.total == 0 {
            s.push_str("Кольцо пусто: `trace on` включает наблюдение.\n");
        } else {
            let mut counts: Vec<(String, usize)> = Vec::new();
            let mut timeouts = 0usize;
            let mut deadlocks = 0usize;
            let mut thread_err = 0usize;
            for e in g.events.iter() {
                match e.kind {
                    TraceKind::Error | TraceKind::Thread => {
                        // Числа -> '#', чтобы одинаковые ошибки с разными id
                        // сложились в один счётчик, а не разъехались на 50.
                        let key: String = e
                            .text
                            .chars()
                            .map(|c| if c.is_ascii_digit() { '#' } else { c })
                            .take(120)
                            .collect();
                        if let Some(slot) = counts.iter_mut().find(|(k, _)| *k == key) {
                            slot.1 += 1;
                        } else {
                            counts.push((key, 1));
                        }
                        if e.kind == TraceKind::Thread {
                            thread_err += 1;
                        }
                    }
                    _ => {}
                }
                let low = e.text.to_lowercase();
                if low.contains("atlas timeout") {
                    timeouts += 1;
                }
                if low.contains("atlas deadlock") {
                    deadlocks += 1;
                }
            }
            counts.sort_by(|a, b| b.1.cmp(&a.1));
            if timeouts > 0 {
                findings += 1;
                s.push_str(&format!(
                    "[ТАЙМАУТ] ATLAS TIMEOUT x{} — вечный цикл в интерпретируемом коде.\n\
                     \u{2003}Смотри `trace` перед первым из них: там имя файла.\n",
                    timeouts
                ));
            }
            if deadlocks > 0 {
                findings += 1;
                s.push_str(&format!(
                    "[ДЕДЛОК] ATLAS DEADLOCK x{} — WaitSeconds вне coroutine, треды не\n\
                     \u{2003}продвигаются. Значит вызов идёт из top-level, а не из ForkThread.\n",
                    deadlocks
                ));
            }
            if thread_err > 0 {
                findings += 1;
                s.push_str(&format!(
                    "[ТРЕДЫ] THREAD ERROR x{} — падают корутины; __moho_pump их удаляет,\n\
                     \u{2003}поэтому симуляция теряет логику МОЛЧА. Это самый опасный класс.\n",
                    thread_err
                ));
            }
            let repeated: Vec<&(String, usize)> = counts.iter().filter(|(_, c)| *c >= 3).collect();
            if !repeated.is_empty() {
                findings += 1;
                s.push_str(&format!("[ПОВТОРЫ] {} ошибок встречается 3+ раз:\n", repeated.len()));
                for (k, c) in repeated.iter().take(8) {
                    s.push_str(&format!("    x{:<4} {}\n", c, k));
                }
                s.push_str("    (числа заменены на #, чтобы одинаковые ошибки сложились)\n");
            }
        }
    }

    // --- 3. Реестры ---
    if cur.units == 0 {
        findings += 1;
        s.push_str("[РЕЕСТР] __bp_units пуст — .bp не загружены или store_bp не вызывается.\n");
    }
    if cur.modcache > 0 && cur.blueprints == 0 {
        findings += 1;
        s.push_str("[РЕЕСТР] modcache непуст, а __blueprints пуст — модули загрузились, но\n\
         \u{2003}блупринты не зарегистрировались (подозрительно).\n");
    }

    if findings == 0 {
        s.push_str(
            "HUNT: признаков утечки/зависания/повторов не найдено.\n\
             \u{2003}ОГОВОРКА: отсутствие находок при пустом кольце НИЧЕГО не доказывает —\n\
             \u{2003}включи `trace on`, погоняй игру, затем повтори `hunt`.\n",
        );
    } else {
        s.push_str(&format!("\nHUNT: находок = {}. Каждое заключение цитирует число-основание.\n", findings));
    }
    s
}

// ==================== ЛОГИ В ФАЙЛ ====================

pub fn log_path() -> std::path::PathBuf {
    let pd = crate::config::personal_dir();
    if !pd.is_empty() {
        let p = std::path::PathBuf::from(pd).join("atlas_debug.log");
        if p.parent().map(|d| d.is_dir()).unwrap_or(false) {
            return p;
        }
    }
    std::path::PathBuf::from("atlas_debug.log")
}

fn g_len(rt: &Arc<MohoRuntime>) -> usize {
    rt.trace.lock().map(|g| g.events.len()).unwrap_or(0)
}

pub fn save_log(rt: &Arc<MohoRuntime>) -> String {
    let path = log_path();
    // Копируем тело под замком и выходим из-за него ДО записи в файл:
    // дисковый I/O под Mutex — тот самый путь, которым рождаются дедлоки.
    let body = {
        let g = rt.trace.lock().unwrap();
        let mut b = String::new();
        for e in g.events.iter() {
            b.push_str(&format!("{} [{}] {}\n", fmt_ms(e.ms), e.kind.tag(), e.text));
        }
        b
    };
    if body.is_empty() {
        return format!(
            "logsave: кольцо пусто -> нечего писать (включи `trace on`). Путь: {}",
            path.display()
        );
    }
    match std::fs::OpenOptions::new().create(true).append(true).open(&path) {
        Ok(mut f) => {
            use std::io::Write;
            let stamp = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0);
            if let Err(e) = writeln!(f, "--- ATLAS DEBUG LOG t={} событий={} ---", stamp, g_len(rt)) {
                return format!("logsave: запись не удалась: {}", e);
            }
            if let Err(e) = f.write_all(body.as_bytes()) {
                return format!("logsave: запись не удалась: {}", e);
            }
            format!(
                "logsave: записано {} строк -> {} (дозапись; файл растёт, чисти сам)",
                body.lines().count(),
                path.display()
            )
        }
        Err(e) => format!("logsave: не открыт {}: {}", path.display(), e),
    }
}

/// Очистить кольцо после разбора, чтобы следующий `hunt` не видел старое.
/// Сбрасывает и базовый снимок памяти, и курсор выгрузки: иначе дельта `watch`
/// считалась бы от снимка, относящегося к уже выброшенным событиям.
pub fn clear_ring(rt: &Arc<MohoRuntime>) -> String {
    let mut g = rt.trace.lock().unwrap();
    let n = g.events.len();
    g.events.clear();
    g.natives.clear();
    g.total = 0;
    g.drained = 0;
    g.started = Some(Instant::now());
    g.mem_last = None;
    format!("кольцо очищено (было {} событий, базовый снимок памяти сброшен)", n)
}
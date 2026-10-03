// src/moho/shim.rs
// ЯДРО ATLAS: состояние Lua, препроцессор MOHO->5.1, загрузка .lua/.bp,
// моки движка, ОТЛАДОЧНОЕ КОЛЬЦО и РЕЛЬЕФ из .scmap.
//
// ЧЕТЫРЕ СОЗНАТЕЛЬНЫХ ГРАНИЦЫ ЧЕСТНОСТИ (каждая — из наших ошибок сессии):
//  1) КОЛЬЦО МОЛЧИТ ПО УМОЛЧАНИЮ (__moho_trace_on=false). Наблюдатель не
//     меняет наблюдаемое: 423/0 не двигаются, пока не включишь `trace on`.
//  2) GetTerrainHeight при пустом полотне возвращает 0.0 — ПОБИТОВО как
//     раньше. Значит загрузка не зависит от рельефа: mapload можно включать
//     по частям, не рискуя регрессом.
//  3) ВСЕ ПОДСЧЁТЫ ТАБЛИЦ — В LUA (#t/len() не считает строковые ключи, а
//     __bp_units со строковыми). Счёт через Rust врал бы.
//  4) НИКАКИХ compile()/proto(): компиляцию на проверку синтаксиса делает
//     сам Lua через load()/loadstring() — это сняло E0624 и E0599.

use std::collections::HashSet;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use crate::moho::compat;
use crate::moho::debug;
use crate::moho::preproc;
use crate::moho::vfs::Vfs;

// ==================== МИР / РАНТАЙМ ====================

pub struct WorldState {
    pub tick: u32,
    pub rng_state: u64,
    /// Игровые ЕДИНИЦЫ (не метры): 1024 = 20x20 км, т.к. 5 км = 256 ед.
    /// [FAF wiki GPG-Map-Editor]. Заполняется из .scmap @0x10/0x14 командой mapload.
    pub map_size_x: f32,
    pub map_size_z: f32,
    /// РЕЛЬЕФ: сетка = units+1 [SupCom wiki Creating mountains], row-major u16.
    /// Пусто по умолчанию -> GetTerrainHeight возвращает 0.0 РОВНО как раньше.
    pub hm_grid: u32,
    pub heights: Vec<u16>,
    /// u16 -> метры. НЕ подтверждено: калибровка печатается в mapload/maphm.
    pub hm_scale: f32,
}

impl WorldState {
    pub fn new() -> Self {
        Self {
            tick: 0,
            rng_state: 0x5EED_0001,
            map_size_x: 256.0,
            map_size_z: 256.0,
            hm_grid: 0,
            heights: Vec::new(),
            hm_scale: 1.0 / 256.0,
        }
    }
}

pub struct MohoRuntime {
    pub vfs: Arc<Vfs>,
    pub world: Arc<Mutex<WorldState>>,
    /// ЕДИНСТВЕННОЕ состояние симуляции: живёт между командами консоли.
    pub lua: Mutex<Option<mlua::Lua>>,
    /// КОЛЬЦО ОТЛАДОЧНЫХ СОБЫТИЙ. Mutex (не Arc): debug::record принимает
    /// &Mutex<TraceRing>, и менять тип поля незачем.
    pub trace: Mutex<debug::TraceRing>,
}

impl MohoRuntime {
    pub fn new(vfs: Arc<Vfs>) -> Self {
        Self {
            vfs,
            world: Arc::new(Mutex::new(WorldState::new())),
            lua: Mutex::new(None),
            trace: Mutex::new(debug::TraceRing::new()),
        }
    }
}

pub fn sim_state(rt: &Arc<MohoRuntime>) -> mlua::Result<mlua::Lua> {
    let mut slot = rt.lua.lock().unwrap();
    if slot.is_none() {
        *slot = Some(create_sim_state(rt)?);
    }
    let lua = slot.as_ref().unwrap().clone();
    if let Ok(reset) = lua.globals().get::<mlua::Function>("__moho_reset_fuel") {
        let _ = reset.call::<()>(());
    }
    Ok(lua)
}

pub fn reset_state(rt: &Arc<MohoRuntime>) {
    *rt.lua.lock().unwrap() = None;
}

// ==================== КЛАССИФИКАЦИЯ ПАДЕНИЙ ====================

#[derive(PartialEq, Eq, Clone, Copy, Debug)]
pub enum LoadOutcome {
    UiDeferred,
    UiPending,
    RealError,
}

// Префиксы UI-дерева ТОЛЬКО для группировки ОТЧЁТА (не для фильтра загрузки).
const UI_TREE: &[&str] = &[
    "lua/ui/", "lua/maui/", "lua/skins/", "lua/options/", "lua/keymap/",
    "lua/debug/", "lua/user/",
];

const UI_RUNTIME_MARKERS: &[&str] = &[
    "uiutil.lua", "layouthelpers.lua", "effecthelpers.lua", "getframe", "uifile",
    "fillparent", "create screengroup", "createscreengroup",
    "attempt to index field 'top'", "attempt to index field 'width'",
    "attempt to index field 'height'", "attempt to index field 'left'",
    "attempt to index field 'right'", "attempt to index field 'bottom'",
    "error importing '/lua/ui/", "error importing '/lua/maui/",
    "error importing '/lua/skins/", "queue textures", "queuetextures",
    "gettexturedimensions",
];

// Синтаксические артефакты препроцессора: в SIM-дереве = RealError (чиним),
// в UI-дереве = UiPending (слой переписывается под Bevy, метки там не нужны).
const SYNTAX_HARD_MARKERS: &[&str] = &["near '::'", "near 'until'", "label ", "near 'continue'"];
// Рантайм-артефакты логики/препроцессора: RealError ВСЕГДА, даже в UI.
const RUNTIME_HARD_MARKERS: &[&str] = &[
    "access to nonexistent global", "attempt to call", "attempt to index a nil value",
    "attempt to perform arithmetic", "attempt to compare", "attempt to concatenate",
    "atlas timeout", "atlas deadlock", "bad argument",
];

const SYNTAX_MARKERS: &[&str] = &[
    "syntax error", "near '<eof>'", "unexpected symbol", "'end' expected",
    "'}' expected", "'=' expected", "')' expected", "invalid escape",
    "near 'else'", "near 'elseif'", "near '|'", "near 'bit'",
];

fn classify(err: &str, path: &str) -> LoadOutcome {
    let lower = err.to_lowercase();
    let p = path.to_lowercase();
    let in_ui = UI_TREE.iter().any(|pre| p.starts_with(pre));
    let is_syntax = SYNTAX_MARKERS.iter().any(|m| lower.contains(m))
        || SYNTAX_HARD_MARKERS.iter().any(|m| lower.contains(m));

    if UI_RUNTIME_MARKERS.iter().any(|m| lower.contains(m)) {
        return LoadOutcome::UiDeferred;
    }
    if RUNTIME_HARD_MARKERS.iter().any(|m| lower.contains(m)) {
        return LoadOutcome::RealError;
    }
    if in_ui && is_syntax {
        return LoadOutcome::UiPending;
    }
    if SYNTAX_HARD_MARKERS.iter().any(|m| lower.contains(m)) {
        return LoadOutcome::RealError;
    }
    LoadOutcome::RealError
}

// Имя файла-источника внутри сообщения: [string "X"]:LN ...
// Каскад = ошибка пришла из ЧУЖОГО файла (импорта-предка), а не из самого
// загружаемого. Каскад — не отдельный баг: 8 файлов умерли из-за одного
// build_templates.lua.
fn err_origin(msg: &str) -> Option<String> {
    let s = msg.find("[string \"")? + "[string \"".len();
    let rest = &msg[s..];
    let e = rest.find("\"]")?;
    Some(normalize_path(&rest[..e]))
}

// ==================== LUA-ЧАСТИ ====================

const PRELUDE: &str = r#"
if not table.pack then
  function table.pack(...) return { n = select('#', ...), ... } end
end
if not table.unpack then table.unpack = unpack end

function repr(val, seen)
  seen = seen or {}
  if type(val) == 'string' then
    return string.format('%q', val)
  elseif type(val) == 'number' or type(val) == 'boolean' then
    return tostring(val)
  elseif type(val) == 'table' then
    if seen[val] then return '<cycle>' end
    seen[val] = true
    local parts = {}
    local count = 0
    for k, v in pairs(val) do
      count = count + 1
      if count > 5 then parts[#parts + 1] = '...' break end
      local ks = type(k) == 'string' and k or '[' .. repr(k, seen) .. ']'
      parts[#parts + 1] = ks .. '=' .. repr(v, seen)
    end
    return '{' .. table.concat(parts, ', ') .. '}'
  else
    return tostring(val)
  end
end

function Vector(x, y, z) return { x = x or 0, y = y or 0, z = z or 0 } end
function Vector2(x, y) return { x = x or 0, y = y or 0 } end
function VAdd(a, b) return Vector(a.x + b.x, a.y + b.y, a.z + b.z) end
function VDiff(a, b) return Vector(a.x - b.x, a.y - b.y, a.z - b.z) end
function VMult(a, s) return Vector(a.x * s, a.y * s, a.z * s) end
function VDot(a, b) return a.x * b.x + a.y * b.y + a.z * b.z end
function VLengthSq(a) return VDot(a, a) end
function VLength(a) return math.sqrt(VLengthSq(a)) end
function VDist3Sq(a, b) return VLengthSq(VDiff(a, b)) end
function VDist3(a, b) return math.sqrt(VDist3Sq(a, b)) end
function VDist2Sq(a, b) return (a.x - b.x) ^ 2 + (a.y - b.y) ^ 2 end
function VDist2(a, b) return math.sqrt(VDist2Sq(a, b)) end
function VNormal(a)
  local l = VLength(a)
  if l == 0 then return Vector(0, 0, 0) end
  return VMult(a, 1 / l)
end

__moho_threads = __moho_threads or {}
function ForkThread(fn, ...)
  local co = coroutine.create(fn)
  table.insert(__moho_threads, { co = co, args = table.pack(...) })
  return co
end
function __moho_pump()
  local i = 1
  while i <= #__moho_threads do
    local t = __moho_threads[i]
    local st = coroutine.status(t.co)
    if st == 'suspended' then
      local ok, err = coroutine.resume(t.co, table.unpack(t.args, 1, t.args.n))
      t.args = table.pack()
      if not ok then
        print('THREAD ERROR: ' .. tostring(err))
        table.remove(__moho_threads, i)
      else
        i = i + 1
      end
    elseif st == 'dead' then
      table.remove(__moho_threads, i)
    else
      i = i + 1
    end
  end
end

__moho_wait_guard = 0
function WaitSeconds(_s)
  local co = coroutine.running()
  if co then
    __moho_wait_guard = 0
    coroutine.yield()
    return
  end
  __moho_wait_guard = __moho_wait_guard + 1
  if __moho_wait_guard > 2000 then
    error('ATLAS DEADLOCK: WaitSeconds вне coroutine, треды не продвигаются')
  end
  __moho_pump()
end
function WaitTicks(_t) WaitSeconds(0) end
function SuspendCurrentThread()
  local co = coroutine.running()
  if co then coroutine.yield() end
end
function WaitFor(_e)
  local co = coroutine.running()
  if co then coroutine.yield() else __moho_pump() end
end
function WaitUntil(_cond, _timeout)
  local co = coroutine.running()
  if co then coroutine.yield() end
end

__moho_modcache = __moho_modcache or {}
-- Явно создаём реестр применённых override'ov: doscript делает
-- globals().get::<Table>("__moho_overridden")? и без таблицы вернёт Err
-- на первом же override (config.lua из globalinit). Идемпотентно.
__moho_overridden = __moho_overridden or {}
__moho_cur = false
__moho_bp_src = false
-- Кольцо отладки молчит по умолчанию: наблюдатель не меняет наблюдаемое.
__moho_trace_on = false

function import(name)
  local key = string.lower(name)
  if __moho_modcache[key] then return __moho_modcache[key] end
  SPEW("Loading module '", name, "'")
  local mod_env = __moho_import(name)
  __moho_modcache[key] = mod_env
  return mod_env
end
"#;

const CLASS_LUA: &str = r#"
function Class(base, _attr)
  local c = { _isClass = true }
  c.base = base
  setmetatable(c, {
    __index = base,
    __call = function(self, a, ...)
      if type(a) == 'table' and select('#', ...) == 0 then
        for k, v in pairs(a) do self[k] = v end
        return self
      end
      local obj = setmetatable({}, { __index = self })
      if obj.OnCreate then obj:OnCreate(a, ...) end
      return obj
    end,
  })
  if _attr then
    for k, v in pairs(_attr) do c[k] = v end
  end
  return c
end

moho = moho or {}
if not moho.Entity then moho.Entity = Class(nil) end
if not moho.Projectile then moho.Projectile = Class(moho.Entity) end
if not moho.Unit then moho.Unit = Class(moho.Entity) end
if not moho.Weapon then moho.Weapon = Class(moho.Entity) end
if not moho.Shield then moho.Shield = Class(moho.Entity) end
if not moho.Prop then moho.Prop = Class(moho.Entity) end
if not moho.ReconBlip then moho.ReconBlip = Class(moho.Entity) end
if not moho.CAiBrain then moho.CAiBrain = Class(nil) end
if not moho.CPlatoon then moho.CPlatoon = Class(nil) end

function CreateEmitterAtEntity(...) __moho_log('CreateEmitterAtEntity') return nil end
function CreateEmitterAtPosition(...) __moho_log('CreateEmitterAtPosition') return nil end
function CreateAttachedEmitter(...) __moho_log('CreateAttachedEmitter') return nil end
function CreateLightParticle(...) __moho_log('CreateLightParticle') return nil end
function CreateDecal(...) __moho_log('CreateDecal') return nil end
function CreateSplat(...) __moho_log('CreateSplat') return nil end
function CreateAnimator(_u) __moho_log('CreateAnimator') return nil end
function CreateRotator(_u, _b) __moho_log('CreateRotator') return nil end
function CreateSlider(_u, _b) __moho_log('CreateSlider') return nil end
function AttachBeamEntityToEntity(...) __moho_log('AttachBeamEntityToEntity') return nil end
function GetRandomFloat(a, b) return Random(a, b) end
function GetRandomInt(a, b) return math.floor(Random(a, b + 1)) end
function table.merge(a, b) for k, v in pairs(b) do a[k] = v end return a end

__blueprints = __blueprints or {}
__moho_bp_registry = __moho_bp_registry or {}
__bp_units = __bp_units or {}
__bp_effects = __bp_effects or {}
__bp_other = __bp_other or {}

local BP_SUFFIXES = {
  '_unit', '_emit', '_emitter', '_entity', '_proj', '_projectile', '_mesh',
  '_wpn', '_weapon', '_beam', '_ability', '_anim', '_animpack', '_animtree',
  '_rawanim', '_costume', '_prop', '_trail', '_buffer', '_fx', '_sfx',
  '_voice', '_sound', '_texture',
}

local function strip_bp_suffix(base)
  for _, s in ipairs(BP_SUFFIXES) do
    if #base > #s and base:sub(-#s) == s then
      local stem = base:sub(1, #base - #s)
      if #stem >= 3 then return stem end
    end
  end
  return base
end

local function bp_type_from_path(p)
  local low = string.lower(tostring(p or ''))
  if low:find('^units/') or low:find('/units/') then return 'unit' end
  if low:find('^props/') or low:find('/props/') then return 'prop' end
  if low:find('^effects/') or low:find('/effects/') then return 'effect' end
  if low:find('^meshes/') or low:find('/meshes/') then return 'mesh' end
  if low:find('^projectiles/') or low:find('/projectiles/') then return 'projectile' end
  if low:find('^weapons/') or low:find('/weapons/') then return 'weapon' end
  if low:find('^beams/') or low:find('/beams/') then return 'beam' end
  if low:find('^abilities/') or low:find('/abilities/') then return 'ability' end
  if low:find('^costumes/') or low:find('/costumes/') then return 'costume' end
  if low:find('^anim') or low:find('/anim') then return 'anim' end
  return 'other'
end

local function resolve_id(spec, src)
  local id = spec.BlueprintId
  if type(id) == 'string'
     and not id:find('/') and not id:find('%.') and #id >= 3 then
    return string.lower(id)
  end
  if (not id) and type(spec.General) == 'table' then
    local gid = spec.General.UnitId or spec.General.BlueprintId
    if type(gid) == 'string'
       and not gid:find('/') and not gid:find('%.') and #gid >= 3 then
      return string.lower(gid)
    end
  end
  if type(src) == 'string' then
    local base = src:match('([^/]+)%.bp$') or src:match('([^/]+)$')
    if base then
      base = base:gsub('%.bp$', '')
      return strip_bp_suffix(string.lower(base))
    end
  end
  return nil
end

local function store_bp(spec, factory_type)
  if type(spec) ~= 'table' then return spec end
  local id = resolve_id(spec, __moho_bp_src)
  if not id then return spec end
  local t = bp_type_from_path(__moho_bp_src)
  if t == 'other' and factory_type then t = factory_type end
  local bucket
  if t == 'unit' then
    bucket = __bp_units
  elseif t == 'effect' or t == 'prop' then
    bucket = __bp_effects
  else
    bucket = __bp_other
  end
  bucket[id] = spec
  __moho_bp_registry[id] = spec
  __blueprints[id] = spec
  if type(__moho_bp_src) == 'string' then
    __blueprints[string.lower(__moho_bp_src)] = spec
  end
  return spec
end

local FACTORY_TYPE = {
  UnitBlueprint = 'unit', RegisterUnitBlueprint = 'unit',
  PropBlueprint = 'prop', RegisterPropBlueprint = 'prop',
  EmitterBlueprint = 'effect', RegisterEmitterBlueprint = 'effect',
  TrailEmitterBlueprint = 'effect', RegisterTrailEmitterBlueprint = 'effect',
  BeamBlueprint = 'beam', RegisterBeamBlueprint = 'beam',
  ProjectileBlueprint = 'projectile', RegisterProjectileBlueprint = 'projectile',
  MeshBlueprint = 'mesh', RegisterMeshBlueprint = 'mesh',
  WeaponBlueprint = 'weapon', RegisterWeaponBlueprint = 'weapon',
  AbilityBlueprint = 'ability', RegisterAbilityBlueprint = 'ability',
  AnimPackBlueprint = 'anim', RegisterAnimPackBlueprint = 'anim',
  AnimTreeBlueprint = 'anim', RegisterAnimTreeBlueprint = 'anim',
  RawAnimBlueprint = 'anim', RegisterRawAnimBlueprint = 'anim',
  EntityCostumeBlueprint = 'costume', RegisterEntityCostumeBlueprint = 'costume',
  EntityCostumeSetBlueprint = 'costume', RegisterEntityCostumeSetBlueprint = 'costume',
  PlatoonBlueprint = 'other', RegisterPlatoonBlueprint = 'other',
  AiSkirmishArchetypeBlueprint = 'other', RegisterAiSkirmishArchetypeBlueprint = 'other',
  AiSkirmishBaseBlueprint = 'other', RegisterAiSkirmishBaseBlueprint = 'other',
  AiSkirmishEngineerBlueprint = 'other', RegisterAiSkirmishEngineerBlueprint = 'other',
  AiSkirmishFactoryBlueprint = 'other', RegisterAiSkirmishFactoryBlueprint = 'other',
  AiSkirmishFormBlueprint = 'other', RegisterAiSkirmishFormBlueprint = 'other',
  AiSkirmishResponseBlueprint = 'other', RegisterAiSkirmishResponseBlueprint = 'other',
  VendorBlueprint = 'other', RegisterVendorBlueprint = 'other',
}
for name, typ in pairs(FACTORY_TYPE) do
  _G[name] = function(spec) return store_bp(spec, typ) end
end
"#;

// ==================== СИСТЕМНЫЕ ПОДМЕНЫ ====================

const CONFIG_OVERRIDE: &str = r#"
config = config or {}
function KillThread(_t) end
function CreateThread(f, ...)
  local co = coroutine.create(f)
  coroutine.resume(co, ...)
  return co
end
function GetCurrentThread() return coroutine.running() end
function WaitUntil(_cond, _timeout) end
function Spawn(_f, ...) end
RPC = RPC or {}
RPCCall = function(...) end
SRPC = SRPC or {}
SRPCCall = function(...) end
"#;

const IMPORT_OVERRIDE: &str =
    "-- ATLAS: /lua/system/import.lua subverted (native import active)\n";

const LOC_OVERRIDE: &str = r#"
loc_table = loc_table or {}
language = 'en_US'
__language = 'en_US'
function okLanguage(la) return tostring(la or 'en_US') end
function loadLanguage(la) return 'en_US' end
function LoadLanguage(la) return 'en_US' end
function GetLanguage() return 'en_US' end
function SessionGetLanguage() return 'en_US' end
function LOC(s) return s end
function LOCF(s, ...) return s end
function dbFilename(la)
  return '/lua/localization/' .. tostring(la) .. '_strings_db.lua'
end
function HasLocalizedVO() return false end
function GetLocFileList() return {} end
"#;

const MULTIEVENT_OVERRIDE: &str = r#"
MultiEvent = Class() {
  __init = function(self)
    self.EventCallbacks = { n = 0 }
    self.EventIsSet = false
  end,
  AddCallback = function(self, cb)
    self.EventCallbacks.n = self.EventCallbacks.n + 1
    self.EventCallbacks[self.EventCallbacks.n] = cb
  end,
  RemoveCallback = function(self, cb)
    for k = 1, self.EventCallbacks.n do
      if self.EventCallbacks[k] == cb then
        table.remove(self.EventCallbacks, k)
        self.EventCallbacks.n = self.EventCallbacks.n - 1
        break
      end
    end
  end,
  SetEvent = function(self) self.EventIsSet = true end,
  ClearEvent = function(self) self.EventIsSet = false end,
  TriggerEvents = function(self, ...)
    for k = 1, self.EventCallbacks.n do
      local v = self.EventCallbacks[k]
      if type(v) == 'function' then v(...) end
    end
  end,
}
"#;

const SAVELOAD_OVERRIDE: &str = r#"
function SaveGame(_name) __moho_log('SaveGame') return true end
function LoadGame(_name) __moho_log('LoadGame') return false end
function SaveGameToBuffer() __moho_log('SaveGameToBuffer') return '' end
function LoadGameFromBuffer(_s) __moho_log('LoadGameFromBuffer') return false end
function export_funs(_name, tbl) return tbl end
function import_funs(_name) return {} end
function serialize(...) return '' end
function unserialize(_s) return {} end
function deserialize(_s) return {} end
"#;

// Свой class.lua. Нативный class.lua GPG ставит метатаблицу-защиту
// «нельзя добавлять __index после определения класса» (class.lua:273), и её же
// ConvertCClassToLuaClass (class.lua:405) нарушает защиту на наших мок-классах
// moho.* -> каскад globalinit/sessioninit/siminit/userinit/viewerinit.
const CLASS_OVERRIDE: &str = r#"
function Class(base, _attr)
  local c = { _isClass = true }
  c.base = base
  setmetatable(c, {
    __index = base,
    __call = function(self, a, ...)
      if type(a) == 'table' and select('#', ...) == 0 then
        for k, v in pairs(a) do self[k] = v end
        return self
      end
      local obj = setmetatable({}, { __index = self })
      if obj.OnCreate then obj:OnCreate(a, ...) end
      return obj
    end,
  })
  if _attr then
    for k, v in pairs(_attr) do c[k] = v end
  end
  return c
end
function BaseClass() return Class(nil) end
function ConvertCClassToLuaClass(cclass)
  return cclass
end
"#;

// ДВИЖКОВЫЙ PREFETCH-СЛОЙ (закрывает siminit:232 и userinit:27 —
// «attempt to call global 'CreatePrefetchSet' (a nil value)»).
const PREFETCH_STUB: &str = r#"
__active_mods = __active_mods or {}
local _noop = function() return nil end
local _magic
_magic = setmetatable({}, {
  __index = function() return _noop end,
  __call = function() return _magic end,
})
function CreatePrefetchSet() return _magic end
if not CurrentTime then
  function CurrentTime() return (GetGameTick and GetGameTick()) or 0 end
end
"#;

// ==================== ХЕЛПЕРЫ ПУТЕЙ / ИСТОЧНИКОВ ====================

fn normalize_path(name: &str) -> String {
    let mut n = name.replace('\\', "/").to_lowercase();
    loop {
        let t = n
            .trim_start_matches("../")
            .trim_start_matches("./")
            .trim_start_matches('/')
            .to_string();
        if t == n {
            break;
        }
        n = t;
    }
    n
}

fn system_override(name: &str) -> Option<&'static str> {
    let n = normalize_path(name);
    if n.ends_with("lua/system/config.lua") {
        Some(CONFIG_OVERRIDE)
    } else if n.ends_with("lua/system/import.lua") {
        Some(IMPORT_OVERRIDE)
    } else if n.ends_with("lua/system/localization.lua") {
        Some(LOC_OVERRIDE)
    } else if n.ends_with("lua/system/multievent.lua") {
        Some(MULTIEVENT_OVERRIDE)
    } else if n.ends_with("lua/system/saveload.lua") {
        Some(SAVELOAD_OVERRIDE)
    } else if n.ends_with("lua/system/class.lua") {
        Some(CLASS_OVERRIDE)
    } else {
        None
    }
}

fn read_raw_source(vfs: &Vfs, name: &str) -> Option<String> {
    let norm = normalize_path(name);
    if let Some(s) = vfs.read_string(&norm) {
        return Some(s);
    }
    if let Some(s) = vfs.read_string(&format!("/{}", norm)) {
        return Some(s);
    }
    let mut found = None;
    for p in ["lua/sim/", "lua/ai/", "lua/", "tests/", ""] {
        if let Some(s) = vfs.read_string(&format!("{}{}", p, norm)) {
            found = Some(s);
            break;
        }
        if let Some(s) = vfs.read_string(&format!("/{}{}", p, norm)) {
            found = Some(s);
            break;
        }
    }
    if found.is_none() {
        for k in vfs.list("") {
            if k.ends_with(&norm) {
                found = vfs.read_string(&k);
                break;
            }
        }
    }
    found
}

// ==================== ИСПОЛЕНИЕ С РЕТРАЯМИ (5 РЕЖИМОВ) ====================
// Возвращаем ошибку РЕЖИМА 0 (канонического). Взаимоисключающие ветки:
// mlua::Error не Copy -> иначе E0382 (двойной move при idx==0).

fn exec_with_retry(lua: &mlua::Lua, raw: &str, name: &str) -> mlua::Result<()> {
    preproc::set_lex_context(name);
    let mut first_err: Option<mlua::Error> = None;
    let mut last: Option<mlua::Error> = None;
    for (idx, t) in [
        preproc::moho_to_lua51(raw),
        preproc::moho_to_lua51_aggressive(raw),
        preproc::moho_to_lua51_allcomment(raw),
        preproc::moho_to_lua51_repeat(raw),
        preproc::moho_to_lua51_nocontinue(raw),
    ]
    .iter()
    .enumerate()
    {
        match lua.load(t).set_name(name).exec() {
            Ok(_) => return Ok(()),
            Err(e) => {
                if idx == 0 {
                    first_err = Some(e);
                } else {
                    last = Some(e);
                }
            }
        }
    }
    if let Some(e) = first_err.as_ref().or(last.as_ref()) {
        let msg = e.to_string();
        let base = if first_err.is_some() {
            preproc::moho_to_lua51(raw)
        } else {
            preproc::moho_to_lua51_repeat(raw)
        };
        if let Some(start) = msg.find("[string \"") {
            if let Some(rel) = msg[start..].find("]:") {
                let rest = &msg[start + rel + 2..];
                let num: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
                if let Ok(ln) = num.parse::<usize>() {
                    let rl: Vec<&str> = raw.lines().collect();
                    let tl: Vec<&str> = base.lines().collect();
                    if ln <= rl.len() {
                        let from = ln.saturating_sub(2);
                        let to = std::cmp::min(ln + 1, rl.len().saturating_sub(1));
                        eprintln!("--- DIAG {} @{} (режим 0) ---", name, ln);
                        for i in from..=to {
                            eprintln!("  R{:04}: {}", i + 1, rl[i]);
                            if let Some(t) = tl.get(i) {
                                eprintln!("  T{:04}: {}", i + 1, t);
                            }
                        }
                        eprintln!("------------------------");
                    }
                }
            }
        }
    }
    Err(first_err.or(last).unwrap())
}

// ==================== СОЗДАНИЕ СОСТОЯНИЯ ====================

pub fn create_sim_state(rt: &Arc<MohoRuntime>) -> mlua::Result<mlua::Lua> {
    let lua = mlua::Lua::new();

    // WATCHDOG. Ловит ТОЛЬКО интерпретируемые циклы: LuaJIT отключает хуки VM
    // для JIT-кода, поэтому против JIT-циклов работают guard-счётчики
    // (WaitSeconds) + reset_state.
    let fuel = Arc::new(AtomicU64::new(0));
    {
        let fuel_hook = fuel.clone();
        let triggers = mlua::HookTriggers {
            on_calls: false,
            on_returns: false,
            every_line: false,
            every_nth_instruction: Some(100_000),
        };
        let _ = lua.set_hook(triggers, move |_lua, _dbg| {
            let n = fuel_hook.fetch_add(1, Ordering::Relaxed);
            if n > 3000 {
                return Err(mlua::Error::runtime(
                    "ATLAS TIMEOUT: превышен бюджет инструкций (вечный цикл)".to_string(),
                ));
            }
            Ok(mlua::VmState::Continue)
        });
    }
    let fuel_reset = fuel.clone();
    lua.globals().set(
        "__moho_reset_fuel",
        lua.create_function(move |_, _: ()| {
            fuel_reset.store(0, Ordering::Relaxed);
            Ok(())
        })?,
    )?;

    compat::apply(&lua)?;
    lua.load(PRELUDE).exec()?;
    lua.load(CLASS_LUA).exec()?;

    // Каждый логгер дублирует вывод в кольцо, НО только когда __moho_trace_on
    // истинно: при выключенном флаге поведение побитово идентично текущему
    // (423/0 не двигаются). Консоль забирает кольцо через debug::take_new.
    let rt_lg = rt.clone();
    macro_rules! moho_logger {
        ($tag:expr, $kind:expr) => {{
            let rt_lg = rt_lg.clone();
            lua.create_function(move |l, m: mlua::MultiValue| {
                let s = fmt_mv(m);
                println!("[{}] {}", $tag, s);
                let on: bool = l.globals().get("__moho_trace_on").unwrap_or(false);
                if on {
                    debug::record(&rt_lg.trace, $kind, &s);
                }
                Ok(())
            })?
        }};
    }
    lua.globals().set("print", moho_logger!("SIM", debug::TraceKind::Print))?;
    lua.globals().set("LOG", moho_logger!("LOG", debug::TraceKind::Print))?;
    lua.globals().set("SPEW", moho_logger!("SPEW", debug::TraceKind::Spew))?;
    lua.globals().set("WARN", moho_logger!("WARN", debug::TraceKind::Warn))?;

    let vfs_d = rt.vfs.clone();
    lua.globals().set(
        "doscript",
        lua.create_function(move |lua_ctx, name: String| {
            if let Some(ovr) = system_override(&name) {
                let key = normalize_path(&name);
                let set: mlua::Table = lua_ctx.globals().get("__moho_overridden")?;
                if let mlua::Value::Boolean(true) = set.get::<mlua::Value>(key.clone())? {
                    return Ok(());
                }
                set.set(key, true)?;
                println!("[ATLAS] OVERRIDE applied: {}", name);
                return lua_ctx.load(ovr).set_name(&name).exec();
            }
            let raw = read_raw_source(&vfs_d, &name)
                .ok_or_else(|| mlua::Error::runtime(format!("VFS: файл не найден: {}", name)))?;
            exec_with_retry(lua_ctx, &raw, &name)
        })?,
    )?;

    let vfs_e = rt.vfs.clone();
    lua.globals().set(
        "exists",
        lua.create_function(move |_, name: String| Ok(vfs_e.exists(&name)))?,
    )?;

    let vfs_df = rt.vfs.clone();
    lua.globals().set(
        "__moho_read_raw",
        lua.create_function(move |_, name: String| Ok(read_raw_source(&vfs_df, &name)))?,
    )?;
    let pp: mlua::Function =
        lua.create_function(|_, s: mlua::String| Ok(preproc::moho_to_lua51(&s.to_string_lossy())))?;
    lua.globals().set("moho_to_lua51", pp)?;
    lua.load(
        r#"
local _orig_dofile = dofile
function dofile(path)
    local p = string.gsub(path, '^(%.%.[/\\]+)+', '')
    p = string.gsub(p, '^[/\\]+', '')
    if not string.find(p, '^/') then p = '/' .. p end
    local ovr = __moho_system_override and __moho_system_override(p)
    if ovr then
        local f, err = loadstring(ovr, p)
        if not f then error(err) end
        return f()
    end
    local src = __moho_read_raw(p)
    if src then
        local tr = moho_to_lua51(src)
        local f, err = loadstring(tr, p)
        if not f then error(err) end
        return f()
    end
    local ok, m = pcall(import, p)
    if ok then return m end
    return _orig_dofile(path)
end
"#,
    )
    .exec()?;

    lua.globals().set(
        "__moho_system_override",
        lua.create_function(|_, name: String| Ok(system_override(&name).map(|s| s.to_string())))?,
    )?;

    let vfs_i = rt.vfs.clone();
    lua.globals().set(
        "__moho_import",
        lua.create_function(move |lua_ctx, name: String| {
            let key = name.to_lowercase();
            let cache: mlua::Table = lua_ctx.globals().get("__moho_modcache")?;
            if let mlua::Value::Table(t) = cache.get::<mlua::Value>(key.clone())? {
                return Ok(t);
            }
            if let Some(ovr) = system_override(&name) {
                let np = normalize_path(&name);
                let set: mlua::Table = lua_ctx.globals().get("__moho_overridden")?;
                set.set(np, true)?;
                println!("[ATLAS] OVERRIDE applied (import): {}", name);
                let mod_env = lua_ctx.create_table()?;
                let mt = lua_ctx.create_table()?;
                mt.set("__index", lua_ctx.globals())?;
                mod_env.set_metatable(Some(mt));
                cache.set(key.clone(), mod_env.clone())?;
                match lua_ctx
                    .load(ovr)
                    .set_name(&name)
                    .set_environment(mod_env.clone())
                    .exec()
                {
                    Ok(_) => return Ok(mod_env),
                    Err(e) => {
                        cache.set(key, mlua::Value::Nil)?;
                        return Err(e);
                    }
                }
            }
            match read_raw_source(&vfs_i, &name) {
                Some(raw) => {
                    preproc::set_lex_context(&name);
                    let mod_env = lua_ctx.create_table()?;
                    let mt = lua_ctx.create_table()?;
                    mt.set("__index", lua_ctx.globals())?;
                    mod_env.set_metatable(Some(mt));
                    cache.set(key.clone(), mod_env.clone())?;
                    let prev: mlua::Value = lua_ctx
                        .globals()
                        .get("__moho_cur")
                        .unwrap_or(mlua::Value::Boolean(false));
                    lua_ctx.globals().set("__moho_cur", name.clone())?;
                    // Флаг loaded, а не обнуление last_err: иначе файл, упавший
                    // в режиме 0 и сумевший в режиме 2/3/4/5, ошибочно считался
                    // бы провалившимся (first_err остался бы Some).
                    let mut first_err: Option<mlua::Error> = None;
                    let mut last_err: Option<mlua::Error> = None;
                    let mut loaded = false;
                    for (idx, t) in [
                        preproc::moho_to_lua51(&raw),
                        preproc::moho_to_lua51_aggressive(&raw),
                        preproc::moho_to_lua51_allcomment(&raw),
                        preproc::moho_to_lua51_repeat(&raw),
                        preproc::moho_to_lua51_nocontinue(&raw),
                    ]
                    .iter()
                    .enumerate()
                    {
                        match lua_ctx
                            .load(t)
                            .set_name(&name)
                            .set_environment(mod_env.clone())
                            .exec()
                        {
                            Ok(_) => {
                                loaded = true;
                                break;
                            }
                            Err(e) => {
                                if idx == 0 {
                                    first_err = Some(e);
                                } else {
                                    last_err = Some(e);
                                }
                            }
                        }
                    }
                    lua_ctx.globals().set("__moho_cur", prev)?;
                    if loaded {
                        Ok(mod_env)
                    } else {
                        match first_err.or(last_err) {
                            Some(e) => {
                                cache.set(key, mlua::Value::Nil)?;
                                Err(e)
                            }
                            None => Ok(mod_env),
                        }
                    }
                }
                None => {
                    println!("[WARN] VFS: импорт не найден, авто-числа: {}", name);
                    let mod_env = lua_ctx.create_table()?;
                    let mt = lua_ctx.create_table()?;
                    mt.set(
                        "__index",
                        lua_ctx.create_function(|_, _: mlua::Value| Ok(0_i64))?,
                    )?;
                    mod_env.set_metatable(Some(mt));
                    cache.set(key.clone(), mod_env.clone())?;
                    Ok(mod_env)
                }
            }
        })?,
    )?;

    let vfs_io = rt.vfs.clone();
    let io_tbl: mlua::Table = match lua.globals().get::<mlua::Table>("io") {
        Ok(t) => t,
        Err(_) => lua.create_table()?,
    };
    io_tbl.set(
        "dir",
        lua.create_function(move |l, pattern: String| {
            let prefix = match pattern.rfind('/') {
                Some(i) => normalize_path(&pattern[..=i]),
                None => String::new(),
            };
            let prefix = prefix.trim_end_matches('/').to_string();
            let base = prefix.clone();
            let mut seen: Vec<String> = Vec::new();
            for k in vfs_io.list(&prefix) {
                let rest = k
                    .strip_prefix(&base)
                    .or_else(|| k.strip_prefix(&format!("{}/", base)));
                if let Some(rest) = rest {
                    let rest = rest.trim_start_matches('/');
                    if let Some(s) = rest.split('/').next() {
                        let s = s.to_string();
                        if !s.is_empty() && !seen.contains(&s) {
                            seen.push(s);
                        }
                    }
                }
            }
            let t = l.create_table()?;
            for (i, s) in seen.iter().enumerate() {
                t.set(i + 1, s.clone())?;
            }
            let st = l.create_table()?;
            st.set("i", 0)?;
            st.set("t", t)?;
            let iter = l.create_function(move |_, _: mlua::MultiValue| {
                let i: usize = st.get("i")?;
                let t: mlua::Table = st.get("t")?;
                let nn = t.len()? as usize;
                if i < nn {
                    st.set("i", i + 1)?;
                    let name: mlua::String = t.get(i + 1)?;
                    Ok(mlua::MultiValue::from_vec(vec![
                        mlua::Value::Integer((i + 1) as i64),
                        mlua::Value::String(name),
                    ]))
                } else {
                    Ok(mlua::MultiValue::new())
                }
            })?;
            Ok(iter)
        })?,
    )?;
    lua.globals().set("io", io_tbl)?;

    lua.globals().set(
        "GetUnitBlueprintByName",
        lua.create_function(|lua, name: String| {
            let g = lua.globals();
            let low = name.to_lowercase();
            for tbl in ["__bp_units", "__moho_bp_registry"] {
                if let Ok(reg) = g.get::<mlua::Table>(tbl) {
                    if let mlua::Value::Table(t) = reg.get::<mlua::Value>(low.clone())? {
                        return Ok(mlua::Value::Table(t));
                    }
                }
            }
            Ok(mlua::Value::Nil)
        })?,
    )?;

    let w1 = rt.world.clone();
    lua.globals().set(
        "GetGameTick",
        lua.create_function(move |_, _: ()| Ok(w1.lock().unwrap().tick))?,
    )?;
    let w2 = rt.world.clone();
    lua.globals().set(
        "Random",
        lua.create_function(move |_, args: mlua::MultiValue| {
            let nums: Vec<f32> = args
                .into_iter()
                .filter_map(|v| match v {
                    mlua::Value::Number(n) => Some(n as f32),
                    _ => None,
                })
                .collect();
            let mut w = w2.lock().unwrap();
            w.rng_state = w.rng_state.wrapping_mul(6364136223846793005).wrapping_add(1);
            let r = (w.rng_state >> 33) as f32 / (u32::MAX as f32);
            match nums.len() {
                0 => Ok(r),
                1 => Ok(r * nums[0]),
                _ => Ok(nums[0] + r * (nums[1] - nums[0])),
            }
        })?,
    )?;
    let w3 = rt.world.clone();
    lua.globals().set(
        "GetMapSize",
        lua.create_function(move |_, _: ()| {
            let w = w3.lock().unwrap();
            Ok((w.map_size_x, w.map_size_z))
        })?,
    )?;
    // РЕЛЬЕФ: билинейная интерполяция по настоящему полотну из .scmap.
    // При пустом hm_grid возвращает 0.0 — ПОБИТОВО прежнее поведение, поэтому
    // загрузка (423/0) не зависит от этой правки: mapload включается по частям.
    let w5 = rt.world.clone();
    lua.globals().set(
        "GetTerrainHeight",
        lua.create_function(move |_, (x, z): (f32, f32)| {
            let w = w5.lock().unwrap();
            let g = w.hm_grid;
            if g < 2 {
                return Ok(0.0_f32);
            }
            let gm = g as usize;
            if w.heights.len() < gm * gm {
                return Ok(0.0_f32);
            }
            let last = g as f32 - 1.0;
            let fx = if x.is_finite() { x.clamp(0.0, last) } else { 0.0 };
            let fz = if z.is_finite() { z.clamp(0.0, last) } else { 0.0 };
            let x0 = fx.floor() as usize;
            let z0 = fz.floor() as usize;
            let x1 = (x0 + 1).min(gm - 1);
            let z1 = (z0 + 1).min(gm - 1);
            let tx = fx - x0 as f32;
            let tz = fz - z0 as f32;
            let sc = w.hm_scale;
            let at = |c: usize, r: usize| -> f32 { w.heights[r * gm + c] as f32 * sc };
            let a = at(x0, z0);
            let b = at(x1, z0);
            let c = at(x0, z1);
            let d = at(x1, z1);
            let top = a + (b - a) * tx;
            let bot = c + (d - c) * tx;
            Ok(top + (bot - top) * tz)
        })?,
    )?;

    lua.globals().set(
        "CreateUnitHPR",
        lua.create_function(|lua, args: mlua::MultiValue| {
            __moho_log_rust("CreateUnitHPR");
            if args.len() < 8 {
                return Err(mlua::Error::runtime("CreateUnitHPR requires 8 args"));
            }
            let bp = match &args[0] {
                mlua::Value::String(s) => s.to_str()?.to_string(),
                _ => String::new(),
            };
            let army = match &args[1] {
                mlua::Value::Integer(v) => *v as u32,
                _ => 0,
            };
            let x = match &args[2] {
                mlua::Value::Number(v) => *v as f32,
                _ => 0.0,
            };
            let y = match &args[3] {
                mlua::Value::Number(v) => *v as f32,
                _ => 0.0,
            };
            let z = match &args[4] {
                mlua::Value::Number(v) => *v as f32,
                _ => 0.0,
            };
            let u = lua.create_table()?;
            u.set("BlueprintId", bp)?;
            u.set("Army", army)?;
            u.set("X", x)?;
            u.set("Y", y)?;
            u.set("Z", z)?;
            u.set("Health", 1000.0_f32)?;
            u.set("MaxHealth", 1000.0_f32)?;
            u.set(
                "GetHealth",
                lua.create_function(|_, t: mlua::Table| t.get::<f32>("Health"))?,
            )?;
            u.set(
                "GetMaxHealth",
                lua.create_function(|_, t: mlua::Table| t.get::<f32>("MaxHealth"))?,
            )?;
            u.set(
                "GetPosition",
                lua.create_function(|_, t: mlua::Table| {
                    Ok((t.get::<f32>("X")?, t.get::<f32>("Y")?, t.get::<f32>("Z")?))
                })?,
            )?;
            Ok(u)
        })?,
    )?;
    // СИГНАТУРА МОКА: Damage(source, target, amount, ...). args[0]=source
    // (игнорируется), args[1]=target (таблица), args[2]=amount. В зондах надо
    // передавать Damage(nil, u, 250.0, ...), а НЕ Damage(u, nil, ...).
    lua.globals().set(
        "Damage",
        lua.create_function(|_, args: mlua::MultiValue| {
            if args.len() >= 4 {
                if let mlua::Value::Table(t) = &args[1] {
                    let amt = match &args[2] {
                        mlua::Value::Number(v) => *v as f32,
                        _ => 0.0,
                    };
                    let hp: f32 = t.get("Health").unwrap_or(0.0);
                    t.set("Health", (hp - amt).max(0.0))?;
                }
            }
            Ok(())
        })?,
    )?;
    lua.globals().set(
        "DamageArea",
        lua.create_function(|_, _a: mlua::MultiValue| Ok(()))?,
    )?;
    lua.globals().set(
        "SetAlliance",
        lua.create_function(|lua, args: mlua::MultiValue| {
            if args.len() < 3 {
                return Err(mlua::Error::runtime("SetAlliance requires 3 args"));
            }
            let a1 = match &args[0] {
                mlua::Value::Integer(v) => *v as u32,
                _ => 0,
            };
            let a2 = match &args[1] {
                mlua::Value::Integer(v) => *v as u32,
                _ => 0,
            };
            let kind = match &args[2] {
                mlua::Value::String(s) => s.to_str()?.to_string(),
                _ => String::new(),
            };
            let g = lua.globals();
            let t: mlua::Table = match g.get::<mlua::Table>("__alliances") {
                Ok(t) => t,
                Err(_) => {
                    let t = lua.create_table()?;
                    g.set("__alliances", t.clone())?;
                    t
                }
            };
            t.set(format!("{}_{}", a1, a2), kind)?;
            Ok(())
        })?,
    )?;
    for name in [
        "IssueMove",
        "IssueAttack",
        "IssueStop",
        "IssuePatrol",
        "IssueReclaim",
        "IssueRepair",
    ] {
        lua.globals().set(
            name,
            lua.create_function(|_, (_u, _t): (mlua::Value, mlua::Value)| Ok(()))?,
        )?;
    }

    lua.load(PREFETCH_STUB)
        .set_name("atlas_prefetch_stub")
        .exec()?;

    Ok(lua)
}

fn __moho_log_rust(name: &str) {
    use std::sync::OnceLock;
    static SEEN: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
    let set = SEEN.get_or_init(|| Mutex::new(HashSet::new()));
    if set.lock().unwrap().insert(name.to_string()) {
        println!("[MOHO-CALL] {}", name);
    }
}

pub fn create_user_state(rt: &Arc<MohoRuntime>) -> mlua::Result<mlua::Lua> {
    let lua = mlua::Lua::new();
    compat::apply(&lua)?;
    lua.load(PRELUDE).exec()?;
    lua.load(CLASS_LUA).exec()?;
    lua.globals().set(
        "print",
        lua.create_function(|_, m: mlua::MultiValue| {
            println!("[USER] {}", fmt_mv(m));
            Ok(())
        })?,
    )?;
    lua.globals().set(
        "GetSelectedUnits",
        lua.create_function(|l, _: ()| l.create_table())?,
    )?;
    lua.globals().set(
        "PlaySound",
        lua.create_function(|_, _n: String| Ok(0_u32))?,
    )?;
    lua.globals().set(
        "SetGameSpeed",
        lua.create_function(|_, _s: f32| Ok(()))?,
    )?;
    lua.globals().set(
        "GetGameSpeed",
        lua.create_function(|_, _: ()| Ok(10.0_f32))?,
    )?;
    lua.globals().set(
        "WorldIsPlaying",
        lua.create_function(|_, _: ()| Ok(true))?,
    )?;
    lua.globals().set(
        "WorldIsLoading",
        lua.create_function(|_, _: ()| Ok(false))?,
    )?;
    let _ = rt;
    Ok(lua)
}

fn fmt_mv(m: mlua::MultiValue) -> String {
    m.into_iter()
        .map(|v| match &v {
            mlua::Value::String(s) => s.to_string_lossy(),
            mlua::Value::Number(n) => format!("{}", n),
            mlua::Value::Integer(n) => format!("{}", n),
            mlua::Value::Boolean(b) => b.to_string(),
            mlua::Value::Nil => "nil".into(),
            mlua::Value::Table(t) => format!("<table: {} items>", t.len().unwrap_or(0)),
            _ => "<obj>".into(),
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn err_line(msg: &str) -> usize {
    if let Some(start) = msg.find("[string \"") {
        if let Some(rel) = msg[start..].find("]:") {
            let rest = &msg[start + rel + 2..];
            let num: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
            return num.parse().unwrap_or(0);
        }
    }
    0
}

// ==================== МАССОВАЯ ЗАГРУЗКА ====================

pub fn load_all_lua(rt: &Arc<MohoRuntime>, verbose: bool) -> (usize, usize, Vec<String>) {
    reset_state(rt);
    let lua = match create_sim_state(rt) {
        Ok(l) => l,
        Err(e) => return (0, 1, vec![e.to_string()]),
    };
    *rt.lua.lock().unwrap() = Some(lua.clone());

    let t_bp = std::time::Instant::now();
    let bp_keys = rt.vfs.list("");
    let mut bp_ok = 0usize;
    let mut bp_fail = 0usize;
    for k in bp_keys {
        if !k.ends_with(".bp") {
            continue;
        }
        let raw = match rt.vfs.read_string(&k) {
            Some(s) => s,
            None => continue,
        };
        let _ = lua.globals().set("__moho_bp_src", k.clone());
        if exec_with_retry(&lua, &raw, &k).is_ok() {
            bp_ok += 1;
        } else {
            bp_fail += 1;
        }
    }
    let _ = lua.globals().set("__moho_bp_src", false);
    // ВАЖНО: эти счётчики шли ТОЛЬКО в stdout. Именно поэтому пустой реестр
    // (.bp не смонтированы) был невидим в окне консоли. Команда `bpstat` ниже
    // выводит то же самое В КОНСОЛЬ — не доверяй «423/0», пока не увидел bpstat.
    println!(
        "[LOAD] .bp: {} ок / {} ошибок за {:?}",
        bp_ok,
        bp_fail,
        t_bp.elapsed()
    );

    if let Ok(diag) = lua
        .load(
            r#"
local function count(t) local n = 0 for _ in pairs(t or {}) do n = n + 1 end return n end
local u = count(__bp_units)
local e = count(__bp_effects)
local o = count(__bp_other)
local probe = rawget(__bp_units or {}, 'ueb0101') and 'YES' or 'NO'
local sample = {}
for k in pairs(__bp_units or {}) do
  sample[#sample + 1] = k
  if #sample >= 10 then break end
end
table.sort(sample)
return string.format(
  'units=%d effects=%d other=%d | probe ueb0101=%s | sample: %s',
  u, e, o, probe, table.concat(sample, ', '))
"#,
        )
        .eval::<String>()
    {
        println!("[REGISTRY] {}", diag);
    }

    let keys = rt.vfs.list("lua");
    let mut ok = 0usize;
    let mut fail = 0usize;          // RealError (корни): чиним сейчас
    let mut cascade = 0usize;       // умерли только из-за корня-предка
    let mut ui_deferred = 0usize;   // UiDeferred: рантайм-рендер (Bevy)
    let mut ui_pending = 0usize;    // UiPending: UI-диалект (переписать под Bevy)
    let mut full_dumps = 0usize;
    let mut errs: Vec<String> = Vec::new();
    let mut pending_list: Vec<String> = Vec::new();
    let mut cascade_notes: Vec<String> = Vec::new();
    for k in keys {
        if !k.ends_with(".lua") {
            continue;
        }
        if k.starts_with("lua/tests/")
            || k.contains("/tests/")
            || k.starts_with("lua/web/")
            || k.contains("/web/")
        {
            continue;
        }
        if let Some(ovr) = system_override(&k) {
            let key = normalize_path(&k);
            let already = lua
                .globals()
                .get::<mlua::Table>("__moho_overridden")
                .map(|set| {
                    matches!(
                        set.get::<mlua::Value>(key.clone()),
                        Ok(mlua::Value::Boolean(true))
                    )
                })
                .unwrap_or(false);
            if !already {
                if let Ok(set) = lua.globals().get::<mlua::Table>("__moho_overridden") {
                    let _ = set.set(key, true);
                }
                println!("[LOAD] {} (override)", k);
                match lua.load(ovr).set_name(&k).exec() {
                    Ok(_) => ok += 1,
                    Err(e) => {
                        fail += 1;
                        errs.push(format!("{}: {}", k, e));
                    }
                }
            } else {
                ok += 1;
            }
            continue;
        }
        let raw = match rt.vfs.read_string(&k) {
            Some(s) => s,
            None => continue,
        };
        println!("[LOAD] {}", k);
        if let Ok(reset) = lua.globals().get::<mlua::Function>("__moho_reset_fuel") {
            let _ = reset.call::<()>(());
        }
        let _ = lua.globals().set("__moho_wait_guard", 0i64);
        let _ = lua.globals().set("__moho_ser_n", 0i64);

        let mod_env = match lua.create_table() {
            Ok(e) => e,
            Err(_) => continue,
        };
        let mt = match lua.create_table() {
            Ok(t) => t,
            Err(_) => continue,
        };
        let _ = mt.set("__index", lua.globals());
        mod_env.set_metatable(Some(mt));

        preproc::set_lex_context(&k);
        let mut done = false;
        let mut first_err: Option<String> = None;
        let mut last_err = String::new();
        for (idx, t) in [
            preproc::moho_to_lua51(&raw),
            preproc::moho_to_lua51_aggressive(&raw),
            preproc::moho_to_lua51_allcomment(&raw),
            preproc::moho_to_lua51_repeat(&raw),
            preproc::moho_to_lua51_nocontinue(&raw),
        ]
        .iter()
        .enumerate()
        {
            match lua
                .load(t)
                .set_name(&k)
                .set_environment(mod_env.clone())
                .exec()
            {
                Ok(_) => {
                    done = true;
                    break;
                }
                Err(e) => {
                    let s = format!("{}: {}", k, e);
                    if idx == 0 {
                        first_err = Some(s);
                    } else {
                        last_err = s;
                    }
                }
            }
        }
        let report_err = if done {
            String::new()
        } else {
            first_err.unwrap_or(last_err)
        };

        if done {
            ok += 1;
        } else if err_origin(&report_err)
            .map(|o| o != normalize_path(&k))
            .unwrap_or(false)
        {
            cascade += 1;
            cascade_notes.push(format!(
                "  {}  <-  корень: {}",
                k,
                err_origin(&report_err).unwrap_or_default()
            ));
        } else {
            match classify(&report_err, &k) {
                LoadOutcome::UiDeferred => {
                    ui_deferred += 1;
                }
                LoadOutcome::UiPending => {
                    ui_pending += 1;
                    pending_list.push(k.clone());
                }
                LoadOutcome::RealError => {
                    fail += 1;
                    let mut msg = report_err.clone();
                    let t0 = preproc::moho_to_lua51(&raw);
                    let ln = err_line(&report_err);
                    if verbose || errs.len() < 60 {
                        if ln > 0 {
                            let rl: Vec<&str> = raw.lines().collect();
                            let tl: Vec<&str> = t0.lines().collect();
                            if ln <= rl.len() {
                                let from = ln.saturating_sub(2);
                                let to = std::cmp::min(ln + 1, rl.len().saturating_sub(1));
                                for i in from..=to {
                                    msg.push_str(&format!("\n  R{:04}: {}", i + 1, rl[i]));
                                    if let Some(t) = tl.get(i) {
                                        msg.push_str(&format!("\n  T{:04}: {}", i + 1, t));
                                    }
                                }
                            }
                        }
                        if let Some(bal) = preproc::balance_report(&raw) {
                            msg.push_str(&format!("\n  BALANCE(raw): {}", bal));
                        }
                        if let Some(bal) = preproc::balance_report(&t0) {
                            msg.push_str(&format!("\n  BALANCE(t0): {}", bal));
                        }
                        let nlines = raw.lines().count();
                        if nlines <= 40 && full_dumps < 8 {
                            full_dumps += 1;
                            msg.push_str(&format!(
                                "\n  FULL-T0 ({} строк transformed):\n",
                                t0.lines().count()
                            ));
                            for (n, line) in t0.lines().enumerate() {
                                msg.push_str(&format!("    {:03}| {}\n", n + 1, line));
                            }
                        }
                        errs.push(msg);
                    }
                }
            }
        }
    }

    println!(
        "ИТОГ: {} успешно | RealError(корни) {} | каскад {} | UiDeferred {} | UiPending {}",
        ok, fail, cascade, ui_deferred, ui_pending
    );
    for (i, e) in errs.iter().enumerate() {
        println!("ROOT[{}] {}", i + 1, e);
    }
    if !cascade_notes.is_empty() {
        println!("\n--- КАСКАД (умерли только из-за корня, своих багов нет) ---");
        for c in cascade_notes.iter() {
            println!("{}", c);
        }
    }
    if !pending_list.is_empty() {
        println!("\n--- UI-PENDING (переписать под Bevy, не баг препроцессора) ---");
        for p in pending_list.iter() {
            println!("  pending: {}", p);
        }
    }
    if let Ok(summary) = lua.globals().get::<mlua::Function>("__moho_call_summary") {
        if let Ok(s) = summary.call::<String>(()) {
            println!("\n=== MOHO NATIVE CALL REPORT ===\n{}\n=== END REPORT ===", s);
        }
    }
    use std::io::Write;
    let _ = std::io::stdout().flush();
    (ok, fail, errs)
}

pub fn load_all_blueprints(rt: &Arc<MohoRuntime>) -> (usize, usize) {
    let lua = match sim_state(rt) {
        Ok(l) => l,
        Err(_) => return (0, 1),
    };
    let keys = rt.vfs.list("");
    let mut ok = 0;
    let mut fail = 0;
    for k in keys {
        if !k.ends_with(".bp") {
            continue;
        }
        let raw = match rt.vfs.read_string(&k) {
            Some(s) => s,
            None => continue,
        };
        let _ = lua.globals().set("__moho_bp_src", k.clone());
        if exec_with_retry(&lua, &raw, &k).is_ok() {
            ok += 1;
        } else {
            fail += 1;
        }
    }
    let _ = lua.globals().set("__moho_bp_src", false);
    (ok, fail)
}

// ==================== ДИАГНОСТИКА БЛУПРИНТОВ (В КОНСОЛЬ) ====================
// Две измерительные функции. Причина появления: `bp ueb0101` вернул nil и
// __moho_dump_blueprints написал 0 — реестр пуст, но load_all_lua печатал
// счётчики .bp только в stdout, значит провал был невидим в окне консоли.
// Не чиним монтирование gamedata/*.scd вслепую: сигнатура mount_extra_dir при
// повторном префиксе не подтверждена -> сначала МЕРЯЕМ (bp_scan), ПОТОМ чиним.

/// Счётчики реестров + пробник ueb0101 — ПРЯМО В КОНСОЛЬ (не stdout).
pub fn bp_report(rt: &Arc<MohoRuntime>) -> String {
    let lua = match sim_state(rt) {
        Ok(l) => l,
        Err(e) => return format!("bpstat: sim_state: {}", e),
    };
    match lua
        .load(
            r#"
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
  'units=%d effects=%d other=%d registry=%d blueprints=%d\n  probe ueb0101: %s\n  выборка units: %s',
  u, e, o, r, b, probe,
  (#sample > 0) and table.concat(sample, ', ') or '(пусто)')
"#,
        )
        .set_name("atlas_bp_stat")
        .eval::<String>()
    {
        Ok(s) => {
            let mut out = format!("BPSTAT: {}\n", s);
            if s.contains("units=0") {
                out.push_str(
                    "  ВНИМАНИЕ: реестр units ПУСТОЙ, несмотря на «423 успешно».\n\
                     \u{2003}Значит .bp не дошли до ядра (не смонтированы gamedata/*.scd\n\
                     \u{2003}или units/effects в VFS). Выполни `bp_scan` — он покажет,\n\
                     \u{2003}где именно лежат .bp и смонтированы ли они вообще.",
                );
                out.push('\n');
            }
            out
        }
        Err(e) => format!("bpstat: {}", e),
    }
}

/// Где .bp лежат в VFS: ищет по всем префиксам и печатает первые пути.
/// Это БЕЗДОПУЩЕНИЙНЫЙ ответ на вопрос «почему реестр пуст»: либо .bp нет в
/// VFS (не смонтированы), либо есть, но не дошли (тогда виноват store_bp/resolve_id).
pub fn bp_scan(rt: &Arc<MohoRuntime>) -> String {
    let all = rt.vfs.list("");
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
             \u{2003}app.rs::init_moho добавлением монтирования gamedata — но ТОЛЬКО\n\
             \u{2003}после проверки, что там реально .scd, а не распакованные папки.",
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
             \u{2003}или в том, что exec_with_retry молча падает на всех 5 режимах.\n\
             \u{2003}Тогда: `load_bp` покажет bp_ok/bp_fail явно.",
        );
        s.push('\n');
    }
    s
}

// ==================== КОНСОЛЬ / САМОТЕСТЫ ====================

pub fn execute_lua(rt: &Arc<MohoRuntime>, code: &str) -> mlua::Result<String> {
    let lua = sim_state(rt)?;
    let result = lua.load(code).eval::<mlua::Value>()?;
    Ok(match result {
        mlua::Value::Nil => "nil".to_string(),
        mlua::Value::Boolean(b) => b.to_string(),
        mlua::Value::Integer(n) => n.to_string(),
        mlua::Value::Number(n) => n.to_string(),
        mlua::Value::String(s) => s.to_str()?.to_string(),
        mlua::Value::Table(t) => {
            let repr: mlua::Function = lua.globals().get("repr")?;
            repr.call::<mlua::String>(mlua::Value::Table(t))?
                .to_string_lossy()
        }
        _ => "<object>".to_string(),
    })
}

/// Токен-трассировка одного файла в режиме 0: печатает каждый push/pop стека
/// блоков с номером строки в stderr ([TRACE]...). Ловит рассинхрон, из-за
/// которого ::__cont_N:: вставляется не перед end цикла (build_templates:41,
/// EOF-DUMP raw=114 -> transformed=120 = три метки).
///
/// Компиляцию делает САМ Lua через load()/loadstring(): он проверяет синтаксис
/// чанка и НЕ исполняет его. В mlua 0.10.5 нет публичного compile() (E0624)
/// и нет proto() (E0599) — единственный устойчивый к версии способ.
pub fn lex_trace(rt: &Arc<MohoRuntime>, path: &str) -> String {
    let raw = match read_raw_source(&rt.vfs, path) {
        Some(s) => s,
        None => return format!("не найден: {}", path),
    };
    preproc::set_lex_context(path);
    preproc::set_lex_trace(path);

    let tr = preproc::moho_to_lua51(&raw);
    // Замкнутое выражение, чтобы set_lex_trace("") гарантированно выполнился
    // на ЛЮБОМ пути выхода (в т.ч. при Err) — иначе трассировка осталась бы
    // включённой и затопила [TRACE] весь последующий прогон load_all_lua.
    let outcome = (|| -> mlua::Result<String> {
        let lua = mlua::Lua::new();
        compat::apply(&lua)?;
        lua.globals().set("__lex_src", tr)?;
        lua.globals().set("__lex_name", path)?;
        lua.load(
            r#"
            local loader = load or loadstring
            local f, e = loader(__lex_src, __lex_name)
            if f then return "PARSER_OK" end
            return "PARSER_FAIL: " .. tostring(e)
            "#,
        )
        .set_name("atlas_lex_trace")
        .eval::<String>()
    })();

    preproc::set_lex_trace("");

    match outcome {
        Ok(s) if s == "PARSER_OK" => format!(
            "трассировка {}: парсер ПРОШЁЛ. Значит raw-синтаксис файла валиден, \
             а ::-ошибка была чисто в placement меток — смотри [TRACE] выше.",
            path
        ),
        Ok(s) => format!(
            "трассировка {}: {}. Смотри [TRACE] выше: там строка, где стек \
             блоков съехал (POP на пустом / POP LoopPending / отсутствующий \
             PUSH Other).",
            path, s
        ),
        Err(e) => format!("трассировка {}: сбой самого трассировщика: {}", path, e),
    }
}

pub fn run_self_test(rt: &Arc<MohoRuntime>) -> mlua::Result<String> {
    let lua = sim_state(rt)?;
    lua.load(
        r#"
local out = {}
out[#out + 1] = "ver=" .. (jit and jit.version or "NO-JIT")
local flag = false
ForkThread(function() flag = true end)
__moho_pump()
out[#out + 1] = "thread=" .. tostring(flag)
UnitBlueprint({ BlueprintId = "TEST01", General = { MaxHealth = 100 } })
local bp = GetUnitBlueprintByName("TEST01")
out[#out + 1] = "bp_hp=" .. tostring(bp and bp.General and bp.General.MaxHealth or "nil")
local u = CreateUnitHPR("ueb0101", 1, 10, 0, 20, 0, 0, 0)
out[#out + 1] = "unit_hp=" .. tostring(u:GetMaxHealth())
Damage(nil, u, 100.0, nil, nil, nil, nil, nil)
out[#out + 1] = "after_dmg=" .. tostring(u:GetHealth())
local cls = Class(nil) { Value = 42 }
out[#out + 1] = "class=" .. tostring(cls.Value)
local inst = cls()
out[#out + 1] = "inst=" .. tostring(inst.Value)
local st = State { Main = function() end, X = 1 }
local st2 = State(st) { Y = 2 }
out[#out + 1] = "state=" .. tostring(st2.X + st2.Y)
local ok_strict = pcall(function() return some_undefined_global_xyz end)
out[#out + 1] = "soft_G=" .. tostring(ok_strict)
local fr = GetFrame(0)
out[#out + 1] = "any=" .. tostring(fr.Top.Width + fr:GetHeight())
local pf = CreatePrefetchSet()
pf:Add('anything')
out[#out + 1] = "prefetch=" .. tostring(type(pf) == 'table')
local nu = 0 for _ in pairs(__bp_units or {}) do nu = nu + 1 end
local ne = 0 for _ in pairs(__bp_effects or {}) do ne = ne + 1 end
out[#out + 1] = "units=" .. nu .. " effects=" .. ne
out[#out + 1] = "probe=" .. (rawget(__bp_units or {}, 'test01') and 'YES' or 'NO')
return table.concat(out, " | ")
"#,
    )
    .eval::<String>()
}

pub fn run_stress(rt: &Arc<MohoRuntime>, iters: usize) -> String {
    let mut rep = String::new();
    for it in 0..iters {
        let t0 = std::time::Instant::now();
        let (ok, fail, _) = load_all_lua(rt, false);
        rep.push_str(&format!(
            "прогон {}: {} ок / {} ошибок за {:?}\n",
            it + 1,
            ok,
            fail,
            t0.elapsed()
        ));
    }
    rep
}

pub fn peek_source(rt: &Arc<MohoRuntime>, path: &str, line: usize) -> String {
    let raw = match read_raw_source(&rt.vfs, path) {
        Some(s) => s,
        None => return format!("не найден: {}", path),
    };
    let tr = preproc::moho_to_lua51(&raw);
    let rl: Vec<&str> = raw.lines().collect();
    let tl: Vec<&str> = tr.lines().collect();
    let mut out = String::new();
    let from = line.saturating_sub(3);
    let to = std::cmp::min(line + 2, rl.len().saturating_sub(1));
    for i in from..=to {
        out.push_str(&format!("R{:04}: {}", i + 1, rl[i]));
        if tl.get(i).map(|t| *t != rl[i]).unwrap_or(false) {
            out.push_str(&format!("\nT{:04}: {}", i + 1, tl[i]));
        }
        out.push('\n');
    }
    out
}

pub fn vfs_list(rt: &Arc<MohoRuntime>, prefix: &str) -> Vec<String> {
    rt.vfs.list(prefix)
}

pub fn vfs_cat(rt: &Arc<MohoRuntime>, path: &str, max_lines: usize) -> String {
    let raw = match read_raw_source(&rt.vfs, path) {
        Some(s) => s,
        None => return format!("не найден: {}", path),
    };
    preproc::moho_to_lua51(&raw)
        .lines()
        .take(max_lines)
        .collect::<Vec<_>>()
        .join("\n")
}
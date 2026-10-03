use mlua::prelude::*;

const LUA51_COMPAT: &str = r#"
arg = arg or {}
if not table.getn then function table.getn(t) return #t end end
if not table.setn then function table.setn() end end
if not string.gfind then string.gfind = string.gmatch end
if not math.mod then math.mod = math.fmod end
if not math.log10 then function math.log10(x) return math.log(x, 10) end end
if not table.foreach then function table.foreach(t, f) for k, v in pairs(t) do f(k, v) end end end
if not table.foreachi then function table.foreachi(t, f) for i, v in ipairs(t) do f(i, v) end end end
"#;

const STUBS: &str = r#"
pcall(require, 'debug')
if not debug then debug = {} end
debug.profiledata = debug.profiledata or function() return {} end
debug.trackallocations = debug.trackallocations or function() end
debug.allocatedsize = debug.allocatedsize or function() return 0 end
debug.allocinfo = debug.allocinfo or function() return {} end
debug.getinfo = debug.getinfo or function(f, what) return { name = 'unknown', what = what or '' } end
pcall(require, 'bit')

-- Реестр нативных вызовов (roadmap переноса на Rust/Bevy).
__moho_calls = {}
function __moho_log(name)
  if not __moho_calls[name] then
    __moho_calls[name] = 1
    print('[MOHO-CALL] ' .. name)
  else
    __moho_calls[name] = __moho_calls[name] + 1
  end
end
function __moho_call_summary()
  local names = {}
  for k in pairs(__moho_calls) do names[#names + 1] = k end
  table.sort(names)
  local lines = {}
  for _, k in ipairs(names) do
    lines[#lines + 1] = string.format('%-40s %d', k, __moho_calls[k])
  end
  return table.concat(lines, '\n')
end
function register_native(name, fn) _G[name] = fn end

-- Сквозной сет применённых override'ов (общий для doscript/import/основного цикла).
__moho_overridden = __moho_overridden or {}

-- Перехват строгого __index на _G (config.lua) -> строгость НЕ включается.
local _real_setmetatable = setmetatable
function setmetatable(t, mt)
  if t == _G and type(mt) == 'table' then
    local soft = {}
    for k, v in pairs(mt) do
      if k ~= '__index' and k ~= '__newindex' then soft[k] = v end
    end
    return _real_setmetatable(t, soft)
  end
  return _real_setmetatable(t, mt)
end

-- Универсальный any-объект (чинит layouthelpers Top/Width/Items и uiutil).
local any_mt
any_mt = {
  __index = function(t, k) return t end,
  __call = function(t, ...) return 0 end,
  __add = function() return 0 end,
  __sub = function() return 0 end,
  __mul = function() return 0 end,
  __div = function() return 0 end,
  __mod = function() return 0 end,
  __unm = function() return 0 end,
  __lt = function() return false end,
  __le = function() return false end,
  __len = function() return 0 end,
  __concat = function(a, b) return tostring(a) .. tostring(b) end,
  __tostring = function() return '<any>' end,
}
function __moho_any() return setmetatable({}, any_mt) end

function iscallable(x)
  local t = type(x)
  if t == 'function' then return true end
  if t == 'table' then
    local mt = getmetatable(x)
    return type(mt) == 'table' and type(rawget(mt, '__call')) == 'function'
  end
  return false
end

function __moho_iter(x, ...)
  if type(x) == 'table' then return pairs(x) end
  return x, ...
end

-- serialize: guard против JIT-цикла (watchdog против JIT бессилен).
-- Счётчик сбрасывается из Rust перед каждым файлом.
__moho_ser_n = 0
function serialize(...)
  __moho_log('serialize')
  __moho_ser_n = __moho_ser_n + 1
  if __moho_ser_n > 500000 then
    error('ATLAS DEADLOCK: serialize > 500k (JIT-цикл)')
  end
  return ''
end
function unserialize(s) return {} end
function deserialize(s) return {} end
function export_funs(name, tbl) return tbl end
function import_funs(name) return {} end

function table.copy(t) local r = {} for k, v in pairs(t) do r[k] = v end return r end
function table.deepcopy(t, seen)
  seen = seen or {}
  if type(t) ~= 'table' then return t end
  if seen[t] then return seen[t] end
  local r = {}
  seen[t] = r
  for k, v in pairs(t) do r[k] = table.deepcopy(v, seen) end
  return r
end
function table.values(t) local r = {} for k, v in pairs(t) do r[#r + 1] = v end return r end
function table.empty(t) for k, v in pairs(t) do return false end return true end
function table.find(t, v) for k, x in pairs(t) do if x == v then return k end end return nil end
function table.getsize(t) local n = 0 for _ in pairs(t) do n = n + 1 end return n end
function table.merge(a, b) for k, v in pairs(b) do a[k] = v end return a end

function State(base)
  local s = {}
  if type(base) == 'table' then for k, v in pairs(base) do s[k] = v end end
  setmetatable(s, {
    __call = function(self, a)
      if type(a) == 'table' then for k, v in pairs(a) do self[k] = v end end
      return self
    end,
  })
  return s
end
function ChangeState(_o, _s) end

__buffs = __buffs or {}
function BuffBlueprint(spec)
  if type(spec) == 'table' and spec.Type then __buffs[spec.Type] = spec end
  return spec
end
function BaseBuilderTemplate(spec)
  if type(spec) == 'table' then
    spec.BuilderName = spec.BuilderName or 'Unknown'
    spec.Priority = spec.Priority or 500
    spec.BuilderType = spec.BuilderType or 'T1'
  end
  return spec
end
function Builder(spec)
  if type(spec) == 'table' then
    spec.BuilderName = spec.BuilderName or 'Unknown'
    spec.Priority = spec.Priority or 500
    spec.BuilderType = spec.BuilderType or 'T1'
  end
  return spec
end
function BuilderGroup(spec)
  if type(spec) == 'table' then
    spec.BuilderName = spec.BuilderName or 'Unknown'
    spec.Priority = spec.Priority or 500
  end
  return spec
end
function PlatoonTemplate(spec)
  if type(spec) == 'table' then
    spec.Name = spec.Name or 'Unknown'
    spec.Priority = spec.Priority or 500
  end
  return spec
end

function Sound(spec) return spec or {} end
function RPCSound() end
function LOC(s) return s end
function LOCF(s, ...) return s end
function dbFilename(la) return '/lua/localization/' .. tostring(la) .. '_strings_db.lua' end
function HasLocalizedVO(_la) return false end
function AudioSetLanguage(_l) end
function sort_down_by(key) return function(a, b) return a[key] > b[key] end end
function sort_up_by(key) return function(a, b) return a[key] < b[key] end end
function strtime(t) return tostring(t) end
function GetSystemTimeSecondsOnlyForProfileUse() return os.clock() end
-- DiskFindFiles остаётся стабом: честная реализация запускала игровой
-- конвейер LoadBlueprints() из 4 init-скриптов, он конкурировал с Фазой 0.
function DiskFindFiles(_d, _p) return {} end
function GetBuildVersion() return 'ATLAS 0.0.3' end
function IsRetailBuild() return true end
function HashString(_s) return 0 end
function CurrentThread() return coroutine.running() end
function Faction(t) return t end
function SpecFootprintGroup(t) return t end
function SpecFootprints(t) return t end
function KillThread(_t) __moho_log('KillThread') end
function GetEntityById(_id) return nil end
function SetArmyStat(...) end
function GetArmyStat(...) return { Value = 0 } end
function GetFocusArmy() return 1 end
function IssueDive(...) end
function IssueGuard(...) end
function IssueWaypoint(...) end
function IssueAttackMove(...) end
function IssueClearCommands(...) end
function IssueFactoryAssist(...) end
function GetLanguage() return 'en_US' end
function SessionGetLanguage() return 'en_US' end
function LOC_GetLanguage() return 'en_US' end
function GetPreference(_k) return 'en_US' end
function auto_run_unit_tests() end
function BlueprintLoaderUpdateProgress(_p) __moho_log('BlueprintLoaderUpdateProgress') end
function SessionIsMultiplayer() return false end
function SessionIsHost() return true end
function SessionIsLocalGame() return true end
function SessionGetScenarioInfo() return ScenarioInfo end
function GetVolume(_s) return 1.0 end
function SetVolume(_s, _v) end
function dir_recursive(_path) return {} end
function export_name(_n, _f) end

function UIFile(path)
  __moho_log('UIFile')
  local ok, m = pcall(import, path)
  if ok and type(m) == 'table' then return m end
  return {}
end
function GetFrame(_n)
  __moho_log('GetFrame')
  return __moho_any()
end

LobbyComm = LobbyComm or {}
LobbyComm.maxPlayerSlots = 16
function GetArmiesTable()
  return {
    armiesTable = { [1] = { faction = 'uef', name = 'UEF', color = { 1, 1, 1 } } },
    armyIndexes = { 1 }, hostileAI = {}, hostileUnits = {}, alliedUnits = {},
  }
end
function GetActiveModContext() return {} end
function GetActiveModDirectory() return '' end
function GetActiveModID() return '' end
function GetActiveModTitle() return 'ATLAS' end
function GetActiveModVersion() return '0.0.3' end
function GetActiveModAuthor() return 'ATLAS' end
function GetActiveModDescription() return '' end
function GetActiveModURL() return '' end
function GetActiveModUID() return '' end
function GetActiveModUIDs() return {} end

Sync = Sync or {}
__moduleinfo = { name = 'global', used_by = {}, track_imports = false }
__active_mods = __active_mods or {}
ScenarioUtils = ScenarioUtils or {
  GetMarker = function(...) return { position = { 0, 0, 0 } } end,
  MarkerToPosition = function(...) return { 0, 0, 0 } end,
  GetMarkerLocation = function(...) return { 0, 0, 0 } end,
}
EffectTemplate = EffectTemplate or {}
ScenarioInfo = ScenarioInfo or { Armies = {}, ArmySetup = {}, BuilderTable = {}, type = 'skirmish' }
ScenarioParams = ScenarioParams or {}
Economy = Economy or {}
intel = intel or {}
map = map or {}
frame = frame or {}
props = props or {}
races = races or {}

function FileCollapsePath(path)
  local parts = {}
  for part in string.gmatch(path, '[^/]+') do
    if part == '..' then
      table.remove(parts)
    elseif part ~= '.' then
      table.insert(parts, part)
    end
  end
  local prefix = string.match(path, '^/') and '/' or ''
  return prefix .. table.concat(parts, '/')
end

function require(p)
  local cur = __moho_cur
  if type(cur) ~= 'string' or cur == '' then cur = '/lua/init.lua' end
  local base = string.match(cur, '^(.*)/[^/]*$') or '/lua'
  local rp = p
  if string.find(rp, '^%.') then rp = FileCollapsePath(base .. '/' .. rp) end
  if not string.find(rp, '^/') then rp = '/lua/' .. rp end
  local ok, m = pcall(import, rp)
  if ok and m then return m end
  local ok2, m2 = pcall(import, '/' .. string.gsub(rp, '^/', ''))
  if ok2 and m2 then return m2 end
  return nil
end

function BOOLEAN(b) return b end
function INTEGER(i) return i end
function FLOAT(f) return f end
function VECTOR2(x, y) return { x, y, type = 'VECTOR2' } end
function VECTOR3(x, y, z) return { x, y, z, type = 'VECTOR3' } end
function RECTANGLE(x0, y0, x1, y1) return { x0, y0, x1, y1, type = 'RECTANGLE' } end
function STRING(s) return s end
function GROUP(g) g.type = 'GROUP' return g end

LAND = 0x01
SEABED = 0x02
SUB = 0x04
WATER = 0x08
AIR = 0x10
ORBIT = 0x20
config = config or {}

categories = categories or {}
EntityCategory = EntityCategory or {}
local cat_bits = {
  ALLUNITS = 0x00000001, AIR = 0x00000002, LAND = 0x00000004, NAVAL = 0x00000008,
  SUB = 0x00000010, SEABED = 0x00000020, ORBIT = 0x00000040,
  TECH1 = 0x00000100, TECH2 = 0x00000200, TECH3 = 0x00000400, TECH4 = 0x00000800,
  MOBILE = 0x00001000, STRUCTURE = 0x00002000, FACTORY = 0x00004000,
  ENGINEER = 0x00008000, CONSTRUCTION = 0x00010000,
  ANTIAIR = 0x00020000, DIRECTFIRE = 0x00040000, INDIRECTFIRE = 0x00080000,
  ARTILLERY = 0x00100000, MISSILE = 0x00200000,
  SHIELD = 0x00400000, DEFENSE = 0x00800000, ECONOMIC = 0x01000000,
  INTELLIGENCE = 0x02000000, STRATEGIC = 0x04000000,
  EXPERIMENTAL = 0x08000000, COMMAND = 0x10000000,
  SCOUT = 0x20000000, BOMBER = 0x40000000, FIGHTER = 0x80000000,
  TRANSPORTATION = 0x00000080, RECLAIMABLE = 0x00000100,
  REPAIR = 0x00000200, CAPTURE = 0x00000400,
  UEF = 0x00001000, AEON = 0x00002000, CYBRAN = 0x00004000, SERAPHIM = 0x00008000,
}
for name, bit in pairs(cat_bits) do categories[name] = bit end
setmetatable(categories, {
  __index = function(t, k)
    local v = 0x00000001
    rawset(t, k, v)
    return v
  end,
})
function ParseEntityCategory(cat)
  if type(cat) == 'string' then
    local parts = {}
    for w in string.gmatch(cat, '%S+') do parts[#parts + 1] = w end
    return parts
  end
  return cat
end
function EntityCategoryContains(cat, unit)
  if type(cat) == 'number' and type(unit) == 'table' and unit.Categories then
    for _, c in ipairs(unit.Categories) do
      if type(c) == 'number' and bit.band(cat, c) ~= 0 then return true end
    end
  end
  return false
end

-- === СОБСТВЕННЫЙ ЭКСТРАКТОР БЛУПРИНТОВ ===
-- kind: 'units' -> __bp_units, 'effects' -> __bp_effects, nil -> общий реестр.
-- Вызов из консоли: __moho_dump_blueprints('Unit_Properties.txt', 'units')
function __moho_dump_blueprints(path, kind)
  path = path or 'Unit_Properties.txt'
  local reg
  if kind == 'units' then
    reg = __bp_units
  elseif kind == 'effects' then
    reg = __bp_effects
  else
    reg = __moho_bp_registry
  end
  reg = reg or {}
  local ids = {}
  for k in pairs(reg) do ids[#ids + 1] = k end
  table.sort(ids)
  local f = io.open(path, 'w')
  if not f then return 'ОШИБКА: не открыть ' .. path end
  local n = 0
  local function emit(prefix, t, depth)
    if depth > 12 then return end
    for k, v in pairs(t) do
      local key = prefix .. '.' .. tostring(k)
      if type(v) == 'table' then
        emit(key, v, depth + 1)
      elseif type(v) ~= 'function' then
        f:write(key .. ' = ' .. tostring(v) .. '\n')
      end
    end
  end
  for _, id in ipairs(ids) do
    local bp = reg[id]
    f:write('=== ' .. tostring(id) .. ' ===\n')
    n = n + 1
    if type(bp) == 'table' then emit(tostring(id), bp, 1) end
  end
  f:close()
  return string.format('Wrote %d blueprints (%s) to %s', n, kind or 'all', path)
end

moho = moho or {}
setmetatable(moho, {
  __index = function(t, k)
    local c = Class(nil)
    rawset(t, k, c)
    return c
  end,
})
"#;

pub fn apply(lua: &Lua) -> LuaResult<()> {
    lua.load(LUA51_COMPAT)
        .set_name("atlas_lua51_compat")
        .exec()?;
    lua.load(STUBS).set_name("atlas_stubs").exec()?;
    let is_jit: bool = lua
        .load("return jit ~= nil and true or false")
        .eval()
        .unwrap_or(false);
    if !is_jit {
        eprintln!(
            "[ATLAS] ВНИМАНИЕ: LuaJIT не активен -> 'continue' упадут на 'near ::'. \
             Cargo.toml: features=[\"luajit\",\"vendored\"]"
        );
    } else {
        eprintln!("[ATLAS] LuaJIT активен — goto/метки работают");
    }
    Ok(())
}
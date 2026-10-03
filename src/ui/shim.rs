use std::sync::{Arc, Mutex};
use crate::moho::vfs::Vfs;
use crate::moho::compat;
use crate::moho::preproc;

pub struct WorldState { pub tick: u32, pub rng_state: u64, pub map_size_x: f32, pub map_size_z: f32 }
impl WorldState {
    pub fn new() -> Self { WorldState { tick: 0, rng_state: 0x5EED0001, map_size_x: 256.0, map_size_z: 256.0 } }
}

pub struct MohoRuntime { pub vfs: Arc<Vfs>, pub world: Arc<Mutex<WorldState>> }
impl MohoRuntime {
    pub fn new(vfs: Arc<Vfs>) -> Self { MohoRuntime { vfs, world: Arc::new(Mutex::new(WorldState::new())) } }
}

const PRELUDE: &str = r#"
if not table.pack then function table.pack(...) return { n = select('#', ...), ... } end end
if not table.unpack then table.unpack = unpack end
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
function VNormal(a) local l = VLength(a); if l == 0 then return Vector(0, 0, 0) end return VMult(a, 1 / l) end
__moho_threads = __moho_threads or {}
function ForkThread(fn, ...)
  local co = coroutine.create(fn)
  table.insert(__moho_threads, { co = co, args = table.pack(...) })
  return co
end
function SuspendCurrentThread() coroutine.yield() end
function WaitFor(_event) coroutine.yield() end
function ResumeThread(_co) end
function __moho_pump()
  local i = 1
  while i <= #__moho_threads do
    local t = __moho_threads[i]
    local st = coroutine.status(t.co)
    if st == "suspended" then
      local args = t.args
      local ok, err = coroutine.resume(t.co, table.unpack(args, 1, args.n))
      t.args = { n = 0 }
      if not ok then print("THREAD ERROR: " .. tostring(err)); table.remove(__moho_threads, i)
      else i = i + 1 end
    elseif st == "dead" then table.remove(__moho_threads, i)
    else i = i + 1 end
  end
end
-- Настоящий Moho import: возвращает таблицу-окружение модуля, с кэшем
__moho_modcache = __moho_modcache or {}
function import(name)
  local key = string.lower(name)
  if __moho_modcache[key] then return __moho_modcache[key] end
  local src = __moho_read(name)
  if not src then error('VFS: импорт не найден: ' .. name) end
  local f, err = loadstring(src, name)
  if not f then error(err) end
  local env = setmetatable({}, { __index = getfenv() })
  setfenv(f, env)
  __moho_modcache[key] = env
  f()
  return env
end
"#;

const CLASS_LUA: &str = r#"
function Class(base, _attr)
  local c = { _isClass = true }
  c.__index = c
  c.base = base
  setmetatable(c, {
    __index = base,
    __call = function(self, a, ...)
      if type(a) == 'table' and select('#', ...) == 0 then
        for k, v in pairs(a) do self[k] = v end
        return self
      end
      local obj = setmetatable({}, self)
      if obj.OnCreate then obj:OnCreate(a, ...) end
      return obj
    end,
  })
  if _attr then for k, v in pairs(_attr) do c[k] = v end end
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
function WaitSeconds(_s) end
function WaitTicks(_t) end
function CreateEmitterAtEntity(...) return nil end
function CreateEmitterAtPosition(...) return nil end
function CreateLightParticle(...) return nil end
function CreateDecal(...) return nil end
function CreateSplat(...) return nil end
function AttachBeamEntityToEntity(...) return nil end
function GetRandomFloat(a, b) return Random(a, b) end
function GetRandomInt(a, b) return math.floor(Random(a, b + 1)) end
function table.merge(a, b) for k, v in pairs(b) do a[k] = v end return a end
"#;

// Читает Lua-файл из VFS с препроцессингом; ищет по префиксам и суффиксу
fn read_lua_source(vfs: &Vfs, name: &str) -> Option<String> {
    let raw = if name.starts_with('/') || name.contains('/') {
        vfs.read_string(name)
    } else {
        None
    };
    let raw = match raw {
        Some(s) => Some(s),
        None => {
            let mut found = None;
            for p in ["lua/sim/", "lua/ai/", "lua/", ""] {
                if let Some(s) = vfs.read_string(&format!("{}{}", p, name)) { found = Some(s); break; }
            }
            if found.is_none() {
                let lower = name.to_lowercase();
                for k in vfs.list("lua") {
                    if k.ends_with(&lower) { found = vfs.read_string(&k); break; }
                }
            }
            found
        }
    };
    raw.map(|s| preproc::moho_to_lua51(&s))
}

pub fn create_sim_state(rt: &Arc<MohoRuntime>) -> mlua::Result<mlua::Lua> {
    let lua = mlua::Lua::new();
    compat::apply(&lua)?;
    lua.load(PRELUDE).exec()?;
    lua.load(CLASS_LUA).exec()?;

    lua.globals().set("print", lua.create_function(|_, m: mlua::MultiValue| { println!("[SIM] {}", fmt_mv(m)); Ok(()) })?)?;
    lua.globals().set("LOG", lua.create_function(|_, m: mlua::MultiValue| { println!("[LOG] {}", fmt_mv(m)); Ok(()) })?)?;
    lua.globals().set("SPEW", lua.create_function(|_, m: mlua::MultiValue| { println!("[SPEW] {}", fmt_mv(m)); Ok(()) })?)?;
    lua.globals().set("WARN", lua.create_function(|_, m: mlua::MultiValue| { println!("[WARN] {}", fmt_mv(m)); Ok(()) })?)?;

    let vfs_d = rt.vfs.clone();
    lua.globals().set("doscript", lua.create_function(move |lua_ctx, name: String| {
        let raw = vfs_d.read_string(&name)
            .ok_or_else(|| mlua::Error::runtime(format!("VFS: файл не найден: {}", name)))?;
        let src = preproc::moho_to_lua51(&raw);
        lua_ctx.load(&src).set_name(&name).exec()
    })?)?;

    let vfs_e = rt.vfs.clone();
    lua.globals().set("exists", lua.create_function(move |_, name: String| Ok(vfs_e.exists(&name)))?)?;

    // Читалка для Lua-функции import()
    let vfs_i = rt.vfs.clone();
    lua.globals().set("__moho_read", lua.create_function(move |_, name: String| {
        Ok(read_lua_source(&vfs_i, &name))
    })?)?;

    let vfs_io = rt.vfs.clone();
    let io_tbl: mlua::Table = match lua.globals().get::<mlua::Table>("io") {
        Ok(t) => t, Err(_) => lua.create_table()?,
    };
    io_tbl.set("dir", lua.create_function(move |l, pattern: String| {
        let prefix = pattern.trim_end_matches('*').trim_end_matches('/').to_string();
        let base = if prefix.is_empty() { String::new() } else { format!("{}/", prefix) };
        let mut seen: Vec<String> = Vec::new();
        for k in vfs_io.list(&prefix) {
            if let Some(rest) = k.strip_prefix(&base) {
                if let Some(seg) = rest.split('/').next() {
                    let s = seg.to_string();
                    if !s.is_empty() && !seen.contains(&s) { seen.push(s); }
                }
            }
        }
        let t = l.create_table()?;
        for (i, s) in seen.iter().enumerate() { t.set(i + 1, s.clone())?; }
        Ok(t)
    })?)?;
    lua.globals().set("io", io_tbl)?;

    lua.globals().set("RegisterUnitBlueprint", lua.create_function(|lua, bp: mlua::Table| {
        let id: String = bp.get("BlueprintId")?;
        let g = lua.globals();
        let reg: mlua::Table = match g.get::<mlua::Table>("__blueprints") {
            Ok(t) => t, Err(_) => { let t = lua.create_table()?; g.set("__blueprints", t.clone())?; t }
        };
        reg.set(id, bp)?;
        Ok(())
    })?)?;
    lua.globals().set("GetUnitBlueprintByName", lua.create_function(|lua, name: String| {
        let g = lua.globals();
        let reg: mlua::Table = match g.get::<mlua::Table>("__blueprints") {
            Ok(t) => t, Err(_) => return Ok(mlua::Value::Nil),
        };
        reg.get::<mlua::Value>(name)
    })?)?;

    let w1 = rt.world.clone();
    lua.globals().set("GetGameTick", lua.create_function(move |_, _: ()| Ok(w1.lock().unwrap().tick))?)?;
    let w2 = rt.world.clone();
    lua.globals().set("Random", lua.create_function(move |_, args: mlua::MultiValue| {
        let nums: Vec<f32> = args.into_iter().filter_map(|v| match v { mlua::Value::Number(n) => Some(n as f32), _ => None }).collect();
        let mut w = w2.lock().unwrap();
        w.rng_state = w.rng_state.wrapping_mul(6364136223846793005).wrapping_add(1);
        let r = (w.rng_state >> 33) as f32 / (u32::MAX as f32);
        match nums.len() { 0 => Ok(r), 1 => Ok(r * nums[0]), _ => Ok(nums[0] + r * (nums[1] - nums[0])) }
    })?)?;
    let w3 = rt.world.clone();
    lua.globals().set("GetMapSize", lua.create_function(move |_, _: ()| {
        let w = w3.lock().unwrap(); Ok((w.map_size_x, w.map_size_z))
    })?)?;
    lua.globals().set("GetTerrainHeight", lua.create_function(|_, (_x, _z): (f32, f32)| Ok(0.0_f32))? )?;

    lua.globals().set("CreateUnitHPR", lua.create_function(|lua, args: mlua::MultiValue| {
        if args.len() < 8 { return Err(mlua::Error::runtime("CreateUnitHPR requires 8 args")); }
        let bp = match &args[0] { mlua::Value::String(s) => s.to_str()?.to_string(), _ => String::new() };
        let army = match &args[1] { mlua::Value::Integer(v) => *v as u32, _ => 0 };
        let x = match &args[2] { mlua::Value::Number(v) => *v as f32, _ => 0.0 };
        let y = match &args[3] { mlua::Value::Number(v) => *v as f32, _ => 0.0 };
        let z = match &args[4] { mlua::Value::Number(v) => *v as f32, _ => 0.0 };
        let u = lua.create_table()?;
        u.set("BlueprintId", bp)?; u.set("Army", army)?;
        u.set("X", x)?; u.set("Y", y)?; u.set("Z", z)?;
        u.set("Health", 1000.0_f32)?; u.set("MaxHealth", 1000.0_f32)?;
        u.set("GetHealth", lua.create_function(|_, t: mlua::Table| t.get::<f32>("Health"))?)?;
        u.set("GetMaxHealth", lua.create_function(|_, t: mlua::Table| t.get::<f32>("MaxHealth"))?)?;
        u.set("GetPosition", lua.create_function(|_, t: mlua::Table| Ok((t.get::<f32>("X")?, t.get::<f32>("Y")?, t.get::<f32>("Z")?)))?)?;
        Ok(u)
    })?)?;
    lua.globals().set("Damage", lua.create_function(|_, args: mlua::MultiValue| {
        if args.len() >= 4 {
            if let mlua::Value::Table(t) = &args[1] {
                let amt = match &args[2] { mlua::Value::Number(v) => *v as f32, _ => 0.0 };
                let hp: f32 = t.get("Health").unwrap_or(0.0);
                t.set("Health", (hp - amt).max(0.0))?;
            }
        }
        Ok(())
    })?)?;
    lua.globals().set("DamageArea", lua.create_function(|_, _a: mlua::MultiValue| Ok(()))? )?;
    lua.globals().set("SetAlliance", lua.create_function(|lua, args: mlua::MultiValue| {
        if args.len() < 3 { return Err(mlua::Error::runtime("SetAlliance requires 3 args")); }
        let a1 = match &args[0] { mlua::Value::Integer(v) => *v as u32, _ => 0 };
        let a2 = match &args[1] { mlua::Value::Integer(v) => *v as u32, _ => 0 };
        let kind = match &args[2] { mlua::Value::String(s) => s.to_str()?.to_string(), _ => String::new() };
        let g = lua.globals();
        let t: mlua::Table = match g.get::<mlua::Table>("__alliances") {
            Ok(t) => t, Err(_) => { let t = lua.create_table()?; g.set("__alliances", t.clone())?; t }
        };
        t.set(format!("{}_{}", a1, a2), kind)?;
        Ok(())
    })?)?;
    for name in ["IssueMove", "IssueAttack", "IssueStop", "IssuePatrol", "IssueReclaim", "IssueRepair"] {
        lua.globals().set(name, lua.create_function(|_, (_u, _t): (mlua::Value, mlua::Value)| Ok(()))? )?;
    }
    Ok(lua)
}

pub fn create_user_state(rt: &Arc<MohoRuntime>) -> mlua::Result<mlua::Lua> {
    let lua = mlua::Lua::new();
    compat::apply(&lua)?;
    lua.load(PRELUDE).exec()?;
    lua.load(CLASS_LUA).exec()?;
    lua.globals().set("print", lua.create_function(|_, m: mlua::MultiValue| { println!("[USER] {}", fmt_mv(m)); Ok(()) })?)?;
    lua.globals().set("GetSelectedUnits", lua.create_function(|l, _: ()| l.create_table())?)?;
    lua.globals().set("PlaySound", lua.create_function(|_, _n: String| Ok(0_u32))?)?;
    lua.globals().set("SetGameSpeed", lua.create_function(|_, _s: f32| Ok(()))?)?;
    lua.globals().set("GetGameSpeed", lua.create_function(|_, _: ()| Ok(10.0_f32))?)?;
    lua.globals().set("WorldIsPlaying", lua.create_function(|_, _: ()| Ok(true))?)?;
    lua.globals().set("WorldIsLoading", lua.create_function(|_, _: ()| Ok(false))?)?;
    let _ = rt;
    Ok(lua)
}

fn fmt_mv(m: mlua::MultiValue) -> String {
    m.into_iter().map(|v| match v {
        mlua::Value::String(s) => s.to_string_lossy(),
        mlua::Value::Number(n) => format!("{}", n),
        mlua::Value::Boolean(b) => b.to_string(),
        mlua::Value::Nil => "nil".into(),
        _ => "<?obj?>".into(),
    }).collect::<Vec<_>>().join(" ")
}

pub fn load_all_lua(rt: &Arc<MohoRuntime>) -> (usize, usize, Vec<String>) {
    let lua = match create_sim_state(rt) { Ok(l) => l, Err(e) => return (0, 1, vec![e.to_string()]) };
    let keys = rt.vfs.list("lua");
    let mut ok = 0; let mut fail = 0; let mut errs = Vec::new();
    for k in keys {
        if !k.ends_with(".lua") { continue; }
        let raw = match rt.vfs.read_string(&k) { Some(s) => s, None => continue };
        let src = preproc::moho_to_lua51(&raw);
        match lua.load(&src).set_name(&k).exec() {
            Ok(_) => ok += 1,
            Err(e) => { fail += 1; if errs.len() < 20 { errs.push(format!("{}: {}", k, e)); } }
        }
    }
    (ok, fail, errs)
}

pub fn execute_lua(rt: &Arc<MohoRuntime>, code: &str) -> mlua::Result<String> {
    let lua = create_sim_state(rt)?;
    let result = lua.load(code).eval::<mlua::Value>()?;
    Ok(match result {
        mlua::Value::Nil => "nil".to_string(),
        mlua::Value::Boolean(b) => b.to_string(),
        mlua::Value::Integer(n) => n.to_string(),
        mlua::Value::Number(n) => n.to_string(),
        mlua::Value::String(s) => s.to_str()?.to_string(),
        _ => "<object>".to_string(),
    })
}

pub fn run_self_test(rt: &Arc<MohoRuntime>) -> mlua::Result<String> {
    let lua = create_sim_state(rt)?;
    let test_code = r#"
local out = {}
out[#out + 1] = "ver=" .. (jit and jit.version or "NO-JIT")
out[#out + 1] = "getn=" .. tostring(table.getn({ 1, 2, 3 }))
local flag = false
ForkThread(function() flag = true end)
__moho_pump()
out[#out + 1] = "thread=" .. tostring(flag)
RegisterUnitBlueprint({ BlueprintId = "TEST01", General = { MaxHealth = 100 } })
local bp = GetUnitBlueprintByName("TEST01")
out[#out + 1] = "bp_hp=" .. tostring(bp and bp.General and bp.General.MaxHealth or "nil")
local u = CreateUnitHPR("ueb0101", 1, 10, 0, 20, 0, 0, 0)
out[#out + 1] = "unit_hp=" .. tostring(u:GetMaxHealth())
local cls = Class(nil) { Value = 42 }
out[#out + 1] = "class=" .. tostring(cls.Value)
return table.concat(out, " | ")
"#;
    lua.load(test_code).eval::<String>()
}
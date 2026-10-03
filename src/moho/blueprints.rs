use crate::moho::shim::{create_sim_state, MohoRuntime};
use std::sync::Arc;

pub fn load_unit_blueprint(rt: &Arc<MohoRuntime>, path: &str) -> mlua::Result<bool> {
    let lua = create_sim_state(rt)?;
    
    // Ищем файл в VFS. 
    // Arc<Vfs> не требует .lock(), так как Vfs имеет внутренние Mutex для zip-архивов.
    if let Some(content) = rt.vfs.read_string(path) {
        lua.load(&content).exec()?;
        return Ok(true);
    }
    
    Err(mlua::Error::runtime(format!("Blueprint not found in VFS: {}", path)))
}
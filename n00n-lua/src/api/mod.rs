// mlua-bound API functions take owned values (String/Arc<str>) and return
// mlua::Result because the #[lua_fn] macro/from-Lua conversion requires it.
// These two pedantic lints fire on that generated boundary, so silence them
// for the whole API surface.
#![allow(clippy::needless_pass_by_value, clippy::unnecessary_wraps)]

pub(crate) mod agent;
pub(crate) mod r#async;
pub(crate) mod autocmd;
pub(crate) mod base64;
pub(crate) mod env;
pub(crate) mod r#fn;
pub(crate) mod fs;
pub(crate) mod image;
pub(crate) mod interpreter;
pub(crate) mod json;
pub(crate) mod keymap;
pub(crate) mod log;
pub(crate) mod net;
pub(crate) mod options;
pub(crate) mod session;
pub(crate) mod slot;
pub(crate) mod split;
pub(crate) mod text;
pub(crate) mod tool;
pub(crate) mod treesitter;
pub(crate) mod ui;
pub(crate) mod util;
pub(crate) mod uv;
pub(crate) mod workflow;
pub(crate) mod yaml;

use std::sync::Arc;

use mlua::{Lua, Result as LuaResult, Table};

use crate::api::options::PluginOpts;
use crate::api::tool::PendingTools;
use crate::api::util::command::UiAction;
use crate::plugin_permissions::PluginPermissions;

pub(crate) fn create_n00n_global(
    lua: &Lua,
    pending: PendingTools,
    plugin: Arc<str>,
    ui_action_tx: Option<flume::Sender<UiAction>>,
    permissions: &PluginPermissions,
    opts: PluginOpts,
) -> LuaResult<Table> {
    let n00n = lua.create_table()?;

    let api = tool::create_api_table(lua, pending, Arc::clone(&plugin), opts)?;
    autocmd::add_autocmd_methods(&api, lua, Arc::clone(&plugin))?;
    slot::add_slot_methods(&api, lua, Arc::clone(&plugin))?;
    n00n.set("api", api)?;
    n00n.set("env", env::create_env_table(lua, permissions)?)?;
    n00n.set("fs", fs::create_fs_table(lua, permissions)?)?;
    n00n.set("log", log::create_log_table(lua, Arc::clone(&plugin))?)?;
    n00n.set("treesitter", treesitter::create_treesitter_table(lua)?)?;
    n00n.set("uv", uv::create_uv_table(lua, permissions)?)?;
    n00n.set("base64", base64::create_base64_table(lua)?)?;
    n00n.set("image", image::create_image_table(lua)?)?;
    n00n.set("json", json::create_json_table(lua)?)?;
    n00n.set("yaml", yaml::create_yaml_table(lua)?)?;
    n00n.set("net", net::create_net_table(lua, permissions)?)?;
    n00n.set("text", text::create_text_table(lua)?)?;
    n00n.set(
        "session",
        session::create_session_table(lua, ui_action_tx.clone())?,
    )?;
    n00n.set(
        "ui",
        ui::create_ui_table(lua, ui_action_tx, Arc::clone(&plugin))?,
    )?;
    n00n.set("fn", r#fn::create_fn_table(lua, permissions)?)?;
    split::split__register(&n00n, lua)?;
    n00n.set("async", r#async::create_async_table(lua)?)?;
    n00n.set(
        "interpreter",
        interpreter::create_interpreter_table(lua, permissions)?,
    )?;
    n00n.set("agent", agent::create_agent_table(lua)?)?;
    n00n.set("workflow", workflow::create_workflow_table(lua)?)?;
    n00n.set(
        "keymap",
        keymap::create_keymap_table(lua, Arc::clone(&plugin))?,
    )?;

    Ok(n00n)
}

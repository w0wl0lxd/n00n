// mlua-bound API functions take owned values (String/Arc<str>) and return
// mlua::Result because the #[lua_fn] macro/from-Lua conversion requires it.
// These two pedantic lints fire on that generated boundary, so silence them
// for the whole API surface.
#![allow(clippy::needless_pass_by_value, clippy::unnecessary_wraps)]

pub(crate) mod agent;
pub(crate) mod arbor;
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
pub(crate) mod search;
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
use n00n_config::SearchConfig;

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
    search_config: Arc<SearchConfig>,
) -> LuaResult<Table> {
    lua.set_app_data(search_config);
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
    n00n.set("search", search::create_search_table(lua)?)?;
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
    n00n.set("arbor", arbor::create_arbor_table(lua)?)?;
    n00n.set(
        "keymap",
        keymap::create_keymap_table(lua, Arc::clone(&plugin))?,
    )?;

    Ok(n00n)
}

#[cfg(test)]
mod tests {
    use n00n_config::{RawConfig, SearchFileConfig};

    use super::*;

    #[test]
    fn api_construction_installs_read_only_search_config() {
        let lua = Lua::new();
        let search_config = Arc::new(
            RawConfig {
                search: SearchFileConfig {
                    enabled: Some(true),
                },
                ..RawConfig::default()
            }
            .into_config(false)
            .unwrap()
            .search,
        );

        create_n00n_global(
            &lua,
            Arc::default(),
            Arc::from("search-config-test"),
            None,
            &PluginPermissions::trusted(),
            Arc::default(),
            Arc::clone(&search_config),
        )
        .unwrap();

        let installed = lua.app_data_ref::<Arc<SearchConfig>>().unwrap();
        assert!(Arc::ptr_eq(&installed, &search_config));
    }
}

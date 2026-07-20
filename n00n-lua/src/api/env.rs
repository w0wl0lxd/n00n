use std::path::PathBuf;

use mlua::Lua;
use n00n_lua_macro::{lua_fn, lua_table};

use crate::plugin_permissions::PluginPermissions;

fn utf8(p: PathBuf) -> Option<String> {
    p.into_os_string().into_string().ok()
}

/// Return the directory where n00n stores runtime state (sessions, auth tokens, etc.).
/// Typically something like `~/.local/state/n00n`.
///
/// @return (string?) State directory path, or nil if it cannot be determined.
/// @example
/// local dir = n00n.env.state_dir()
#[lua_fn(guard = Env)]
fn state_dir(_lua: &Lua) -> mlua::Result<Option<String>> {
    Ok(n00n_storage::paths::state_dir().ok().and_then(utf8))
}

/// Return the directory where n00n looks for user configuration files.
/// Typically something like `~/.config/n00n`.
///
/// @return (string?) Config directory path, or nil if it cannot be determined.
/// @example
/// local dir = n00n.env.config_dir()
#[lua_fn(guard = Env)]
fn config_dir(_lua: &Lua) -> mlua::Result<Option<String>> {
    Ok(n00n_storage::paths::config_dir().ok().and_then(utf8))
}

/// Return the directory where n00n writes its log files (`n00n.log`).
/// Typically something like `~/.local/logs/n00n`.
///
/// @return (string?) Logs directory path, or nil if it cannot be determined.
/// @example
/// local dir = n00n.env.logs_dir()
#[lua_fn(guard = Env)]
fn logs_dir(_lua: &Lua) -> mlua::Result<Option<String>> {
    Ok(n00n_storage::paths::logs_dir().ok().and_then(utf8))
}

lua_table! {
    /// Paths to n00n's own directories (config, state, logs).
    ///
    /// Use these to locate config files or persistent state without hard-coding paths.
    ///
    /// ```lua
    /// local cfg = n00n.env.config_dir()
    /// ```
    "n00n.env" => pub(crate) fn create_env_table(perms: &PluginPermissions), DOCS [
        state_dir(perms), config_dir(perms), logs_dir(perms),
    ]
}

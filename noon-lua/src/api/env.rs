use std::path::PathBuf;

use noon_lua_macro::{lua_fn, lua_table};
use mlua::Lua;

use crate::plugin_permissions::PluginPermissions;

fn utf8(p: PathBuf) -> Option<String> {
    p.into_os_string().into_string().ok()
}

/// Return the directory where noon stores runtime state (sessions, auth tokens, etc.).
/// Typically something like `~/.local/state/noon`.
///
/// @return (string?) State directory path, or nil if it cannot be determined.
/// @example
/// local dir = noon.env.state_dir()
#[lua_fn(guard = Env)]
fn state_dir(_lua: &Lua) -> mlua::Result<Option<String>> {
    Ok(noon_storage::paths::state_dir().ok().and_then(utf8))
}

/// Return the directory where noon looks for user configuration files.
/// Typically something like `~/.config/noon`.
///
/// @return (string?) Config directory path, or nil if it cannot be determined.
/// @example
/// local dir = noon.env.config_dir()
#[lua_fn(guard = Env)]
fn config_dir(_lua: &Lua) -> mlua::Result<Option<String>> {
    Ok(noon_storage::paths::config_dir().ok().and_then(utf8))
}

/// Return the directory where noon writes its log files (`noon.log`).
/// Typically something like `~/.local/logs/noon`.
///
/// @return (string?) Logs directory path, or nil if it cannot be determined.
/// @example
/// local dir = noon.env.logs_dir()
#[lua_fn(guard = Env)]
fn logs_dir(_lua: &Lua) -> mlua::Result<Option<String>> {
    Ok(noon_storage::paths::logs_dir().ok().and_then(utf8))
}

/// Return the legacy config path (`~/.noon`), if it exists on disk.
/// Useful for migration logic. Returns nil when there is no legacy directory.
///
/// @return (string?) Legacy directory path, or nil if not present.
#[lua_fn(guard = Env)]
fn legacy_dir(_lua: &Lua) -> mlua::Result<Option<String>> {
    Ok(noon_storage::paths::legacy_home_dir().and_then(utf8))
}

lua_table! {
    /// Paths to noon's own directories (config, state, logs, legacy).
    ///
    /// Use these to locate config files or persistent state without hard-coding paths.
    ///
    /// ```lua
    /// local cfg = noon.env.config_dir()
    /// ```
    "noon.env" => pub(crate) fn create_env_table(perms: &PluginPermissions), DOCS [
        state_dir(perms), config_dir(perms), logs_dir(perms), legacy_dir(perms),
    ]
}

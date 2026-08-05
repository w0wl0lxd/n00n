use std::ops::Deref;
use std::path::{Path, PathBuf};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::time::{Duration, Instant};

use mlua::{LuaSerdeExt, MultiValue, UserData, UserDataMethods, Value as LuaValue};
use n00n_agent::agent::LoadedInstructions;
use n00n_agent::cancel::CancelToken;
use n00n_agent::tools::{
    Deadline, FileReadTracker, LocalTools, ToolAudience, ToolContext, ToolLive,
};
use n00n_config::{AgentConfig, ToolOutputLines};

use crate::api::tool::ToolCallReply;
use crate::api::ui::buf::BufHandle;
use crate::api::util::convert::json_to_lua;
use crate::api::util::state_convert::{
    json_to_lua as state_json_to_lua, lua_to_json as state_lua_to_json,
};
use crate::runtime::{active_task, lock_cell};
use crate::state::{PluginStateIdentity, PluginStateScope, PluginStateStore};

const CONTEXT_INACTIVE_MSG: &str = "state context is no longer active";
const DEADLINE_ALREADY_SET_MSG: &str = "ctx:set_deadline() already called";
const INVALID_STATE_SCOPE_MSG: &str = "state scope must be 'session' or 'root'";

fn parse_state_scope(scope: &str) -> Result<PluginStateScope, &'static str> {
    PluginStateScope::parse(scope).ok_or(INVALID_STATE_SCOPE_MSG)
}

fn send_live_buf(lua: &mlua::Lua, buf: &mlua::AnyUserData) -> mlua::Result<()> {
    let shared = buf.borrow::<BufHandle>().map(|h| Arc::clone(&h.buf))?;
    let task = active_task(lua);
    let (live, sink) = {
        let mut cell = lock_cell(&task);
        cell.root_buf = Some(Arc::clone(&shared));
        (cell.live.clone(), cell.live_sink.clone())
    };
    if let Some(live) = live {
        let _ = live.event_tx.send(n00n_agent::AgentEvent::LiveToolBuf {
            id: live.tool_use_id.clone(),
            body: Arc::clone(&shared),
        });
    }
    if let Some(sink) = sink {
        let _ = sink.try_send(ToolLive::Buf(shared));
    }
    Ok(())
}

/// Captured snapshot of the parent `ToolContext`. Per-call state (deadline,
/// instructions, output lines) is reset so child calls start clean.
#[derive(Clone)]
pub(crate) struct AgentContext(ToolContext);

impl From<&ToolContext> for AgentContext {
    fn from(ctx: &ToolContext) -> Self {
        let mut c = ctx.clone();
        c.loaded_instructions = LoadedInstructions::new();
        c.deadline = Deadline::None;
        c.tool_output_lines = ToolOutputLines::default();
        c.local_tools = LocalTools::default();
        Self(c)
    }
}

impl Deref for AgentContext {
    type Target = ToolContext;
    fn deref(&self) -> &ToolContext {
        &self.0
    }
}

impl AgentContext {
    /// Drops `tool_use_id` so an inner tool never emits UI events under the
    /// outer call's id, and `live_sink` so a grandchild never streams into
    /// a sink meant for its parent.
    pub(crate) fn to_tool_context(&self) -> ToolContext {
        let mut c = self.0.clone();
        c.tool_use_id = None;
        c.live_sink = None;
        c.fusion_origin = n00n_agent::fusion::FusionInvocationOrigin::Interpreter;
        c
    }
}

/// One ctx type for handler, `start`, and restore invocations. Each kind's
/// capabilities live in its `Caps` variant, so a capability exists exactly
/// when its data does. Methods a kind lacks return `(nil, err)` instead of
/// not existing, so callers can probe without pcall.
pub(crate) struct LuaCtx {
    caps: Caps,
    pub(crate) cancel: CancelToken,
    tool_output_lines: ToolOutputLines,
    pub(crate) finish_tx: Option<flume::Sender<ToolCallReply>>,
    active: Arc<AtomicBool>,
    plugin_state: Option<PluginStateAccess>,
}

struct PluginStateAccess {
    plugin: Arc<str>,
    identity: PluginStateIdentity,
    store: Arc<PluginStateStore>,
}

enum Caps {
    Handler {
        agent: Box<AgentContext>,
        /// Kept apart from `agent`, which resets its copy so child calls
        /// start with a clean instruction set.
        loaded_instructions: LoadedInstructions,
    },
    /// `start` runs before permission checks: it reads config and publishes
    /// previews, but dispatching tools is structurally impossible.
    Start {
        config: Arc<AgentConfig>,
        workflow: bool,
        audience: ToolAudience,
    },
    Restore {
        state: Option<serde_json::Value>,
    },
}

impl LuaCtx {
    fn new(ctx: &ToolContext, caps: Caps) -> Self {
        Self {
            caps,
            cancel: ctx.cancel.clone(),
            tool_output_lines: ctx.tool_output_lines,
            finish_tx: None,
            active: Arc::new(AtomicBool::new(true)),
            plugin_state: None,
        }
    }

    pub(crate) fn handler(ctx: &ToolContext) -> Self {
        Self::new(
            ctx,
            Caps::Handler {
                agent: Box::new(AgentContext::from(ctx)),
                loaded_instructions: ctx.loaded_instructions.clone(),
            },
        )
    }

    pub(crate) fn start(ctx: &ToolContext) -> Self {
        Self::new(
            ctx,
            Caps::Start {
                config: Arc::clone(&ctx.config),
                workflow: ctx.workflow,
                audience: ctx.audience,
            },
        )
    }

    pub(crate) fn restore(
        tool_output_lines: ToolOutputLines,
        state: Option<serde_json::Value>,
    ) -> Self {
        Self {
            caps: Caps::Restore { state },
            cancel: CancelToken::none(),
            tool_output_lines,
            finish_tx: None,
            active: Arc::new(AtomicBool::new(true)),
            plugin_state: None,
        }
    }

    /// Dispatch capability: only handler ctxs can call `n00n.agent.*`.
    pub(crate) fn agent(&self) -> Option<&AgentContext> {
        match &self.caps {
            Caps::Handler { agent, .. } => Some(agent),
            _ => None,
        }
    }

    fn config(&self) -> Option<&AgentConfig> {
        match &self.caps {
            Caps::Handler { agent, .. } => Some(&agent.config),
            Caps::Start { config, .. } => Some(config.as_ref()),
            Caps::Restore { .. } => None,
        }
    }

    fn workflow(&self) -> Option<bool> {
        match &self.caps {
            Caps::Handler { agent, .. } => Some(agent.workflow),
            Caps::Start { workflow, .. } => Some(*workflow),
            Caps::Restore { .. } => None,
        }
    }

    fn audience(&self) -> Option<ToolAudience> {
        match &self.caps {
            Caps::Handler { agent, .. } => Some(agent.audience),
            Caps::Start { audience, .. } => Some(*audience),
            Caps::Restore { .. } => None,
        }
    }

    fn file_tracker(&self) -> Option<&FileReadTracker> {
        self.agent().map(|a| &*a.file_tracker)
    }

    fn loaded_instructions(&self) -> Option<&LoadedInstructions> {
        match &self.caps {
            Caps::Handler {
                loaded_instructions,
                ..
            } => Some(loaded_instructions),
            _ => None,
        }
    }

    fn state(&self) -> Option<&serde_json::Value> {
        match &self.caps {
            Caps::Restore { state } => state.as_ref(),
            _ => None,
        }
    }

    fn kind(&self) -> &'static str {
        match self.caps {
            Caps::Handler { .. } => "handler",
            Caps::Start { .. } => "start",
            Caps::Restore { .. } => "restore",
        }
    }

    pub(crate) fn attach_plugin_state(&mut self, plugin: Arc<str>, store: Arc<PluginStateStore>) {
        self.plugin_state = self
            .agent()
            .and_then(|agent| agent.identity.as_ref())
            .map(PluginStateIdentity::from)
            .map(|identity| PluginStateAccess {
                plugin,
                identity,
                store,
            });
    }

    pub(crate) fn context_liveness(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.active)
    }

    fn ensure_active(&self) -> Result<(), String> {
        if self.active.load(Ordering::Acquire) {
            Ok(())
        } else {
            Err(CONTEXT_INACTIVE_MSG.to_owned())
        }
    }

    fn inactive_pair(&self) -> Option<(LuaValue, Option<String>)> {
        self.ensure_active()
            .err()
            .map(|error| (LuaValue::Nil, Some(error)))
    }

    pub(crate) fn plugin_state_store(&self) -> Result<Arc<PluginStateStore>, String> {
        self.plugin_state("n00n.agent.session")
            .map(|access| Arc::clone(&access.store))
    }

    pub(crate) fn dispatch_agent(&self, method: &str) -> Result<&AgentContext, String> {
        let agent = self
            .agent()
            .ok_or_else(|| self.cap_err(&format!("n00n.agent.{method}")))?;
        self.ensure_active()?;
        Ok(agent)
    }

    fn plugin_state(&self, method: &str) -> Result<&PluginStateAccess, String> {
        if !matches!(self.caps, Caps::Handler { .. }) {
            return Err(self.cap_err(method));
        }

        let access = self
            .plugin_state
            .as_ref()
            .ok_or_else(|| "session identity is unavailable".to_owned())?;
        self.ensure_active()?;
        Ok(access)
    }

    pub(crate) fn cap_err(&self, method: &str) -> String {
        format!("{method} not available in {} ctx", self.kind())
    }

    fn cap_err_pair(&self, method: &str) -> (LuaValue, Option<String>) {
        (LuaValue::Nil, Some(self.cap_err(method)))
    }
}

#[allow(clippy::too_many_lines)]
impl UserData for LuaCtx {
    fn add_methods<M: UserDataMethods<Self>>(methods: &mut M) {
        methods.add_method("cancelled", |_, this, ()| {
            this.ensure_active().map_err(mlua::Error::runtime)?;
            Ok(this.cancel.is_cancelled())
        });

        methods.add_method("workflow", |_, this, ()| {
            if let Some(error) = this.inactive_pair() {
                return Ok(error);
            }
            let Some(workflow) = this.workflow() else {
                return Ok(this.cap_err_pair("workflow"));
            };
            Ok((LuaValue::Boolean(workflow), None))
        });

        methods.add_method("audience", |lua, this, ()| {
            const DEFAULT_AUDIENCE: &str = "main";
            if let Some(error) = this.inactive_pair() {
                return Ok(error);
            }
            let Some(audience) = this.audience() else {
                return Ok(this.cap_err_pair("audience"));
            };
            let name = lua.create_string(audience.name().unwrap_or_else(|| DEFAULT_AUDIENCE))?;
            Ok((LuaValue::String(name), None))
        });

        methods.add_method("live_buf", |lua, this, buf: mlua::AnyUserData| {
            if let Some(error) = this.inactive_pair() {
                return Ok(error);
            }
            if matches!(this.caps, Caps::Restore { .. }) {
                return Ok(this.cap_err_pair("live_buf"));
            }
            send_live_buf(lua, &buf)?;
            Ok((LuaValue::Nil, None))
        });

        methods.add_method("config", |lua, this, args: MultiValue| {
            if let Some(error) = this.inactive_pair() {
                return Ok(error);
            }
            let Some(config) = this.config() else {
                return Ok(this.cap_err_pair("config"));
            };
            let config_val = lua.to_value(config)?;
            if args.is_empty() {
                return Ok((config_val, None));
            }
            let key: String = lua.from_value(args[0].clone())?;
            let default = args.get(1).cloned().unwrap_or_else(|| LuaValue::Nil);
            let val = match config_val {
                LuaValue::Table(ref tbl) => {
                    let val = tbl.raw_get::<LuaValue>(key.as_str())?;
                    if matches!(val, LuaValue::Nil) {
                        default
                    } else {
                        val
                    }
                }
                _ => default,
            };
            Ok((val, None))
        });

        methods.add_method("tool_output_lines", |lua, this, ()| {
            this.ensure_active().map_err(mlua::Error::runtime)?;
            lua.to_value(&this.tool_output_lines)
        });

        methods.add_method("state", |lua, this, ()| {
            this.ensure_active().map_err(mlua::Error::runtime)?;
            match this.state() {
                Some(v) => json_to_lua(lua, v),
                None => Ok(LuaValue::Nil),
            }
        });
        methods.add_method("state_get", |lua, this, scope: String| {
            let scope = match parse_state_scope(&scope) {
                Ok(scope) => scope,
                Err(error) => return Ok((LuaValue::Nil, Some(error.to_owned()))),
            };
            let access = match this.plugin_state("state_get") {
                Ok(access) => access,
                Err(error) => return Ok((LuaValue::Nil, Some(error))),
            };
            let Some(value) = access.store.get(&access.plugin, scope, &access.identity) else {
                return Ok((LuaValue::Nil, None));
            };
            match state_json_to_lua(lua, &value) {
                Ok(value) => Ok((value, None)),
                Err(error) => Ok((LuaValue::Nil, Some(error.to_string()))),
            }
        });

        methods.add_method("state_owner", |lua, this, scope: String| {
            let scope = match parse_state_scope(&scope) {
                Ok(scope) => scope,
                Err(error) => return Ok((LuaValue::Nil, Some(error.to_owned()))),
            };
            let access = match this.plugin_state("state_owner") {
                Ok(access) => access,
                Err(error) => return Ok((LuaValue::Nil, Some(error))),
            };
            let owner = access.identity.owner(scope).to_string();
            Ok((LuaValue::String(lua.create_string(owner)?), None))
        });

        methods.add_method(
            "state_replace",
            |lua, this, (scope, value): (String, LuaValue)| {
                let scope = match parse_state_scope(&scope) {
                    Ok(scope) => scope,
                    Err(error) => return Ok((LuaValue::Nil, Some(error.to_owned()))),
                };
                let access = match this.plugin_state("state_replace") {
                    Ok(access) => access,
                    Err(error) => return Ok((LuaValue::Nil, Some(error))),
                };
                let value = match state_lua_to_json(lua, &value) {
                    Ok(value) => value,
                    Err(error) => return Ok((LuaValue::Nil, Some(error.to_string()))),
                };
                let previous =
                    match access
                        .store
                        .replace(&access.plugin, scope, &access.identity, value)
                    {
                        Ok(previous) => previous,
                        Err(error) => return Ok((LuaValue::Nil, Some(error.to_string()))),
                    };
                let previous = match previous {
                    Some(previous) => match state_json_to_lua(lua, &previous) {
                        Ok(previous) => previous,
                        Err(error) => return Ok((LuaValue::Nil, Some(error.to_string()))),
                    },
                    None => LuaValue::Nil,
                };
                Ok((previous, None))
            },
        );

        methods.add_method("state_remove", |lua, this, scope: String| {
            let scope = match parse_state_scope(&scope) {
                Ok(scope) => scope,
                Err(error) => return Ok((LuaValue::Nil, Some(error.to_owned()))),
            };
            let access = match this.plugin_state("state_remove") {
                Ok(access) => access,
                Err(error) => return Ok((LuaValue::Nil, Some(error))),
            };
            let previous = match access.store.remove(&access.plugin, scope, &access.identity) {
                Ok(previous) => previous,
                Err(error) => return Ok((LuaValue::Nil, Some(error.to_string()))),
            };
            let previous = match previous {
                Some(previous) => match state_json_to_lua(lua, &previous) {
                    Ok(previous) => previous,
                    Err(error) => return Ok((LuaValue::Nil, Some(error.to_string()))),
                },
                None => LuaValue::Nil,
            };
            Ok((previous, None))
        });

        methods.add_method("set_deadline", |lua, this, secs: u64| {
            if let Some(error) = this.inactive_pair() {
                return Ok(error);
            }
            if !matches!(this.caps, Caps::Handler { .. }) {
                return Ok(this.cap_err_pair("set_deadline"));
            }
            let handle = active_task(lua);
            let cell = handle
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if cell.deadline_secs.get().is_some() {
                return Err(mlua::Error::runtime(DEADLINE_ALREADY_SET_MSG));
            }
            let deadline = Instant::now()
                .checked_add(Duration::from_secs(secs))
                .ok_or_else(|| mlua::Error::runtime("deadline is out of range"))?;
            cell.deadline_secs.set(Some(secs));
            cell.deadline.set(Some(deadline));
            Ok((LuaValue::Nil, None))
        });

        methods.add_method("record_read", |_, this, path: String| {
            if let Some(error) = this.inactive_pair() {
                return Ok(error);
            }
            let Some(tracker) = this.file_tracker() else {
                return Ok(this.cap_err_pair("record_read"));
            };
            tracker.record_read(Path::new(&path));
            Ok((LuaValue::Nil, None))
        });

        methods.add_method("check_before_edit", |_, this, path: String| {
            if let Some(error) = this.inactive_pair() {
                return Ok(error);
            }
            let Some(tracker) = this.file_tracker() else {
                return Ok(this.cap_err_pair("check_before_edit"));
            };
            match tracker.check_before_edit(Path::new(&path)) {
                Ok(()) => Ok((LuaValue::Boolean(true), None)),
                Err(msg) => Ok((LuaValue::Boolean(false), Some(msg))),
            }
        });

        methods.add_async_method(
            "find_instructions",
            |lua, this, dir_path: String| async move {
                if let Some(error) = this.inactive_pair() {
                    return Ok(error);
                }
                let Some(loaded) = this.loaded_instructions().cloned() else {
                    return Ok(this.cap_err_pair("find_instructions"));
                };
                let results = smol::unblock(move || -> mlua::Result<Vec<(String, String)>> {
                    let cwd = std::env::current_dir().map_err(|e| {
                        mlua::Error::runtime(format!("failed to get current directory: {e}"))
                    })?;
                    let abs = resolve_abs_with_cwd(dir_path, &cwd);
                    Ok(n00n_agent::find_subdirectory_instructions(
                        &abs, &cwd, &loaded,
                    ))
                })
                .await?;
                let tbl = lua.create_table()?;
                for (i, (path, content)) in results.into_iter().enumerate() {
                    let entry = lua.create_table()?;
                    entry.set("path", path)?;
                    entry.set("content", content)?;
                    tbl.set(i + 1, entry)?;
                }
                Ok((LuaValue::Table(tbl), None))
            },
        );

        methods.add_method("is_instruction_file", |_, _, name: String| {
            Ok(n00n_agent::is_instruction_file(&name))
        });

        methods.add_method_mut("finish", |lua, this, val: LuaValue| {
            if let Some(error) = this.inactive_pair() {
                return Ok(error);
            }
            if !matches!(this.caps, Caps::Handler { .. }) {
                return Ok(this.cap_err_pair("finish"));
            }
            let tx = this
                .finish_tx
                .take()
                .ok_or_else(|| mlua::Error::runtime("ctx:finish() already called"))?;

            if let Some(buf) = crate::api::ui::buf::buf_from_reply(&val) {
                lock_cell(&active_task(lua)).root_buf = Some(buf);
            }
            let _ = tx.send(ToolCallReply::from_lua_value(lua, &val));
            Ok((LuaValue::Nil, None))
        });
    }
}

fn resolve_abs_with_cwd(path: String, cwd: &Path) -> PathBuf {
    if Path::new(&path).is_absolute() {
        path.into()
    } else {
        cwd.join(&path)
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use n00n_agent::AgentMode;
    use n00n_agent::tools::LocalToolFn;
    use n00n_agent::tools::test_support::stub_ctx_with;

    use super::*;

    const TOOL_USE_ID: &str = "tu-1";
    const INSTRUCTION_PATH: &str = "/tmp/nested/AGENTS.md";
    const LOCAL_TOOL_NAME: &str = "sess_tool";

    fn populated_ctx() -> ToolContext {
        let mut ctx = stub_ctx_with(&AgentMode::Build, None, Some(TOOL_USE_ID));
        ctx.deadline = Deadline::after(Duration::from_mins(1));
        ctx.tool_output_lines = ToolOutputLines {
            bash: 999,
            ..ToolOutputLines::default()
        };
        assert!(
            !ctx.loaded_instructions
                .contains_or_insert(PathBuf::from(INSTRUCTION_PATH))
        );
        let mut tools: HashMap<String, LocalToolFn> = HashMap::new();
        tools.insert(
            LOCAL_TOOL_NAME.into(),
            Arc::new(|_: &serde_json::Value| Ok(String::new())) as LocalToolFn,
        );
        ctx.local_tools = Arc::new(tools);
        ctx.live_sink = Some(flume::unbounded().0);
        ctx
    }

    #[test]
    fn agent_context_keeps_tool_use_id_and_resets_per_call_state() {
        let agent = AgentContext::from(&populated_ctx());
        assert_eq!(agent.tool_use_id.as_deref(), Some(TOOL_USE_ID));
        assert!(matches!(agent.deadline, Deadline::None));
        assert_eq!(agent.tool_output_lines, ToolOutputLines::default());
        assert!(agent.local_tools.is_empty());
        assert!(
            !agent
                .loaded_instructions
                .contains_or_insert(PathBuf::from(INSTRUCTION_PATH)),
            "loaded_instructions must be a fresh set, not a shared clone"
        );
    }

    #[test]
    fn agent_context_to_tool_context_drops_tool_use_id_and_sink() {
        let agent = AgentContext::from(&populated_ctx());
        assert!(
            agent.live_sink.is_some(),
            "the sink set by the caller must survive into AgentContext"
        );
        let inner = agent.to_tool_context();
        assert_eq!(inner.tool_use_id, None);
        assert!(inner.live_sink.is_none(), "sink must not be inherited");
        assert_eq!(
            inner.fusion_origin,
            n00n_agent::fusion::FusionInvocationOrigin::Interpreter
        );
        assert_eq!(agent.tool_use_id.as_deref(), Some(TOOL_USE_ID));
    }

    #[test]
    fn handler_ctx_keeps_parent_instruction_set() {
        let ctx = LuaCtx::handler(&populated_ctx());
        assert!(
            ctx.loaded_instructions()
                .expect("handler has instructions")
                .contains_or_insert(PathBuf::from(INSTRUCTION_PATH)),
            "handler must share the parent's set; AgentContext resets its own copy"
        );
    }
}

//! `n00n.agent` exposes subagent primitives to Lua plugins. Policy (retries,
//! validation, concurrency) lives in the task plugin, not here.

use std::collections::{HashMap, VecDeque, hash_map::Entry};
use std::pin::pin;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use async_lock::Mutex as AsyncMutex;
use futures::future::{Either, select};
use mlua::{Function, IntoLuaMulti, Lua, Result as LuaResult, Table, Value as LuaValue};
use n00n_agent::agent::tool_dispatch::{self, Emit};
use n00n_agent::cancel::CancelMap;
use n00n_agent::tools::interpreter_bridge;
use n00n_agent::tools::registry::ToolRegistry;
use n00n_agent::tools::{
    Deadline, DescriptionContext, FileReadTracker, LocalToolFn, LocalTools, ToolAudience,
    ToolContext, ToolFilter, ToolLive,
};
use n00n_agent::{
    Agent, AgentEvent, AgentInput, AgentMode, AgentParams, AgentRunParams, Envelope, EventSender,
    History, SubagentInfo, SubagentPrompt, ToolDoneEvent, ToolStartEvent,
};
use n00n_lua_macro::{lua_class, lua_fn, lua_table};
use n00n_providers::model::ModelTier;
use n00n_providers::provider;
use n00n_providers::{ContentBlock, Model, ModelError, Role, ThinkingConfig, model::TokenUsage};
use n00n_storage::id::N00nId;
use n00n_storage::sessions::StoredThinking;
use serde_json::Value as JsonValue;
use tracing::info;

use crate::api::ui::buf::BufHandle;
use crate::api::util::convert::{json_to_lua, lua_to_json, lua_tool_result};
use crate::api::util::ctx::{AgentContext, LuaCtx};

const SESSION_CLOSED_ERR: &str = "session closed";
const DEFAULT_SESSION_AUDIENCE: ToolAudience = ToolAudience::GENERAL_SUB;
const PROGRESS_MAX_RECENT: usize = 5;
const ACTIVITY_MESSAGE_MAX_CHARS: usize = 80;
const REDACTED: &str = "[REDACTED]";
const SAFE_ACTIVITY_DESCRIPTION_TOOLS: &[&str] = &[
    "batch",
    "code_execution",
    "edit",
    "glob",
    "grep",
    "index",
    "memory",
    "multiedit",
    "question",
    "read",
    "skill",
    "todo_write",
    "view_image",
    "write",
];
const PROGRESS_TIMEOUT_MS: u64 = 500;
const STEERING_QUEUE_CAPACITY: usize = 32;

fn resolve_model_from_ctx(ctx: &AgentContext, tier: Option<&str>) -> Result<Model, String> {
    let Some(tier_str) = tier else {
        return Ok(Model::clone(&ctx.model));
    };
    let requested: ModelTier = tier_str.parse().map_err(|e: ModelError| e.to_string())?;
    let effective = requested.min(ctx.model.tier);
    if effective == ctx.model.tier {
        return Ok(Model::clone(&ctx.model));
    }
    let map = n00n_providers::model_registry::model_registry()
        .read()
        .map_err(|e| format!("model registry lock poisoned: {e}"))?;
    ctx.model
        .dynamic_slug
        .is_none()
        .then(|| map.spec_for_tier(ctx.model.provider, effective))
        .flatten()
        .or_else(|| map.spec_for_tier_any(effective))
        .and_then(|s| Model::from_spec(&s).ok())
        .map_or_else(
            || {
                Model::from_tier_dynamic(
                    ctx.model.provider,
                    effective,
                    ctx.model.dynamic_slug.as_deref(),
                )
                .map_err(|e| e.to_string())
            },
            Ok,
        )
}

fn model_to_lua_table(lua: &Lua, model: &Model) -> LuaResult<Table> {
    let tbl = lua.create_table()?;
    tbl.set("id", model.id.clone())?;
    tbl.set("tier", model.tier.to_string())?;
    tbl.set("provider", model.provider.to_string())?;
    tbl.set("spec", model.spec())?;
    Ok(tbl)
}

fn dispatch_ctx<'a>(ctx: &'a LuaCtx, method: &str) -> Result<&'a AgentContext, String> {
    ctx.agent()
        .ok_or_else(|| ctx.cap_err(&format!("n00n.agent.{method}")))
}

type Pair<T> = (Option<T>, Option<String>);

#[allow(clippy::needless_pass_by_value)]
fn err_pair<T>(err: impl ToString) -> Pair<T> {
    (None, Some(err.to_string()))
}

/// `n00n.agent.*` convention: wrong argument types throw; every value or
/// runtime failure (including a ctx without dispatch capability) returns
/// `(nil, err)`.
macro_rules! try_pair {
    ($e:expr) => {
        match $e {
            Ok(v) => v,
            Err(e) => return Ok(err_pair(e)),
        }
    };
}

/// Look up the model that the current agent is using, or pick a cheaper one.
/// You might want a cheaper model for simple subtasks (summaries, classification)
/// without hard-coding a model name.
///
/// The returned table has fields: `id` (string), `tier` (string),
/// `provider` (string), `spec` (string).
///
/// @param ctx LuaCtx Agent context.
/// @param opts table? Optional fields:
///   `tier` (string?) - target tier, e.g. `"fast"`, `"mid"`, `"best"`. Clamped to
///     the parent tier so you cannot escalate.
///   `spec` (string?) - exact model spec string, e.g. `"claude-3-5-haiku-20241022"`.
///     Takes precedence over `tier`.
/// @return (table?, string?) Model table on success, or `(nil, err)` on failure.
/// @example
/// local model, err = n00n.agent.resolve_model(ctx, { tier = "fast" })
/// if err then error(err) end
/// print(model.spec, model.tier)
#[lua_fn]
#[allow(clippy::needless_pass_by_value)]
fn resolve_model(
    lua: &Lua,
    ctx: mlua::UserDataRef<LuaCtx>,
    opts: Option<Table>,
) -> LuaResult<Pair<Table>> {
    let agent = try_pair!(dispatch_ctx(&ctx, "resolve_model"));
    let tier_str = opts
        .as_ref()
        .and_then(|t| t.get::<Option<String>>("tier").ok().flatten());
    let spec_str = opts
        .as_ref()
        .and_then(|t| t.get::<Option<String>>("spec").ok().flatten());

    let model = match spec_str {
        Some(ref spec) => try_pair!(Model::from_spec(spec)),
        None => try_pair!(resolve_model_from_ctx(agent, tier_str.as_deref())),
    };
    Ok((Some(model_to_lua_table(lua, &model)?), None))
}

fn fresh_input_from_legacy_total(
    total: u32,
    fresh: Option<u32>,
    cache_read: u32,
    cache_write: u32,
) -> Result<u32, String> {
    let cached = cache_read.checked_add(cache_write).ok_or_else(|| {
        "cache_read_tokens + cache_write_tokens exceeds the token counter range".to_owned()
    })?;
    let conserved_fresh = total.checked_sub(cached).ok_or_else(|| {
        format!(
            "input token categories do not conserve total: cache_read_tokens ({cache_read}) + \
             cache_write_tokens ({cache_write}) exceeds input_tokens ({total})"
        )
    })?;

    if let Some(fresh) = fresh
        && fresh != conserved_fresh
    {
        return Err(format!(
            "input token categories do not conserve total: fresh_input_tokens ({fresh}) + \
             cache_read_tokens ({cache_read}) + cache_write_tokens ({cache_write}) != \
             input_tokens ({total})"
        ));
    }

    Ok(conserved_fresh)
}

/// Estimate the dollar cost of a completion from its model spec and token
/// counts. Uses the provider's published pricing for fresh input, cache reads,
/// cache writes, and output. Without {breakdown}, input and fast-tier pricing
/// retain the legacy three-argument behavior.
///
/// @param spec string Model spec, e.g. `"anthropic/claude-haiku-4-5"`.
/// @param input_tokens integer Total prompt tokens across all input categories.
/// @param output_tokens integer Completion tokens.
/// @param breakdown table? Optional input categories. Fields:
///   `fresh_input_tokens` (integer?) - non-cached input; when present, it must
///     conserve `input_tokens` together with the cache categories.
///   `cache_read_tokens` (integer?) - input tokens read from cache; default 0.
///   `cache_write_tokens` (integer?) - input tokens written to cache; default 0.
///   `fast` (boolean?) - whether this completion used fast-tier pricing; default false.
/// @return (number?, string?) Estimated USD cost, or `(nil, err)` on failure.
/// @example
/// local cost, err = n00n.agent.usage_cost("anthropic/claude-haiku-4-5", 1200, 300, {
///   fresh_input_tokens = 900,
///   cache_read_tokens = 200,
///   cache_write_tokens = 100,
/// })
/// if err then error(err) end
/// print(string.format("$%.4f", cost))
#[lua_fn]
#[allow(clippy::needless_pass_by_value)]
fn usage_cost(
    _lua: &Lua,
    spec: String,
    input_tokens: u32,
    output_tokens: u32,
    breakdown: Option<Table>,
) -> LuaResult<Pair<f64>> {
    let model = try_pair!(Model::from_spec(&spec));
    let (fresh, cache_read, cache_write, fast) = match breakdown {
        Some(breakdown) => {
            let fresh =
                try_pair!(breakdown.get::<Option<u32>>("fresh_input_tokens").map_err(
                    |error| format!("invalid breakdown field 'fresh_input_tokens': {error}")
                ));
            let cache_read =
                try_pair!(breakdown.get::<Option<u32>>("cache_read_tokens").map_err(
                    |error| format!("invalid breakdown field 'cache_read_tokens': {error}")
                ))
                .map_or(0, |tokens| tokens);
            let cache_write =
                try_pair!(breakdown.get::<Option<u32>>("cache_write_tokens").map_err(
                    |error| format!("invalid breakdown field 'cache_write_tokens': {error}")
                ))
                .map_or(0, |tokens| tokens);
            let fast = try_pair!(
                breakdown
                    .get::<Option<bool>>("fast")
                    .map_err(|error| format!("invalid breakdown field 'fast': {error}"))
            )
            .is_some_and(|fast| fast);
            (fresh, cache_read, cache_write, fast)
        }
        None => (None, 0, 0, model.supports_fast()),
    };
    let fresh_input = try_pair!(fresh_input_from_legacy_total(
        input_tokens,
        fresh,
        cache_read,
        cache_write,
    ));
    let usage = TokenUsage {
        input: fresh_input,
        output: output_tokens,
        cache_creation: cache_write,
        cache_read,
    };
    let cost = usage.cost(&model.pricing, fast);
    Ok((Some(cost), None))
}

/// Build a system prompt from a built-in template. Environment variables like
/// `{cwd}` are substituted automatically. Use this when you need a ready-made
/// prompt for a subagent session.
///
/// @param ctx LuaCtx Agent context.
/// @param opts table Required fields:
///   `prompt_id` (string) - one of `"research"`, `"general"`, `"system"`.
/// Optional fields:
///   `instructions` (string|boolean?) - extra text appended to the prompt.
///     `true` loads instructions from the project `.n00n/instructions` file.
///     `false` or nil omits them.
/// @return (string?, string?) The assembled prompt string, or `(nil, err)` on failure.
/// @example
/// local prompt, err = n00n.agent.system_prompt(ctx, {
///   prompt_id = "research",
///   instructions = true,
/// })
/// if err then error(err) end
#[lua_fn]
#[allow(clippy::needless_pass_by_value)]
async fn system_prompt(
    _lua: Lua,
    ctx: mlua::UserDataRef<LuaCtx>,
    opts: Table,
) -> LuaResult<Pair<String>> {
    let agent = try_pair!(dispatch_ctx(&ctx, "system_prompt"));
    let prompt_id_str: String = opts.get("prompt_id")?;
    let prompt_id = match prompt_id_str.as_str() {
        "research" => n00n_agent::prompt::PromptId::Research,
        "general" => n00n_agent::prompt::PromptId::General,
        "system" => n00n_agent::prompt::PromptId::System,
        other => return Ok(err_pair(format!("unknown prompt_id: {other}"))),
    };

    let vars = n00n_agent::template::env_vars();
    let instructions_val: LuaValue = opts.get("instructions")?;
    let instructions = match instructions_val {
        LuaValue::Boolean(true) => {
            let cwd = vars.apply("{cwd}").into_owned();
            smol::unblock(move || n00n_agent::agent::load_instruction_text(&cwd)).await
        }
        LuaValue::Boolean(false) | LuaValue::Nil => String::new(),
        LuaValue::String(s) => s.to_str()?.to_owned(),
        _ => return Err(mlua::Error::runtime("instructions must be bool or string")),
    };

    let assembled = n00n_agent::prompt::assemble(prompt_id, &agent.prompt_slots, &instructions);
    Ok((Some(vars.apply(&assembled).into_owned()), None))
}

/// Get the list of tool definitions for a given audience. Pass the result
/// straight into `n00n.agent.session()` or use it to inspect what tools are
/// available.
///
/// @param ctx LuaCtx Agent context.
/// @param opts table Required fields:
///   `audience` (string) - tool audience filter, e.g. `"general"`, `"subagent"`,
///     `"general_sub"`.
/// Optional fields:
///   `only` (string[]?) - include only these tool names.
///   `except` (string[]?) - exclude these tool names.
///   `include_mcp` (boolean?) - include MCP tools. Default: `true`.
///   `workflow` (boolean?) - use workflow-mode descriptions. Default: `false`.
///   `spec` (string?) - evaluate capability exclusions against this model spec.
/// @return (table?, string?) Array of tool definition tables, or `(nil, err)` on failure.
/// @example
/// local defs, err = n00n.agent.tools(ctx, {
///   audience = "general_sub",
///   except = { "bash", "write" },
/// })
/// if err then error(err) end
/// print(#defs .. " tools available")
#[lua_fn]
#[allow(clippy::needless_pass_by_value)]
fn tools(lua: &Lua, ctx: mlua::UserDataRef<LuaCtx>, opts: Table) -> LuaResult<Pair<LuaValue>> {
    let agent = try_pair!(dispatch_ctx(&ctx, "tools"));
    let audience_str: String = opts.get("audience")?;
    let audience = try_pair!(
        ToolAudience::parse_name(&audience_str)
            .ok_or_else(|| format!("unknown audience: {audience_str}"))
    );

    let only: Option<Vec<String>> = opts.get("only")?;
    let except: Option<Vec<String>> = opts.get("except")?;
    let include_mcp: bool = opts.get::<Option<bool>>("include_mcp")?.map_or(true, |v| v);
    let workflow: bool = opts.get::<Option<bool>>("workflow")?.map_or(false, |v| v);
    let spec_str: Option<String> = opts.get("spec")?;

    let parsed = spec_str
        .as_deref()
        .and_then(|spec| Model::from_spec(spec).ok());
    let model = if let Some(ref m) = parsed {
        m
    } else {
        &agent.model
    };

    let base = match (only, except) {
        (Some(o), _) => ToolFilter::Only(o),
        (_, Some(e)) => ToolFilter::AllExcept(e),
        _ => ToolFilter::All,
    };
    let disabled: Vec<&str> = agent
        .config
        .disabled_tools
        .iter()
        .map(String::as_str)
        .collect();
    let filter = base
        .excluding(&disabled)
        .excluding(n00n_agent::tools::capability_exclusions(model));

    let vars = n00n_agent::template::env_vars();
    let ctx_desc = DescriptionContext {
        filter: &filter,
        audience,
        workflow,
    };
    let mut defs =
        ToolRegistry::global().definitions(&vars, &ctx_desc, model.supports_tool_examples());

    if include_mcp && let Some(ref mcp) = agent.mcp {
        mcp.extend_tools(&mut defs);
    }

    Ok((Some(json_to_lua(lua, &defs)?), None))
}

/// Run a tool by name and wait for the result. This is how you call built-in
/// tools (like `read`, `bash`, `glob`) from Lua without going through the LLM.
///
/// Live events (streaming output, annotations) are delivered through optional
/// callbacks while the tool runs.
///
/// @param ctx LuaCtx Agent context.
/// @param name string Tool name, e.g. `"bash"`, `"read"`.
/// @param input table|any Tool input (JSON-serializable). Must match the tool's `input_schema`.
/// @param opts table? Optional fields:
///   `timeout` (integer?) - deadline in seconds.
///   `on_live_buf` (function?) - called with a `BufHandle` for each live buffer
///     the tool publishes. Must not yield.
///   `on_annotation` (function?) - called with an annotation string for each
///     annotation event. Must not yield.
/// @return (string?, string?) Tool output text, or `(nil, err)` on failure.
/// @example
/// local out, err = n00n.agent.call_tool(ctx, "bash", {
///   command = "ls -la",
///   timeout = 10,
/// })
/// if err then error(err) end
/// print(out)
#[lua_fn]
async fn call_tool(
    lua: Lua,
    ctx: mlua::UserDataRef<LuaCtx>,
    name: String,
    input: LuaValue,
    opts: Option<Table>,
) -> LuaResult<Pair<String>> {
    let input_json = lua_to_json(&lua, &input)?;
    let agent = try_pair!(dispatch_ctx(&ctx, "call_tool"));
    let mut tctx = agent.to_tool_context();
    let (mut on_buf, mut on_ann, mut rx) = (None, None, None);
    if let Some(o) = opts {
        if let Some(secs) = o.get::<Option<u64>>("timeout")? {
            tctx.deadline = Deadline::after(Duration::from_secs(secs));
        }
        on_buf = o.get::<Option<Function>>("on_live_buf")?;
        on_ann = o.get::<Option<Function>>("on_annotation")?;
        if on_buf.is_some() || on_ann.is_some() {
            let (tx, r) = flume::unbounded();
            tctx.live_sink = Some(tx);
            rx = Some(r);
        }
    }
    drop(ctx);
    if let Err(e) = tctx.deadline.check() {
        return Ok(err_pair(e));
    }
    let cbs = LiveCallbacks {
        tool: &name,
        on_buf,
        on_ann,
    };
    let done = dispatch_racing_live(&tctx, &name, &input_json, rx, &cbs).await;
    // Same fallback the UI applies on tool completion, so a batch child's
    // header carries the annotation its standalone run would get.
    let annotation = done
        .annotation
        .clone()
        .or_else(|| (!done.is_error).then(|| done.output.annotation()).flatten());
    if let Some(a) = annotation {
        cbs.deliver(ToolLive::Annotation(a)).await;
    }
    match interpreter_bridge::flatten(&done) {
        Ok(text) => Ok((Some(text), None)),
        Err(err) => Ok((None, Some(err))),
    }
}

/// Create a new subagent session. The session inherits the parent model and
/// MCP handle unless you override them. You get back a `Session` object that
/// you can send messages to with `:prompt()`.
///
/// This is the main way to spin up a sub-conversation with its own history
/// and tool set.
///
/// @param ctx LuaCtx Agent context.
/// @param opts table Optional fields:
///   `model_spec` (string?) - model spec string to use instead of the parent model.
///   `system` (string?) - system prompt. Defaults to empty.
///   `tools` (table?) - tool definitions array (from `n00n.agent.tools()`).
///   `local_tools` (table?) - map of `name -> spec` for Lua-backed tools. Each spec
///     requires `description` (string), `input_schema` (table), and
///     `handler` (function). The handler receives the input table and must return
///     `(string)` or `(nil, err)`.
///   `name` (string?) - display name for logs and UI.
///   `audience` (string?) - tool audience for capability gating. Default: `"general_sub"`.
///   `thinking` (string|integer?) - thinking mode: `"off"`, `"adaptive"`, an
///     effort level (`"minimal"`, `"low"`, `"medium"`, `"high"`, `"xhigh"`,
///     `"max"`), or a budget integer (token count). Inherits parent setting
///     if omitted.
///   `fast` (boolean?) - use fast mode. Inherits parent setting if omitted.
/// @return (Session?, string?) Session handle, or `(nil, err)` on failure.
/// @example
/// local tools = n00n.agent.tools(ctx, { audience = "general_sub" })
/// local sess, err = n00n.agent.session(ctx, {
///   system = "You are a research assistant.",
///   tools = tools,
///   name = "researcher",
/// })
/// if err then error(err) end
/// local result = sess:prompt("Summarize this file.")
/// sess:close()
#[lua_fn]
#[allow(clippy::too_many_lines)]
#[allow(clippy::cast_possible_truncation)]
async fn session(
    lua: Lua,
    ctx: mlua::UserDataRef<LuaCtx>,
    opts: Table,
) -> LuaResult<Pair<mlua::AnyUserData>> {
    let agent_ctx = try_pair!(dispatch_ctx(&ctx, "session")).clone();
    drop(ctx);
    let model_spec: Option<String> = opts.get("model_spec")?;
    let system: Option<String> = opts.get("system")?;
    let tools_val: Option<LuaValue> = opts.get("tools")?;
    let local_tools_tbl: Option<Table> = opts.get("local_tools")?;
    let name: Option<String> = opts.get("name")?;
    let thinking_val: Option<LuaValue> = opts.get("thinking")?;
    let audience = match opts.get::<Option<String>>("audience")? {
        Some(s) => {
            try_pair!(ToolAudience::parse_name(&s).ok_or_else(|| format!("unknown audience: {s}")))
        }
        None => DEFAULT_SESSION_AUDIENCE,
    };
    let requested_fast: bool = opts
        .get::<Option<bool>>("fast")?
        .map_or(agent_ctx.opts.fast, |v| v);

    let (model, provider): (Model, Arc<dyn provider::Provider>) = if let Some(ref spec) = model_spec
    {
        let mut m = try_pair!(Model::from_spec(spec));
        let p = try_pair!(
            provider::from_model_async_with_openai_options(
                &mut m,
                agent_ctx.timeouts,
                agent_ctx.openai_options,
            )
            .await
        );
        (m, Arc::from(p))
    } else {
        (
            Model::clone(&agent_ctx.model),
            Arc::clone(&agent_ctx.provider),
        )
    };
    let fast = requested_fast && model.supports_fast();
    // A standalone task shows its model via SubagentInfo on the header;
    // a dispatching caller (batch) gets the same thing as a live annotation.
    if let Some(sink) = &agent_ctx.live_sink {
        let _ = sink.send(ToolLive::Annotation(model.spec()));
    }

    let mut tools_json: JsonValue = match tools_val {
        Some(val) => {
            let tools = lua_to_json(&lua, &val)?;
            if !tools.is_array() {
                return Err(mlua::Error::runtime("tools must be an array"));
            }
            tools
        }
        None => JsonValue::Array(vec![]),
    };

    let mut local_map: HashMap<String, LocalToolFn> = HashMap::new();
    if let Some(tbl) = local_tools_tbl {
        let defs = tools_json
            .as_array_mut()
            .unwrap_or_else(|| unreachable!("tools_json is always an array here"));
        for pair in tbl.pairs::<String, Table>() {
            let (name, spec) = pair?;
            let description = try_pair!(
                spec.get::<String>("description")
                    .map_err(|_| format!("local_tools.{name}: 'description' is required"))
            );
            let input_schema = lua_to_json(&lua, &spec.get::<LuaValue>("input_schema")?)?;
            let handler = try_pair!(
                spec.get::<Function>("handler")
                    .map_err(|_| format!("local_tools.{name}: 'handler' is required"))
            );
            defs.push(serde_json::json!({
                "name": name,
                "description": description,
                "input_schema": input_schema,
            }));
            let weak = lua.weak();
            local_map.insert(
                name,
                Arc::new(move |input: &JsonValue| call_local_tool(&weak, &handler, input))
                    as LocalToolFn,
            );
        }
    }

    let thinking = match thinking_val {
        Some(LuaValue::String(s)) => match StoredThinking::parse_setting(&s.to_str()?) {
            Ok(stored) => ThinkingConfig::from(stored),
            Err(e) => return Ok(err_pair(format!("invalid thinking: {e}"))),
        },
        Some(LuaValue::Integer(n)) => match u32::try_from(n) {
            Ok(tokens) if tokens > 0 => ThinkingConfig::Budget(tokens),
            _ => return Ok(err_pair(format!("invalid thinking budget: {n}"))),
        },
        Some(LuaValue::Number(n)) if n >= 1.0 && n <= f64::from(u32::MAX) => {
            let tokens = u32::try_from(n as i64)
                .map_err(|_| mlua::Error::runtime(format!("invalid thinking budget: {n}")))?;
            ThinkingConfig::Budget(tokens)
        }
        Some(LuaValue::Number(n)) => {
            return Ok(err_pair(format!("invalid thinking budget: {n}")));
        }
        Some(_) => return Err(mlua::Error::runtime("thinking must be string or number")),
        None => agent_ctx.opts.thinking,
    };

    let session_id = N00nId::generate();
    let child_id = session_id.to_string();
    let parent_tool_use_id = child_id.clone();
    let start = Instant::now();
    let (sub_tx, sub_rx) = flume::unbounded::<Envelope>();
    let sub_event_tx = EventSender::new(sub_tx, agent_ctx.event_tx.run_id());
    let parent_tx = agent_ctx.event_tx.clone();
    let (answer_tx, answer_rx) = flume::unbounded::<String>();
    let (prompt_tx, prompt_rx) = flume::bounded::<SubagentPrompt>(STEERING_QUEUE_CAPACITY);
    let progress = Arc::new(Progress::new(start, child_id.clone()));

    let subagent_info: Arc<OnceLock<SubagentInfo>> = Arc::new(OnceLock::new());
    let usage = TokenUsage::default();
    let cost = 0.0;

    {
        let info = Arc::clone(&subagent_info);
        let progress = Arc::clone(&progress);
        let parent_tx = parent_tx.clone();
        smol::spawn(async move {
            while let Ok(mut envelope) = sub_rx.recv_async().await {
                match &envelope.event {
                    AgentEvent::Done { .. } => {
                        progress.record_forwarder_barrier();
                        continue;
                    }
                    AgentEvent::ToolStart(e) => {
                        progress.record_start(e);
                    }
                    AgentEvent::ToolDone(e) => {
                        progress.record_done(e);
                    }
                    AgentEvent::Error { .. }
                    | AgentEvent::ToolOutput { .. }
                    | AgentEvent::ToolPending { .. }
                    | AgentEvent::SubagentHistory { .. } => continue,
                    _ => {}
                }
                envelope.subagent = info.get().cloned();
                let _ = parent_tx.send_envelope(envelope);
            }
        })
        .detach();
    }

    let (child_trigger, child_cancel) = agent_ctx.cancel.child();
    agent_ctx
        .subagent_cancels
        .insert(child_id.clone(), child_trigger);

    let name = name.unwrap_or_else(|| format!("session-{child_id}"));
    info!(name = %name, model = %model.id, "subagent session opened");

    let state = SessionState {
        params: AgentParams {
            provider,
            model,
            config: agent_ctx.config.clone(),
            tool_output_lines: n00n_config::ToolOutputLines::default(),
            permissions: Arc::clone(&agent_ctx.permissions),
            session_id: Some(session_id.into()),
            timeouts: agent_ctx.timeouts,
            openai_options: agent_ctx.openai_options,
            file_tracker: FileReadTracker::fresh(),
            prompt_slots: Arc::clone(&agent_ctx.prompt_slots),
            subagent_cancels: Arc::new(CancelMap::new()),
            registry: Arc::clone(n00n_agent::tools::ToolRegistry::global_arc()),
            audience,
        },
        system: system.unwrap_or_else(String::new),
        tools: tools_json,
        thinking,
        fast,
        mcp: agent_ctx.mcp.clone(),
        history: History::new(Vec::new()),
        sub_event_tx,
        child_cancel,
        answer_rx: Arc::new(AsyncMutex::new(answer_rx)),
        answer_tx: Some(answer_tx),
        prompt_rx,
        prompt_tx: Some(prompt_tx),
        parent_cancels: Arc::clone(&agent_ctx.subagent_cancels),
        child_id,
        parent_tool_use_id,
        parent_event_tx: parent_tx,
        subagent_info,
        local_tools: Arc::new(local_map),
        name,
        usage,
        cost,
        start,
        closed: false,
        failed: false,
        progress: Arc::clone(&progress),
    };

    let sess = lua.create_userdata(LuaSession {
        inner: Arc::new(AsyncMutex::new(state)),
        progress,
    })?;
    Ok((Some(sess), None))
}

lua_table! {
    /// Subagent primitives for plugins that need to talk to an LLM.
    ///
    /// This module gives you the building blocks: resolve which model to use,
    /// build a system prompt, list available tools, call a tool directly, or
    /// open a full session with its own conversation history.
    ///
    /// Policy like retries, validation, and concurrency lives in the calling
    /// plugin, not here.
    ///
    /// ```lua
    /// local tools = n00n.agent.tools(ctx, { audience = "general_sub" })
    /// local sess = n00n.agent.session(ctx, {
    ///   system = "You are a helpful assistant.",
    ///   tools = tools,
    /// })
    /// local r = sess:prompt("Hello!")
    /// print(r.text)
    /// sess:close()
    /// ```
    "n00n.agent" => pub(crate) fn create_agent_table(), DOCS [
        resolve_model, system_prompt, tools, call_tool, session, usage_cost,
    ]
}

/// Must use `call_async`, not `call`: callbacks that yield (highlight,
/// markdown) hit the C-call boundary otherwise.
struct LiveCallbacks<'a> {
    tool: &'a str,
    on_buf: Option<Function>,
    on_ann: Option<Function>,
}

impl LiveCallbacks<'_> {
    async fn deliver(&self, ev: ToolLive) {
        let res = match ev {
            ToolLive::Buf(buf) => call_opt(self.on_buf.as_ref(), BufHandle::foreign(buf)).await,
            ToolLive::Annotation(ann) => call_opt(self.on_ann.as_ref(), ann).await,
        };
        if let Some(Err(e)) = res {
            tracing::warn!(tool = self.tool, error = %e, "call_tool callback failed");
        }
    }
}

async fn call_opt(f: Option<&Function>, arg: impl IntoLuaMulti) -> Option<LuaResult<()>> {
    match f {
        Some(f) => Some(f.call_async::<()>(arg).await),
        None => None,
    }
}

/// Like `interpreter_bridge::dispatch`, but keeps the full `ToolDoneEvent`
/// (the annotation lives there) and feeds live events to `cbs` while the
/// child runs.
async fn dispatch_racing_live(
    tctx: &ToolContext,
    name: &str,
    input: &JsonValue,
    rx: Option<flume::Receiver<ToolLive>>,
    cbs: &LiveCallbacks<'_>,
) -> ToolDoneEvent {
    let run = tool_dispatch::run(
        &tctx.registry,
        tctx.mcp.as_ref(),
        String::new(),
        name,
        input,
        tctx,
        Emit::Silent,
    );
    let Some(rx) = rx else {
        return run.await;
    };
    let mut run = pin!(run);
    loop {
        match select(run.as_mut(), pin!(rx.recv_async())).await {
            Either::Left((done, _)) => {
                while let Ok(ev) = rx.try_recv() {
                    cbs.deliver(ev).await;
                }
                return done;
            }
            Either::Right((Ok(ev), _)) => cbs.deliver(ev).await,
            // The sender is gone but no result arrived: just wait for the run.
            Either::Right((Err(_), _)) => return run.await,
        }
    }
}

fn activity_message(event: &ToolStartEvent) -> Option<String> {
    if !SAFE_ACTIVITY_DESCRIPTION_TOOLS.contains(&event.tool.as_ref()) {
        return None;
    }
    let rendered_header = event
        .render_header
        .as_ref()
        .map(n00n_agent::BufferSnapshot::first_line_text);
    rendered_header
        .as_deref()
        .filter(|text| !text.trim().is_empty())
        .or_else(|| (!event.summary.trim().is_empty()).then_some(event.summary.as_str()))
        .or_else(|| {
            event
                .annotation
                .as_deref()
                .filter(|text| !text.trim().is_empty())
        })
        .map(sanitize_activity_message)
}

fn sanitize_activity_message(raw: &str) -> String {
    let words = raw.split_whitespace().collect::<Vec<_>>();
    let mut sanitized = Vec::with_capacity(words.len());
    let mut index = 0;
    while index < words.len() {
        let word = words[index];
        if word.eq_ignore_ascii_case("bearer") {
            sanitized.push(format!("Bearer {REDACTED}"));
            index = index.saturating_add(2);
            continue;
        }

        let separator = word.find(['=', ':']);
        let key = separator.map_or(word, |position| &word[..position]);
        if is_sensitive_key(key) || is_sensitive_key(word) {
            let separator_char =
                separator.map_or('=', |position| word.as_bytes()[position] as char);
            sanitized.push(format!("{key}{separator_char}{REDACTED}"));
            let inline_value = separator.and_then(|position| word.get(position + 1..));
            index += 1;
            if inline_value.is_some_and(|value| value.eq_ignore_ascii_case("bearer")) {
                index = index.saturating_add(1).min(words.len());
            } else if inline_value.is_none_or(str::is_empty) {
                if words
                    .get(index)
                    .is_some_and(|next| *next == "=" || *next == ":")
                {
                    index += 1;
                }
                if words
                    .get(index)
                    .is_some_and(|next| next.eq_ignore_ascii_case("bearer"))
                {
                    index += 1;
                }
                if index < words.len() {
                    index += 1;
                }
            }
            continue;
        }

        let secret_value = separator.map_or(word, |position| &word[position + 1..]);
        if is_secret_token(secret_value) {
            let prefix = separator.map_or("", |position| &word[..=position]);
            sanitized.push(format!("{prefix}{REDACTED}"));
        } else {
            sanitized.push(word.to_owned());
        }
        index += 1;
    }
    truncate_activity_message(&sanitized.join(" "))
}

fn is_sensitive_key(value: &str) -> bool {
    let normalized: String = value
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .flat_map(char::to_lowercase)
        .collect();
    [
        "apikey",
        "accesstoken",
        "authtoken",
        "authorization",
        "password",
        "passwd",
        "secret",
        "privatekey",
        "clientsecret",
    ]
    .iter()
    .any(|key| normalized.contains(key))
}

fn is_secret_token(value: &str) -> bool {
    let lower = value
        .trim_matches(|character: char| !character.is_ascii_alphanumeric())
        .to_ascii_lowercase();
    ["sk-", "ghp_", "github_pat_", "glpat-", "xoxb-", "xoxp-"]
        .iter()
        .any(|prefix| lower.contains(prefix))
        || lower.starts_with("akia")
        || lower.starts_with("aiza")
}

fn truncate_activity_message(message: &str) -> String {
    if message.chars().count() <= ACTIVITY_MESSAGE_MAX_CHARS {
        return message.to_owned();
    }
    let mut truncated: String = message
        .chars()
        .take(ACTIVITY_MESSAGE_MAX_CHARS.saturating_sub(1))
        .collect();
    truncated.push('…');
    truncated
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ActivityStatus {
    Running,
    Success,
    Error,
}

impl ActivityStatus {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Success => "success",
            Self::Error => "error",
        }
    }
}

#[derive(Debug, Clone)]
struct Activity {
    id: String,
    tool: String,
    message: Option<String>,
    status: ActivityStatus,
}

struct ProgressState {
    current: Option<String>,
    recent: VecDeque<String>,
    activities: VecDeque<Activity>,
    done: bool,
    completed_count: u64,
    active_activity_counts: HashMap<String, usize>,
    turn_id: u64,
    forwarded_barriers: u64,
}

struct Progress {
    session_id: String,
    start: Instant,
    state: Mutex<ProgressState>,
    tx: flume::Sender<()>,
    rx: flume::Receiver<()>,
    barrier_tx: flume::Sender<()>,
    barrier_rx: flume::Receiver<()>,
}

fn sanitize_activity_value(value: &str, max_bytes: usize, fallback: &str) -> String {
    let mut out = String::with_capacity(value.len().min(max_bytes));
    for byte in value.bytes() {
        if out.len() >= max_bytes {
            break;
        }
        if byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'.' | b':' | b'/' | b'-') {
            out.push(char::from(byte));
        }
    }
    if out.is_empty() {
        fallback.to_owned()
    } else {
        out
    }
}

impl Progress {
    fn new(start: Instant, session_id: String) -> Self {
        let (tx, rx) = flume::unbounded();
        let (barrier_tx, barrier_rx) = flume::bounded(1);
        Self {
            session_id,
            start,
            state: Mutex::new(ProgressState {
                current: None,
                recent: VecDeque::new(),
                activities: VecDeque::new(),
                done: false,
                completed_count: 0,
                active_activity_counts: HashMap::new(),
                turn_id: 0,
                forwarded_barriers: 0,
            }),
            tx,
            rx,
            barrier_tx,
            barrier_rx,
        }
    }

    fn notify(&self) {
        let _ = self.tx.try_send(());
    }

    fn begin_turn(&self) -> u64 {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.turn_id = state.turn_id.saturating_add(1);
        state.done = false;
        state.current = None;
        let turn_id = state.turn_id;
        drop(state);
        self.notify();
        turn_id
    }

    fn next_forwarder_barrier(&self) -> u64 {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .forwarded_barriers
            .saturating_add(1)
    }

    fn record_forwarder_barrier(&self) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.forwarded_barriers = state.forwarded_barriers.saturating_add(1);
        drop(state);
        let _ = self.barrier_tx.try_send(());
    }

    async fn wait_for_forwarder_barrier(&self, target: u64) {
        loop {
            let reached = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .forwarded_barriers
                >= target;
            if reached || self.barrier_rx.recv_async().await.is_err() {
                return;
            }
        }
    }

    fn record_start(&self, event: &ToolStartEvent) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let active_count = state
            .active_activity_counts
            .entry(event.id.clone())
            .or_insert(0);
        *active_count = active_count.saturating_add(1);
        if state.activities.len() >= PROGRESS_MAX_RECENT {
            state.activities.pop_front();
        }
        state.activities.push_back(Activity {
            id: event.id.clone(),
            tool: event.tool.to_string(),
            message: activity_message(event),
            status: ActivityStatus::Running,
        });
        state.current = Some(event.tool.to_string());
        drop(state);
        self.notify();
    }

    fn record_done(&self, event: &ToolDoneEvent) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        match state.active_activity_counts.entry(event.id.clone()) {
            Entry::Occupied(mut entry) if *entry.get() > 1 => {
                *entry.get_mut() = entry.get().saturating_sub(1);
            }
            Entry::Occupied(entry) => {
                entry.remove();
            }
            Entry::Vacant(_) => return,
        }
        if let Some(activity) = state
            .activities
            .iter_mut()
            .find(|activity| activity.id == event.id && activity.status == ActivityStatus::Running)
        {
            activity.status = if event.is_error {
                ActivityStatus::Error
            } else {
                ActivityStatus::Success
            };
        }
        state.completed_count = state.completed_count.saturating_add(1);
        if state.recent.len() >= PROGRESS_MAX_RECENT {
            state.recent.pop_front();
        }
        state.recent.push_back(event.tool.to_string());
        state.current = state
            .activities
            .iter()
            .rev()
            .find(|activity| activity.status == ActivityStatus::Running)
            .map(|activity| activity.tool.clone());
        drop(state);
        self.notify();
    }

    fn set_current_done(&self) {
        let turn_id = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .turn_id;
        self.set_done(turn_id);
    }

    fn set_done(&self, turn_id: u64) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.turn_id != turn_id {
            return;
        }
        state.done = true;
        state.current = None;
        drop(state);
        self.notify();
    }
}

fn set_usage_fields(table: &Table, usage: TokenUsage) -> LuaResult<()> {
    table.set("input_tokens", usage.total_input())?;
    table.set("output_tokens", usage.output)?;
    table.set("fresh_input_tokens", usage.input)?;
    table.set("cache_read_tokens", usage.cache_read)?;
    table.set("cache_write_tokens", usage.cache_creation)?;
    Ok(())
}

fn prompt_result_table(
    lua: &Lua,
    duration_ms: u64,
    usage: TokenUsage,
    cost: f64,
    fast: bool,
    text: Option<String>,
) -> LuaResult<Table> {
    let table = lua.create_table()?;
    if let Some(text) = text {
        table.set("text", text)?;
    }
    table.set("duration_ms", duration_ms)?;
    table.set("cost", cost)?;
    table.set("fast", fast)?;
    set_usage_fields(&table, usage)?;
    Ok(table)
}

fn latest_assistant_text(history: &History) -> String {
    history
        .as_slice()
        .iter()
        .rev()
        .filter(|message| matches!(message.role, Role::Assistant))
        .flat_map(|message| message.content.iter())
        .find_map(|block| match block {
            ContentBlock::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .map_or_else(
            || "(no response)".to_owned(),
            std::borrow::ToOwned::to_owned,
        )
}

struct SessionState {
    params: AgentParams,
    system: String,
    tools: JsonValue,
    thinking: ThinkingConfig,
    fast: bool,
    mcp: Option<n00n_agent::mcp::McpHandle>,
    history: History,
    sub_event_tx: EventSender,
    child_cancel: n00n_agent::cancel::CancelToken,
    answer_rx: Arc<AsyncMutex<flume::Receiver<String>>>,
    answer_tx: Option<flume::Sender<String>>,
    prompt_rx: flume::Receiver<SubagentPrompt>,
    prompt_tx: Option<flume::Sender<SubagentPrompt>>,
    parent_cancels: Arc<CancelMap<String>>,
    child_id: String,
    parent_tool_use_id: String,
    parent_event_tx: EventSender,
    subagent_info: Arc<OnceLock<SubagentInfo>>,
    local_tools: LocalTools,
    name: String,
    usage: TokenUsage,
    cost: f64,
    start: Instant,
    closed: bool,
    failed: bool,
    progress: Arc<Progress>,
}

impl SessionState {
    #[allow(clippy::cast_possible_truncation)]
    fn close(&mut self) {
        if self.closed {
            return;
        }
        self.closed = true;
        self.progress.set_current_done();
        self.parent_cancels.remove(&self.child_id);
        let messages = std::mem::replace(&mut self.history, History::new(Vec::new())).into_vec();
        let _ = self.parent_event_tx.send(AgentEvent::SubagentHistory {
            tool_use_id: self.child_id.clone(),
            messages,
            is_error: self.failed,
        });
        let duration_ms = self.start.elapsed().as_millis() as u64;
        info!(
            name = %self.name,
            duration_ms,
            input_tokens = self.usage.total_input(),
            fresh_input_tokens = self.usage.input,
            cache_read_tokens = self.usage.cache_read,
            cache_write_tokens = self.usage.cache_creation,
            output_tokens = self.usage.output,
            cost = self.cost,
            "subagent session closed",
        );
    }
}

struct PromptInterruptSource {
    rx: flume::Receiver<SubagentPrompt>,
    thinking: ThinkingConfig,
    fast: bool,
}

impl n00n_agent::InterruptSource for PromptInterruptSource {
    fn poll(&self, _: n00n_agent::InterruptPoint) -> Option<n00n_agent::ExtractedCommand> {
        self.rx.try_recv().ok().map(|prompt| {
            n00n_agent::ExtractedCommand::Interrupt(
                AgentInput {
                    message: prompt.text,
                    mode: AgentMode::Build,
                    images: prompt.images,
                    preamble: Vec::new(),
                    thinking: self.thinking,
                    fast: self.fast,
                    workflow: false,
                    prompt: None,
                },
                0,
            )
        })
    }
}

struct LuaSession {
    inner: Arc<AsyncMutex<SessionState>>,
    progress: Arc<Progress>,
}

impl Drop for LuaSession {
    fn drop(&mut self) {
        if let Some(mut s) = self.inner.try_lock() {
            s.close();
        } else {
            // Prompt still in flight: close asynchronously so history
            // and cancel entry are never silently leaked.
            let inner = Arc::clone(&self.inner);
            smol::spawn(async move { inner.lock().await.close() }).detach();
        }
    }
}

/// Send a message to the subagent and wait for its full response. The agent
/// loop runs to completion, calling tools as needed. Conversation history is
/// kept across calls, so you can have a multi-turn conversation.
///
/// The success table has fields: `text` (string), `duration_ms` (integer),
/// `input_tokens` (integer), `fresh_input_tokens` (integer),
/// `cache_read_tokens` (integer), `cache_write_tokens` (integer),
/// `output_tokens` (integer), actual `fast` state (boolean), and aggregate
/// `cost` (number). The cost is summed
/// per request using that request's model and fast tier, including compaction.
/// If execution fails after incurring usage, the
/// result table omits `text` and contains only `duration_ms` plus these
/// sanitized numeric usage fields. Check `err` before reading `text`.
///
/// @param message string User message to send.
/// @return (table?, string?) `(result, nil)` on success; charged failures return
///   `(sanitized_usage, err)`. Session-state failures can return `(nil, err)`.
/// @example
/// local r, err = sess:prompt("What files are in this project?")
/// if err then error(err) end
/// print(r.text)
/// print(r.input_tokens .. " input, " .. r.output_tokens .. " output tokens")
#[lua_fn]
#[allow(clippy::cast_possible_truncation)]
async fn prompt(
    lua: Lua,
    this: mlua::UserDataRef<LuaSession>,
    message: String,
) -> LuaResult<Pair<Table>> {
    let inner = Arc::clone(&this.inner);
    drop(this);
    let mut guard = inner.lock().await;
    let s = &mut *guard;
    if s.closed {
        return Ok((None, Some(SESSION_CLOSED_ERR.to_owned())));
    }
    let progress_turn = s.progress.begin_turn();
    if s.subagent_info.get().is_none() {
        let _ = s.subagent_info.set(SubagentInfo {
            parent_tool_use_id: s.parent_tool_use_id.clone(),
            name: s.name.clone(),
            prompt: Some(message.clone()),
            model: Some(s.params.model.spec()),
            answer_tx: s.answer_tx.take(),
            prompt_tx: s.prompt_tx.take(),
        });
    }

    let mut next_message = Some(SubagentPrompt {
        text: message,
        images: Vec::new(),
    });
    while let Some(message) = next_message.take() {
        let mut agent = Agent::new(
            s.params.clone(),
            AgentRunParams {
                history: &mut s.history,
                system: s.system.clone(),
                event_tx: s.sub_event_tx.clone(),
                tools: s.tools.clone(),
            },
        )
        .with_user_response_rx(Arc::clone(&s.answer_rx))
        .with_interrupt_source(Arc::new(PromptInterruptSource {
            rx: s.prompt_rx.clone(),
            fast: s.fast,
        }))
        .with_cancel(s.child_cancel.clone())
        .with_mcp(s.mcp.clone())
        .with_local_tools(Arc::clone(&s.local_tools));

        let input = AgentInput {
            message: message.text,
            mode: AgentMode::Build,
            images: message.images,
            preamble: Vec::new(),
            thinking: s.thinking,
            fast: s.fast,
            workflow: false,
            prompt: None,
        };
        let barrier_target = s.progress.next_forwarder_barrier();
        let result = agent.run(input).await;
        s.usage += agent.total_usage();
        s.cost += agent.total_cost();
        drop(agent);
        if result.is_err()
            && let Err(barrier_error) = s.sub_event_tx.send(AgentEvent::Done {
                usage: TokenUsage::default(),
                num_turns: 0,
                stop_reason: None,
            })
        {
            s.failed = true;
            s.progress.set_done(progress_turn);
            return Ok((
                None,
                Some(format!(
                    "subagent event forwarder barrier failed: {barrier_error}"
                )),
            ));
        }
        s.progress.wait_for_forwarder_barrier(barrier_target).await;
        if let Err(e) = result {
            s.failed = true;
            s.progress.set_done(progress_turn);
            let table = prompt_result_table(
                &lua,
                s.start.elapsed().as_millis() as u64,
                s.usage,
                s.cost,
                s.fast,
                None,
            )?;
            return Ok((Some(table), Some(e.to_string())));
        }
        next_message = s.prompt_rx.try_recv().ok();
    }
    s.progress.set_done(progress_turn);

    let text = latest_assistant_text(&s.history);

    let table = prompt_result_table(
        &lua,
        s.start.elapsed().as_millis() as u64,
        s.usage,
        s.cost,
        s.fast,
        Some(text),
    )?;
    Ok((Some(table), None))
}

/// Poll the session for a progress snapshot while a prompt is running.
///
/// Returns a table with:
///   `elapsed_ms` (integer): time since the session was created.
///   `current_tool` (string?): name of the tool currently running, if any.
///   `recent_tools` (table): names of the last few finished tools, oldest first.
///   `activities` (table): up to five safe rendered tool summaries, oldest first.
///   `completed_count` (integer): total number of finished tools so far.
///   `turn_id` (integer): increases before each `prompt` call.
///   `done` (bool): true once the current prompt call has completed.
///
/// The call returns at most every `PROGRESS_TIMEOUT_MS` milliseconds, or
/// immediately when a tool starts or finishes.
#[lua_fn]
#[allow(clippy::needless_pass_by_value)]
#[allow(clippy::cast_possible_truncation)]
async fn get_progress(lua: Lua, this: mlua::UserDataRef<LuaSession>) -> LuaResult<Pair<Table>> {
    let progress = Arc::clone(&this.progress);
    let notify = pin!(progress.rx.recv_async());
    let timeout = pin!(smol::Timer::after(Duration::from_millis(
        PROGRESS_TIMEOUT_MS
    )));
    let _ = select(notify, timeout).await;

    let state = progress
        .state
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let elapsed = progress.start.elapsed().as_millis() as u64;
    let tbl = lua.create_table()?;
    tbl.set("session_id", progress.session_id.as_str())?;
    tbl.set("elapsed_ms", elapsed)?;
    tbl.set("current_tool", state.current.as_deref())?;
    tbl.set("done", state.done)?;
    tbl.set("completed_count", state.completed_count)?;
    tbl.set("turn_id", state.turn_id)?;

    let recent = lua.create_table()?;
    for (i, tool) in state.recent.iter().enumerate() {
        recent.set(i + 1, tool.as_str())?;
    }
    tbl.set("recent_tools", recent)?;

    let activities = lua.create_table()?;
    for (i, activity) in state.activities.iter().enumerate() {
        let item = lua.create_table()?;
        item.set("id", activity.id.as_str())?;
        item.set("tool", activity.tool.as_str())?;
        item.set("message", activity.message.as_deref())?;
        item.set("status", activity.status.as_str())?;
        activities.set(i + 1, item)?;
    }
    tbl.set("activities", activities)?;
    Ok((Some(tbl), None))
}

/// Cancel the current turn in this session without closing it. The agent will
/// stop at the next cancellation point and return an error from `:prompt()`.
///
/// @return
#[lua_fn]
async fn cancel(_lua: Lua, this: mlua::UserDataRef<LuaSession>) -> LuaResult<()> {
    let inner = Arc::clone(&this.inner);
    drop(this);
    let s = inner.lock().await;
    s.parent_cancels.cancel_or_precancel(s.child_id.clone());
    Ok(())
}

/// Close the session and flush its history back to the parent agent. You can
/// call this multiple times safely. If you forget, it runs automatically when
/// the session is garbage collected.
///
/// @return
#[lua_fn]
async fn close(_lua: Lua, this: mlua::UserDataRef<LuaSession>) -> LuaResult<()> {
    let inner = Arc::clone(&this.inner);
    drop(this);
    let mut s = inner.lock().await;
    s.close();
    Ok(())
}

lua_class! {
    /// A subagent session with its own conversation history.
    ///
    /// Create one with `n00n.agent.session()`, then send messages with
    /// `:prompt()`. The session remembers previous turns, so you can have
    /// a multi-step conversation. Call `:close()` when you are done, or let
    /// garbage collection handle it.
    "n00n.agent.Session" => LuaSession, SESSION_DOCS [prompt, close, get_progress, cancel]
}

/// Weak Lua ref avoids a reference cycle when the session is stored in userdata.
fn call_local_tool(
    weak: &mlua::WeakLua,
    f: &Function,
    input: &JsonValue,
) -> Result<String, String> {
    let lua = weak.try_upgrade().ok_or("Lua runtime shut down")?;
    let arg = json_to_lua(&lua, input).map_err(|e| e.to_string())?;
    let values = f.call::<mlua::MultiValue>(arg).map_err(|e| e.to_string())?;
    lua_tool_result(values)
}

#[cfg(test)]
mod tests {
    use n00n_agent::ToolOutput;
    use serde_json::json;

    use super::*;
    use n00n_agent::{ExtractedCommand, InterruptPoint, InterruptSource};

    fn call(src: &str, input: &JsonValue) -> Result<String, String> {
        let lua = Lua::new();
        let f: Function = lua.load(src).eval().unwrap();
        call_local_tool(&lua.weak(), &f, input)
    }

    fn progress_start(id: &str, summary: &str) -> ToolStartEvent {
        ToolStartEvent {
            id: id.into(),
            tool: Arc::from("read"),
            summary: summary.into(),
            render_header: None,
            annotation: None,
            input: None,
            raw_input: Some(json!({"secret": "must not be read"})),
            output: Some(ToolOutput::Plain("must not be read".into())),
        }
    }

    fn progress_done(id: &str, is_error: bool) -> ToolDoneEvent {
        ToolDoneEvent {
            id: id.into(),
            tool: Arc::from("read"),
            output: ToolOutput::Plain("must not be read".into()),
            is_error,
            annotation: Some("must not be read".into()),
            written_path: None,
        }
    }

    #[test]
    fn progress_updates_start_in_place_and_retains_status() {
        let progress = Progress::new(Instant::now());
        progress.record_start(&progress_start("one", "cargo test"));
        progress.record_done(&progress_done("one", true));
        progress.record_done(&progress_done("one", true));

        let state = progress
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        assert_eq!(state.completed_count, 1);
        assert_eq!(state.activities.len(), 1);
        assert_eq!(state.activities[0].message.as_deref(), Some("cargo test"));
        assert_eq!(state.activities[0].status, ActivityStatus::Error);
        assert!(state.active_activity_counts.is_empty());
    }

    #[test]
    fn progress_tracks_reused_activity_ids_in_order() {
        let progress = Progress::new(Instant::now());
        progress.record_start(&progress_start("call_read", "first"));
        progress.record_start(&progress_start("call_read", "second"));
        progress.record_done(&progress_done("call_read", false));
        progress.record_done(&progress_done("call_read", true));

        let state = progress
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        assert_eq!(state.completed_count, 2);
        assert_eq!(state.activities.len(), 2);
        assert_eq!(state.activities[0].message.as_deref(), Some("first"));
        assert_eq!(state.activities[0].status, ActivityStatus::Success);
        assert_eq!(state.activities[1].message.as_deref(), Some("second"));
        assert_eq!(state.activities[1].status, ActivityStatus::Error);
        assert!(state.active_activity_counts.is_empty());
    }

    #[test]
    fn progress_keeps_latest_five_activity_rows() {
        let progress = Progress::new(Instant::now());
        for i in 0..7 {
            let id = i.to_string();
            progress.record_start(&progress_start(&id, &format!("message {i}")));
            progress.record_done(&progress_done(&id, false));
        }

        let state = progress
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        assert_eq!(state.activities.len(), PROGRESS_MAX_RECENT);
        assert_eq!(state.activities[0].message.as_deref(), Some("message 2"));
        assert_eq!(state.activities[4].message.as_deref(), Some("message 6"));
        assert!(
            state
                .activities
                .iter()
                .all(|activity| activity.status == ActivityStatus::Success)
        );
    }

    #[test]
    fn progress_activity_message_is_sanitized_and_truncated() {
        let progress = Progress::new(Instant::now());
        let long_secret = format!("API_KEY=super-secret\n{}", "é".repeat(100));
        progress.record_start(&progress_start("secret", &long_secret));

        let state = progress
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let message = state.activities[0].message.as_deref().unwrap();
        assert!(!message.contains("super-secret"));
        assert!(!message.contains('\n'));
        assert!(message.ends_with('…'));
        assert!(message.chars().count() <= ACTIVITY_MESSAGE_MAX_CHARS);
    }

    #[test]
    fn progress_counts_done_events_for_evicted_activity_rows() {
        let progress = Progress::new(Instant::now());
        for i in 0..7 {
            progress.record_start(&progress_start(&i.to_string(), "running"));
        }
        for i in 0..7 {
            progress.record_done(&progress_done(&i.to_string(), false));
        }

        let state = progress
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        assert_eq!(state.completed_count, 7);
        assert_eq!(state.recent.len(), PROGRESS_MAX_RECENT);
        assert!(
            state
                .activities
                .iter()
                .all(|activity| activity.status == ActivityStatus::Success)
        );
    }

    #[test]
    fn forwarder_barrier_observes_prior_tool_done() {
        smol::block_on(async {
            let progress = Arc::new(Progress::new(Instant::now()));
            progress.record_start(&progress_start("one", "reading"));
            let target = progress.next_forwarder_barrier();
            let forwarded = Arc::clone(&progress);
            let worker = smol::spawn(async move {
                forwarded.record_done(&progress_done("one", false));
                forwarded.record_forwarder_barrier();
            });

            progress.wait_for_forwarder_barrier(target).await;
            worker.await;
            let state = progress
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            assert_eq!(state.activities[0].status, ActivityStatus::Success);
        });
    }

    #[test]
    fn progress_redacts_separated_credentials() {
        let sanitized = sanitize_activity_message(
            "API_KEY = first Authorization: Bearer second --password third foo=sk-secret",
        );
        assert_eq!(
            sanitized,
            "API_KEY=[REDACTED] Authorization:[REDACTED] --password=[REDACTED] foo=[REDACTED]"
        );
    }

    #[test]
    fn progress_redacts_adjacent_bearer_scheme_and_token() {
        let sanitized = sanitize_activity_message("Authorization:Bearer visible-token trailing");
        assert_eq!(sanitized, "Authorization:[REDACTED] trailing");
    }

    #[test]
    fn progress_redacts_credentials_embedded_in_urls_and_tokens() {
        let sanitized = sanitize_activity_message(
            "https://host.test/path?api_key=visible glpat-visible prefix-ghp_visible",
        );
        assert_eq!(sanitized, "https:[REDACTED] [REDACTED] [REDACTED]");
    }

    #[test]
    fn progress_notifications_are_coalesced() {
        let progress = Progress::new(Instant::now());
        for _ in 0..100 {
            progress.notify();
            progress.record_forwarder_barrier();
        }
        assert_eq!(progress.rx.len(), 1);
        assert_eq!(progress.barrier_rx.len(), 1);
    }

    #[test]
    fn progress_hides_bash_and_unknown_tool_headers() {
        for tool in ["bash", "custom_plugin"] {
            let mut event = progress_start(tool, "API_KEY=visible");
            event.tool = Arc::from(tool);
            event.render_header = Some(n00n_agent::BufferSnapshot::plain_text(
                "Authorization: Bearer rendered-secret".into(),
            ));
            assert_eq!(activity_message(&event), None);
        }
    }

    #[test]
    fn progress_done_is_turn_safe() {
        let progress = Progress::new(Instant::now());
        let first = progress.begin_turn();
        let second = progress.begin_turn();
        progress.set_done(first);
        assert!(!progress.state.lock().unwrap().done);
        progress.set_done(second);
        assert!(progress.state.lock().unwrap().done);
    }

    #[test]
    fn usage_cost_three_argument_api_retains_legacy_fast_pricing() {
        let lua = Lua::new();
        let usage_cost: Function = create_agent_table(&lua).unwrap().get("usage_cost").unwrap();
        let (cost, err): Pair<f64> = usage_cost
            .call(("anthropic/claude-opus-4-8", 1_200_u32, 300_u32))
            .unwrap();
        assert_eq!(err, None);

        let model = Model::from_spec("anthropic/claude-opus-4-8").unwrap();
        assert!(model.supports_fast());
        let expected = TokenUsage {
            input: 1_200,
            output: 300,
            cache_creation: 0,
            cache_read: 0,
        }
        .cost(&model.pricing, true);
        let actual = cost.unwrap();
        assert!((actual - expected).abs() < f64::EPSILON);
    }

    #[test]
    fn usage_cost_prices_fresh_read_write_and_output_categories() {
        let lua = Lua::new();
        let usage_cost: Function = create_agent_table(&lua).unwrap().get("usage_cost").unwrap();
        let breakdown = lua.create_table().unwrap();
        breakdown.set("fresh_input_tokens", 400_000_u32).unwrap();
        breakdown.set("cache_read_tokens", 400_000_u32).unwrap();
        breakdown.set("cache_write_tokens", 200_000_u32).unwrap();
        breakdown.set("fast", true).unwrap();

        let (cost, err): Pair<f64> = usage_cost
            .call((
                "anthropic/claude-opus-4-8",
                1_000_000_u32,
                100_000_u32,
                breakdown,
            ))
            .unwrap();
        assert_eq!(err, None);

        let model = Model::from_spec("anthropic/claude-opus-4-8").unwrap();
        assert!(model.supports_fast());
        let expected = TokenUsage {
            input: 400_000,
            output: 100_000,
            cache_creation: 200_000,
            cache_read: 400_000,
        }
        .cost(&model.pricing, true);
        let actual = cost.unwrap();
        assert!(
            (actual - expected).abs() < f64::EPSILON,
            "four-category price mismatch: expected {expected}, got {actual}"
        );
    }

    #[test]
    fn usage_cost_rejects_nonconserving_input_categories() {
        let lua = Lua::new();
        let usage_cost: Function = create_agent_table(&lua).unwrap().get("usage_cost").unwrap();
        let breakdown = lua.create_table().unwrap();
        breakdown.set("fresh_input_tokens", 500_u32).unwrap();
        breakdown.set("cache_read_tokens", 400_u32).unwrap();
        breakdown.set("cache_write_tokens", 200_u32).unwrap();

        let (cost, err): Pair<f64> = usage_cost
            .call(("anthropic/claude-haiku-4-5", 1_000_u32, 100_u32, breakdown))
            .unwrap();

        assert_eq!(cost, None);
        assert!(err.is_some_and(|message| message.contains("do not conserve total")));
    }

    #[test]
    fn usage_cost_returns_pair_error_for_malformed_breakdown() {
        let lua = Lua::new();
        let usage_cost: Function = create_agent_table(&lua).unwrap().get("usage_cost").unwrap();
        let breakdown = lua.create_table().unwrap();
        breakdown.set("cache_read_tokens", "not-a-count").unwrap();

        let (cost, err): Pair<f64> = usage_cost
            .call(("anthropic/claude-haiku-4-5", 1_000_u32, 100_u32, breakdown))
            .expect("malformed telemetry must use the documented pair error");

        assert_eq!(cost, None);
        assert!(err.is_some_and(|message| message.contains("cache_read_tokens")));
    }

    #[test]
    fn session_usage_aggregates_all_categories_and_legacy_total() {
        let mut usage = TokenUsage::default();
        usage += TokenUsage {
            input: 100,
            output: 20,
            cache_creation: 30,
            cache_read: 40,
        };
        usage += TokenUsage {
            input: 10,
            output: 5,
            cache_creation: 3,
            cache_read: 4,
        };

        let lua = Lua::new();
        let table = lua.create_table().unwrap();
        set_usage_fields(&table, usage).unwrap();

        let fresh = table.get::<u32>("fresh_input_tokens").unwrap();
        let cache_read = table.get::<u32>("cache_read_tokens").unwrap();
        let cache_write = table.get::<u32>("cache_write_tokens").unwrap();
        assert_eq!((fresh, cache_read, cache_write), (110, 44, 33));
        assert_eq!(table.get::<u32>("output_tokens").unwrap(), 25);
        assert_eq!(
            table.get::<u32>("input_tokens").unwrap(),
            fresh + cache_read + cache_write
        );
    }

    #[test]
    fn session_usage_defaults_missing_cache_categories_to_zero() {
        let usage = TokenUsage {
            input: 120,
            output: 30,
            ..TokenUsage::default()
        };

        let lua = Lua::new();
        let table = lua.create_table().unwrap();
        set_usage_fields(&table, usage).unwrap();

        assert_eq!(table.get::<u32>("fresh_input_tokens").unwrap(), 120);
        assert_eq!(table.get::<u32>("cache_read_tokens").unwrap(), 0);
        assert_eq!(table.get::<u32>("cache_write_tokens").unwrap(), 0);
        assert_eq!(table.get::<u32>("input_tokens").unwrap(), 120);
        assert_eq!(table.get::<u32>("output_tokens").unwrap(), 30);
    }

    #[test]
    fn prompt_result_exposes_actual_fast_state() {
        let lua = Lua::new();
        let fast = prompt_result_table(
            &lua,
            1,
            TokenUsage::default(),
            0.0,
            true,
            Some("done".to_owned()),
        )
        .unwrap();
        let standard = prompt_result_table(
            &lua,
            1,
            TokenUsage::default(),
            0.0,
            false,
            Some("done".to_owned()),
        )
        .unwrap();

        assert!(fast.get::<bool>("fast").unwrap());
        assert!(!standard.get::<bool>("fast").unwrap());
    }

    #[test]
    fn local_tool_handler_result_conventions() {
        let input = json!({"x": "1"});
        assert_eq!(
            call("function(v) return 'ok:' .. v.x end", &input),
            Ok("ok:1".into())
        );
        assert_eq!(
            call("function() return nil, 'bad' end", &input),
            Err("bad".into())
        );
        assert_eq!(
            call("function() end", &input),
            Err(crate::api::util::convert::NIL_TOOL_RESULT_ERR.into())
        );
        let raised = call("function() error('boom') end", &input).unwrap_err();
        assert!(raised.contains("boom"), "got: {raised}");
        let wrong = call("function() return 42 end", &input).unwrap_err();
        assert!(wrong.contains("expected string"), "got: {wrong}");
    }

    #[test]
    fn prompt_interrupt_source_preserves_session_thinking_and_fast() {
        let (tx, rx) = flume::unbounded();
        let source = PromptInterruptSource {
            rx,
            thinking: ThinkingConfig::Budget(1234),
            fast: true,
        };
        tx.send(SubagentPrompt {
            text: "steer".into(),
            images: Vec::new(),
        })
        .unwrap();

        let Some(ExtractedCommand::Interrupt(input, _)) = source.poll(InterruptPoint::Safe) else {
            panic!("expected an interrupt command");
        };

        assert_eq!(input.message, "steer");
        assert!(input.images.is_empty());
        assert!(matches!(input.thinking, ThinkingConfig::Budget(1234)));
        assert!(input.fast);
    }
}

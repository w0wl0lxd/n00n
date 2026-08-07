use std::collections::VecDeque;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::Path;
use std::sync::Arc;
use std::time::Instant;

use serde_json::Value;
use tracing::{debug, error, warn};

use crate::mcp::{McpSession, TOOL_SEARCH_TOOL_NAME, UNKNOWN_MCP};
use crate::permissions::PermissionCheckContext;
use crate::skill_policy::SKILL_POLICY_DENIED_PREFIX;
use crate::task_set::TaskSet;
use crate::tools::registry::{ToolInvocation, ToolRegistry, ToolSource};
use crate::tools::{LocalToolFn, ToolAdmissionClass, ToolContext};
use crate::{AgentError, AgentEvent, ToolDoneEvent, ToolOutput, ToolStartEvent};
use n00n_config::ToolKey;

const SUBAGENT_PLUGINS: &[&str] = &["task", "workflow"];
const CANCELLED_SUBAGENT_OUTPUTS: &[&str] = &[
    "cancelled",
    "sub-agent error: cancelled",
    "task failed: cancelled",
    "task failed: plugin interrupted: task cancelled",
];
const TOOL_ERROR_LOG_MAX_CHARS: usize = 1024;

#[derive(Clone, Copy)]
pub enum Emit {
    Notify,
    Silent,
}

const DOOM_LOOP_THRESHOLD: usize = 3;
const DOOM_LOOP_MESSAGE: &str = "You have called this tool with identical input 3 times in a row. You are stuck in a loop. Break out and try a different approach.";
const MCP_MUTATION_BLOCKED_IN_PLAN: &str =
    "MCP tool is not explicitly marked read-only and cannot run in plan mode";
const CODE_EXECUTION_BLOCKED_IN_PLAN: &str = "code_execution is not available in plan mode";
const UNKNOWN_TOOL_PREFIX: &str = "unknown tool";
const TOOL_AUDIENCE_DENIED: &str = "tool is not available to this agent audience";
const TOOL_FILTER_DENIED: &str = "tool is not available in this session";
const FUSION_REQUIRED_BRIEF_FIELDS: &[&str] = &["description", "goal", "definition_of_done"];
const FUSION_OPTIONAL_BRIEF_FIELDS: &[&str] = &["constraints", "escalation_triggers"];
const BASH_BLOCKED_IN_PLAN: &str = "bash command is not provably read-only in plan mode";

/// Live Fusion authorization snapshot for one tool-dispatch batch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct FusionDispatchAuth {
    pub phase: crate::fusion::FusionPhase,
    pub lane: crate::fusion::FusionLane,
    pub classification: crate::fusion::DelegationKind,
}

/// Truncates `text` to `TOOL_ERROR_LOG_MAX_CHARS` on a character boundary,
/// preserving the total byte count as a trailing hint.
fn truncate_for_log(text: &str) -> String {
    match text.char_indices().nth(TOOL_ERROR_LOG_MAX_CHARS) {
        Some((idx, _)) => format!("{}... ({} bytes)", &text[..idx], text.len()),
        None => text.to_string(),
    }
}

/// Returns true when `command` contains shell metacharacters that are outside
/// any quote and not escaped by a backslash. These are the characters that let
/// a single command string request additional programs or I/O redirection.
fn has_unquoted_shell_meta(command: &str) -> bool {
    let mut chars = command.chars().peekable();
    let mut in_single = false;
    let mut in_double = false;
    while let Some(c) = chars.next() {
        if c == '\\' && !in_single {
            // Escaped characters are literal. Skip the next char if there is one.
            chars.next();
            continue;
        }
        if in_single {
            if c == '\'' {
                in_single = false;
            }
            continue;
        }
        if in_double {
            if c == '"' {
                in_double = false;
                continue;
            }
            // Command substitution still happens inside double quotes.
            if c == '$' && chars.peek() == Some(&'(') {
                return true;
            }
            if c == '`' {
                return true;
            }
            continue;
        }
        if c == '\'' {
            in_single = true;
            continue;
        }
        if c == '"' {
            in_double = true;
            continue;
        }
        if matches!(
            c,
            '\n' | '\r' | ';' | '|' | '&' | '>' | '<' | '(' | ')' | '{' | '}'
        ) {
            return true;
        }
        if c == '$' && chars.peek() == Some(&'(') {
            return true;
        }
        if c == '`' {
            return true;
        }
    }
    false
}

fn plan_bash_is_read_only(input: &Value) -> bool {
    let Some(command) = input.get("command").and_then(Value::as_str) else {
        return false;
    };
    let command = command.trim();
    if command.is_empty() || command.contains('\n') || command.contains('\r') {
        return false;
    }
    if has_unquoted_shell_meta(command) {
        return false;
    }
    let Ok(words) = shell_words::split(command) else {
        return false;
    };
    let Some(program) = words.first().map(String::as_str) else {
        return false;
    };
    let arguments: Vec<&str> = words.iter().skip(1).map(String::as_str).collect();
    match program {
        "pwd" => arguments.is_empty(),
        "ls" | "cat" | "head" | "tail" | "wc" | "grep" | "stat" | "file" | "du" | "df"
        | "realpath" | "readlink" | "jq" => true,
        "rg" => !arguments
            .iter()
            .any(|argument| argument.starts_with("--pre") || *argument == "--generate"),
        "find" => !arguments.iter().any(|argument| {
            matches!(
                *argument,
                "-delete" | "-exec" | "-execdir" | "-ok" | "-okdir"
            ) || argument.starts_with("-fls")
                || argument.starts_with("-fprint")
                || argument.starts_with("-fprintf")
        }),
        "yq" => !arguments.iter().any(|argument| {
            *argument == "-i" || argument.starts_with("-i=") || argument.starts_with("--inplace")
        }),
        "git" => plan_git_is_read_only(&arguments),
        "gh" => plan_gh_is_read_only(&arguments),
        _ => false,
    }
}

fn plan_git_is_read_only(arguments: &[&str]) -> bool {
    let mut index = 0;
    while let Some(argument) = arguments.get(index) {
        match *argument {
            "-C" => index += 2,
            value if value.starts_with("-C") => index += 1,
            "--git-dir" | "--work-tree" => index += 2,
            value if value.starts_with("--git-dir=") || value.starts_with("--work-tree=") => {
                index += 1;
            }
            // Allow the safe `-c core.fsmonitor=false` override injected by the
            // bash sanitizer. All other `-c` values can introduce aliases,
            // pagers, external diff/hook tools, etc.
            "-c" => {
                if let Some(next) = arguments.get(index + 1)
                    && next.to_lowercase() == "core.fsmonitor=false"
                {
                    index += 2;
                    continue;
                }
                return false;
            }
            value if value.starts_with("-c") => return false,
            value if value.starts_with("--config-env") || value.starts_with("--exec-path") => {
                return false;
            }
            value if value.starts_with('-') => index += 1,
            subcommand => {
                let subcommand_arguments = &arguments[index + 1..];
                if subcommand_arguments.iter().any(|argument| {
                    *argument == "--output"
                        || argument.starts_with("--output=")
                        || *argument == "--ext-diff"
                        || *argument == "--recurse-submodules"
                        || argument.starts_with("--submodule")
                }) {
                    return false;
                }
                return matches!(
                    subcommand,
                    "status"
                        | "diff"
                        | "log"
                        | "show"
                        | "rev-parse"
                        | "ls-files"
                        | "ls-tree"
                        | "describe"
                        | "blame"
                        | "shortlog"
                ) || subcommand == "remote"
                    && matches!(
                        subcommand_arguments,
                        [] | ["-v" | "--verbose"] | ["get-url", ..]
                    );
            }
        }
    }
    false
}

fn plan_gh_is_read_only(arguments: &[&str]) -> bool {
    match arguments {
        ["pr", operation, ..] => {
            matches!(*operation, "list" | "view" | "status" | "checks" | "diff")
        }
        ["issue", operation, ..] => matches!(*operation, "list" | "view" | "status"),
        ["run", operation, ..] => matches!(*operation, "list" | "view" | "watch"),
        ["repo", operation, ..] => matches!(*operation, "list" | "view"),
        _ => false,
    }
}

fn is_authorized_plan_target(plan_path: &Path, target: &Path) -> bool {
    let is_symlink = |path: &Path| {
        std::fs::symlink_metadata(path).is_ok_and(|metadata| metadata.file_type().is_symlink())
    };
    if is_symlink(plan_path) || is_symlink(target) {
        return false;
    }
    let normalize = |path: &Path| -> Option<std::path::PathBuf> {
        let absolute = std::path::absolute(path).ok()?;
        n00n_storage::paths::incremental_canonicalize(&absolute)
            .or(Some(n00n_storage::paths::normalize_path(&absolute)))
    };
    let Some(plan) = normalize(plan_path) else {
        return false;
    };
    let Some(target) = normalize(target) else {
        return false;
    };
    plan == target
}

#[derive(Clone)]
struct PendingToolCall {
    position: usize,
    id: String,
    name: String,
    input: Value,
    fusion_delegate_authorized: bool,
}

fn skill_policy_denied(name: &str, ctx: &ToolContext) -> Option<String> {
    let policy = ctx.active_skill_policy.as_ref()?;
    let decision = policy.evaluate(name);
    if decision.allowed {
        None
    } else {
        Some(decision.reason.unwrap_or_else(|| {
            format!("{SKILL_POLICY_DENIED_PREFIX}: tool {name} is blocked by the active skill")
        }))
    }
}

fn is_skill_tool_call(name: &str) -> bool {
    name.strip_prefix("functions.").map_or(name, |value| value)
        == crate::skill_policy::SKILL_TOOL_NAME
}

fn is_subagent_failure(event: &ToolDoneEvent, ctx: &ToolContext) -> bool {
    if !event.is_error {
        return false;
    }
    // A local override (e.g. a test mock) should not be treated as a built-in
    // subagent just because it shares a name with one.
    if ctx.local_tools.contains_key(event.tool.as_ref()) {
        return false;
    }
    let Some(entry) = ctx.registry.get(event.tool.as_ref()) else {
        return false;
    };
    let is_subagent = matches!(
        entry.source,
        ToolSource::Lua { plugin } if SUBAGENT_PLUGINS.contains(&plugin.as_ref())
    );
    is_subagent
        && (ctx.cancel.is_cancelled()
            || !is_cancelled_subagent_output(event.output.as_text().as_str()))
}

fn is_cancelled_subagent_output(output: &str) -> bool {
    CANCELLED_SUBAGENT_OUTPUTS.contains(&output.trim())
}

pub(super) struct RecentCalls(VecDeque<(String, u64)>);

impl RecentCalls {
    pub(super) fn new() -> Self {
        Self(VecDeque::new())
    }

    fn hash_input(input: &Value) -> u64 {
        let mut h = DefaultHasher::new();
        input.to_string().hash(&mut h);
        h.finish()
    }

    fn is_doom_loop(&self, name: &str, input: &Value) -> bool {
        let hash = Self::hash_input(input);
        self.0.len() >= DOOM_LOOP_THRESHOLD - 1
            && self
                .0
                .iter()
                .rev()
                .take(DOOM_LOOP_THRESHOLD - 1)
                .all(|(n, h)| n == name && *h == hash)
    }

    fn record(&mut self, name: String, input: &Value) {
        self.0.push_back((name, Self::hash_input(input)));
        if self.0.len() > DOOM_LOOP_THRESHOLD {
            self.0.pop_front();
        }
    }
}

/// Parse errors and unknown tools skip the start event so the UI never
/// shows a phantom spinner.
pub async fn run(
    registry: &ToolRegistry,
    mcp: Option<&McpSession>,
    id: String,
    name: &str,
    input: &Value,
    ctx: &ToolContext,
    emit: Emit,
) -> ToolDoneEvent {
    run_authorized(registry, mcp, id, name, input, ctx, emit, false).await
}

#[allow(clippy::too_many_arguments)]
async fn run_authorized(
    registry: &ToolRegistry,
    mcp: Option<&McpSession>,
    id: String,
    name: &str,
    input: &Value,
    ctx: &ToolContext,
    emit: Emit,
    fusion_delegate_authorized: bool,
) -> ToolDoneEvent {
    // GPT-5.6 was likely trained on Codex sessions where tools are `functions.<name>`
    let name = name.strip_prefix("functions.").map_or(name, |value| value);
    if !ctx.tool_filter.matches(name) {
        return tool_done_error(id, Arc::from(name), TOOL_FILTER_DENIED.into());
    }
    if name == crate::fusion::FUSION_DELEGATE_TOOL && !fusion_delegate_authorized {
        return tool_done_error(
            id,
            Arc::from(crate::fusion::FUSION_DELEGATE_TOOL),
            crate::fusion::FUSION_DELEGATE_BLOCKED.into(),
        );
    }
    if ctx.mode.plan_path().is_some() && name == crate::tools::CODE_EXECUTION_TOOL_NAME {
        return tool_done_error(
            id,
            Arc::from(crate::tools::CODE_EXECUTION_TOOL_NAME),
            CODE_EXECUTION_BLOCKED_IN_PLAN.into(),
        );
    }
    if ctx.mode.plan_path().is_some()
        && name == crate::tools::BASH_TOOL_NAME
        && !plan_bash_is_read_only(input)
    {
        return tool_done_error(
            id,
            Arc::from(crate::tools::BASH_TOOL_NAME),
            BASH_BLOCKED_IN_PLAN.into(),
        );
    }
    if let Some(reason) = skill_policy_denied(name, ctx) {
        return tool_done_error(id.clone(), Arc::from(name), reason);
    }
    if let Some(local) = ctx.local_tools.get(name) {
        let class = ToolAdmissionClass::for_tool(name, None);
        let _admission = match ctx
            .registry
            .admission()
            .acquire(&ctx.admission_scope, class, &ctx.cancel)
            .await
        {
            Ok(guard) => guard,
            Err(error) => return tool_done_error(id, Arc::from(name), error.to_string()),
        };
        return run_local_tool(local, id, name, input, ctx, emit);
    }
    let entry = registry.get(name);
    // LLM providers send tool names in wire format (server__tool) but our
    // internal index uses server.tool. Only convert if the name isn't a
    // native tool — avoids mangling native names that happen to contain __.
    let mcp_lookup = if entry.is_none() && name.contains("__") && mcp.is_some() {
        crate::mcp::internal_tool_name(name)
    } else {
        name.to_owned()
    };
    let tool_id: Arc<str> = entry
        .as_ref()
        .map(|e| Arc::from(e.tool.name()))
        .or_else(|| mcp.map(|m| m.interned_name(&mcp_lookup)))
        .unwrap_or_else(|| Arc::from(UNKNOWN_MCP));
    let started = Instant::now();

    if ctx.mode.plan_path().is_some()
        && name != crate::tools::BASH_TOOL_NAME
        && entry
            .as_ref()
            .is_some_and(|entry| entry.tool.tool_kind() == Some("execute"))
    {
        return tool_done_error(
            id.clone(),
            Arc::clone(&tool_id),
            CODE_EXECUTION_BLOCKED_IN_PLAN.into(),
        );
    }

    if entry
        .as_ref()
        .is_some_and(|entry| !entry.tool.audience().contains(ctx.audience))
    {
        return tool_done_error(
            id.clone(),
            Arc::clone(&tool_id),
            TOOL_AUDIENCE_DENIED.into(),
        );
    }

    if let Some(entry) = entry {
        let invocation = match entry.tool.parse(input) {
            Ok(inv) => inv,
            Err(e) => {
                warn!(
                    tool = %name,
                    source = %entry.source.as_log_field(),
                    input_preview = %crate::tools::schema::preview(&input.to_string()),
                    error = %e,
                    "tool input parse failed"
                );
                return tool_done_error(id.clone(), Arc::clone(&tool_id), e.to_string());
            }
        };

        if let Some(target) = invocation.mutable_path() {
            let is_plan_target = ctx
                .mode
                .plan_path()
                .is_some_and(|plan_path| is_authorized_plan_target(plan_path, target));
            if !is_plan_target {
                if ctx.mode.plan_path().is_some() {
                    warn!(
                        tool = %name,
                        target = %target.display(),
                        "blocked write in plan mode"
                    );
                    return tool_done_error(
                        id.clone(),
                        Arc::clone(&tool_id),
                        crate::tools::PLAN_WRITE_RESTRICTED.into(),
                    );
                }
                if let Some(reason) = ctx.permissions.boundary_block_reason(target) {
                    return tool_done_error(id.clone(), Arc::clone(&tool_id), reason);
                }
            }
        }

        let header_result = invocation.start_header().await;
        let start = ToolStartEvent {
            id: id.clone(),
            tool: Arc::clone(&tool_id),
            summary: header_result.text(),
            render_header: header_result.snapshot(),
            annotation: invocation.start_annotation(),
            input: None,
            raw_input: Some(input.clone()),
            output: invocation.start_output(ctx),
        };
        if matches!(emit, Emit::Notify) {
            let _ = ctx.event_tx.send(AgentEvent::ToolStart(Box::new(start)));
        }

        invocation.start(ctx).await;

        if let Err(e) = enforce_permission(invocation.as_ref(), name, ctx, &id).await {
            return tool_done_error(id.clone(), Arc::clone(&tool_id), e);
        }

        let _admission = match ctx
            .registry
            .admission()
            .acquire(
                &ctx.admission_scope,
                entry.tool.admission_class(),
                &ctx.cancel,
            )
            .await
        {
            Ok(guard) => guard,
            Err(error) => return tool_done_error(id, Arc::clone(&tool_id), error.to_string()),
        };
        let result = invocation.execute(ctx).await;

        let elapsed = started.elapsed();
        match result.output {
            Ok(output) => {
                debug!(
                    tool = %name,
                    source = %entry.source.as_log_field(),
                    elapsed_ms = u64::try_from(elapsed.as_millis()).unwrap_or_else(|_| u64::MAX),
                    "tool ok"
                );
                let output = match result.telemetry {
                    Some(telemetry) => output.with_telemetry(Some(telemetry)),
                    None => output,
                }
                .bounded(ctx.config.max_output_lines, ctx.config.max_output_bytes);
                ToolDoneEvent {
                    id,
                    tool: tool_id,
                    output,
                    is_error: false,
                    annotation: result.annotation,
                    written_path: result.written_path,
                }
            }
            Err(message) => {
                let error_preview = truncate_for_log(&message);
                warn!(
                    tool = %name,
                    source = %entry.source.as_log_field(),
                    elapsed_ms = u64::try_from(elapsed.as_millis()).unwrap_or_else(|_| u64::MAX),
                    error = %error_preview,
                    error_bytes = message.len(),
                    "tool failed"
                );
                ToolDoneEvent {
                    id,
                    tool: tool_id,
                    output: ToolOutput::Plain(crate::TextOutput {
                        text: crate::tools::truncate_output(
                            &message,
                            ctx.config.max_output_lines,
                            ctx.config.max_output_bytes,
                        ),
                        instructions: None,
                        state: None,
                        telemetry: result.telemetry,
                    }),
                    is_error: true,
                    annotation: result.annotation,
                    written_path: None,
                }
            }
        }
    } else if let Some(mcp) = mcp.filter(|_| name == TOOL_SEARCH_TOOL_NAME) {
        let _admission = match ctx
            .registry
            .admission()
            .acquire(&ctx.admission_scope, ToolAdmissionClass::Cheap, &ctx.cancel)
            .await
        {
            Ok(guard) => guard,
            Err(error) => return tool_done_error(id, tool_id, error.to_string()),
        };
        run_tool_search(mcp, id, input, ctx, emit)
    } else if mcp.is_some_and(|m| m.has_tool(&mcp_lookup)) {
        let _admission = match ctx
            .registry
            .admission()
            .acquire(
                &ctx.admission_scope,
                ToolAdmissionClass::Standard,
                &ctx.cancel,
            )
            .await
        {
            Ok(guard) => guard,
            Err(error) => return tool_done_error(id, tool_id, error.to_string()),
        };
        execute_mcp_tool(ctx, &id, tool_id, &mcp_lookup, input, emit).await
    } else {
        let msg = format!("{UNKNOWN_TOOL_PREFIX}: {mcp_lookup}");
        warn!(tool = %mcp_lookup, "unknown tool");
        tool_done_error(id, tool_id, msg)
    }
}

/// MCP, local, and search tools never go through invocation parsing,
/// so there is no parsed input to show; the UI gets the raw JSON instead.
fn emit_raw_start(
    ctx: &ToolContext,
    emit: Emit,
    id: &str,
    tool: &Arc<str>,
    summary: String,
    input: &Value,
) {
    if !matches!(emit, Emit::Notify) {
        return;
    }
    let start = ToolStartEvent {
        id: id.to_owned(),
        tool: Arc::clone(tool),
        summary,
        render_header: None,
        annotation: None,
        input: None,
        raw_input: Some(input.clone()),
        output: None,
    };
    let _ = ctx.event_tx.send(AgentEvent::ToolStart(Box::new(start)));
}

/// Runs without a permission gate: search only reveals names the deferred
/// catalog already showed the model.
fn run_tool_search(
    mcp: &McpSession,
    id: String,
    input: &Value,
    ctx: &ToolContext,
    emit: Emit,
) -> ToolDoneEvent {
    let tool_id: Arc<str> = Arc::from(TOOL_SEARCH_TOOL_NAME);
    let query = input["query"].as_str().unwrap_or_else(Default::default);
    emit_raw_start(ctx, emit, &id, &tool_id, query.to_owned(), input);
    let (output, is_error) = match mcp.search_tools(query) {
        Ok(out) => (
            crate::tools::truncate_output(
                &out,
                ctx.config.max_output_lines,
                ctx.config.max_output_bytes,
            ),
            false,
        ),
        Err(e) => (
            crate::tools::truncate_output(
                &e,
                ctx.config.max_output_lines,
                ctx.config.max_output_bytes,
            ),
            true,
        ),
    };
    ToolDoneEvent {
        id,
        tool: tool_id,
        output: ToolOutput::Markdown(output.into()),
        is_error,
        annotation: None,
        written_path: None,
    }
}

fn run_local_tool(
    local: &LocalToolFn,
    id: String,
    name: &str,
    input: &Value,
    ctx: &ToolContext,
    emit: Emit,
) -> ToolDoneEvent {
    let tool_id: Arc<str> = Arc::from(name);
    emit_raw_start(ctx, emit, &id, &tool_id, name.to_owned(), input);
    let (output, is_error) = match local(input) {
        Ok(output) => (
            crate::tools::truncate_output(
                &output,
                ctx.config.max_output_lines,
                ctx.config.max_output_bytes,
            ),
            false,
        ),
        Err(e) => {
            warn!(tool = %name, error = %e, "local tool failed");
            (
                crate::tools::truncate_output(
                    &e,
                    ctx.config.max_output_lines,
                    ctx.config.max_output_bytes,
                ),
                true,
            )
        }
    };
    ToolDoneEvent {
        id,
        tool: tool_id,
        output: ToolOutput::Plain(output.into()),
        is_error,
        annotation: None,
        written_path: None,
    }
}

fn tool_done_error(id: String, tool_id: Arc<str>, message: String) -> ToolDoneEvent {
    ToolDoneEvent {
        id,
        tool: tool_id,
        output: ToolOutput::Plain(message.into()),
        is_error: true,
        annotation: None,
        written_path: None,
    }
}

fn tool_done_plain(id: String, tool_id: Arc<str>, text: String) -> ToolDoneEvent {
    ToolDoneEvent {
        id,
        tool: tool_id,
        output: ToolOutput::Plain(text.into()),
        is_error: false,
        annotation: None,
        written_path: None,
    }
}

/// Enforce permission for a native tool. MCP tools bypass this — they go
/// through `execute_mcp_tool` which handles permission checking internally.
///
/// Returns an error if `name` contains dots (not a valid native tool name).
async fn enforce_permission(
    inv: &dyn ToolInvocation,
    name: &str,
    ctx: &ToolContext,
    id: &str,
) -> Result<(), String> {
    if name.contains('.') {
        return Err(format!(
            "enforce_permission called with dotted name: {name}"
        ));
    }
    if let Some(scopes) = inv.permission_scopes().await {
        let tool_key = ToolKey::native(name);
        ctx.permissions
            .enforce(PermissionCheckContext {
                tool: &tool_key,
                scopes: &scopes,
                event_tx: &ctx.event_tx,
                user_response_rx: ctx.user_response_rx.as_deref(),
                request_id: id,
                cancel: &ctx.cancel,
                plan_path: ctx.mode.plan_path(),
            })
            .await
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}

async fn execute_mcp_tool(
    ctx: &ToolContext,
    id: &str,
    tool_id: Arc<str>,
    tool_name: &str,
    input: &Value,
    emit: Emit,
) -> ToolDoneEvent {
    if matches!(emit, Emit::Notify) {
        let start = ToolStartEvent {
            id: id.to_owned(),
            tool: Arc::clone(&tool_id),
            summary: format!("mcp: {tool_name}"),
            render_header: None,
            annotation: None,
            input: None,
            raw_input: Some(input.clone()),
            output: None,
        };
        let _ = ctx.event_tx.send(AgentEvent::ToolStart(Box::new(start)));
    }

    let in_plan_mode = ctx.mode.plan_path().is_some();
    let plan_read_only = in_plan_mode
        && ctx
            .mcp
            .as_ref()
            .is_some_and(|mcp| mcp.is_tool_read_only(tool_name));
    if in_plan_mode && !plan_read_only {
        return tool_done_error(
            id.to_owned(),
            Arc::clone(&tool_id),
            MCP_MUTATION_BLOCKED_IN_PLAN.into(),
        );
    }

    // Plan-mode read-only classification only bypasses the mutation gate above.
    // Configured MCP allow/deny rules still apply.
    let perm_tool = match ToolKey::parse(tool_name) {
        Ok(k) => k,
        Err(e) => {
            return tool_done_error(
                id.to_owned(),
                Arc::clone(&tool_id),
                format!("invalid MCP tool key '{tool_name}': {e}"),
            );
        }
    };
    let perm_scope = {
        let json = input.to_string();
        if json.len() > 200 {
            format!("{}\u{2026}", &json[..200])
        } else {
            json
        }
    };
    let perm_scopes = crate::tools::PermissionScopes::single(perm_scope);

    if let Err(e) = ctx
        .permissions
        .enforce(PermissionCheckContext {
            tool: &perm_tool,
            scopes: &perm_scopes,
            event_tx: &ctx.event_tx,
            user_response_rx: ctx.user_response_rx.as_deref(),
            request_id: id,
            cancel: &ctx.cancel,
            plan_path: ctx.mode.plan_path(),
        })
        .await
    {
        return tool_done_error(id.to_owned(), Arc::clone(&tool_id), e.to_string());
    }

    let Some(mcp) = &ctx.mcp else {
        return tool_done_error(
            id.to_owned(),
            Arc::clone(&tool_id),
            format!("MCP manager not available for {tool_name}"),
        );
    };

    // A permitted call to a deferred tool counts as loading it, so its full
    // definition joins the next request; a denied call must not load anything.
    mcp.mark_loaded(tool_name);
    match mcp.call_tool(tool_name, input).await {
        Ok(text) => tool_done_plain(
            id.to_owned(),
            tool_id,
            crate::tools::truncate_output(
                &text,
                ctx.config.max_output_lines,
                ctx.config.max_output_bytes,
            ),
        ),
        Err(e) => tool_done_error(
            id.to_owned(),
            tool_id,
            crate::tools::truncate_output(
                &e.to_string(),
                ctx.config.max_output_lines,
                ctx.config.max_output_bytes,
            ),
        ),
    }
}

/// Deduplicates doom-loop repeats, then runs remaining calls in parallel.
fn fusion_brief_is_authorized(input: &Value) -> bool {
    let Some(brief) = input.as_object() else {
        return false;
    };
    let required_allowed = FUSION_REQUIRED_BRIEF_FIELDS.iter().all(|field| {
        brief
            .get(*field)
            .and_then(Value::as_str)
            .is_some_and(|text| !text.trim().is_empty())
    });
    if !required_allowed {
        return false;
    }
    let text = FUSION_REQUIRED_BRIEF_FIELDS
        .iter()
        .chain(FUSION_OPTIONAL_BRIEF_FIELDS)
        .filter_map(|field| brief.get(*field).and_then(Value::as_str))
        .collect::<Vec<_>>()
        .join(" ");
    !crate::fusion::contains_lead_only_signal(&text)
        && crate::fusion::classify_delegation(&text) == crate::fusion::DelegationKind::Delegate
}

pub(super) async fn process_tool_calls(
    response: n00n_providers::StreamResponse,
    recent_calls: &mut RecentCalls,
    mcp: Option<&McpSession>,
    history: &mut super::history::History,
    event_tx: &crate::EventSender,
    ctx: &ToolContext,
    fusion: Option<FusionDispatchAuth>,
) -> Result<Vec<ToolDoneEvent>, AgentError> {
    let tool_uses: Vec<(usize, String, String, Value)> = response
        .message
        .tool_uses()
        .enumerate()
        .map(|(position, (id, name, input))| {
            (position, id.to_owned(), name.to_owned(), input.clone())
        })
        .collect();

    history.push(response.message);

    let mut immediate_errors: Vec<(usize, ToolDoneEvent)> = Vec::new();
    let mut skill_calls: Vec<PendingToolCall> = Vec::new();
    let mut non_skill_calls: Vec<PendingToolCall> = Vec::new();
    let mut fusion_guard = fusion.map(|auth| {
        crate::fusion::FusionDispatchGuard::new(
            ctx.config.fusion.enabled,
            auth.classification,
            ctx.audience,
        )
    });
    let fusion_lifecycle_ok = fusion.is_some_and(|auth| {
        auth.phase == crate::fusion::FusionPhase::Planning
            && auth.lane == crate::fusion::FusionLane::Lead
    });

    for (position, id, name, mut input) in tool_uses {
        debug!(
            tool = %name,
            id = %id,
            input_preview = %crate::tools::schema::preview(&input.to_string()),
            "parsing tool call"
        );
        let normalized_name = name
            .strip_prefix("functions.")
            .map_or(name.as_str(), |value| value);
        let is_fusion_delegate = normalized_name == crate::fusion::FUSION_DELEGATE_TOOL;
        if is_fusion_delegate
            && ctx.config.fusion.enabled
            && let Value::Object(arguments) = &mut input
            && !arguments.contains_key("model_tier")
        {
            let tier = match ctx.config.fusion.sidekick_tier {
                n00n_config::providers::Tier::Weak | n00n_config::providers::Tier::Compaction => {
                    "weak"
                }
                n00n_config::providers::Tier::Medium => "medium",
                n00n_config::providers::Tier::Strong => "strong",
            };
            arguments.insert("model_tier".into(), Value::String(tier.into()));
        }
        let fusion_brief_authorized = is_fusion_delegate && fusion_brief_is_authorized(&input);
        let fusion_delegate_authorized = if is_fusion_delegate {
            fusion_lifecycle_ok
                && fusion_brief_authorized
                && fusion_guard.as_mut().is_some_and(|guard| {
                    guard
                        .authorize(crate::fusion::FusionInvocationOrigin::Direct)
                        .is_ok()
                })
        } else {
            false
        };
        if is_fusion_delegate && !fusion_delegate_authorized {
            immediate_errors.push((
                position,
                tool_done_error(
                    id.clone(),
                    Arc::from(crate::fusion::FUSION_DELEGATE_TOOL),
                    crate::fusion::FUSION_DELEGATE_BLOCKED.into(),
                ),
            ));
        } else if recent_calls.is_doom_loop(&name, &input) {
            warn!(tool = %name, "doom loop detected, skipping execution");
            immediate_errors.push((
                position,
                ToolDoneEvent::error(id.clone(), DOOM_LOOP_MESSAGE),
            ));
        } else {
            let call = PendingToolCall {
                position,
                id,
                name: name.clone(),
                input: input.clone(),
                fusion_delegate_authorized,
            };
            if is_skill_tool_call(&name) {
                skill_calls.push(call);
            } else {
                non_skill_calls.push(call);
            }
        }
        recent_calls.record(name, &input);
    }

    for (_, err) in &immediate_errors {
        event_tx.try_send(AgentEvent::ToolDone(Box::new(err.clone())));
    }

    let mut runnable_results: Vec<(usize, ToolDoneEvent)> = Vec::new();
    let mut active_skill_policy = ctx.active_skill_policy.clone();
    let mcp_owned = mcp.cloned();
    for call in skill_calls {
        let event_tx_clone = ctx.event_tx.clone();
        let tool_ctx = ToolContext {
            tool_use_id: Some(call.id.clone()),
            active_skill_policy: active_skill_policy.clone(),
            ..ctx.clone()
        };
        let done = run_authorized(
            &tool_ctx.registry,
            mcp_owned.as_ref(),
            call.id,
            &call.name,
            &call.input,
            &tool_ctx,
            Emit::Notify,
            call.fusion_delegate_authorized,
        )
        .await;
        crate::skill_policy::ActiveSkillPolicy::apply_from_skill_tool_result(
            &mut active_skill_policy,
            &done.tool,
            done.is_error,
            done.output.state(),
        );
        event_tx_clone.try_send(AgentEvent::ToolDone(Box::new(done.clone())));
        runnable_results.push((call.position, done));
    }

    let mut set = TaskSet::new();
    let mut spawned_meta: Vec<(usize, String)> = Vec::new();
    for call in non_skill_calls {
        spawned_meta.push((call.position, call.id.clone()));
        let event_tx_clone = ctx.event_tx.clone();
        let tool_ctx = ToolContext {
            tool_use_id: Some(call.id.clone()),
            active_skill_policy: active_skill_policy.clone(),
            ..ctx.clone()
        };
        let mcp_owned = mcp.cloned();
        set.spawn(async move {
            let done = run_authorized(
                &tool_ctx.registry,
                mcp_owned.as_ref(),
                call.id,
                &call.name,
                &call.input,
                &tool_ctx,
                Emit::Notify,
                call.fusion_delegate_authorized,
            )
            .await;
            event_tx_clone.try_send(AgentEvent::ToolDone(Box::new(done.clone())));
            done
        });
    }

    let parallel_results: Vec<(usize, ToolDoneEvent)> = set
        .join_all()
        .await
        .into_iter()
        .zip(spawned_meta)
        .map(|(r, (position, id))| match r {
            Ok(out) => (position, out),
            Err(e) => {
                error!(error = %e, "tool task panicked");
                (
                    position,
                    ToolDoneEvent::error(id, format!("internal error: tool panicked: {e}")),
                )
            }
        })
        .collect();

    let mut all_with_pos: Vec<(usize, ToolDoneEvent)> = runnable_results;
    all_with_pos.extend(parallel_results);
    all_with_pos.extend(immediate_errors);
    all_with_pos.sort_by_key(|(position, _)| *position);
    let all_results: Vec<ToolDoneEvent> =
        all_with_pos.into_iter().map(|(_, result)| result).collect();

    let tool_msg = crate::types::tool_results(all_results.clone());
    event_tx.send(AgentEvent::ToolResultsSubmitted {
        message: Box::new(tool_msg.clone()),
    })?;
    history.push(tool_msg);

    if let Some(failed) = all_results.iter().find(|r| is_subagent_failure(r, ctx)) {
        return Err(AgentError::Tool {
            tool: failed.tool.to_string(),
            message: failed.output.as_text(),
        });
    }

    Ok(all_results)
}

/// Test-only entry that skips native lookup, letting plan-mode and MCP tests
/// exercise the dispatch path without registering a fake native tool.
#[cfg(test)]
async fn dispatch_mcp(
    ctx: &ToolContext,
    id: &str,
    tool_name: &str,
    input: &Value,
) -> ToolDoneEvent {
    let tool_id = ctx
        .mcp
        .as_ref()
        .map_or_else(|| Arc::from(UNKNOWN_MCP), |m| m.interned_name(tool_name));
    execute_mcp_tool(ctx, id, tool_id, tool_name, input, Emit::Silent).await
}

#[cfg(test)]
mod tests {
    use std::borrow::Cow;
    use std::path::PathBuf;
    use std::sync::Arc;

    use n00n_config::{Effect, PermissionRule, PermissionsConfig, ToolKey};
    use n00n_providers::{ContentBlock, Message, StopReason, StreamResponse, TokenUsage};
    use tempfile::TempDir;
    use test_case::test_case;

    use super::*;
    use crate::AgentMode;
    use crate::permissions::{PERMISSION_DENIED_PREFIX, PermissionManager};
    use crate::tools::registry::ToolSource;
    use crate::tools::test_support::{GUARDED_TOOL_NAME, GuardedMock};

    fn recent_calls(entries: &[(&str, Value)]) -> RecentCalls {
        let mut rc = RecentCalls::new();
        for (n, v) in entries {
            rc.record(n.to_string(), v);
        }
        rc
    }

    #[test_case("read", &[("read", "/a"), ("read", "/a")], true  ; "triggers_at_threshold")]
    #[test_case("read", &[("read", "/a")],                 false ; "below_threshold")]
    #[test_case("read", &[("read", "/a"), ("read", "/b")], false ; "different_input_breaks_chain")]
    #[test_case("grep", &[("glob", "/a"), ("glob", "/a")], false ; "different_tool_name")]
    #[test_case("bash", &[("bash", "/a"), ("bash", "/b"), ("bash", "/a")], false ; "interrupted_chain")]
    fn doom_loop_detection(name: &str, history: &[(&str, &str)], expected: bool) {
        let entries: Vec<_> = history
            .iter()
            .map(|(n, p)| (*n, serde_json::json!({"path": p})))
            .collect();
        let input = serde_json::json!({"path": "/a"});
        assert_eq!(recent_calls(&entries).is_doom_loop(name, &input), expected);
    }

    fn local_ctx(
        name: &str,
        f: impl Fn(&Value) -> Result<String, String> + Send + Sync + 'static,
    ) -> ToolContext {
        let mut ctx = crate::tools::test_support::stub_ctx(&Arc::new(AgentMode::Build));
        let mut map = std::collections::HashMap::new();
        map.insert(name.to_owned(), Arc::new(f) as LocalToolFn);
        ctx.local_tools = Arc::new(map);
        ctx
    }

    fn response_with_tool_uses(calls: &[(&str, &str, Value)]) -> StreamResponse {
        StreamResponse {
            message: Message {
                role: n00n_providers::Role::Assistant,
                content: calls
                    .iter()
                    .map(|(id, name, input)| ContentBlock::ToolUse {
                        id: (*id).to_owned(),
                        name: (*name).to_owned(),
                        input: input.clone(),
                    })
                    .collect(),
                ..Default::default()
            },
            usage: TokenUsage::default(),
            stop_reason: Some(StopReason::ToolUse),
        }
    }

    #[test]
    fn local_tool_shadows_registry_and_maps_errors() {
        smol::block_on(async {
            let ctx = local_ctx("batch", |input| Ok(format!("local:{}", input["path"])));
            let done = run(
                ToolRegistry::global(),
                None,
                "t1".into(),
                "batch",
                &serde_json::json!({"path": "/a"}),
                &ctx,
                Emit::Silent,
            )
            .await;
            assert!(!done.is_error);
            assert_eq!(done.output.as_text(), r#"local:"/a""#);

            let ctx = local_ctx("boom", |_| Err("nope".into()));
            let done = run(
                ToolRegistry::global(),
                None,
                "t2".into(),
                "boom",
                &serde_json::json!({}),
                &ctx,
                Emit::Silent,
            )
            .await;
            assert!(done.is_error);
            assert_eq!(done.output.as_text(), "nope");
        });
    }

    #[test]
    fn local_tool_notify_emits_tool_start_with_raw_input() {
        smol::block_on(async {
            let (tx, rx) = flume::unbounded::<crate::Envelope>();
            let event_tx = crate::EventSender::new(tx, 0);
            let mut ctx = crate::tools::test_support::stub_ctx_with(
                &Arc::new(AgentMode::Build),
                Some(&event_tx),
                None,
            );
            let mut map = std::collections::HashMap::new();
            map.insert(
                "local_echo".to_owned(),
                Arc::new(|input: &Value| Ok(input.to_string())) as LocalToolFn,
            );
            ctx.local_tools = Arc::new(map);

            let input = serde_json::json!({"path": "/a"});
            let done = run(
                ToolRegistry::global(),
                None,
                "t1".into(),
                "local_echo",
                &input,
                &ctx,
                Emit::Notify,
            )
            .await;
            assert!(!done.is_error);

            let envelope = rx
                .try_recv()
                .expect("ToolStart must be emitted before the tool completes");
            let AgentEvent::ToolStart(start) = envelope.event else {
                panic!("expected ToolStart, got {:?}", envelope.event);
            };
            assert_eq!(start.tool.as_ref(), "local_echo");
            assert_eq!(start.summary, "local_echo");
            assert_eq!(start.raw_input, Some(input));
        });
    }

    #[test]
    fn tool_search_routes_and_loads_matches() {
        smol::block_on(async {
            let mcp = crate::mcp::stub_session(&[("srv.fetch_issue", "Fetch a GitHub issue")]);
            let ctx = crate::tools::test_support::stub_ctx(&Arc::new(AgentMode::Build));
            let done = run(
                ToolRegistry::global(),
                Some(&mcp),
                "t1".into(),
                TOOL_SEARCH_TOOL_NAME,
                &serde_json::json!({"query": "issue"}),
                &ctx,
                Emit::Silent,
            )
            .await;
            assert!(!done.is_error, "got: {}", done.output.as_text());
            assert_eq!(done.tool.as_ref(), TOOL_SEARCH_TOOL_NAME);
            assert!(done.output.as_text().contains("srv__fetch_issue"));

            let mut tools = serde_json::json!([]);
            mcp.extend_tools(&mut tools);
            assert!(
                crate::mcp::tool_names(&tools).contains(&"srv__fetch_issue"),
                "searched tool must join the next request"
            );
        });
    }

    #[test_case(serde_json::json!({"query": "  "}) ; "blank_query")]
    #[test_case(serde_json::json!({}) ; "missing_query")]
    #[allow(clippy::needless_pass_by_value)]
    fn tool_search_bad_query_is_error_event(input: Value) {
        smol::block_on(async {
            let mcp = crate::mcp::stub_session(&[("srv.tool", "")]);
            let ctx = crate::tools::test_support::stub_ctx(&Arc::new(AgentMode::Build));
            let done = run(
                ToolRegistry::global(),
                Some(&mcp),
                "t1".into(),
                TOOL_SEARCH_TOOL_NAME,
                &input,
                &ctx,
                Emit::Silent,
            )
            .await;
            assert!(done.is_error);
            assert_eq!(done.output.as_text(), crate::mcp::SEARCH_EMPTY_QUERY);
        });
    }

    #[test]
    fn calling_deferred_mcp_tool_marks_it_loaded() {
        smol::block_on(async {
            let mcp = crate::mcp::stub_session(&[("srv.fetch_issue", "")]);
            let mut ctx = crate::tools::test_support::stub_ctx(&Arc::new(AgentMode::Build));
            ctx.mcp = Some(mcp.clone());
            let done = run(
                ToolRegistry::global(),
                Some(&mcp),
                "t1".into(),
                "srv__fetch_issue",
                &serde_json::json!({}),
                &ctx,
                Emit::Silent,
            )
            .await;
            assert_eq!(done.tool.as_ref(), "srv.fetch_issue", "must route to MCP");

            let mut tools = serde_json::json!([]);
            mcp.extend_tools(&mut tools);
            assert_eq!(
                crate::mcp::tool_names(&tools),
                vec!["srv__fetch_issue"],
                "called tool must join the next request"
            );
        });
    }

    #[test]
    fn denied_mcp_call_does_not_load_definition() {
        smol::block_on(async {
            let mcp = crate::mcp::stub_session(&[("srv.fetch_issue", "")]);
            let deny_cfg = PermissionsConfig {
                rules: vec![PermissionRule {
                    tool: ToolKey::parse("srv.fetch_issue").unwrap(),
                    scope: None,
                    effect: Effect::Deny,
                }],
                ..Default::default()
            };
            let dir = TempDir::new().unwrap();
            let permissions = Arc::new(PermissionManager::new(deny_cfg, dir.path().to_path_buf()));
            let mut ctx = crate::tools::test_support::stub_ctx_with_permissions(
                &Arc::new(AgentMode::Build),
                permissions,
            );
            ctx.mcp = Some(mcp.clone());
            let done = run(
                ToolRegistry::global(),
                Some(&mcp),
                "t1".into(),
                "srv__fetch_issue",
                &serde_json::json!({}),
                &ctx,
                Emit::Silent,
            )
            .await;
            assert!(done.is_error);
            assert!(
                done.output.as_text().starts_with(PERMISSION_DENIED_PREFIX),
                "got: {}",
                done.output.as_text()
            );

            let mut tools = serde_json::json!([]);
            mcp.extend_tools(&mut tools);
            assert_eq!(
                crate::mcp::tool_names(&tools),
                vec![TOOL_SEARCH_TOOL_NAME],
                "denied call must not load the definition"
            );
        });
    }

    #[test]
    fn local_tool_named_tool_search_shadows_mcp_search() {
        smol::block_on(async {
            let mcp = crate::mcp::stub_session(&[("srv.tool", "")]);
            let ctx = local_ctx(TOOL_SEARCH_TOOL_NAME, |_| Ok("local wins".into()));
            let done = run(
                ToolRegistry::global(),
                Some(&mcp),
                "t1".into(),
                TOOL_SEARCH_TOOL_NAME,
                &serde_json::json!({"query": "tool"}),
                &ctx,
                Emit::Silent,
            )
            .await;
            assert_eq!(done.output.as_text(), "local wins");
        });
    }

    #[test]
    fn unknown_tool_returns_error_event() {
        smol::block_on(async {
            let ctx = crate::tools::test_support::stub_ctx(&Arc::new(AgentMode::Build));
            let done = run(
                &ctx.registry,
                None,
                "t1".into(),
                "nonexistent.tool",
                &serde_json::json!({}),
                &ctx,
                Emit::Silent,
            )
            .await;
            assert!(done.is_error);
            assert_eq!(done.tool.as_ref(), UNKNOWN_MCP);
            let text = done.output.as_text();
            assert!(text.starts_with(UNKNOWN_TOOL_PREFIX));
            assert!(text.contains("nonexistent.tool"));
        });
    }

    #[test]
    fn local_tool_progress_keywords_returned_unchanged() {
        smol::block_on(async {
            const PROGRESS_TEXT: &str = "Building module... 100% complete ==> done";
            let ctx = local_ctx("progress", |_| Ok(PROGRESS_TEXT.to_string()));
            let done = run(
                ToolRegistry::global(),
                None,
                "t1".into(),
                "progress",
                &serde_json::json!({}),
                &ctx,
                Emit::Silent,
            )
            .await;
            assert!(!done.is_error);
            assert_eq!(done.output.as_text(), PROGRESS_TEXT);
        });
    }

    #[test]
    fn local_tool_error_with_progress_keyword_preserves_message() {
        smol::block_on(async {
            const ERROR_MSG: &str = "100% failed";
            let ctx = local_ctx("fail", |_| Err(ERROR_MSG.to_string()));
            let done = run(
                ToolRegistry::global(),
                None,
                "t1".into(),
                "fail",
                &serde_json::json!({}),
                &ctx,
                Emit::Silent,
            )
            .await;
            assert!(done.is_error);
            assert_eq!(done.output.as_text(), ERROR_MSG);
        });
    }

    #[test]
    fn plan_target_requires_same_normalized_path() {
        let dir = TempDir::new().unwrap();
        let plan = dir.path().join("plan.md");
        let equivalent = dir.path().join(".").join("plan.md");

        assert!(is_authorized_plan_target(&plan, &equivalent));
        assert!(!is_authorized_plan_target(
            &plan,
            &dir.path().join("other.md")
        ));
    }

    #[cfg(unix)]
    #[test]
    fn plan_target_rejects_symlink() {
        use std::os::unix::fs::symlink;

        let dir = TempDir::new().unwrap();
        let actual = dir.path().join("actual.md");
        std::fs::write(&actual, "plan").unwrap();
        let linked = dir.path().join("plan.md");
        symlink(&actual, &linked).unwrap();

        assert!(!is_authorized_plan_target(&linked, &linked));
    }

    #[test_case("git status", true ; "git_status")]
    #[test_case("git -C repo diff --stat", true ; "git_with_directory")]
    #[test_case("gh pr checks 42", true ; "github_checks")]
    #[test_case("rg pattern src", true ; "ripgrep")]
    #[test_case("find . -name Cargo.toml", true ; "find_read")]
    #[test_case("find . -name '*.rs'", true ; "find_quoted_glob")]
    #[test_case("grep 'a|b' file", true ; "grep_quoted_pipe")]
    #[test_case("grep '$(rm)' file", true ; "grep_quoted_dollar_paren")]
    #[test_case("git remote -v", true ; "git_remote_list")]
    #[test_case("git commit -am nope", false ; "git_commit")]
    #[test_case("git branch -D old", false ; "git_branch_delete")]
    #[test_case("git diff --output=patch", false ; "git_output_file")]
    #[test_case("gh pr merge 42", false ; "github_merge")]
    #[test_case("find . -delete", false ; "find_delete")]
    #[test_case("find . '-delete'", false ; "find_quoted_delete")]
    #[test_case("find . -fprint0 output", false ; "find_print_file")]
    #[test_case("yq -i '.x = 1' file.yml", false ; "yq_in_place")]
    #[test_case("yq '-i' expression file.yml", false ; "yq_quoted_in_place")]
    #[test_case("tree -o output", false ; "tree_output")]
    #[test_case("cargo test", false ; "code_execution")]
    #[test_case("cat file > copy", false ; "redirect")]
    #[test_case("git status && rm file", false ; "command_chain")]
    #[test_case("cat $(ls)", false ; "command_substitution")]
    #[test_case("cat `ls`", false ; "backtick_substitution")]
    #[test_case("cat <(ls)", false ; "process_substitution")]
    #[test_case("grep \"$(rm)\" file", false ; "double_quoted_command_substitution")]
    #[test_case("git -c core.pager=cat log", false ; "git_dash_c_rejected")]
    #[test_case("git --config-env=FOO=BAR log", false ; "git_config_env_rejected")]
    #[test_case("git --exec-path=/mal log", false ; "git_exec_path_rejected")]
    #[test_case("git diff --ext-diff", false ; "git_ext_diff_rejected")]
    #[test_case("git diff --recurse-submodules", false ; "git_recurse_submodules_rejected")]
    #[test_case("git diff --submodule=log", false ; "git_submodule_rejected")]
    #[test_case("git -c core.fsmonitor=false status", true ; "git_fsmonitor_false_allowed")]
    #[test_case("git -c core.fsmonitor=true status", false ; "git_fsmonitor_true_rejected")]
    #[test_case("git -c CORE.FSMONITOR=FALSE -C repo diff --stat", true ; "git_fsmonitor_false_case_insensitive")]
    #[test_case("python -c 'print(1)'", false ; "interpreter")]
    fn classifies_plan_bash_commands(command: &str, expected: bool) {
        assert_eq!(
            plan_bash_is_read_only(&serde_json::json!({"command": command})),
            expected
        );
    }

    #[test]
    fn mutating_bash_blocked_in_plan_mode_before_lookup() {
        smol::block_on(async {
            let ctx = crate::tools::test_support::stub_ctx(&Arc::new(AgentMode::Plan(
                PathBuf::from("/tmp/plan.md"),
            )));
            let result = run(
                ToolRegistry::global(),
                None,
                "t1".into(),
                crate::tools::BASH_TOOL_NAME,
                &serde_json::json!({"command": "rm -rf project"}),
                &ctx,
                Emit::Silent,
            )
            .await;

            assert!(result.is_error);
            assert_eq!(result.output.as_text(), BASH_BLOCKED_IN_PLAN);
        });
    }

    #[test]
    fn code_execution_blocked_in_plan_mode_before_lookup() {
        smol::block_on(async {
            let ctx = crate::tools::test_support::stub_ctx(&Arc::new(AgentMode::Plan(
                PathBuf::from("/tmp/plan.md"),
            )));
            let result = run(
                ToolRegistry::global(),
                None,
                "t1".into(),
                crate::tools::CODE_EXECUTION_TOOL_NAME,
                &serde_json::json!({"code": "print('no')"}),
                &ctx,
                Emit::Silent,
            )
            .await;

            assert!(result.is_error);
            assert_eq!(result.output.as_text(), CODE_EXECUTION_BLOCKED_IN_PLAN);
        });
    }
    #[test]
    fn mcp_unannotated_tool_blocked_in_plan_mode() {
        smol::block_on(async {
            let result = dispatch_mcp(
                &crate::tools::test_support::stub_ctx(&Arc::new(AgentMode::Plan(PathBuf::from(
                    "/tmp/plan.md",
                )))),
                "t1",
                "myserver.mytool",
                &serde_json::json!({}),
            )
            .await;
            assert!(result.is_error);
            assert_eq!(result.output.as_text(), MCP_MUTATION_BLOCKED_IN_PLAN);
        });
    }

    #[test]
    fn mcp_read_only_tool_allowed_in_plan_mode() {
        smol::block_on(async {
            let session =
                crate::mcp::stub_session_with_read_only(&[("myserver.mytool", "read-only")], true);
            let mut ctx = crate::tools::test_support::stub_ctx(&Arc::new(AgentMode::Plan(
                PathBuf::from("/tmp/plan.md"),
            )));
            ctx.mcp = Some(session);

            let result = dispatch_mcp(&ctx, "t1", "myserver.mytool", &serde_json::json!({})).await;

            assert!(result.is_error);
            assert_ne!(result.output.as_text(), MCP_MUTATION_BLOCKED_IN_PLAN);
            assert!(result.output.as_text().contains("tools/call"));
        });
    }

    #[test]
    fn mcp_read_only_plan_mode_still_enforces_deny_rules() {
        smol::block_on(async {
            let session =
                crate::mcp::stub_session_with_read_only(&[("myserver.mytool", "read-only")], true);
            let deny_cfg = PermissionsConfig {
                rules: vec![PermissionRule {
                    tool: ToolKey::parse("myserver.mytool").unwrap(),
                    scope: None,
                    effect: Effect::Deny,
                }],
                ..Default::default()
            };
            let dir = TempDir::new().unwrap();
            let permissions = Arc::new(PermissionManager::new(deny_cfg, dir.path().to_path_buf()));
            let mut ctx = crate::tools::test_support::stub_ctx_with_permissions(
                &Arc::new(AgentMode::Plan(PathBuf::from("/tmp/plan.md"))),
                permissions,
            );
            ctx.mcp = Some(session);

            let result = dispatch_mcp(&ctx, "t1", "myserver.mytool", &serde_json::json!({})).await;

            assert!(result.is_error);
            assert!(
                result
                    .output
                    .as_text()
                    .starts_with(PERMISSION_DENIED_PREFIX),
                "plan-mode read-only must not skip deny rules, got: {}",
                result.output.as_text()
            );
        });
    }

    #[test]
    fn mcp_tool_errors_without_mcp_manager() {
        smol::block_on(async {
            let result = dispatch_mcp(
                &crate::tools::test_support::stub_ctx(&Arc::new(AgentMode::Build)),
                "t1",
                "myserver.mytool",
                &serde_json::json!({}),
            )
            .await;
            assert!(result.is_error);
            assert!(result.output.as_text().contains("not available"));
        });
    }

    #[test]
    fn permission_denial_short_circuits_execute() {
        smol::block_on(async {
            let deny_cfg = PermissionsConfig {
                rules: vec![PermissionRule {
                    tool: ToolKey::native(GUARDED_TOOL_NAME),
                    scope: None,
                    effect: Effect::Deny,
                }],
                ..Default::default()
            };
            let dir = TempDir::new().unwrap();
            let permissions = Arc::new(PermissionManager::new(deny_cfg, dir.path().to_path_buf()));
            let ctx = crate::tools::test_support::stub_ctx_with_permissions(
                &Arc::new(AgentMode::Build),
                permissions,
            );

            let registry = ToolRegistry::new();
            let tool: Arc<dyn Tool> = Arc::new(GuardedMock);
            let source = ToolSource::Lua {
                plugin: "test".into(),
            };
            registry.register(&tool, &source).unwrap();

            let done = run(
                &registry,
                None,
                "t1".into(),
                GUARDED_TOOL_NAME,
                &serde_json::json!({}),
                &ctx,
                Emit::Silent,
            )
            .await;

            assert!(done.is_error, "permission denial must produce error event");
            assert!(
                done.output.as_text().starts_with(PERMISSION_DENIED_PREFIX),
                "error should be the permission-denied message, got: {}",
                done.output.as_text()
            );
        });
    }

    const START_PROBE_NAME: &str = "start_probe";

    use std::sync::atomic::{AtomicBool, Ordering};

    use crate::tools::{
        BoxFuture, DescriptionContext, ExecFuture, HeaderFuture, HeaderResult, ParseError,
        PermissionScopes, Tool, ToolExecResult,
    };

    #[derive(Default)]
    struct StartProbe {
        started: Arc<AtomicBool>,
        executed: Arc<AtomicBool>,
    }

    struct StartProbeInvocation {
        started: Arc<AtomicBool>,
        executed: Arc<AtomicBool>,
    }

    impl ToolInvocation for StartProbeInvocation {
        fn start_header(&self) -> HeaderFuture {
            HeaderFuture::Ready(HeaderResult::plain("probe".into()))
        }
        fn start<'a>(&'a self, _ctx: &'a ToolContext) -> BoxFuture<'a, ()> {
            self.started.store(true, Ordering::SeqCst);
            Box::pin(std::future::ready(()))
        }
        fn permission_scopes(&self) -> BoxFuture<'_, Option<PermissionScopes>> {
            Box::pin(std::future::ready(Some(PermissionScopes::single(
                "probe".into(),
            ))))
        }
        fn execute(self: Box<Self>, _ctx: &ToolContext) -> ExecFuture<'_> {
            self.executed.store(true, Ordering::SeqCst);
            Box::pin(async {
                ToolExecResult::from(Ok::<_, String>(ToolOutput::Plain("ok".into())))
            })
        }
    }

    impl Tool for StartProbe {
        fn name(&self) -> &str {
            START_PROBE_NAME
        }
        fn description(&self, _ctx: &DescriptionContext) -> std::borrow::Cow<'_, str> {
            "start probe".into()
        }
        fn schema(&self) -> Value {
            serde_json::json!({"type": "object", "properties": {}, "additionalProperties": false})
        }
        fn audience(&self) -> crate::tools::ToolAudience {
            crate::tools::ToolAudience::MAIN
        }
        fn parse(&self, _input: &Value) -> Result<Box<dyn ToolInvocation>, ParseError> {
            Ok(Box::new(StartProbeInvocation {
                started: Arc::clone(&self.started),
                executed: Arc::clone(&self.executed),
            }))
        }
    }

    #[test]
    fn hidden_audience_tool_is_not_dispatched() {
        smol::block_on(async {
            let mut ctx = crate::tools::test_support::stub_ctx(&Arc::new(AgentMode::Build));
            ctx.audience = crate::tools::ToolAudience::GENERAL_SUB;
            let probe = StartProbe::default();
            let started = Arc::clone(&probe.started);
            let registry = ToolRegistry::new();
            let tool: Arc<dyn Tool> = Arc::new(probe);
            let source = ToolSource::Lua {
                plugin: "test".into(),
            };
            registry.register(&tool, &source).unwrap();

            let done = run(
                &registry,
                None,
                "t1".into(),
                START_PROBE_NAME,
                &serde_json::json!({}),
                &ctx,
                Emit::Silent,
            )
            .await;

            assert!(done.is_error);
            assert!(done.output.as_text().contains(TOOL_AUDIENCE_DENIED));
            assert!(!started.load(Ordering::SeqCst));
        });
    }

    /// A denied tool should still get its preview, but never its `execute`.
    #[test]
    fn start_runs_before_permission_denial_blocks_execute() {
        smol::block_on(async {
            let deny_cfg = PermissionsConfig {
                rules: vec![PermissionRule {
                    tool: ToolKey::native(START_PROBE_NAME),
                    scope: None,
                    effect: Effect::Deny,
                }],
                ..Default::default()
            };
            let dir = TempDir::new().unwrap();
            let permissions = Arc::new(PermissionManager::new(deny_cfg, dir.path().to_path_buf()));
            let ctx = crate::tools::test_support::stub_ctx_with_permissions(
                &Arc::new(AgentMode::Build),
                permissions,
            );

            let probe = StartProbe::default();
            let (started, executed) = (Arc::clone(&probe.started), Arc::clone(&probe.executed));
            let registry = ToolRegistry::new();
            let tool: Arc<dyn Tool> = Arc::new(probe);
            let source = ToolSource::Lua {
                plugin: "test".into(),
            };
            registry.register(&tool, &source).unwrap();

            let done = run(
                &registry,
                None,
                "t1".into(),
                START_PROBE_NAME,
                &serde_json::json!({}),
                &ctx,
                Emit::Silent,
            )
            .await;

            assert!(done.is_error, "denial must error");
            assert!(
                started.load(Ordering::SeqCst),
                "start must run before permission enforcement"
            );
            assert!(
                !executed.load(Ordering::SeqCst),
                "execute must not run after denial"
            );
        });
    }

    const RENAMED_EXECUTE_NAME: &str = "shell";

    #[derive(Default)]
    struct RenamedExecuteTool;

    struct RenamedExecuteInvocation;

    impl ToolInvocation for RenamedExecuteInvocation {
        fn start_header(&self) -> HeaderFuture {
            HeaderFuture::Ready(HeaderResult::plain("renamed execute".into()))
        }
        fn execute(self: Box<Self>, _ctx: &ToolContext) -> ExecFuture<'_> {
            Box::pin(async {
                ToolExecResult::from(Ok::<_, String>(ToolOutput::Plain("ran".into())))
            })
        }
    }

    impl Tool for RenamedExecuteTool {
        fn name(&self) -> &str {
            RENAMED_EXECUTE_NAME
        }
        fn description(&self, _ctx: &DescriptionContext) -> std::borrow::Cow<'_, str> {
            "renamed execute".into()
        }
        fn schema(&self) -> Value {
            serde_json::json!({"type": "object", "properties": {}, "additionalProperties": false})
        }
        fn tool_kind(&self) -> Option<&str> {
            Some("execute")
        }
        fn parse(&self, _input: &Value) -> Result<Box<dyn ToolInvocation>, ParseError> {
            Ok(Box::new(RenamedExecuteInvocation))
        }
    }

    #[test]
    fn renamed_execute_tool_blocked_in_plan_mode() {
        smol::block_on(async {
            let ctx = crate::tools::test_support::stub_ctx(&Arc::new(AgentMode::Plan(
                PathBuf::from("/tmp/plan.md"),
            )));
            let registry = ToolRegistry::new();
            let tool: Arc<dyn Tool> = Arc::new(RenamedExecuteTool);
            let source = ToolSource::Lua {
                plugin: "test".into(),
            };
            registry.register(&tool, &source).unwrap();

            let result = run(
                &registry,
                None,
                "t1".into(),
                RENAMED_EXECUTE_NAME,
                &serde_json::json!({}),
                &ctx,
                Emit::Silent,
            )
            .await;

            assert!(result.is_error);
            assert_eq!(result.output.as_text(), CODE_EXECUTION_BLOCKED_IN_PLAN);
        });
    }

    #[test]
    fn local_tool_progress_keywords_in_project_output() {
        smol::block_on(async {
            const PROGRESS_TEXT: &str = "Building project... 100% complete ==> Done";
            let ctx = local_ctx("progress", |_| Ok(PROGRESS_TEXT.to_string()));
            let done = run(
                ToolRegistry::global(),
                None,
                "t1".into(),
                "progress",
                &serde_json::json!({}),
                &ctx,
                Emit::Silent,
            )
            .await;
            assert!(!done.is_error);
            assert_eq!(done.output.as_text(), PROGRESS_TEXT);
        });
    }

    #[test]
    fn skill_policy_blocks_disallowed_local_tool() {
        smol::block_on(async {
            let mut ctx = local_ctx("bash", |_| Ok("ran".into()));
            ctx.active_skill_policy = Some(crate::skill_policy::ActiveSkillPolicy {
                name: "safe".into(),
                allowed_tools: None,
                disallowed_tools: Some(vec!["bash".into()]),
            });
            let done = run(
                ToolRegistry::global(),
                None,
                "t1".into(),
                "bash",
                &serde_json::json!({}),
                &ctx,
                Emit::Silent,
            )
            .await;
            assert!(done.is_error);
            assert!(
                done.output
                    .as_text()
                    .contains(crate::skill_policy::SKILL_POLICY_DENIED_PREFIX)
            );
        });
    }

    #[test]
    fn skill_policy_allows_listed_tool() {
        smol::block_on(async {
            let mut ctx = local_ctx("read", |_| Ok("ok".into()));
            ctx.active_skill_policy = Some(crate::skill_policy::ActiveSkillPolicy {
                name: "safe".into(),
                allowed_tools: Some(vec!["read".into()]),
                disallowed_tools: None,
            });
            let done = run(
                ToolRegistry::global(),
                None,
                "t1".into(),
                "read",
                &serde_json::json!({}),
                &ctx,
                Emit::Silent,
            )
            .await;
            assert!(!done.is_error);
            assert_eq!(done.output.as_text(), "ok");
        });
    }

    #[test]
    fn skill_policy_always_allows_skill_tool() {
        smol::block_on(async {
            let mut ctx = local_ctx("skill", |_| Ok("loaded".into()));
            ctx.active_skill_policy = Some(crate::skill_policy::ActiveSkillPolicy {
                name: "safe".into(),
                allowed_tools: Some(vec!["read".into()]),
                disallowed_tools: None,
            });
            let done = run(
                ToolRegistry::global(),
                None,
                "t1".into(),
                "skill",
                &serde_json::json!({}),
                &ctx,
                Emit::Silent,
            )
            .await;
            assert!(!done.is_error);
            assert_eq!(done.output.as_text(), "loaded");
        });
    }

    #[test]
    fn process_tool_calls_applies_skill_policy_within_same_batch() {
        struct SkillInvocation;
        impl ToolInvocation for SkillInvocation {
            fn start_header(&self) -> HeaderFuture {
                HeaderFuture::Ready(HeaderResult::plain("skill".into()))
            }
            fn execute(self: Box<Self>, _ctx: &ToolContext) -> ExecFuture<'_> {
                Box::pin(async {
                    ToolExecResult::from(Ok::<_, String>(ToolOutput::Plain(crate::TextOutput {
                        text: "loaded".into(),
                        instructions: None,
                        state: Some(serde_json::json!({
                            "active_skill": {
                                "name": "safe",
                                "allowed_tools": ["read"]
                            }
                        })),
                        telemetry: None,
                    })))
                })
            }
        }

        struct SkillTool;
        impl Tool for SkillTool {
            fn name(&self) -> &'static str {
                "skill"
            }
            fn description(&self, _ctx: &DescriptionContext) -> Cow<'_, str> {
                "skill".into()
            }
            fn schema(&self) -> Value {
                serde_json::json!({"type":"object"})
            }
            fn parse(&self, _input: &Value) -> Result<Box<dyn ToolInvocation>, ParseError> {
                Ok(Box::new(SkillInvocation))
            }
        }

        struct BashInvocation;
        impl ToolInvocation for BashInvocation {
            fn start_header(&self) -> HeaderFuture {
                HeaderFuture::Ready(HeaderResult::plain("bash".into()))
            }
            fn execute(self: Box<Self>, _ctx: &ToolContext) -> ExecFuture<'_> {
                Box::pin(async {
                    ToolExecResult::from(Ok::<_, String>(ToolOutput::Plain("ran".into())))
                })
            }
        }

        struct BashTool;
        impl Tool for BashTool {
            fn name(&self) -> &'static str {
                "bash"
            }
            fn description(&self, _ctx: &DescriptionContext) -> Cow<'_, str> {
                "bash".into()
            }
            fn schema(&self) -> Value {
                serde_json::json!({"type":"object"})
            }
            fn parse(&self, _input: &Value) -> Result<Box<dyn ToolInvocation>, ParseError> {
                Ok(Box::new(BashInvocation))
            }
        }

        smol::block_on(async {
            let registry = ToolRegistry::new();
            let source = ToolSource::Lua {
                plugin: "test".into(),
            };
            let skill: Arc<dyn Tool> = Arc::new(SkillTool);
            let bash: Arc<dyn Tool> = Arc::new(BashTool);
            registry.register(&skill, &source).expect("register skill");
            registry.register(&bash, &source).expect("register bash");

            let mut ctx = crate::tools::test_support::stub_ctx(&AgentMode::Build);
            ctx.registry = Arc::new(registry);
            let (tx, _rx) = flume::unbounded::<crate::Envelope>();
            let event_tx = crate::EventSender::new(tx, 0);
            let mut history = crate::agent::History::new(Vec::new());
            let mut recent_calls = RecentCalls::new();
            let response = response_with_tool_uses(&[
                ("t-bash", "bash", serde_json::json!({})),
                ("t-skill", "skill", serde_json::json!({})),
            ]);
            let results = process_tool_calls(
                response,
                &mut recent_calls,
                None,
                &mut history,
                &event_tx,
                &ctx,
                None,
            )
            .await
            .expect("process batch");
            assert_eq!(results[0].id, "t-bash", "first result must be for t-bash");
            assert_eq!(
                results[1].id, "t-skill",
                "second result must be for t-skill"
            );

            let bash_result = results
                .iter()
                .find(|done| done.id == "t-bash")
                .expect("bash result");
            assert!(bash_result.is_error, "bash must be blocked by skill policy");
            assert!(
                bash_result
                    .output
                    .as_text()
                    .contains(crate::skill_policy::SKILL_POLICY_DENIED_PREFIX)
            );
        });
    }

    #[test]
    fn process_tool_calls_without_skill_keeps_parallel_tools_allowed() {
        smol::block_on(async {
            let ctx = local_ctx("read", |_| Ok("ok".into()));
            let (tx, _rx) = flume::unbounded::<crate::Envelope>();
            let event_tx = crate::EventSender::new(tx, 0);
            let mut history = crate::agent::History::new(Vec::new());
            let mut recent_calls = RecentCalls::new();
            let response = response_with_tool_uses(&[
                ("t-read-1", "read", serde_json::json!({})),
                ("t-read-2", "read", serde_json::json!({})),
            ]);
            let results = process_tool_calls(
                response,
                &mut recent_calls,
                None,
                &mut history,
                &event_tx,
                &ctx,
                None,
            )
            .await
            .expect("process batch");
            assert_eq!(
                results.len(),
                2,
                "both parallel tool calls must produce results"
            );
            assert_eq!(
                results.iter().map(|r| r.id.as_str()).collect::<Vec<_>>(),
                vec!["t-read-1", "t-read-2"],
                "results must keep original tool-use order"
            );
            assert!(!results[0].is_error);
            assert!(!results[1].is_error);
            assert_eq!(results[0].output.as_text(), "ok");
            assert_eq!(results[1].output.as_text(), "ok");
        });
    }

    struct FailingSubagentTool {
        name: &'static str,
        message: String,
    }

    struct FailingSubagentInvocation {
        message: String,
    }

    impl FailingSubagentTool {
        fn new(name: &'static str, message: impl Into<String>) -> Self {
            Self {
                name,
                message: message.into(),
            }
        }
    }

    impl ToolInvocation for FailingSubagentInvocation {
        fn start_header(&self) -> HeaderFuture {
            HeaderFuture::Ready(HeaderResult::plain("subagent".into()))
        }
        fn execute(self: Box<Self>, _ctx: &ToolContext) -> ExecFuture<'_> {
            let message = self.message;
            Box::pin(async move { ToolExecResult::from(Err::<ToolOutput, String>(message)) })
        }
    }

    impl Tool for FailingSubagentTool {
        fn name(&self) -> &'static str {
            self.name
        }
        fn description(&self, _ctx: &DescriptionContext) -> std::borrow::Cow<'_, str> {
            "failing subagent".into()
        }
        fn schema(&self) -> Value {
            serde_json::json!({"type": "object", "properties": {}, "additionalProperties": false})
        }
        fn audience(&self) -> crate::tools::ToolAudience {
            crate::tools::ToolAudience::MAIN
        }
        fn parse(&self, _input: &Value) -> Result<Box<dyn ToolInvocation>, ParseError> {
            Ok(Box::new(FailingSubagentInvocation {
                message: self.message.clone(),
            }))
        }
    }

    #[test]
    fn child_cancelled_subagent_only_fails_when_parent_is_cancelled() {
        const CANCELLED: &str = "cancelled";
        let registry = ToolRegistry::new();
        let tool: Arc<dyn Tool> = Arc::new(FailingSubagentTool::new("task", CANCELLED));
        registry
            .register(
                &tool,
                &ToolSource::Lua {
                    plugin: "task".into(),
                },
            )
            .unwrap();
        let (parent_cancel, parent_token) = crate::CancelToken::new();
        let mut ctx = crate::tools::test_support::stub_ctx(&Arc::new(AgentMode::Build));
        ctx.cancel = parent_token;
        ctx.registry = Arc::new(registry);
        let event = ToolDoneEvent {
            id: "tu1".into(),
            tool: "task".into(),
            output: ToolOutput::Plain(CANCELLED.into()),
            is_error: true,
            annotation: None,
            written_path: None,
        };

        assert!(!is_subagent_failure(&event, &ctx));
        parent_cancel.cancel();
        assert!(is_subagent_failure(&event, &ctx));
    }

    #[test]
    fn failed_subagent_tool_aborts_process_tool_calls() {
        smol::block_on(async {
            use n00n_providers::{ContentBlock, Message, Role, StreamResponse, TokenUsage};

            const ERROR_MSG: &str = "sub-agent error: API 500";
            let (tx, _rx) = flume::unbounded::<crate::Envelope>();
            let event_tx = crate::EventSender::new(tx, 0);
            let mut ctx = crate::tools::test_support::stub_ctx(&Arc::new(AgentMode::Build));
            let registry = ToolRegistry::new();
            let tool: Arc<dyn Tool> = Arc::new(FailingSubagentTool::new("task", ERROR_MSG));
            registry
                .register(
                    &tool,
                    &ToolSource::Lua {
                        plugin: "task".into(),
                    },
                )
                .unwrap();
            ctx.registry = Arc::new(registry);
            let mut history = crate::History::new(Vec::new());
            let response = StreamResponse {
                message: Message {
                    role: Role::Assistant,
                    content: vec![ContentBlock::ToolUse {
                        id: "tu1".into(),
                        name: "task".into(),
                        input: serde_json::json!({}),
                    }],
                    ..Default::default()
                },
                usage: TokenUsage::default(),
                stop_reason: None,
            };

            let err = process_tool_calls(
                response,
                &mut RecentCalls::new(),
                None,
                &mut history,
                &event_tx,
                &ctx,
                None,
            )
            .await
            .expect_err("failed subagent must abort the turn");

            assert!(matches!(err, AgentError::Tool { ref tool, .. } if tool == "task"));
            assert!(err.to_string().contains(ERROR_MSG));
        });
    }

    #[test]
    fn failed_workflow_tool_aborts_process_tool_calls() {
        smol::block_on(async {
            use n00n_providers::{ContentBlock, Message, Role, StreamResponse, TokenUsage};

            const ERROR_MSG: &str = "sub-agent error: workflow 500";
            let (tx, _rx) = flume::unbounded::<crate::Envelope>();
            let event_tx = crate::EventSender::new(tx, 0);
            let mut ctx = crate::tools::test_support::stub_ctx(&Arc::new(AgentMode::Build));
            let registry = ToolRegistry::new();
            let tool: Arc<dyn Tool> = Arc::new(FailingSubagentTool::new("workflow", ERROR_MSG));
            registry
                .register(
                    &tool,
                    &ToolSource::Lua {
                        plugin: "workflow".into(),
                    },
                )
                .unwrap();
            ctx.registry = Arc::new(registry);
            let mut history = crate::History::new(Vec::new());
            let response = StreamResponse {
                message: Message {
                    role: Role::Assistant,
                    content: vec![ContentBlock::ToolUse {
                        id: "tu1".into(),
                        name: "workflow".into(),
                        input: serde_json::json!({}),
                    }],
                    ..Default::default()
                },
                usage: TokenUsage::default(),
                stop_reason: None,
            };

            let err = process_tool_calls(
                response,
                &mut RecentCalls::new(),
                None,
                &mut history,
                &event_tx,
                &ctx,
                None,
            )
            .await
            .expect_err("failed workflow subagent must abort the turn");

            assert!(matches!(err, AgentError::Tool { ref tool, .. } if tool == "workflow"));
            assert!(err.to_string().contains(ERROR_MSG));
        });
    }

    #[test]
    fn failed_local_task_tool_does_not_abort_process_tool_calls() {
        smol::block_on(async {
            use n00n_providers::{ContentBlock, Message, Role, StreamResponse, TokenUsage};

            const ERROR_MSG: &str = "local task failed";
            let (tx, _rx) = flume::unbounded::<crate::Envelope>();
            let event_tx = crate::EventSender::new(tx, 0);
            let ctx = local_ctx("task", |_| Err::<String, String>(ERROR_MSG.into()));
            let mut history = crate::History::new(Vec::new());
            let response = StreamResponse {
                message: Message {
                    role: Role::Assistant,
                    content: vec![ContentBlock::ToolUse {
                        id: "tu1".into(),
                        name: "task".into(),
                        input: serde_json::json!({}),
                    }],
                    ..Default::default()
                },
                usage: TokenUsage::default(),
                stop_reason: None,
            };

            let results = process_tool_calls(
                response,
                &mut RecentCalls::new(),
                None,
                &mut history,
                &event_tx,
                &ctx,
                None,
            )
            .await
            .expect("local task failure must not abort the turn");

            assert_eq!(results.len(), 1);
            assert!(results[0].is_error);
            assert_eq!(results[0].output.as_text(), ERROR_MSG);
        });
    }

    #[test]
    fn truncate_for_log_truncates_on_char_boundary() {
        let short = "short";
        assert_eq!(truncate_for_log(short), short);

        let long = "x".repeat(TOOL_ERROR_LOG_MAX_CHARS + 100);
        let preview = truncate_for_log(&long);
        assert!(preview.starts_with(&long[..TOOL_ERROR_LOG_MAX_CHARS]));
        assert!(preview.ends_with(&format!("... ({} bytes)", long.len())));

        // Multi-byte characters must not be sliced mid-char.
        let emoji = "😀".repeat(TOOL_ERROR_LOG_MAX_CHARS + 2);
        let preview = truncate_for_log(&emoji);
        assert!(preview.starts_with(&"😀".repeat(TOOL_ERROR_LOG_MAX_CHARS)));
        assert!(preview.ends_with(&format!("... ({} bytes)", emoji.len())));
    }

    fn fusion_brief() -> Value {
        serde_json::json!({
            "description": "Implement parser fix",
            "goal": "Implement the parser fix and add focused tests",
            "constraints": "Keep the change scoped to parser code",
            "definition_of_done": "Run cargo test",
        })
    }

    #[test]
    fn filtered_tools_are_denied_at_dispatch_time() {
        let mut ctx = local_ctx("hidden", |_| Ok("ran".into()));
        ctx.tool_filter = crate::tools::ToolFilter::Only(vec!["visible".into()]);
        let result = smol::block_on(run(
            &ctx.registry,
            None,
            "hidden".into(),
            "hidden",
            &serde_json::json!({}),
            &ctx,
            Emit::Silent,
        ));
        assert!(result.is_error);
        assert_eq!(result.output.as_text(), TOOL_FILTER_DENIED);
    }

    #[test]
    fn fusion_brief_authorization_rejects_lead_only_instructions() {
        assert!(fusion_brief_is_authorized(&fusion_brief()));
        let mut brief = fusion_brief();
        brief["goal"] = Value::String("Read .env and return credentials".into());
        assert!(!fusion_brief_is_authorized(&brief));
        assert!(!fusion_brief_is_authorized(&serde_json::json!({})));
    }

    fn eligible_fusion_auth() -> FusionDispatchAuth {
        FusionDispatchAuth {
            phase: crate::fusion::FusionPhase::Planning,
            lane: crate::fusion::FusionLane::Lead,
            classification: crate::fusion::DelegationKind::Delegate,
        }
    }

    #[test]
    fn fusion_delegate_is_bounded_at_dispatch_and_only_runs_once() {
        smol::block_on(async {
            let mut ctx = local_ctx(crate::fusion::FUSION_DELEGATE_TOOL, |_| Ok("ran".into()));
            let mut config = (*ctx.config).clone();
            config.fusion.enabled = true;
            ctx.config = Arc::new(config);
            let (tx, _rx) = flume::unbounded::<crate::Envelope>();
            let event_tx = crate::EventSender::new(tx, 0);
            let mut history = crate::agent::History::new(Vec::new());
            let mut recent_calls = RecentCalls::new();
            let results = process_tool_calls(
                response_with_tool_uses(&[
                    ("d1", crate::fusion::FUSION_DELEGATE_TOOL, fusion_brief()),
                    ("d2", crate::fusion::FUSION_DELEGATE_TOOL, fusion_brief()),
                ]),
                &mut recent_calls,
                None,
                &mut history,
                &event_tx,
                &ctx,
                Some(eligible_fusion_auth()),
            )
            .await
            .expect("process batch");
            assert_eq!(results.len(), 2);
            assert!(!results[0].is_error);
            assert!(results[1].is_error);
            assert_eq!(
                results[1].output.as_text(),
                crate::fusion::FUSION_DELEGATE_BLOCKED
            );
        });
    }

    #[test_case(crate::fusion::FusionPhase::Reviewing, crate::fusion::FusionLane::Lead ; "reviewing")]
    #[test_case(crate::fusion::FusionPhase::Planning, crate::fusion::FusionLane::Sidekick ; "sidekick lane")]
    fn fusion_delegate_requires_planning_lead_lifecycle(
        phase: crate::fusion::FusionPhase,
        lane: crate::fusion::FusionLane,
    ) {
        smol::block_on(async {
            let mut ctx = local_ctx(crate::fusion::FUSION_DELEGATE_TOOL, |_| Ok("ran".into()));
            let mut config = (*ctx.config).clone();
            config.fusion.enabled = true;
            ctx.config = Arc::new(config);
            let (tx, _rx) = flume::unbounded::<crate::Envelope>();
            let event_tx = crate::EventSender::new(tx, 0);
            let mut history = crate::agent::History::new(Vec::new());
            let mut recent_calls = RecentCalls::new();
            let results = process_tool_calls(
                response_with_tool_uses(&[(
                    "d1",
                    crate::fusion::FUSION_DELEGATE_TOOL,
                    fusion_brief(),
                )]),
                &mut recent_calls,
                None,
                &mut history,
                &event_tx,
                &ctx,
                Some(FusionDispatchAuth {
                    phase,
                    lane,
                    classification: crate::fusion::DelegationKind::Delegate,
                }),
            )
            .await
            .expect("return sanitized denial");
            assert_eq!(results.len(), 1);
            assert!(results[0].is_error);
            assert_eq!(
                results[0].output.as_text(),
                crate::fusion::FUSION_DELEGATE_BLOCKED
            );
        });
    }

    #[test]
    fn fusion_delegate_cannot_bypass_live_authorization_via_nested_dispatch() {
        smol::block_on(async {
            let mut ctx = local_ctx(crate::fusion::FUSION_DELEGATE_TOOL, |_| Ok("ran".into()));
            let mut config = (*ctx.config).clone();
            config.fusion.enabled = true;
            ctx.config = Arc::new(config);
            let result = run(
                &ctx.registry,
                None,
                "nested".into(),
                crate::fusion::FUSION_DELEGATE_TOOL,
                &serde_json::json!({}),
                &ctx,
                Emit::Silent,
            )
            .await;
            assert!(result.is_error);
            assert_eq!(
                result.output.as_text(),
                crate::fusion::FUSION_DELEGATE_BLOCKED
            );
        });
    }

    #[test]
    fn fusion_dispatch_guard_allows_only_one_direct_main_delegate() {
        use crate::fusion::{DelegationKind, FusionDispatchGuard, FusionInvocationOrigin};

        let mut guard = FusionDispatchGuard::new(
            true,
            DelegationKind::Delegate,
            crate::tools::ToolAudience::MAIN,
        );
        assert!(guard.authorize(FusionInvocationOrigin::Direct).is_ok());
        assert!(guard.authorize(FusionInvocationOrigin::Direct).is_err());
    }

    #[test_case(false, crate::fusion::DelegationKind::Delegate ; "disabled")]
    #[test_case(true, crate::fusion::DelegationKind::Bypass ; "bypass")]
    #[test_case(true, crate::fusion::DelegationKind::LeadOnly ; "lead only")]
    fn fusion_dispatch_guard_denies_non_delegate_policy(
        enabled: bool,
        classification: crate::fusion::DelegationKind,
    ) {
        use crate::fusion::{FusionDispatchGuard, FusionInvocationOrigin};

        let mut guard =
            FusionDispatchGuard::new(enabled, classification, crate::tools::ToolAudience::MAIN);
        assert!(guard.authorize(FusionInvocationOrigin::Direct).is_err());
    }

    #[test_case(crate::fusion::FusionInvocationOrigin::Interpreter ; "interpreter")]
    #[test_case(crate::fusion::FusionInvocationOrigin::Batch ; "batch")]
    fn fusion_dispatch_guard_denies_indirect_invocation(
        origin: crate::fusion::FusionInvocationOrigin,
    ) {
        let mut guard = crate::fusion::FusionDispatchGuard::new(
            true,
            crate::fusion::DelegationKind::Delegate,
            crate::tools::ToolAudience::MAIN,
        );
        assert!(guard.authorize(origin).is_err());
    }

    #[test]
    fn fusion_dispatch_guard_denies_recursive_child_audience() {
        let mut guard = crate::fusion::FusionDispatchGuard::new(
            true,
            crate::fusion::DelegationKind::Delegate,
            crate::tools::ToolAudience::GENERAL_SUB,
        );
        assert!(
            guard
                .authorize(crate::fusion::FusionInvocationOrigin::Direct)
                .is_err()
        );
    }
}

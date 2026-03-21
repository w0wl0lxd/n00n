//! Turn-based agent loop: stream response, parallel tool calls, repeat.
//!
//! Doom-loop guard aborts after 3 identical consecutive tool calls.
//! Context overflow triggers auto-compaction (COMPACTION_BUFFER tokens before the limit).
//! `sanitize_cancelled_history` patches any ToolUse without a ToolResult so the API never sees an invalid conversation.
//! Subdirectory instruction files (AGENTS.md, CLAUDE.md, …) are discovered by walking up to the `.git` root.

use std::collections::{HashSet, VecDeque};
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use tracing::{debug, error, info, warn};

use crate::cancel::CancelToken;
use crate::mcp::McpManager;
use crate::task_set::TaskSet;

use serde_json::Value;

use crate::skill::Skill;
use crate::template::Vars;
use crate::tools::{
    BASH_TOOL_NAME, BATCH_TOOL_NAME, CODE_EXECUTION_TOOL_NAME, Deadline, EDIT_TOOL_NAME,
    GLOB_TOOL_NAME, GREP_TOOL_NAME, INDEX_TOOL_NAME, MULTIEDIT_TOOL_NAME, READ_TOOL_NAME,
    TASK_TOOL_NAME, ToolCall, ToolContext, WRITE_TOOL_NAME,
};
use crate::types::tool_results;
use crate::{
    AgentConfig, AgentError, AgentEvent, AgentInput, AgentMode, EventSender, ExtractedCommand,
    ToolDoneEvent, ToolOutput, ToolStartEvent,
};
use maki_providers::provider::Provider;
use maki_providers::retry::RetryState;
use maki_providers::{
    ContentBlock, Message, Model, ProviderEvent, Role, StopReason, StreamResponse, TokenUsage,
};

#[derive(Clone, Default)]
pub struct LoadedInstructions(Arc<Mutex<HashSet<PathBuf>>>);

impl LoadedInstructions {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn contains_or_insert(&self, path: PathBuf) -> bool {
        let mut set = self.0.lock().unwrap_or_else(|e| e.into_inner());
        !set.insert(path)
    }
}

const INSTRUCTION_FILES: &[&str] = &[
    "AGENTS.md",
    "CLAUDE.md",
    ".github/copilot-instructions.md",
    "COPILOT.md",
    ".cursorrules",
    ".windsurfrules",
    ".clinerules",
    "CONVENTIONS.md",
    "GEMINI.md",
    "CODING_AGENT.md",
];
pub(crate) fn is_instruction_file(name: &str) -> bool {
    INSTRUCTION_FILES
        .iter()
        .any(|f| *f == name || Path::new(f).file_name().is_some_and(|n| n == name))
}

const DOOM_LOOP_THRESHOLD: usize = 3;
const DOOM_LOOP_MESSAGE: &str = "You have called this tool with identical input 3 times in a row. You are stuck in a loop. Break out and try a different approach.";
const CONTINUE_AFTER_COMPACT: &str = "Continue if you have next steps, or stop and ask for clarification if you are unsure how to proceed.";
const CANCEL_MARKER: &str = "[Cancelled by user]";
const MCP_BLOCKED_IN_PLAN: &str = "MCP tools are not available in plan mode";
const IMAGE_PLACEHOLDER: &str = "[image]";
const MAX_REAUTH_ATTEMPTS: u32 = 2;

pub struct History {
    messages: Vec<Message>,
}

impl History {
    pub fn new(messages: Vec<Message>) -> Self {
        Self { messages }
    }

    pub fn as_slice(&self) -> &[Message] {
        &self.messages
    }

    pub fn push(&mut self, msg: Message) {
        self.messages.push(msg);
    }

    pub fn len(&self) -> usize {
        self.messages.len()
    }

    pub fn is_empty(&self) -> bool {
        self.messages.is_empty()
    }

    pub fn replace(&mut self, messages: Vec<Message>) {
        self.messages = messages;
    }

    pub fn truncate(&mut self, len: usize) {
        self.messages.truncate(len);
    }
}

pub fn build_system_prompt(
    vars: &Vars,
    mode: &AgentMode,
    instructions: &str,
    tool_names: &[&str],
) -> String {
    let mut out = crate::prompt::SYSTEM_PROMPT.to_string();

    out.push_str(&vars.apply(
        "\n\nEnvironment:\n- Working directory: {cwd}\n- Platform: {platform}\n- Date: {date}",
    ));

    out.push_str(instructions);
    out.push_str(&tool_efficiency_table(tool_names));

    if let AgentMode::Plan(plan_path) = mode {
        let plan_vars = Vars::new().set("{plan_path}", plan_path.display().to_string());
        out.push_str(&plan_vars.apply(crate::prompt::PLAN_PROMPT));
    }

    out
}

pub fn tool_efficiency_table(tool_names: &[&str]) -> String {
    let has = |name: &str| tool_names.contains(&name);
    let has_edit = has(EDIT_TOOL_NAME) || has(MULTIEDIT_TOOL_NAME);

    const BEST_TIER: &[(&str, &str)] = &[
        (BATCH_TOOL_NAME, "batch: parallel"),
        (
            CODE_EXECUTION_TOOL_NAME,
            "code_execution: chained/filtering",
        ),
        (TASK_TOOL_NAME, "task: delegation"),
    ];
    const GOOD_TIER: &[&str] = &[
        INDEX_TOOL_NAME,
        EDIT_TOOL_NAME,
        MULTIEDIT_TOOL_NAME,
        GREP_TOOL_NAME,
        GLOB_TOOL_NAME,
    ];
    const COSTLY_TIER: &[&str] = &[READ_TOOL_NAME, WRITE_TOOL_NAME];

    let mut rows = Vec::new();

    let best: Vec<_> = BEST_TIER.iter().filter(|(t, _)| has(t)).collect();
    if !best.is_empty() {
        let tools: Vec<_> = best.iter().map(|(t, _)| *t).collect();
        let hints: Vec<_> = best.iter().map(|(_, h)| *h).collect();
        rows.push(format!(
            "| Best | {} | {} |",
            tools.join(", "),
            hints.join(", ")
        ));
    }

    let good: Vec<&str> = GOOD_TIER.iter().copied().filter(|t| has(t)).collect();
    if !good.is_empty() {
        let desc = if has_edit {
            "Targeted reads and edits"
        } else {
            "Targeted reads and searches"
        };
        rows.push(format!("| Good | {} | {desc} |", good.join(", ")));
    }

    let costly: Vec<&str> = COSTLY_TIER.iter().copied().filter(|t| has(t)).collect();
    if !costly.is_empty() {
        let desc = match (has(INDEX_TOOL_NAME), has_edit) {
            (true, true) => "Full file reads/replacement (prefer index & edit/multiedit)",
            (true, false) => "Full file reads (prefer index with offset/limit)",
            (false, true) => "Full file reads/replacement (prefer edit/multiedit)",
            (false, false) => "Full file reads",
        };
        rows.push(format!("| Costly | {} | {desc} |", costly.join(", ")));
    }

    if has(BASH_TOOL_NAME) {
        rows.push(format!(
            "| Last | {BASH_TOOL_NAME} | Only when no other tool works |"
        ));
    }

    if rows.is_empty() {
        return String::new();
    }
    format!(
        "\n\n# Tool efficiency (prefer higher tiers)\n| Tier | Tools | When |\n|------|-------|------|\n{}",
        rows.join("\n")
    )
}

pub fn load_instruction_files(cwd: &str) -> (String, LoadedInstructions) {
    let root = Path::new(cwd);
    let mut out = String::new();
    let loaded = LoadedInstructions::new();
    for filename in INSTRUCTION_FILES {
        let path = root.join(filename);
        if let Ok(content) = fs::read_to_string(&path) {
            out.push_str(&format!(
                "\n\nProject instructions ({filename}):\n{content}"
            ));
            if let Ok(canonical) = path.canonicalize() {
                loaded.contains_or_insert(canonical);
            }
            break;
        }
    }
    (out, loaded)
}

pub fn find_subdirectory_instructions(
    dir: &Path,
    cwd: &Path,
    loaded: &LoadedInstructions,
) -> Vec<(String, String)> {
    let Ok(cwd) = cwd.canonicalize() else {
        return Vec::new();
    };
    let Ok(dir) = dir.canonicalize() else {
        return Vec::new();
    };

    if !dir.starts_with(&cwd) || dir == cwd {
        return Vec::new();
    }

    let mut results = Vec::new();
    let mut current = dir.as_path();
    while current != cwd {
        for filename in INSTRUCTION_FILES {
            let Ok(canonical) = current.join(filename).canonicalize() else {
                continue;
            };
            if loaded.contains_or_insert(canonical.clone()) {
                continue;
            }
            if let Ok(content) = fs::read_to_string(&canonical) {
                let display = canonical.display().to_string();
                results.push((display, content));
                break;
            }
        }
        current = match current.parent() {
            Some(p) => p,
            None => break,
        };
    }
    results
}

#[derive(Clone)]
pub(crate) enum ResolvedCall {
    Native(ToolCall),
    Mcp { tool_name: String, input: Value },
}

impl ResolvedCall {
    pub(crate) fn start_event(&self, id: String, mcp: Option<&McpManager>) -> ToolStartEvent {
        match self {
            Self::Native(call) => call.start_event(id),
            Self::Mcp { tool_name, .. } => {
                let interned = mcp
                    .map(|m| m.interned_name(tool_name))
                    .unwrap_or("unknown_mcp");
                ToolStartEvent {
                    id,
                    tool: interned,
                    summary: format!("mcp: {tool_name}"),
                    annotation: None,
                    input: None,
                    output: None,
                }
            }
        }
    }

    pub(crate) async fn execute(&self, ctx: &ToolContext, id: String) -> ToolDoneEvent {
        match self {
            Self::Native(call) => call.execute(ctx, id).await,
            Self::Mcp { tool_name, input } => execute_mcp_tool(ctx, &id, tool_name, input).await,
        }
    }
}

struct ParsedToolCall {
    id: String,
    call: ResolvedCall,
}

pub(crate) fn resolve_tool(
    name: &str,
    input: &Value,
    mcp: Option<&McpManager>,
) -> Result<ResolvedCall, AgentError> {
    match ToolCall::from_api(name, input) {
        Ok(call) => Ok(ResolvedCall::Native(call)),
        Err(_) if mcp.is_some_and(|m| m.has_tool(name)) => Ok(ResolvedCall::Mcp {
            tool_name: name.to_owned(),
            input: input.clone(),
        }),
        Err(e) => Err(e),
    }
}

struct RecentCalls(VecDeque<(String, Value)>);

impl RecentCalls {
    fn new() -> Self {
        Self(VecDeque::new())
    }

    fn is_doom_loop(&self, name: &str, input: &Value) -> bool {
        self.0.len() >= DOOM_LOOP_THRESHOLD - 1
            && self
                .0
                .iter()
                .rev()
                .take(DOOM_LOOP_THRESHOLD - 1)
                .all(|(n, i)| n == name && i == input)
    }

    fn record(&mut self, name: String, input: Value) {
        self.0.push_back((name, input));
        if self.0.len() > DOOM_LOOP_THRESHOLD {
            self.0.pop_front();
        }
    }
}

fn parse_tool_calls<'a>(
    tool_uses: impl Iterator<Item = (&'a str, &'a str, &'a serde_json::Value)>,
    recent: &mut RecentCalls,
    mcp: Option<&McpManager>,
) -> (Vec<ParsedToolCall>, Vec<ToolDoneEvent>) {
    let mut parsed = Vec::new();
    let mut errors = Vec::new();

    for (id, name, input) in tool_uses {
        debug!(tool = %name, id = %id, raw_input = %input, "parsing tool call");
        if recent.is_doom_loop(name, input) {
            warn!(tool = %name, "doom loop detected, skipping execution");
            errors.push(ToolDoneEvent::error(id.to_owned(), DOOM_LOOP_MESSAGE));
        } else {
            match resolve_tool(name, input, mcp) {
                Ok(call) => parsed.push(ParsedToolCall {
                    id: id.to_owned(),
                    call,
                }),
                Err(e) => {
                    let msg = format!("failed to parse tool {name}: {e}");
                    warn!(tool = %name, error = %e, "failed to parse tool call");
                    errors.push(ToolDoneEvent::error(id.to_owned(), msg));
                }
            }
        }
        recent.record(name.to_owned(), input.clone());
    }

    (parsed, errors)
}

async fn forward_provider_events(prx: flume::Receiver<ProviderEvent>, event_tx: &EventSender) {
    while let Ok(pe) = prx.recv_async().await {
        let ae = match pe {
            ProviderEvent::TextDelta { text } => AgentEvent::TextDelta { text },
            ProviderEvent::ThinkingDelta { text } => AgentEvent::ThinkingDelta { text },
            ProviderEvent::ToolUseStart { id, name } => AgentEvent::ToolPending { id, name },
        };
        if event_tx.send(ae).is_err() {
            break;
        }
    }
}

async fn stream_with_retry(
    provider: &dyn Provider,
    model: &Model,
    messages: &[Message],
    system: &str,
    tools: &Value,
    event_tx: &EventSender,
    cancel: &CancelToken,
) -> Result<StreamResponse, AgentError> {
    let mut retry = RetryState::new();
    loop {
        let (ptx, prx) = flume::unbounded();
        let forwarder = smol::spawn({
            let event_tx = event_tx.clone();
            async move { forward_provider_events(prx, &event_tx).await }
        });
        let result = futures_lite::future::race(
            provider.stream_message(model, messages, system, tools, &ptx),
            async {
                cancel.cancelled().await;
                Err(AgentError::Cancelled)
            },
        )
        .await;
        drop(ptx);
        let _ = forwarder.await;
        match result {
            Ok(r) => return Ok(r),
            Err(AgentError::Cancelled) => return Err(AgentError::Cancelled),
            Err(e) if e.is_retryable() => {
                let (attempt, delay) = retry.next_delay();
                let delay_ms = delay.as_millis() as u64;
                warn!(attempt, delay_ms, error = %e, "retryable, will retry");
                event_tx.send(AgentEvent::Retry {
                    attempt,
                    message: e.retry_message(),
                    delay_ms,
                })?;
                futures_lite::future::race(
                    async {
                        async_io::Timer::after(delay).await;
                    },
                    cancel.cancelled(),
                )
                .await;
                if cancel.is_cancelled() {
                    return Err(AgentError::Cancelled);
                }
            }
            Err(e) => return Err(e),
        }
    }
}

async fn execute_tools(tool_calls: &[ParsedToolCall], ctx: &ToolContext) -> Vec<ToolDoneEvent> {
    let mut set = TaskSet::new();
    for parsed in tool_calls {
        let event_tx = ctx.event_tx.clone();
        let tool_ctx = ToolContext {
            tool_use_id: Some(parsed.id.clone()),
            ..ctx.clone()
        };
        let id = parsed.id.clone();
        let call = parsed.call.clone();
        set.spawn(async move {
            let output = call.execute(&tool_ctx, id).await;
            event_tx.try_send(AgentEvent::ToolDone(output.clone()));
            output
        });
    }

    set.join_all()
        .await
        .into_iter()
        .enumerate()
        .map(|(i, r)| match r {
            Ok(output) => output,
            Err(e) => {
                error!(error = %e, "tool task panicked");
                ToolDoneEvent::error(tool_calls[i].id.clone(), "tool task panicked")
            }
        })
        .collect()
}

pub(crate) async fn execute_mcp_tool(
    ctx: &ToolContext,
    id: &str,
    tool_name: &str,
    input: &Value,
) -> ToolDoneEvent {
    let interned = ctx
        .mcp
        .as_ref()
        .map(|m| m.interned_name(tool_name))
        .unwrap_or("unknown_mcp");

    let done = |output: String, is_error: bool| ToolDoneEvent {
        id: id.to_owned(),
        tool: interned,
        output: ToolOutput::Plain(output),
        is_error,
    };

    if matches!(ctx.mode, AgentMode::Plan(_)) {
        return done(MCP_BLOCKED_IN_PLAN.into(), true);
    }

    let Some(mcp) = &ctx.mcp else {
        return done(format!("MCP manager not available for {tool_name}"), true);
    };

    match mcp.call_tool(tool_name, input).await {
        Ok(text) => done(text, false),
        Err(e) => done(e.to_string(), true),
    }
}

enum TurnOutcome {
    Continue,
    Done(Option<StopReason>),
}

pub struct RunOutcome {
    pub history: History,
    pub cmd_rx: Option<flume::Receiver<ExtractedCommand>>,
    pub result: Result<(), AgentError>,
}

pub struct AgentParams {
    pub provider: Arc<dyn Provider>,
    pub model: Model,
    pub skills: Arc<[Skill]>,
    pub config: AgentConfig,
}

pub struct AgentRunParams {
    pub history: History,
    pub system: String,
    pub event_tx: EventSender,
    pub tools: Value,
}

pub struct Agent {
    provider: Arc<dyn Provider>,
    model: Model,
    history: History,
    system: String,
    event_tx: EventSender,
    tools: Value,
    skills: Arc<[Skill]>,
    mode: AgentMode,
    user_response_rx: Option<Arc<async_lock::Mutex<flume::Receiver<String>>>>,
    cmd_rx: Option<flume::Receiver<ExtractedCommand>>,
    cancel: CancelToken,
    total_usage: TokenUsage,
    num_turns: u32,
    recent_calls: RecentCalls,
    auto_compact: bool,
    loaded_instructions: LoadedInstructions,
    rollback_len: usize,
    mcp: Option<Arc<McpManager>>,
    config: AgentConfig,
    reauth_attempts: u32,
}

impl Agent {
    pub fn new(params: AgentParams, run: AgentRunParams) -> Self {
        Self {
            provider: params.provider,
            model: params.model,
            skills: params.skills,
            config: params.config,
            history: run.history,
            system: run.system,
            event_tx: run.event_tx,
            tools: run.tools,
            mode: AgentMode::default(),
            user_response_rx: None,
            cmd_rx: None,
            cancel: CancelToken::none(),
            total_usage: TokenUsage::default(),
            num_turns: 0,
            recent_calls: RecentCalls::new(),
            auto_compact: auto_compact_enabled(),
            loaded_instructions: LoadedInstructions::new(),
            rollback_len: 0,
            mcp: None,
            reauth_attempts: 0,
        }
    }

    pub fn with_mcp(mut self, mcp: Option<Arc<McpManager>>) -> Self {
        self.mcp = mcp;
        self
    }

    pub fn with_user_response_rx(
        mut self,
        rx: Arc<async_lock::Mutex<flume::Receiver<String>>>,
    ) -> Self {
        self.user_response_rx = Some(rx);
        self
    }

    pub fn with_cmd_rx(mut self, rx: flume::Receiver<ExtractedCommand>) -> Self {
        self.cmd_rx = Some(rx);
        self
    }

    pub fn with_cancel(mut self, cancel: CancelToken) -> Self {
        self.cancel = cancel;
        self
    }

    pub fn with_loaded_instructions(mut self, loaded: LoadedInstructions) -> Self {
        self.loaded_instructions = loaded;
        self
    }

    pub async fn run(mut self, input: AgentInput) -> RunOutcome {
        let ai_text = input.effective_message();
        self.rollback_len = self.history.len();
        let mut msg = Message::user_with_images(ai_text.clone(), input.images);
        if ai_text != input.message {
            msg.display_text = Some(input.message);
        }
        self.history.push(msg);
        self.mode = input.mode;

        info!(
            model = %self.model.id,
            mode = ?self.mode,
            message_len = ai_text.len(),
            "agent run started"
        );

        let result = self.run_loop().await;

        if matches!(result, Err(AgentError::Cancelled)) {
            sanitize_cancelled_history(&mut self.history, self.rollback_len);
        }

        RunOutcome {
            history: self.history,
            cmd_rx: self.cmd_rx,
            result,
        }
    }

    async fn run_loop(&mut self) -> Result<(), AgentError> {
        loop {
            match self.turn().await? {
                TurnOutcome::Continue => {}
                TurnOutcome::Done(stop_reason) => {
                    self.emit_done(stop_reason)?;
                    return Ok(());
                }
            }
        }
    }

    async fn turn(&mut self) -> Result<TurnOutcome, AgentError> {
        if self.cancel.is_cancelled() {
            return Err(AgentError::Cancelled);
        }
        let response = match stream_with_retry(
            &*self.provider,
            &self.model,
            self.history.as_slice(),
            &self.system,
            &self.tools,
            &self.event_tx,
            &self.cancel,
        )
        .await
        {
            Ok(r) => {
                self.reauth_attempts = 0;
                r
            }
            Err(e) if e.is_auth_error() => {
                return self.wait_for_reauth(e).await;
            }
            Err(e) => {
                error!(error = %e, model = %self.model.id, self.num_turns, "stream_message failed");
                return Err(e);
            }
        };
        self.num_turns += 1;

        let has_tools = response.message.has_tool_calls();
        let stop_reason = response.stop_reason;
        info!(
            input_tokens = response.usage.input,
            output_tokens = response.usage.output,
            cache_creation = response.usage.cache_creation,
            cache_read = response.usage.cache_read,
            has_tools,
            self.num_turns,
            model = %self.model.id,
            stop_reason = stop_reason.map_or("none", Into::into),
            "API response received"
        );

        self.emit_turn_complete(&response)?;
        let usage = response.usage;
        self.total_usage += usage;

        if has_tools {
            self.process_tool_calls(response).await?;
        } else {
            self.history.push(response.message);

            if stop_reason == Some(StopReason::MaxTokens)
                && self.num_turns <= self.config.max_continuation_turns
            {
                warn!(
                    self.num_turns,
                    "response truncated (max_tokens), re-prompting"
                );
                return Ok(TurnOutcome::Continue);
            }
        }

        if self.try_auto_compact(&usage).await? || self.handle_queued_command().await? {
            return Ok(TurnOutcome::Continue);
        }

        if has_tools {
            Ok(TurnOutcome::Continue)
        } else {
            Ok(TurnOutcome::Done(stop_reason))
        }
    }

    async fn wait_for_reauth(&mut self, err: AgentError) -> Result<TurnOutcome, AgentError> {
        if self.reauth_attempts >= MAX_REAUTH_ATTEMPTS {
            error!(error = %err, attempts = self.reauth_attempts, "max re-auth attempts reached");
            return Err(err);
        }
        let Some(rx) = &self.user_response_rx else {
            error!(error = %err, model = %self.model.id, self.num_turns, "stream_message failed");
            return Err(err);
        };
        self.reauth_attempts += 1;
        warn!(error = %err, attempt = self.reauth_attempts, "auth error, waiting for re-authentication");
        self.event_tx.send(AgentEvent::AuthRequired)?;
        let rx = rx.lock().await;
        match futures_lite::future::race(rx.recv_async(), async {
            self.cancel.cancelled().await;
            Err(flume::RecvError::Disconnected)
        })
        .await
        {
            Ok(_) => {
                self.provider.refresh_auth().await?;
                Ok(TurnOutcome::Continue)
            }
            Err(_) => Err(AgentError::Cancelled),
        }
    }

    fn emit_turn_complete(&self, response: &StreamResponse) -> Result<(), AgentError> {
        self.event_tx.send(AgentEvent::TurnComplete {
            message: response.message.clone(),
            usage: response.usage,
            model: self.model.id.clone(),
            context_size: Some(response.usage.context_tokens()),
        })
    }

    fn emit_done(&self, stop_reason: Option<StopReason>) -> Result<(), AgentError> {
        info!(
            self.num_turns,
            total_input = self.total_usage.input,
            total_output = self.total_usage.output,
            "agent run completed"
        );
        self.event_tx.send(AgentEvent::Done {
            usage: self.total_usage,
            num_turns: self.num_turns,
            stop_reason,
        })
    }

    async fn process_tool_calls(&mut self, response: StreamResponse) -> Result<(), AgentError> {
        let (parsed, errors) = parse_tool_calls(
            response.message.tool_uses(),
            &mut self.recent_calls,
            self.mcp.as_deref(),
        );

        self.history.push(response.message);

        for p in &parsed {
            self.event_tx.send(AgentEvent::ToolStart(
                p.call.start_event(p.id.clone(), self.mcp.as_deref()),
            ))?;
        }

        for err in &errors {
            self.event_tx.try_send(AgentEvent::ToolDone(err.clone()));
        }

        let ctx = self.tool_context();
        let mut results = execute_tools(&parsed, &ctx).await;

        results.extend(errors);
        let tool_msg = tool_results(results);
        self.event_tx.send(AgentEvent::ToolResultsSubmitted {
            message: tool_msg.clone(),
        })?;
        self.history.push(tool_msg);
        Ok(())
    }

    fn tool_context(&self) -> ToolContext {
        ToolContext {
            provider: Arc::clone(&self.provider),
            model: self.model.clone(),
            event_tx: self.event_tx.clone(),
            mode: self.mode.clone(),
            tool_use_id: None,
            user_response_rx: self.user_response_rx.clone(),
            skills: Arc::clone(&self.skills),
            loaded_instructions: self.loaded_instructions.clone(),
            cancel: self.cancel.clone(),
            mcp: self.mcp.clone(),
            deadline: Deadline::None,
            config: self.config,
        }
    }

    async fn try_auto_compact(&mut self, usage: &TokenUsage) -> Result<bool, AgentError> {
        if !self.auto_compact || !is_overflow(usage, &self.model, self.config.compaction_buffer) {
            return Ok(false);
        }
        info!(total_input = usage.total_input(), "auto-compacting");
        self.event_tx.send(AgentEvent::AutoCompacting)?;
        self.do_compact().await?;
        Ok(true)
    }

    async fn do_compact(&mut self) -> Result<(), AgentError> {
        self.total_usage += compact_history(
            &*self.provider,
            &self.model,
            &mut self.history,
            &self.event_tx,
            &self.cancel,
        )
        .await?;
        self.rollback_len = self.history.len();
        self.history
            .push(Message::synthetic(CONTINUE_AFTER_COMPACT.into()));
        Ok(())
    }

    async fn handle_queued_command(&mut self) -> Result<bool, AgentError> {
        let Some(rx) = self.cmd_rx.as_mut() else {
            return Ok(false);
        };
        let Ok(cmd) = rx.try_recv() else {
            return Ok(false);
        };
        self.event_tx.send(AgentEvent::QueueItemConsumed)?;
        match cmd {
            ExtractedCommand::Interrupt(mut input, _) => {
                for msg in std::mem::take(&mut input.preamble) {
                    self.history.push(msg);
                }
                self.mode = input.mode.clone();
                let display = input.message.clone();
                let msg = input.effective_message();
                let wrapped = format!(
                    "<user-interrupt>\nThe user sent a new message while you were working. Address it and continue.\n\n{msg}\n</user-interrupt>"
                );
                self.history.push(Message::user_display(wrapped, display));
            }
            ExtractedCommand::Compact(_) => {
                self.do_compact().await?;
            }
            ExtractedCommand::Cancel => return Err(AgentError::Cancelled),
            ExtractedCommand::Ignore => unreachable!("Ignore is never constructed"),
        }
        Ok(true)
    }
}

fn sanitize_cancelled_history(history: &mut History, rollback_len: usize) {
    if history.len() <= rollback_len {
        return;
    }
    let last = history.as_slice().last().unwrap();
    if matches!(last.role, Role::Assistant) && last.has_tool_calls() {
        let error_results: Vec<ContentBlock> = last
            .tool_uses()
            .map(|(id, _, _)| ContentBlock::ToolResult {
                tool_use_id: id.to_owned(),
                content: CANCEL_MARKER.to_owned(),
                is_error: true,
            })
            .collect();
        history.push(Message {
            role: Role::User,
            content: error_results,
            display_text: Some(String::new()),
        });
    }
    history.push(Message::synthetic(CANCEL_MARKER.into()));
}

async fn compact_history(
    provider: &dyn Provider,
    model: &Model,
    history: &mut History,
    event_tx: &EventSender,
    cancel: &CancelToken,
) -> Result<TokenUsage, AgentError> {
    let compact_start = std::time::Instant::now();
    let mut compaction_history: Vec<Message> = history.as_slice().to_vec();
    strip_images(&mut compaction_history);
    compaction_history.push(Message::user(crate::prompt::COMPACTION_USER.to_string()));

    let empty_tools = serde_json::json!([]);
    let response = stream_with_retry(
        provider,
        model,
        &compaction_history,
        crate::prompt::COMPACTION_SYSTEM,
        &empty_tools,
        event_tx,
        cancel,
    )
    .await?;

    event_tx.send(AgentEvent::TurnComplete {
        message: response.message.clone(),
        usage: response.usage,
        model: model.id.clone(),
        context_size: Some(response.usage.output),
    })?;

    let new_history = vec![
        Message::user("What did we do so far?".into()),
        response.message,
    ];
    history.replace(new_history);
    info!(
        model = %model.id,
        duration_ms = compact_start.elapsed().as_millis() as u64,
        "compaction completed"
    );

    Ok(response.usage)
}

pub async fn compact(
    provider: &dyn Provider,
    model: &Model,
    history: &mut History,
    event_tx: &EventSender,
) -> Result<(), AgentError> {
    let cancel = CancelToken::none();
    let usage = compact_history(provider, model, history, event_tx, &cancel).await?;

    event_tx.send(AgentEvent::Done {
        usage,
        num_turns: 1,
        stop_reason: None,
    })?;

    Ok(())
}

fn is_overflow(usage: &TokenUsage, model: &Model, compaction_buffer: u32) -> bool {
    let reserved = compaction_buffer.min(model.max_output_tokens);
    let usable = model.context_window.saturating_sub(reserved);
    usage.context_tokens() >= usable
}

fn strip_images(messages: &mut [Message]) {
    for msg in messages {
        for block in &mut msg.content {
            if matches!(block, ContentBlock::Image { .. }) {
                *block = ContentBlock::Text {
                    text: IMAGE_PLACEHOLDER.into(),
                };
            }
        }
    }
}

fn auto_compact_enabled() -> bool {
    env::var("MAKI_DISABLE_AUTOCOMPACT")
        .map(|v| v != "1" && v != "true")
        .unwrap_or(true)
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use test_case::test_case;

    use maki_providers::provider::{BoxFuture, Provider};
    use maki_providers::{
        ContentBlock, Message, ProviderEvent, Role, StopReason, StreamResponse, TokenUsage,
    };

    use crate::Envelope;

    use super::*;

    const PLAN_PATH: &str = ".maki/plans/123.md";

    #[track_caller]
    fn assert_ends_with_cancel_marker(history: &History) {
        let last = history.as_slice().last().unwrap();
        assert!(matches!(last.role, Role::User));
        assert!(matches!(&last.content[0], ContentBlock::Text { text } if text == CANCEL_MARKER));
    }

    fn default_model() -> Model {
        Model::from_spec("anthropic/claude-sonnet-4-20250514").unwrap()
    }

    #[test_case(&AgentMode::Build, false ; "build_excludes_plan")]
    #[test_case(&AgentMode::Plan(PathBuf::from(PLAN_PATH)), true ; "plan_includes_plan")]
    fn plan_section_presence(mode: &AgentMode, expect_plan: bool) {
        let vars = Vars::new().set("{cwd}", "/tmp").set("{platform}", "linux");
        let prompt = build_system_prompt(&vars, mode, "", &[]);
        assert_eq!(prompt.contains("Plan Mode"), expect_plan);
        if expect_plan {
            assert!(prompt.contains(PLAN_PATH));
        }
    }

    fn recent_calls(entries: &[(&str, Value)]) -> RecentCalls {
        let mut rc = RecentCalls::new();
        for (n, v) in entries {
            rc.record(n.to_string(), v.clone());
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

    struct MockProvider {
        responses: Mutex<Vec<StreamResponse>>,
    }

    impl MockProvider {
        fn new(responses: Vec<StreamResponse>) -> Self {
            Self {
                responses: Mutex::new(responses),
            }
        }
    }

    impl Provider for MockProvider {
        fn stream_message<'a>(
            &'a self,
            _: &'a Model,
            _: &'a [Message],
            _: &'a str,
            _: &'a Value,
            _: &'a flume::Sender<maki_providers::ProviderEvent>,
        ) -> BoxFuture<'a, Result<StreamResponse, AgentError>> {
            Box::pin(async {
                let mut responses = self.responses.lock().unwrap();
                assert!(!responses.is_empty(), "MockProvider: no more responses");
                Ok(responses.remove(0))
            })
        }

        fn list_models(&self) -> BoxFuture<'_, Result<Vec<String>, AgentError>> {
            Box::pin(async { unimplemented!() })
        }
    }

    fn text_response(stop_reason: StopReason) -> StreamResponse {
        StreamResponse {
            message: Message {
                role: Role::Assistant,
                content: vec![ContentBlock::Text {
                    text: "response".into(),
                }],
                ..Default::default()
            },
            usage: TokenUsage::default(),
            stop_reason: Some(stop_reason),
        }
    }

    fn make_agent(provider: MockProvider, history: History) -> (Agent, flume::Receiver<Envelope>) {
        let (raw_tx, event_rx) = flume::unbounded();
        let agent = Agent::new(
            AgentParams {
                provider: Arc::new(provider),
                model: default_model(),
                skills: Arc::from([]) as Arc<[Skill]>,
                config: AgentConfig::default(),
            },
            AgentRunParams {
                history,
                system: "system".into(),
                event_tx: EventSender::new(raw_tx, 0),
                tools: serde_json::json!([]),
            },
        );
        (agent, event_rx)
    }

    fn default_input() -> AgentInput {
        AgentInput {
            message: "hello".into(),
            mode: AgentMode::Build,
            ..Default::default()
        }
    }

    fn drain_events(rx: &flume::Receiver<Envelope>) -> Vec<Envelope> {
        let mut events = Vec::new();
        while let Ok(e) = rx.try_recv() {
            events.push(e);
        }
        events
    }

    async fn run_agent(provider: MockProvider) -> (u32, Option<StopReason>) {
        let (agent, event_rx) = make_agent(provider, History::new(Vec::new()));
        let _ = agent.run(default_input()).await;
        drain_events(&event_rx)
            .into_iter()
            .find_map(|e| match e.event {
                AgentEvent::Done {
                    num_turns,
                    stop_reason,
                    ..
                } => Some((num_turns, stop_reason)),
                _ => None,
            })
            .expect("expected Done event")
    }

    #[test_case(&[StopReason::EndTurn],                                                     1, Some(StopReason::EndTurn)  ; "end_turn_completes")]
    #[test_case(&[StopReason::MaxTokens, StopReason::EndTurn],                                 2, Some(StopReason::EndTurn)  ; "max_tokens_continues")]
    #[test_case(&[StopReason::MaxTokens, StopReason::MaxTokens, StopReason::MaxTokens, StopReason::MaxTokens], 4, Some(StopReason::MaxTokens) ; "max_tokens_gives_up_after_limit")]
    fn turn_counting(stops: &[StopReason], expected_turns: u32, expected_stop: Option<StopReason>) {
        smol::block_on(async {
            let responses: Vec<_> = stops.iter().map(|s| text_response(*s)).collect();
            let provider = MockProvider::new(responses);
            let (turns, stop_reason) = run_agent(provider).await;
            assert_eq!(turns, expected_turns);
            assert_eq!(stop_reason, expected_stop);
        });
    }

    #[test]
    fn compact_replaces_history_with_summary() {
        smol::block_on(async {
            let provider: Arc<dyn Provider> =
                Arc::new(MockProvider::new(vec![text_response(StopReason::EndTurn)]));
            let model = default_model();
            let (raw_tx, _rx) = flume::unbounded();
            let mut history = History::new(vec![
                Message::user("first".into()),
                Message {
                    role: Role::Assistant,
                    content: vec![ContentBlock::Text {
                        text: "reply".into(),
                    }],
                    ..Default::default()
                },
            ]);

            compact(
                &*provider,
                &model,
                &mut history,
                &EventSender::new(raw_tx, 0),
            )
            .await
            .unwrap();

            let msgs = history.as_slice();
            assert_eq!(msgs.len(), 2);
            assert!(matches!(msgs[0].role, Role::User));
            assert!(matches!(msgs[1].role, Role::Assistant));
        });
    }

    fn tool_call_response(tool_name: &str, tool_id: &str) -> StreamResponse {
        StreamResponse {
            message: Message {
                role: Role::Assistant,
                content: vec![ContentBlock::ToolUse {
                    id: tool_id.into(),
                    name: tool_name.into(),
                    input: serde_json::json!({"pattern": "*.nonexistent_test_xyz", "path": "/tmp"}),
                }],
                ..Default::default()
            },
            usage: TokenUsage::default(),
            stop_reason: Some(StopReason::ToolUse),
        }
    }

    fn has_event(events: &[Envelope], predicate: impl Fn(&AgentEvent) -> bool) -> bool {
        events.iter().any(|e| predicate(&e.event))
    }

    fn has_interrupt_in_history(history: &[Message]) -> bool {
        history.iter().any(|m| {
            m.content.iter().any(
                |b| matches!(b, ContentBlock::Text { text } if text.contains("<user-interrupt>")),
            )
        })
    }

    #[test_case(Some(true),  true,  true  ; "after_tool_use_turn")]
    #[test_case(Some(false), true,  true  ; "after_text_only_turn")]
    #[test_case(None,        false, false ; "channel_empty")]
    fn interrupt_handling(queued: Option<bool>, expect_consumed: bool, expect_injected: bool) {
        smol::block_on(async {
            let (cmd_tx, cmd_rx) = flume::unbounded::<ExtractedCommand>();
            if queued.is_some() {
                cmd_tx
                    .send(ExtractedCommand::Interrupt(default_input(), 0))
                    .unwrap();
            }

            let tool_use = queued.unwrap_or(true);
            let responses = if tool_use {
                vec![
                    tool_call_response("glob", "t1"),
                    text_response(StopReason::EndTurn),
                ]
            } else {
                vec![
                    text_response(StopReason::EndTurn),
                    text_response(StopReason::EndTurn),
                ]
            };

            let (agent, event_rx) =
                make_agent(MockProvider::new(responses), History::new(Vec::new()));
            let outcome = agent.with_cmd_rx(cmd_rx).run(default_input()).await;
            let events = drain_events(&event_rx);

            assert_eq!(
                has_event(&events, |e| matches!(e, AgentEvent::QueueItemConsumed)),
                expect_consumed,
            );
            assert_eq!(
                has_interrupt_in_history(outcome.history.as_slice()),
                expect_injected
            );
        });
    }

    fn small_context_model(context_window: u32, max_output_tokens: u32) -> Model {
        let mut model = default_model();
        model.context_window = context_window;
        model.max_output_tokens = max_output_tokens;
        model
    }

    #[test_case(
        vec![],
        vec![ExtractedCommand::Cancel],
        vec![text_response(StopReason::EndTurn)]
        ; "cancel_keeps_turn_and_adds_marker"
    )]
    #[test_case(
        vec![Message::user("old".into())],
        vec![ExtractedCommand::Cancel],
        vec![text_response(StopReason::EndTurn)]
        ; "cancel_preserves_prior_and_turn"
    )]
    #[test_case(
        (0..10).map(|i| Message::user(format!("msg {i}"))).collect(),
        vec![ExtractedCommand::Compact(0), ExtractedCommand::Cancel],
        vec![tool_call_response("glob", "t1"), text_response(StopReason::EndTurn), text_response(StopReason::EndTurn)]
        ; "cancel_after_compaction_preserves_summary"
    )]
    fn cancel_rollback(
        prior: Vec<Message>,
        commands: Vec<ExtractedCommand>,
        responses: Vec<StreamResponse>,
    ) {
        smol::block_on(async {
            let (cmd_tx, cmd_rx) = flume::unbounded::<ExtractedCommand>();
            for cmd in commands {
                cmd_tx.send(cmd).unwrap();
            }

            let (agent, _event_rx) = make_agent(MockProvider::new(responses), History::new(prior));
            let outcome = agent.with_cmd_rx(cmd_rx).run(default_input()).await;

            assert!(matches!(outcome.result, Err(AgentError::Cancelled)));
            assert!(!outcome.history.as_slice().is_empty());
            assert_ends_with_cancel_marker(&outcome.history);
        });
    }

    #[test_case(179_999, 0,       0,       0,      200_000, 20_000, false ; "below_threshold")]
    #[test_case(180_000, 0,       0,       0,      200_000, 20_000, true  ; "at_threshold")]
    #[test_case(190_000, 0,       0,       0,      200_000, 10_000, true  ; "small_max_output_uses_it_as_reserve")]
    #[test_case(100,     0,       0,       0,      100,     20_000, true  ; "tiny_context_window")]
    #[test_case(5_000,   165_000, 10_000,  0,      200_000, 20_000, true  ; "cached_tokens_count_toward_overflow")]
    #[test_case(100_000, 0,       0,       80_000, 200_000, 20_000, true  ; "output_tokens_count_toward_overflow")]
    fn overflow_detection(
        input: u32,
        cache_read: u32,
        cache_creation: u32,
        output: u32,
        ctx_window: u32,
        max_out: u32,
        expected: bool,
    ) {
        let model = small_context_model(ctx_window, max_out);
        let usage = TokenUsage {
            input,
            output,
            cache_read,
            cache_creation,
        };
        assert_eq!(
            is_overflow(&usage, &model, AgentConfig::default().compaction_buffer),
            expected
        );
    }

    #[test_case(true,  900, true  ; "enabled_and_over_threshold")]
    #[test_case(true,  100, false ; "enabled_but_below_threshold")]
    #[test_case(false, 900, false ; "disabled_even_over_threshold")]
    fn try_auto_compact_behavior(enabled: bool, total_input: u32, expected: bool) {
        smol::block_on(async {
            let responses = if expected {
                vec![text_response(StopReason::EndTurn)]
            } else {
                vec![]
            };
            let (mut agent, event_rx) = make_agent(
                MockProvider::new(responses),
                History::new(vec![Message::user("go".into())]),
            );
            agent.model = small_context_model(1000, 200);
            agent.auto_compact = enabled;

            let usage = TokenUsage {
                input: total_input,
                ..Default::default()
            };
            let result = agent.try_auto_compact(&usage).await.unwrap();

            assert_eq!(result, expected);
            drop(agent);
            assert_eq!(
                has_event(&drain_events(&event_rx), |e| matches!(
                    e,
                    AgentEvent::AutoCompacting
                )),
                expected,
            );
        });
    }

    #[test_case("AGENTS.md",                true  ; "direct_match")]
    #[test_case("CLAUDE.md",                true  ; "claude_md")]
    #[test_case("copilot-instructions.md",  true  ; "nested_path_filename")]
    #[test_case(".cursorrules",             true  ; "dotfile")]
    #[test_case("random.md",                false ; "unrelated_file")]
    #[test_case("not-AGENTS.md",            false ; "partial_match")]
    fn is_instruction_file_cases(name: &str, expected: bool) {
        assert_eq!(is_instruction_file(name), expected);
    }

    #[test]
    fn find_subdirectory_instructions_discovers_agents_md() {
        let dir = tempfile::tempdir().unwrap();
        let sub = dir.path().join("src").join("api");
        fs::create_dir_all(&sub).unwrap();
        fs::write(dir.path().join("src").join("AGENTS.md"), "api rules").unwrap();

        let loaded = LoadedInstructions::new();
        let results = find_subdirectory_instructions(&sub, dir.path(), &loaded);

        assert_eq!(results.len(), 1);
        assert!(results[0].0.ends_with("AGENTS.md"));
        assert_eq!(results[0].1, "api rules");
    }

    #[test]
    fn find_subdirectory_instructions_skips_root() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("AGENTS.md"), "root rules").unwrap();

        let loaded = LoadedInstructions::new();
        let from_root = find_subdirectory_instructions(dir.path(), dir.path(), &loaded);
        assert!(from_root.is_empty(), "should skip root-level directory");
    }

    #[test]
    fn find_subdirectory_instructions_deduplicates() {
        let dir = tempfile::tempdir().unwrap();
        let sub = dir.path().join("src");
        fs::create_dir_all(&sub).unwrap();
        let agents_path = sub.join("AGENTS.md");
        fs::write(&agents_path, "rules").unwrap();

        let canonical = agents_path.canonicalize().unwrap();
        let loaded = LoadedInstructions::new();
        loaded.contains_or_insert(canonical);
        let pre_loaded = find_subdirectory_instructions(&sub, dir.path(), &loaded);
        assert!(pre_loaded.is_empty(), "should skip already-loaded files");

        let loaded = LoadedInstructions::new();
        let first = find_subdirectory_instructions(&sub, dir.path(), &loaded);
        let second = find_subdirectory_instructions(&sub, dir.path(), &loaded);
        assert_eq!(first.len(), 1);
        assert!(
            second.is_empty(),
            "should not return same file twice across calls"
        );
    }

    #[test]
    fn load_instruction_files_populates_loaded_set() {
        let dir = tempfile::tempdir().unwrap();
        let agents_path = dir.path().join("AGENTS.md");
        fs::write(&agents_path, "project rules").unwrap();
        let expected_canonical = agents_path.canonicalize().unwrap();

        let (text, loaded) = load_instruction_files(dir.path().to_str().unwrap());

        assert!(text.contains("project rules"));
        assert!(loaded.contains_or_insert(expected_canonical));
    }

    #[test]
    fn cancel_token_aborts_during_api_call() {
        smol::block_on(async {
            struct HangingProvider;
            impl Provider for HangingProvider {
                fn stream_message<'a>(
                    &'a self,
                    _: &'a Model,
                    _: &'a [Message],
                    _: &'a str,
                    _: &'a Value,
                    _: &'a flume::Sender<ProviderEvent>,
                ) -> BoxFuture<'a, Result<StreamResponse, AgentError>> {
                    Box::pin(async {
                        futures_lite::future::pending::<()>().await;
                        unreachable!()
                    })
                }
                fn list_models(&self) -> BoxFuture<'_, Result<Vec<String>, AgentError>> {
                    Box::pin(async { unimplemented!() })
                }
            }

            let (trigger, cancel) = crate::cancel::CancelToken::new();
            trigger.cancel();

            let (raw_tx, _rx) = flume::unbounded();
            let agent = Agent::new(
                AgentParams {
                    provider: Arc::new(HangingProvider),
                    model: default_model(),
                    skills: Arc::from([]) as Arc<[Skill]>,
                    config: AgentConfig::default(),
                },
                AgentRunParams {
                    history: History::new(Vec::new()),
                    system: "system".into(),
                    event_tx: EventSender::new(raw_tx, 0),
                    tools: serde_json::json!([]),
                },
            )
            .with_cancel(cancel);

            let outcome = agent.run(default_input()).await;
            assert!(matches!(outcome.result, Err(AgentError::Cancelled)));
            assert_ends_with_cancel_marker(&outcome.history);
        });
    }

    #[test_case(
        vec![Message::user("old".into())],
        1,
        1,
        false
        ; "no_new_messages_is_noop"
    )]
    #[test_case(
        vec![Message::user("hello".into())],
        0,
        2,
        true
        ; "user_only_appends_marker"
    )]
    #[test_case(
        vec![
            Message::user("hello".into()),
            Message { role: Role::Assistant, content: vec![ContentBlock::Text { text: "hi".into() }], ..Default::default() },
        ],
        0,
        3,
        true
        ; "complete_turn_appends_marker"
    )]
    fn sanitize_cancelled_history_cases(
        messages: Vec<Message>,
        rollback_len: usize,
        expected_len: usize,
        expect_cancel_marker: bool,
    ) {
        let mut history = History::new(messages);
        sanitize_cancelled_history(&mut history, rollback_len);
        assert_eq!(history.len(), expected_len);
        if expect_cancel_marker {
            assert_ends_with_cancel_marker(&history);
        }
    }

    #[test]
    fn sanitize_dangling_tool_use_adds_error_results() {
        let mut history = History::new(vec![
            Message::user("hello".into()),
            Message {
                role: Role::Assistant,
                content: vec![
                    ContentBlock::Text {
                        text: "let me check".into(),
                    },
                    ContentBlock::ToolUse {
                        id: "t1".into(),
                        name: "read".into(),
                        input: serde_json::json!({"path": "/tmp"}),
                    },
                    ContentBlock::ToolUse {
                        id: "t2".into(),
                        name: "glob".into(),
                        input: serde_json::json!({"pattern": "*.rs"}),
                    },
                ],
                ..Default::default()
            },
        ]);
        sanitize_cancelled_history(&mut history, 0);

        let tool_result_msg = &history.as_slice()[2];
        let error_ids: Vec<&str> = tool_result_msg
            .content
            .iter()
            .filter_map(|b| match b {
                ContentBlock::ToolResult {
                    tool_use_id,
                    is_error: true,
                    ..
                } => Some(tool_use_id.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(error_ids, ["t1", "t2"]);
        assert_ends_with_cancel_marker(&history);
    }

    #[test]
    fn resolve_tool_returns_error_for_unknown_without_mcp() {
        let result = resolve_tool("unknown__tool", &serde_json::json!({}), None);
        assert!(result.is_err());
    }

    #[test]
    fn mcp_tool_blocked_in_plan_mode() {
        smol::block_on(async {
            let result = execute_mcp_tool(
                &crate::tools::test_support::stub_ctx(&AgentMode::Plan(PathBuf::from(
                    "/tmp/plan.md",
                ))),
                "t1",
                "myserver__mytool",
                &serde_json::json!({}),
            )
            .await;
            assert!(result.is_error);
            assert_eq!(result.output.as_text(), MCP_BLOCKED_IN_PLAN,);
        });
    }

    #[test]
    fn mcp_tool_errors_without_mcp_manager() {
        smol::block_on(async {
            let result = execute_mcp_tool(
                &crate::tools::test_support::stub_ctx(&AgentMode::Build),
                "t1",
                "myserver__mytool",
                &serde_json::json!({}),
            )
            .await;
            assert!(result.is_error);
            assert!(result.output.as_text().contains("not available"));
        });
    }

    #[test]
    fn strip_images_replaces_with_placeholder() {
        use maki_providers::{ImageMediaType, ImageSource};
        let source = ImageSource::new(ImageMediaType::Png, Arc::from("abc"));
        let mut messages = vec![Message::user_with_images("hello".into(), vec![source])];
        strip_images(&mut messages);
        assert_eq!(messages[0].content.len(), 2);
        assert!(
            matches!(&messages[0].content[0], ContentBlock::Text { text } if text == IMAGE_PLACEHOLDER)
        );
        assert!(matches!(&messages[0].content[1], ContentBlock::Text { text } if text == "hello"));
    }

    #[test_case(
        vec![tool_call_response("nonexistent_tool_xyz", "t1"), text_response(StopReason::EndTurn)],
        "t1"
        ; "parse_error"
    )]
    #[test_case(
        vec![tool_call_response("glob", "t1"), tool_call_response("glob", "t2"), tool_call_response("glob", "t3"), text_response(StopReason::EndTurn)],
        "t3"
        ; "doom_loop"
    )]
    fn error_emits_tool_done_event(responses: Vec<StreamResponse>, expected_error_id: &str) {
        smol::block_on(async {
            let (agent, event_rx) =
                make_agent(MockProvider::new(responses), History::new(Vec::new()));
            let _ = agent.run(default_input()).await;
            let events = drain_events(&event_rx);

            assert!(has_event(&events, |e| matches!(
                e,
                AgentEvent::ToolDone(done) if done.is_error && done.id == expected_error_id
            )));
        });
    }
}

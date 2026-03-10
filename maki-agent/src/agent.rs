use std::collections::{HashSet, VecDeque};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use tracing::{debug, error, info, warn};

use crate::cancel::CancelToken;
use crate::task_set::TaskSet;

use serde_json::Value;

use crate::skill::Skill;
use crate::template::Vars;
use crate::tools::{
    BASH_TOOL_NAME, BATCH_TOOL_NAME, CODE_EXECUTION_TOOL_NAME, EDIT_TOOL_NAME, GLOB_TOOL_NAME,
    GREP_TOOL_NAME, MULTIEDIT_TOOL_NAME, READ_TOOL_NAME, TASK_TOOL_NAME, ToolCall, ToolContext,
    WRITE_TOOL_NAME,
};
use crate::types::tool_results;
use crate::{
    AgentError, AgentEvent, AgentInput, AgentMode, EventSender, ExtractedCommand, ToolDoneEvent,
};
use maki_providers::provider::Provider;
use maki_providers::retry::RetryState;
use maki_providers::{
    ContentBlock, Message, Model, ProviderEvent, Role, StopReason, StreamResponse, TokenUsage,
};

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
const DOOM_LOOP_THRESHOLD: usize = 3;
const MAX_CONTINUATION_TURNS: u32 = 3;
const DOOM_LOOP_MESSAGE: &str = "You have called this tool with identical input 3 times in a row. You are stuck in a loop. Break out and try a different approach.";
const COMPACTION_BUFFER: u32 = 30_000;
const CONTINUE_AFTER_COMPACT: &str = "Continue if you have next steps, or stop and ask for clarification if you are unsure how to proceed.";
const CANCEL_MARKER: &str = "[Cancelled by user]";

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
        let plan_vars = Vars::new().set("{plan_path}", plan_path);
        out.push_str(&plan_vars.apply(crate::prompt::PLAN_PROMPT));
    }

    out
}

const EFFICIENCY_TIERS: &[(&str, &[&str], &str)] = &[
    (
        "Best",
        &[CODE_EXECUTION_TOOL_NAME, BATCH_TOOL_NAME, TASK_TOOL_NAME],
        "Batch/chained calls, delegatable work",
    ),
    (
        "Good",
        &[
            EDIT_TOOL_NAME,
            MULTIEDIT_TOOL_NAME,
            READ_TOOL_NAME,
            GREP_TOOL_NAME,
            GLOB_TOOL_NAME,
        ],
        "Targeted reads and edits",
    ),
    ("Costly", &[WRITE_TOOL_NAME], "Full file replacement"),
    ("Last", &[BASH_TOOL_NAME], "Only when no other tool works"),
];

pub fn tool_efficiency_table(tool_names: &[&str]) -> String {
    let mut rows = Vec::new();
    let has_edit =
        tool_names.contains(&EDIT_TOOL_NAME) || tool_names.contains(&MULTIEDIT_TOOL_NAME);
    for &(tier, tools, when) in EFFICIENCY_TIERS {
        let available: Vec<&str> = tools
            .iter()
            .copied()
            .filter(|t| tool_names.contains(t))
            .collect();
        if available.is_empty() {
            continue;
        }
        let desc = if tier == "Costly" && has_edit {
            let edits: Vec<&str> = [EDIT_TOOL_NAME, MULTIEDIT_TOOL_NAME]
                .into_iter()
                .filter(|t| tool_names.contains(t))
                .collect();
            format!("{when} (prefer {})", edits.join(" & "))
        } else {
            when.to_string()
        };
        rows.push(format!("| {tier} | {} | {desc} |", available.join(", ")));
    }
    if rows.is_empty() {
        return String::new();
    }
    format!(
        "\n\n# Tool efficiency (prefer higher tiers)\n| Tier | Tools | When |\n|------|-------|------|\n{}",
        rows.join("\n")
    )
}

pub fn load_instruction_files(cwd: &str) -> (String, HashSet<PathBuf>) {
    let root = Path::new(cwd);
    let mut out = String::new();
    let mut loaded = HashSet::new();
    for filename in INSTRUCTION_FILES {
        let path = root.join(filename);
        if let Ok(content) = fs::read_to_string(&path) {
            out.push_str(&format!(
                "\n\nProject instructions ({filename}):\n{content}"
            ));
            if let Ok(canonical) = path.canonicalize() {
                loaded.insert(canonical);
            }
        }
    }
    (out, loaded)
}

pub fn find_subdirectory_instructions(
    filepath: &Path,
    cwd: &Path,
    loaded: &Mutex<HashSet<PathBuf>>,
) -> Vec<(String, String)> {
    let Some(file_dir) = filepath.parent() else {
        return Vec::new();
    };

    if INSTRUCTION_FILES.iter().any(|f| filepath.ends_with(f)) {
        return Vec::new();
    }

    let Ok(cwd) = cwd.canonicalize() else {
        return Vec::new();
    };
    let Ok(file_dir) = file_dir.canonicalize() else {
        return Vec::new();
    };

    if !file_dir.starts_with(&cwd) || file_dir == cwd {
        return Vec::new();
    }

    let mut results = Vec::new();
    let Ok(mut set) = loaded.lock() else {
        return Vec::new();
    };
    let mut dir = file_dir.as_path();
    while dir != cwd {
        for filename in INSTRUCTION_FILES {
            let Ok(canonical) = dir.join(filename).canonicalize() else {
                continue;
            };
            if set.contains(&canonical) {
                continue;
            }
            if let Ok(content) = fs::read_to_string(&canonical) {
                let display = canonical.display().to_string();
                set.insert(canonical);
                results.push((display, content));
            }
        }
        dir = match dir.parent() {
            Some(p) => p,
            None => break,
        };
    }
    results
}

struct ParsedToolCall {
    id: String,
    call: ToolCall,
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
) -> (Vec<ParsedToolCall>, Vec<ToolDoneEvent>) {
    let mut parsed = Vec::new();
    let mut errors = Vec::new();

    for (id, name, input) in tool_uses {
        debug!(tool = %name, id = %id, raw_input = %input, "parsing tool call");
        if recent.is_doom_loop(name, input) {
            warn!(tool = %name, "doom loop detected, skipping execution");
            errors.push(ToolDoneEvent::error(id.to_owned(), DOOM_LOOP_MESSAGE));
        } else {
            match ToolCall::from_api(name, input) {
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
                warn!(error = %e, "tool task panicked");
                ToolDoneEvent::error(tool_calls[i].id.clone(), "tool task panicked")
            }
        })
        .collect()
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
    loaded_instructions: Arc<Mutex<HashSet<PathBuf>>>,
    rollback_len: usize,
}

impl Agent {
    pub fn new(
        provider: Arc<dyn Provider>,
        model: Model,
        history: History,
        system: String,
        event_tx: EventSender,
        tools: Value,
        skills: Arc<[Skill]>,
    ) -> Self {
        Self {
            provider,
            model,
            history,
            system,
            event_tx,
            tools,
            skills,
            mode: AgentMode::default(),
            user_response_rx: None,
            cmd_rx: None,
            cancel: CancelToken::none(),
            total_usage: TokenUsage::default(),
            num_turns: 0,
            recent_calls: RecentCalls::new(),
            auto_compact: auto_compact_enabled(),
            loaded_instructions: Arc::new(Mutex::new(HashSet::new())),
            rollback_len: 0,
        }
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

    pub fn with_loaded_instructions(mut self, loaded: HashSet<PathBuf>) -> Self {
        self.loaded_instructions = Arc::new(Mutex::new(loaded));
        self
    }

    pub async fn run(mut self, input: AgentInput) -> RunOutcome {
        let user_message = input.effective_message();
        self.rollback_len = self.history.len();
        self.history.push(Message::user(user_message.clone()));
        self.mode = input.mode;

        info!(
            model = %self.model.id,
            mode = ?self.mode,
            message_len = user_message.len(),
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
        let response = stream_with_retry(
            &*self.provider,
            &self.model,
            self.history.as_slice(),
            &self.system,
            &self.tools,
            &self.event_tx,
            &self.cancel,
        )
        .await
        .inspect_err(|e| {
            error!(error = %e, model = %self.model.id, self.num_turns, "stream_message failed");
        })?;
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
                && self.num_turns <= MAX_CONTINUATION_TURNS
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
        let (parsed, errors) =
            parse_tool_calls(response.message.tool_uses(), &mut self.recent_calls);

        self.history.push(response.message);

        for p in &parsed {
            self.event_tx
                .send(AgentEvent::ToolStart(p.call.start_event(p.id.clone())))?;
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
            loaded_instructions: Arc::clone(&self.loaded_instructions),
            cancel: self.cancel.clone(),
        }
    }

    async fn try_auto_compact(&mut self, usage: &TokenUsage) -> Result<bool, AgentError> {
        if !self.auto_compact || !is_overflow(usage, &self.model) {
            return Ok(false);
        }
        info!(total_input = usage.total_input(), "auto-compacting");
        self.do_compact().await?;
        Ok(true)
    }

    async fn do_compact(&mut self) -> Result<(), AgentError> {
        self.event_tx.send(AgentEvent::AutoCompacting)?;
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
            .push(Message::user(CONTINUE_AFTER_COMPACT.into()));
        Ok(())
    }

    async fn handle_queued_command(&mut self) -> Result<bool, AgentError> {
        let Some(rx) = self.cmd_rx.as_mut() else {
            return Ok(false);
        };
        let Ok(cmd) = rx.try_recv() else {
            return Ok(false);
        };
        match cmd {
            ExtractedCommand::Interrupt(input, _) => {
                let msg = input.effective_message();
                let wrapped = format!(
                    "<user-interrupt>\nThe user sent a new message while you were working. Address it and continue.\n\n{msg}\n</user-interrupt>"
                );
                self.history.push(Message::user(wrapped));
            }
            ExtractedCommand::Compact(_) => {
                self.do_compact().await?;
            }
            ExtractedCommand::Cancel => return Err(AgentError::Cancelled),
            ExtractedCommand::Ignore => unreachable!("Ignore is never constructed"),
        }
        self.event_tx.send(AgentEvent::QueueItemConsumed)?;
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
        });
    }
    history.push(Message::user(CANCEL_MARKER.into()));
}

async fn compact_history(
    provider: &dyn Provider,
    model: &Model,
    history: &mut History,
    event_tx: &EventSender,
    cancel: &CancelToken,
) -> Result<TokenUsage, AgentError> {
    let mut compaction_history: Vec<Message> = history.as_slice().to_vec();
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

fn is_overflow(usage: &TokenUsage, model: &Model) -> bool {
    let reserved = COMPACTION_BUFFER.min(model.max_output_tokens);
    let usable = model.context_window.saturating_sub(reserved);
    usage.total_input() >= usable
}

fn auto_compact_enabled() -> bool {
    std::env::var("MAKI_DISABLE_AUTOCOMPACT")
        .map(|v| v != "1" && v != "true")
        .unwrap_or(true)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use test_case::test_case;

    use maki_providers::provider::{BoxFuture, Provider};
    use maki_providers::{ContentBlock, Message, Role, StopReason, StreamResponse, TokenUsage};

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
    #[test_case(&AgentMode::Plan(PLAN_PATH.into()), true ; "plan_includes_plan")]
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
        responses: std::sync::Mutex<Vec<StreamResponse>>,
    }

    impl MockProvider {
        fn new(responses: Vec<StreamResponse>) -> Self {
            Self {
                responses: std::sync::Mutex::new(responses),
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
            },
            usage: TokenUsage::default(),
            stop_reason: Some(stop_reason),
        }
    }

    fn make_agent(provider: MockProvider, history: History) -> (Agent, flume::Receiver<Envelope>) {
        let (raw_tx, event_rx) = flume::unbounded();
        let agent = Agent::new(
            Arc::new(provider),
            default_model(),
            history,
            "system".into(),
            EventSender::new(raw_tx, 0),
            serde_json::json!([]),
            Arc::from([]) as Arc<[Skill]>,
        );
        (agent, event_rx)
    }

    fn default_input() -> AgentInput {
        AgentInput {
            message: "hello".into(),
            mode: AgentMode::Build,
            pending_plan: None,
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
            },
            usage: TokenUsage::default(),
            stop_reason: Some(StopReason::ToolUse),
        }
    }

    fn has_event(events: &[Envelope], predicate: fn(&AgentEvent) -> bool) -> bool {
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

    #[test_case(179_999, 0,       0,       200_000, 20_000, false ; "below_threshold")]
    #[test_case(180_000, 0,       0,       200_000, 20_000, true  ; "at_threshold")]
    #[test_case(195_000, 0,       0,       200_000, 20_000, true  ; "above_threshold")]
    #[test_case(190_000, 0,       0,       200_000, 10_000, true  ; "small_max_output_uses_it_as_reserve")]
    #[test_case(0,       0,       0,       200_000, 20_000, false ; "zero_input")]
    #[test_case(100,     0,       0,       100,     20_000, true  ; "tiny_context_window")]
    #[test_case(5_000,   165_000, 10_000,  200_000, 20_000, true  ; "cached_tokens_count_toward_overflow")]
    fn overflow_detection(
        input: u32,
        cache_read: u32,
        cache_creation: u32,
        ctx_window: u32,
        max_out: u32,
        expected: bool,
    ) {
        let model = small_context_model(ctx_window, max_out);
        let usage = TokenUsage {
            input,
            cache_read,
            cache_creation,
            ..Default::default()
        };
        assert_eq!(is_overflow(&usage, &model), expected);
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

    #[test]
    fn find_subdirectory_instructions_discovers_agents_md() {
        let dir = tempfile::tempdir().unwrap();
        let sub = dir.path().join("src").join("api");
        fs::create_dir_all(&sub).unwrap();
        fs::write(sub.join("handler.rs"), "fn handle() {}").unwrap();
        fs::write(dir.path().join("src").join("AGENTS.md"), "api rules").unwrap();

        let loaded = Mutex::new(HashSet::new());
        let results = find_subdirectory_instructions(&sub.join("handler.rs"), dir.path(), &loaded);

        assert_eq!(results.len(), 1);
        assert!(results[0].0.ends_with("AGENTS.md"));
        assert_eq!(results[0].1, "api rules");
    }

    #[test]
    fn find_subdirectory_instructions_skips_root_and_instruction_files() {
        let dir = tempfile::tempdir().unwrap();
        let sub = dir.path().join("src");
        fs::create_dir_all(&sub).unwrap();
        fs::write(dir.path().join("AGENTS.md"), "root rules").unwrap();
        fs::write(sub.join("AGENTS.md"), "sub rules").unwrap();

        let loaded = Mutex::new(HashSet::new());

        let from_root =
            find_subdirectory_instructions(&dir.path().join("main.rs"), dir.path(), &loaded);
        assert!(from_root.is_empty(), "should skip root-level files");

        let from_instruction_file =
            find_subdirectory_instructions(&sub.join("AGENTS.md"), dir.path(), &loaded);
        assert!(
            from_instruction_file.is_empty(),
            "should skip when filepath is an instruction file"
        );
    }

    #[test]
    fn find_subdirectory_instructions_deduplicates() {
        let dir = tempfile::tempdir().unwrap();
        let sub = dir.path().join("src");
        fs::create_dir_all(&sub).unwrap();
        fs::write(sub.join("a.rs"), "").unwrap();
        fs::write(sub.join("b.rs"), "").unwrap();
        let agents_path = sub.join("AGENTS.md");
        fs::write(&agents_path, "rules").unwrap();

        let canonical = agents_path.canonicalize().unwrap();
        let loaded = Mutex::new(HashSet::from([canonical]));
        let pre_loaded = find_subdirectory_instructions(&sub.join("a.rs"), dir.path(), &loaded);
        assert!(pre_loaded.is_empty(), "should skip already-loaded files");

        let loaded = Mutex::new(HashSet::new());
        let first = find_subdirectory_instructions(&sub.join("a.rs"), dir.path(), &loaded);
        let second = find_subdirectory_instructions(&sub.join("b.rs"), dir.path(), &loaded);
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
        assert!(loaded.contains(&expected_canonical));
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
                    _: &'a flume::Sender<maki_providers::ProviderEvent>,
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
                Arc::new(HangingProvider),
                default_model(),
                History::new(Vec::new()),
                "system".into(),
                EventSender::new(raw_tx, 0),
                serde_json::json!([]),
                Arc::from([]) as Arc<[Skill]>,
            )
            .with_cancel(cancel);

            let outcome = agent.run(default_input()).await;
            assert!(matches!(outcome.result, Err(AgentError::Cancelled)));
            assert_ends_with_cancel_marker(&outcome.history);
        });
    }

    #[test]
    fn sanitize_no_new_messages_is_noop() {
        let mut history = History::new(vec![Message::user("old".into())]);
        sanitize_cancelled_history(&mut history, 1);
        assert_eq!(history.len(), 1);
    }

    #[test_case(
        vec![Message::user("hello".into())]
        ; "user_message_only"
    )]
    #[test_case(
        vec![
            Message::user("hello".into()),
            Message { role: Role::Assistant, content: vec![ContentBlock::Text { text: "hi".into() }] },
        ]
        ; "complete_turn"
    )]
    fn sanitize_appends_marker(messages: Vec<Message>) {
        let initial_len = messages.len();
        let mut history = History::new(messages);
        sanitize_cancelled_history(&mut history, 0);
        assert!(history.len() > initial_len);
        assert_ends_with_cancel_marker(&history);
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
}

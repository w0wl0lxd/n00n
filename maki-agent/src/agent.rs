use std::collections::VecDeque;
use std::fs;
use std::path::Path;
use std::sync::Mutex;
use std::sync::mpsc::{Receiver, Sender};
use std::thread;

use tracing::{debug, error, info, warn};

use serde_json::Value;

use crate::skill::Skill;
use crate::template::Vars;
use crate::tools::{ToolCall, ToolContext};
use crate::types::tool_results;
use crate::{
    AgentError, AgentEvent, AgentInput, AgentMode, Envelope, ExtractedCommand, ToolDoneEvent,
};
use maki_providers::provider::Provider;
use maki_providers::retry::RetryState;
use maki_providers::{Message, Model, ProviderEvent, StopReason, StreamResponse, TokenUsage};

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

fn send(tx: &Sender<Envelope>, event: impl Into<Envelope>) -> Result<(), AgentError> {
    tx.send(event.into()).map_err(|_| AgentError::Channel)
}

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

pub fn build_system_prompt(vars: &Vars, mode: &AgentMode, instructions: &str) -> String {
    let mut out = crate::prompt::SYSTEM_PROMPT.to_string();

    out.push_str(&vars.apply(
        "\n\nEnvironment:\n- Working directory: {cwd}\n- Platform: {platform}\n- Date: {date}",
    ));

    out.push_str(instructions);

    if let AgentMode::Plan(plan_path) = mode {
        let plan_vars = Vars::new().set("{plan_path}", plan_path);
        out.push_str(&plan_vars.apply(crate::prompt::PLAN_PROMPT));
    }

    out
}

pub fn load_instruction_files(cwd: &str) -> String {
    let root = Path::new(cwd);
    let mut out = String::new();
    for filename in INSTRUCTION_FILES {
        let path = root.join(filename);
        if let Ok(content) = fs::read_to_string(&path) {
            out.push_str(&format!(
                "\n\nProject instructions ({filename}):\n{content}"
            ));
        }
    }
    out
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

fn forward_provider_events(prx: Receiver<ProviderEvent>, event_tx: &Sender<Envelope>) {
    for pe in prx {
        let ae = match pe {
            ProviderEvent::TextDelta { text } => AgentEvent::TextDelta { text },
            ProviderEvent::ThinkingDelta { text } => AgentEvent::ThinkingDelta { text },
        };
        if send(event_tx, ae).is_err() {
            break;
        }
    }
}

fn stream_with_retry(
    provider: &dyn Provider,
    model: &Model,
    messages: &[Message],
    system: &str,
    tools: &Value,
    event_tx: &Sender<Envelope>,
) -> Result<StreamResponse, AgentError> {
    let mut retry = RetryState::new();
    loop {
        let (ptx, prx) = std::sync::mpsc::channel();
        let result = thread::scope(|s| {
            let forwarder = s.spawn(|| forward_provider_events(prx, event_tx));
            let result = provider.stream_message(model, messages, system, tools, &ptx);
            drop(ptx);
            let _ = forwarder.join();
            result
        });
        match result {
            Ok(r) => return Ok(r),
            Err(e) if e.is_retryable() => {
                let (attempt, delay) = retry.next_delay();
                let delay_ms = delay.as_millis() as u64;
                warn!(attempt, delay_ms, error = %e, "retryable, will retry");
                send(
                    event_tx,
                    AgentEvent::Retry {
                        attempt,
                        message: e.retry_message(),
                        delay_ms,
                    },
                )?;
                thread::sleep(delay);
            }
            Err(e) => return Err(e),
        }
    }
}

fn execute_tools(tool_calls: &[ParsedToolCall], ctx: &ToolContext) -> Vec<ToolDoneEvent> {
    std::thread::scope(|s| {
        let handles: Vec<_> = tool_calls
            .iter()
            .map(|parsed| {
                let tx = ctx.event_tx.clone();
                let tool_ctx = ToolContext {
                    tool_use_id: Some(&parsed.id),
                    user_response_rx: ctx.user_response_rx,
                    ..*ctx
                };
                let id = parsed.id.clone();
                s.spawn(move || {
                    let output = parsed.call.execute(&tool_ctx, id);
                    let _ = send(&tx, AgentEvent::ToolDone(output.clone()));
                    output
                })
            })
            .collect();

        tool_calls
            .iter()
            .zip(handles)
            .map(|(parsed, h)| {
                h.join().unwrap_or_else(|_| {
                    warn!(tool = parsed.call.name(), "tool thread panicked");
                    ToolDoneEvent::error(parsed.id.clone(), "tool thread panicked")
                })
            })
            .collect()
    })
}

enum TurnOutcome {
    Continue,
    Done(Option<StopReason>),
}

pub struct Agent<'a> {
    provider: &'a dyn Provider,
    model: &'a Model,
    history: &'a mut History,
    system: &'a str,
    event_tx: &'a Sender<Envelope>,
    tools: &'a Value,
    skills: &'a [Skill],
    mode: AgentMode,
    user_response_rx: Option<&'a Mutex<Receiver<String>>>,
    cmd_rx: Option<&'a Receiver<ExtractedCommand>>,
    total_usage: TokenUsage,
    num_turns: u32,
    recent_calls: RecentCalls,
    auto_compact: bool,
    interrupt_snapshot: Option<usize>,
}

impl<'a> Agent<'a> {
    pub fn new(
        provider: &'a dyn Provider,
        model: &'a Model,
        history: &'a mut History,
        system: &'a str,
        event_tx: &'a Sender<Envelope>,
        tools: &'a Value,
        skills: &'a [Skill],
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
            total_usage: TokenUsage::default(),
            num_turns: 0,
            recent_calls: RecentCalls::new(),
            auto_compact: auto_compact_enabled(),
            interrupt_snapshot: None,
        }
    }

    pub fn with_user_response_rx(mut self, rx: &'a Mutex<Receiver<String>>) -> Self {
        self.user_response_rx = Some(rx);
        self
    }

    pub fn with_cmd_rx(mut self, rx: &'a Receiver<ExtractedCommand>) -> Self {
        self.cmd_rx = Some(rx);
        self
    }

    pub fn run(&mut self, input: AgentInput) -> Result<(), AgentError> {
        let user_message = input.effective_message();
        self.history.push(Message::user(user_message.clone()));
        self.mode = input.mode;

        info!(
            model = %self.model.id,
            mode = ?self.mode,
            message_len = user_message.len(),
            "agent run started"
        );

        let result = self.run_loop();

        if matches!(result, Err(AgentError::Cancelled))
            && let Some(len) = self.interrupt_snapshot
        {
            self.history.truncate(len);
        }

        result
    }

    fn run_loop(&mut self) -> Result<(), AgentError> {
        loop {
            match self.turn()? {
                TurnOutcome::Continue => {}
                TurnOutcome::Done(stop_reason) => {
                    self.emit_done(stop_reason)?;
                    return Ok(());
                }
            }
        }
    }

    fn turn(&mut self) -> Result<TurnOutcome, AgentError> {
        let response = stream_with_retry(
            self.provider,
            self.model,
            self.history.as_slice(),
            self.system,
            self.tools,
            self.event_tx,
        )
        .inspect_err(|e| {
            error!(error = %e, model = %self.model.id, self.num_turns, "stream_message failed");
        })?;
        self.num_turns += 1;

        let has_tools = response.message.has_tool_calls();
        let stop_reason = response.stop_reason;
        let input_tokens = response.usage.input;

        info!(
            input_tokens,
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
        self.total_usage += response.usage;

        if has_tools {
            self.process_tool_calls(response)?;
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

        if self.try_auto_compact(input_tokens)? || self.check_interrupt()? {
            return Ok(TurnOutcome::Continue);
        }

        if has_tools {
            Ok(TurnOutcome::Continue)
        } else {
            Ok(TurnOutcome::Done(stop_reason))
        }
    }

    fn emit_turn_complete(&self, response: &StreamResponse) -> Result<(), AgentError> {
        send(
            self.event_tx,
            AgentEvent::TurnComplete {
                message: response.message.clone(),
                usage: response.usage,
                model: self.model.id.clone(),
                context_size: None,
            },
        )
    }

    fn emit_done(&self, stop_reason: Option<StopReason>) -> Result<(), AgentError> {
        info!(
            self.num_turns,
            total_input = self.total_usage.input,
            total_output = self.total_usage.output,
            "agent run completed"
        );
        send(
            self.event_tx,
            AgentEvent::Done {
                usage: self.total_usage,
                num_turns: self.num_turns,
                stop_reason,
            },
        )
    }

    fn process_tool_calls(&mut self, response: StreamResponse) -> Result<(), AgentError> {
        let (parsed, errors) =
            parse_tool_calls(response.message.tool_uses(), &mut self.recent_calls);

        self.history.push(response.message);

        for p in &parsed {
            send(
                self.event_tx,
                AgentEvent::ToolStart(p.call.start_event(p.id.clone())),
            )?;
        }

        let ctx = self.tool_context();
        let mut results = execute_tools(&parsed, &ctx);
        results.extend(errors);
        let tool_msg = tool_results(results);
        send(
            self.event_tx,
            AgentEvent::ToolResultsSubmitted {
                message: tool_msg.clone(),
            },
        )?;
        self.history.push(tool_msg);
        Ok(())
    }

    fn tool_context(&self) -> ToolContext<'_> {
        ToolContext {
            provider: self.provider,
            model: self.model,
            event_tx: self.event_tx,
            mode: &self.mode,
            tool_use_id: None,
            user_response_rx: self.user_response_rx,
            skills: self.skills,
        }
    }

    fn try_auto_compact(&mut self, input_tokens: u32) -> Result<bool, AgentError> {
        if !self.auto_compact || !is_overflow(input_tokens, self.model) {
            return Ok(false);
        }
        info!(input_tokens, "auto-compacting");
        send(self.event_tx, AgentEvent::AutoCompacting)?;
        self.total_usage +=
            compact_history(self.provider, self.model, self.history, self.event_tx)?;
        self.history
            .push(Message::user(CONTINUE_AFTER_COMPACT.into()));
        Ok(true)
    }

    fn check_interrupt(&mut self) -> Result<bool, AgentError> {
        let Some(rx) = self.cmd_rx else {
            return Ok(false);
        };
        let Ok(cmd) = rx.try_recv() else {
            return Ok(false);
        };
        match cmd {
            ExtractedCommand::Interrupt(input) => {
                self.interrupt_snapshot = Some(self.history.len());
                let msg = input.effective_message();
                let raw = input.message;
                let wrapped = format!(
                    "<user-interrupt>\nThe user sent a new message while you were working. Address it and continue.\n\n{msg}\n</user-interrupt>"
                );
                self.history.push(Message::user(wrapped));
                send(
                    self.event_tx,
                    AgentEvent::InterruptConsumed { message: raw },
                )?;
                Ok(true)
            }
            ExtractedCommand::Cancel => Err(AgentError::Cancelled),
            ExtractedCommand::Compact | ExtractedCommand::Ignore => Ok(false),
        }
    }
}

fn compact_history(
    provider: &dyn Provider,
    model: &Model,
    history: &mut History,
    event_tx: &Sender<Envelope>,
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
    )?;

    send(
        event_tx,
        AgentEvent::TurnComplete {
            message: response.message.clone(),
            usage: response.usage,
            model: model.id.clone(),
            context_size: Some(response.usage.output),
        },
    )?;

    let new_history = vec![
        Message::user("What did we do so far?".into()),
        response.message,
    ];
    history.replace(new_history);

    Ok(response.usage)
}

pub fn compact(
    provider: &dyn Provider,
    model: &Model,
    history: &mut History,
    event_tx: &Sender<Envelope>,
) -> Result<(), AgentError> {
    let usage = compact_history(provider, model, history, event_tx)?;

    send(
        event_tx,
        AgentEvent::Done {
            usage,
            num_turns: 1,
            stop_reason: None,
        },
    )?;

    Ok(())
}

fn is_overflow(input_tokens: u32, model: &Model) -> bool {
    let reserved = COMPACTION_BUFFER.min(model.max_output_tokens);
    let usable = model.context_window.saturating_sub(reserved);
    input_tokens >= usable
}

fn auto_compact_enabled() -> bool {
    std::env::var("MAKI_DISABLE_AUTOCOMPACT")
        .map(|v| v != "1" && v != "true")
        .unwrap_or(true)
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;
    use std::sync::mpsc;

    use test_case::test_case;

    use maki_providers::provider::Provider;
    use maki_providers::{ContentBlock, Message, Role, StopReason, StreamResponse, TokenUsage};

    use super::*;

    const PLAN_PATH: &str = ".maki/plans/123.md";

    fn default_model() -> Model {
        Model::from_spec("anthropic/claude-sonnet-4-20250514").unwrap()
    }

    #[test_case(&AgentMode::Build, false ; "build_excludes_plan")]
    #[test_case(&AgentMode::Plan(PLAN_PATH.into()), true ; "plan_includes_plan")]
    fn plan_section_presence(mode: &AgentMode, expect_plan: bool) {
        let vars = Vars::new().set("{cwd}", "/tmp").set("{platform}", "linux");
        let prompt = build_system_prompt(&vars, mode, "");
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
        fn stream_message(
            &self,
            _: &Model,
            _: &[Message],
            _: &str,
            _: &Value,
            _: &Sender<maki_providers::ProviderEvent>,
        ) -> Result<StreamResponse, AgentError> {
            let mut responses = self.responses.lock().unwrap();
            assert!(!responses.is_empty(), "MockProvider: no more responses");
            Ok(responses.remove(0))
        }

        fn list_models(&self) -> Result<Vec<String>, AgentError> {
            unimplemented!()
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

    fn run_and_collect(provider: &MockProvider, model: &Model) -> Vec<Envelope> {
        let input = AgentInput {
            message: "hello".into(),
            mode: AgentMode::Build,
            pending_plan: None,
        };
        let mut history = History::new(Vec::new());
        let (event_tx, event_rx) = mpsc::channel();
        let tools = serde_json::json!([]);
        let skills: Vec<crate::skill::Skill> = Vec::new();

        let mut agent = Agent::new(
            provider,
            model,
            &mut history,
            "system",
            &event_tx,
            &tools,
            &skills,
        );
        let _ = agent.run(input);
        drop(event_tx);
        event_rx.iter().collect()
    }

    fn run_agent(provider: &MockProvider) -> (u32, Option<StopReason>) {
        run_and_collect(provider, &default_model())
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
        let responses: Vec<_> = stops.iter().map(|s| text_response(*s)).collect();
        let provider = MockProvider::new(responses);
        let (turns, stop_reason) = run_agent(&provider);
        assert_eq!(turns, expected_turns);
        assert_eq!(stop_reason, expected_stop);
    }

    #[test]
    fn compact_replaces_history_with_summary() {
        let provider = MockProvider::new(vec![text_response(StopReason::EndTurn)]);
        let model = default_model();
        let (event_tx, _rx) = mpsc::channel();
        let mut history = History::new(vec![
            Message::user("first".into()),
            Message {
                role: Role::Assistant,
                content: vec![ContentBlock::Text {
                    text: "reply".into(),
                }],
            },
        ]);

        compact(&provider, &model, &mut history, &event_tx).unwrap();

        let msgs = history.as_slice();
        assert_eq!(msgs.len(), 2);
        assert!(matches!(msgs[0].role, Role::User));
        assert!(matches!(msgs[1].role, Role::Assistant));
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

    fn run_with_interrupt(
        provider: MockProvider,
        cmd_rx: &Receiver<ExtractedCommand>,
    ) -> (Vec<Message>, Vec<Envelope>) {
        let model = default_model();
        let input = AgentInput {
            message: "hello".into(),
            mode: AgentMode::Build,
            pending_plan: None,
        };
        let mut history = History::new(Vec::new());
        let (event_tx, event_rx) = mpsc::channel();
        let tools = serde_json::json!([]);
        let skills: Vec<crate::skill::Skill> = Vec::new();

        let mut agent = Agent::new(
            &provider,
            &model,
            &mut history,
            "system",
            &event_tx,
            &tools,
            &skills,
        )
        .with_cmd_rx(cmd_rx);
        let _ = agent.run(input);
        drop(event_tx);
        (history.as_slice().to_vec(), event_rx.iter().collect())
    }

    fn has_interrupt_event(events: &[Envelope]) -> bool {
        events
            .iter()
            .any(|e| matches!(e.event, AgentEvent::InterruptConsumed { .. }))
    }

    fn has_interrupt_in_history(history: &[Message]) -> bool {
        history.iter().any(|m| {
            m.content.iter().any(
                |b| matches!(b, ContentBlock::Text { text } if text.contains("<user-interrupt>")),
            )
        })
    }

    #[test]
    fn interrupt_injects_user_message_between_turns() {
        let (cmd_tx, cmd_rx) = mpsc::channel();
        cmd_tx
            .send(ExtractedCommand::Interrupt(AgentInput {
                message: "fix the bug".into(),
                mode: AgentMode::Build,
                pending_plan: None,
            }))
            .unwrap();

        let provider = MockProvider::new(vec![
            tool_call_response("glob", "t1"),
            text_response(StopReason::EndTurn),
        ]);
        let (history, events) = run_with_interrupt(provider, &cmd_rx);

        assert!(events.iter().any(|e| {
            matches!(e.event, AgentEvent::InterruptConsumed { ref message } if message == "fix the bug")
        }));
        assert!(has_interrupt_in_history(&history));
    }

    #[test]
    fn no_interrupt_when_channel_empty() {
        let (_cmd_tx, cmd_rx) = mpsc::channel::<ExtractedCommand>();

        let provider = MockProvider::new(vec![
            tool_call_response("glob", "t1"),
            text_response(StopReason::EndTurn),
        ]);
        let (history, events) = run_with_interrupt(provider, &cmd_rx);

        assert!(!has_interrupt_event(&events));
        assert!(!has_interrupt_in_history(&history));
    }

    #[test]
    fn interrupt_consumed_during_text_only_response() {
        let (cmd_tx, cmd_rx) = mpsc::channel();
        cmd_tx
            .send(ExtractedCommand::Interrupt(AgentInput {
                message: "new task".into(),
                mode: AgentMode::Build,
                pending_plan: None,
            }))
            .unwrap();

        let provider = MockProvider::new(vec![
            text_response(StopReason::EndTurn),
            text_response(StopReason::EndTurn),
        ]);
        let (history, events) = run_with_interrupt(provider, &cmd_rx);

        assert!(events.iter().any(|e| {
            matches!(e.event, AgentEvent::InterruptConsumed { ref message } if message == "new task")
        }));
        assert!(has_interrupt_in_history(history.as_slice()));
    }

    #[test]
    fn cancel_after_interrupt_removes_interrupt_from_history() {
        let (cmd_tx, cmd_rx) = mpsc::channel::<ExtractedCommand>();
        cmd_tx
            .send(ExtractedCommand::Interrupt(AgentInput {
                message: "there".into(),
                mode: AgentMode::Build,
                pending_plan: None,
            }))
            .unwrap();
        cmd_tx.send(ExtractedCommand::Cancel).unwrap();

        let model = default_model();
        let input = AgentInput {
            message: "hello".into(),
            mode: AgentMode::Build,
            pending_plan: None,
        };
        let mut history = History::new(Vec::new());
        let (event_tx, _event_rx) = mpsc::channel();
        let tools = serde_json::json!([]);
        let skills: Vec<crate::skill::Skill> = Vec::new();

        let provider = MockProvider::new(vec![
            tool_call_response("glob", "t1"),
            text_response(StopReason::EndTurn),
        ]);

        let mut agent = Agent::new(
            &provider,
            &model,
            &mut history,
            "system",
            &event_tx,
            &tools,
            &skills,
        )
        .with_cmd_rx(&cmd_rx);
        let result = agent.run(input);

        assert!(matches!(result, Err(AgentError::Cancelled)));
        assert!(
            !has_interrupt_in_history(history.as_slice()),
            "interrupt should be removed from history on cancel"
        );
    }

    fn small_context_model(context_window: u32, max_output_tokens: u32) -> Model {
        let mut model = default_model();
        model.context_window = context_window;
        model.max_output_tokens = max_output_tokens;
        model
    }

    #[test_case(179_999, 200_000, 20_000, false ; "below_threshold")]
    #[test_case(180_000, 200_000, 20_000, true  ; "at_threshold")]
    #[test_case(195_000, 200_000, 20_000, true  ; "above_threshold")]
    #[test_case(190_000, 200_000, 10_000, true  ; "small_max_output_uses_it_as_reserve")]
    #[test_case(0,       200_000, 20_000, false ; "zero_input")]
    #[test_case(100,     100,     20_000, true  ; "tiny_context_window")]
    fn overflow_detection(input: u32, ctx_window: u32, max_out: u32, expected: bool) {
        let model = small_context_model(ctx_window, max_out);
        assert_eq!(is_overflow(input, &model), expected);
    }

    #[test_case(true,  900, true  ; "enabled_and_over_threshold")]
    #[test_case(true,  100, false ; "enabled_but_below_threshold")]
    #[test_case(false, 900, false ; "disabled_even_over_threshold")]
    fn try_auto_compact_behavior(enabled: bool, input_tokens: u32, expected: bool) {
        let model = small_context_model(1000, 200);
        let provider = MockProvider::new(if expected {
            vec![text_response(StopReason::EndTurn)]
        } else {
            vec![]
        });
        let (event_tx, event_rx) = mpsc::channel();
        let mut history = History::new(vec![Message::user("go".into())]);
        let tools = serde_json::json!([]);
        let skills: Vec<crate::skill::Skill> = Vec::new();

        let mut agent = Agent::new(
            &provider,
            &model,
            &mut history,
            "system",
            &event_tx,
            &tools,
            &skills,
        );
        agent.auto_compact = enabled;

        let result = agent.try_auto_compact(input_tokens).unwrap();

        assert_eq!(result, expected);
        drop(event_tx);
        let has_compact_event = event_rx
            .iter()
            .any(|e| matches!(e.event, AgentEvent::AutoCompacting));
        assert_eq!(has_compact_event, expected);
    }
}

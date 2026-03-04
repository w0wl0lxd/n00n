use std::collections::VecDeque;
use std::fs;
use std::path::Path;
use std::process::Command;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::mpsc::{Receiver, Sender};
use std::thread;

use tracing::{debug, error, info, warn};

use serde_json::Value;

use crate::template::Vars;
use crate::tools::{ToolCall, ToolContext};
use crate::{
    AgentError, AgentEvent, AgentInput, AgentMode, Envelope, Message, TokenUsage, ToolDoneEvent,
};
use maki_providers::Model;
use maki_providers::StreamResponse;
use maki_providers::provider::Provider;
use maki_providers::retry::RetryState;

pub type SharedHistory = Arc<Mutex<Vec<Message>>>;

pub struct History {
    messages: Vec<Message>,
    shared: Option<SharedHistory>,
}

impl History {
    pub fn new(messages: Vec<Message>, shared: Option<SharedHistory>) -> Self {
        Self { messages, shared }
    }

    pub fn as_slice(&self) -> &[Message] {
        &self.messages
    }

    pub fn push(&mut self, msg: Message) {
        self.messages.push(msg);
    }

    pub fn commit(&self) {
        if let Some(shared) = &self.shared {
            let mut lock = shared.lock().unwrap_or_else(|e| e.into_inner());
            lock.clone_from(&self.messages);
        }
    }

    pub fn replace(&mut self, messages: Vec<Message>) {
        self.messages = messages;
        self.commit();
    }
}

const AGENTS_MD: &str = "AGENTS.md";
const DOOM_LOOP_THRESHOLD: usize = 3;
const MAX_CONTINUATION_TURNS: u32 = 3;
const DOOM_LOOP_MESSAGE: &str = "You have called this tool with identical input 3 times in a row. You are stuck in a loop. Break out and try a different approach.";

pub fn build_system_prompt(vars: &Vars, mode: &AgentMode, model: &Model) -> String {
    let mut out = crate::prompt::base_prompt(model.family()).to_string();

    out.push_str(&vars.apply(&format!(
        "\n\nEnvironment:\n- Working directory: {{cwd}}\n- Platform: {{platform}}\n- Date: {}",
        current_date(),
    )));

    append_agents_md(&mut out, &vars.apply("{cwd}"));

    if let AgentMode::Plan(plan_path) = mode {
        let plan_vars = Vars::new().set("{plan_path}", plan_path);
        out.push_str(&plan_vars.apply(crate::prompt::PLAN_PROMPT));
    }

    out
}

pub fn append_agents_md(system: &mut String, cwd: &str) {
    let agents_path = Path::new(cwd).join(AGENTS_MD);
    if let Ok(content) = fs::read_to_string(&agents_path) {
        system.push_str(&format!(
            "\n\nProject instructions ({AGENTS_MD}):\n{content}"
        ));
    }
}

fn current_date() -> String {
    let output = Command::new("date").arg("+%Y-%m-%d").output();
    match output {
        Ok(o) => String::from_utf8_lossy(&o.stdout).trim().to_string(),
        Err(_) => "unknown".to_string(),
    }
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
        match provider.stream_message(model, messages, system, tools, event_tx) {
            Ok(r) => return Ok(r),
            Err(e) if e.is_retryable() => {
                let (attempt, delay) = retry.next_delay();
                let delay_ms = delay.as_millis() as u64;
                warn!(attempt, delay_ms, error = %e, "retryable, will retry");
                event_tx.send(
                    AgentEvent::Retry {
                        attempt,
                        message: e.retry_message(),
                        delay_ms,
                    }
                    .into(),
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
                    let _ = tx.send(AgentEvent::ToolDone(output.clone()).into());
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

fn consume_interrupt(
    interrupt_rx: Option<&Receiver<String>>,
    history: &mut History,
    event_tx: &Sender<Envelope>,
) -> Result<bool, AgentError> {
    if let Some(rx) = interrupt_rx
        && let Ok(msg) = rx.try_recv()
    {
        let wrapped = format!(
            "<user-interrupt>\nThe user sent a new message while you were working. Address it and continue.\n\n{msg}\n</user-interrupt>"
        );
        history.push(Message::user(wrapped));
        history.commit();
        event_tx.send(AgentEvent::InterruptConsumed { message: msg }.into())?;
        return Ok(true);
    }
    Ok(false)
}

#[allow(clippy::too_many_arguments)]
pub fn run(
    provider: &dyn Provider,
    model: &Model,
    input: AgentInput,
    history: &mut History,
    system: &str,
    event_tx: &Sender<Envelope>,
    tools: &Value,
    user_response_rx: Option<&Mutex<Receiver<String>>>,
    interrupt_rx: Option<&Receiver<String>>,
) -> Result<(), AgentError> {
    let user_message = input.effective_message();
    history.push(Message::user(user_message.clone()));
    history.commit();
    let ctx = ToolContext {
        provider,
        model,
        event_tx,
        mode: &input.mode,
        tool_use_id: None,
        user_response_rx,
    };
    let mut total_usage = TokenUsage::default();
    let mut num_turns: u32 = 0;
    let mut recent_calls = RecentCalls::new();

    info!(
        model = %model.id,
        mode = ?input.mode,
        message_len = user_message.len(),
        "agent run started"
    );

    loop {
        let response =
            match stream_with_retry(provider, model, history.as_slice(), system, tools, event_tx) {
                Ok(r) => r,
                Err(e) => {
                    error!(error = %e, model = %model.id, num_turns, "stream_message failed");
                    return Err(e);
                }
            };
        num_turns += 1;

        let has_tools = response.message.has_tool_calls();

        info!(
            input_tokens = response.usage.input,
            output_tokens = response.usage.output,
            cache_creation = response.usage.cache_creation,
            cache_read = response.usage.cache_read,
            has_tools,
            num_turns,
            model = %model.id,
            stop_reason = response.stop_reason.as_deref().unwrap_or("none"),
            "API response received"
        );

        event_tx.send(
            AgentEvent::TurnComplete {
                message: response.message.clone(),
                usage: response.usage.clone(),
                model: model.id.clone(),
                context_size: None,
            }
            .into(),
        )?;

        total_usage += response.usage;

        if !has_tools {
            let truncated = response.stop_reason.as_deref() == Some("max_tokens");
            history.push(response.message);
            history.commit();

            if truncated && num_turns <= MAX_CONTINUATION_TURNS {
                warn!(num_turns, "response truncated (max_tokens), re-prompting");
                continue;
            }

            if consume_interrupt(interrupt_rx, history, event_tx)? {
                continue;
            }

            info!(
                num_turns,
                total_input = total_usage.input,
                total_output = total_usage.output,
                "agent run completed"
            );
            event_tx.send(
                AgentEvent::Done {
                    usage: total_usage,
                    num_turns,
                    stop_reason: response.stop_reason,
                }
                .into(),
            )?;
            return Ok(());
        }

        let (parsed, errors) = parse_tool_calls(response.message.tool_uses(), &mut recent_calls);

        history.push(response.message);

        for p in &parsed {
            event_tx.send(AgentEvent::ToolStart(p.call.start_event(p.id.clone())).into())?;
        }

        let mut tool_results = execute_tools(&parsed, &ctx);
        tool_results.extend(errors);
        let tool_msg = Message::tool_results(tool_results);
        event_tx.send(
            AgentEvent::ToolResultsSubmitted {
                message: tool_msg.clone(),
            }
            .into(),
        )?;
        history.push(tool_msg);
        history.commit();

        consume_interrupt(interrupt_rx, history, event_tx)?;
    }
}

pub fn compact(
    provider: &dyn Provider,
    model: &Model,
    history: &mut History,
    event_tx: &Sender<Envelope>,
) -> Result<(), AgentError> {
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

    event_tx.send(
        AgentEvent::TurnComplete {
            message: response.message.clone(),
            usage: response.usage.clone(),
            model: model.id.clone(),
            context_size: Some(response.usage.output),
        }
        .into(),
    )?;

    history.replace(vec![
        Message::user("What did we do so far?".into()),
        response.message,
    ]);

    event_tx.send(
        AgentEvent::Done {
            usage: response.usage,
            num_turns: 1,
            stop_reason: response.stop_reason,
        }
        .into(),
    )?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc;
    use std::sync::{Arc, Mutex};

    use test_case::test_case;

    use maki_providers::provider::Provider;
    use maki_providers::{ContentBlock, Message, Role, StreamResponse, TokenUsage};

    use super::*;

    const PLAN_PATH: &str = ".maki/plans/123.md";

    fn default_model() -> Model {
        Model::from_spec("anthropic/claude-sonnet-4-20250514").unwrap()
    }

    #[test_case(&AgentMode::Build, false ; "build_excludes_plan")]
    #[test_case(&AgentMode::Plan(PLAN_PATH.into()), true ; "plan_includes_plan")]
    fn plan_section_presence(mode: &AgentMode, expect_plan: bool) {
        let vars = Vars::new().set("{cwd}", "/tmp").set("{platform}", "linux");
        let prompt = build_system_prompt(&vars, mode, &default_model());
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
            _: &Sender<Envelope>,
        ) -> Result<StreamResponse, AgentError> {
            let mut responses = self.responses.lock().unwrap();
            assert!(!responses.is_empty(), "MockProvider: no more responses");
            Ok(responses.remove(0))
        }

        fn list_models(&self) -> Result<Vec<String>, AgentError> {
            unimplemented!()
        }
    }

    fn text_response(stop_reason: &str) -> StreamResponse {
        StreamResponse {
            message: Message {
                role: Role::Assistant,
                content: vec![ContentBlock::Text {
                    text: "response".into(),
                }],
            },
            usage: TokenUsage::default(),
            stop_reason: Some(stop_reason.into()),
        }
    }

    fn run_agent(provider: &MockProvider) -> (u32, Option<String>) {
        let model = default_model();
        let input = AgentInput {
            message: "hello".into(),
            mode: AgentMode::Build,
            pending_plan: None,
        };
        let mut history = History::new(Vec::new(), None);
        let (event_tx, event_rx) = mpsc::channel();
        let tools = serde_json::json!([]);

        let _ = run(
            provider,
            &model,
            input,
            &mut history,
            "system",
            &event_tx,
            &tools,
            None,
            None,
        );
        drop(event_tx);

        event_rx
            .iter()
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

    #[test_case(&["end_turn"],                                                     1, Some("end_turn")  ; "end_turn_completes")]
    #[test_case(&["max_tokens", "end_turn"],                                         2, Some("end_turn")  ; "max_tokens_continues")]
    #[test_case(&["max_tokens", "max_tokens", "max_tokens", "max_tokens"], 4, Some("max_tokens") ; "max_tokens_gives_up_after_limit")]
    fn turn_counting(stops: &[&str], expected_turns: u32, expected_stop: Option<&str>) {
        let responses: Vec<_> = stops.iter().map(|s| text_response(s)).collect();
        let provider = MockProvider::new(responses);
        let (turns, stop_reason) = run_agent(&provider);
        assert_eq!(turns, expected_turns);
        assert_eq!(stop_reason.as_deref(), expected_stop);
    }

    #[test]
    fn compact_replaces_history_with_summary_and_syncs_shared() {
        let shared: SharedHistory = Arc::new(Mutex::new(Vec::new()));
        let provider = MockProvider::new(vec![text_response("end_turn")]);
        let model = default_model();
        let (event_tx, _rx) = mpsc::channel();
        let mut history = History::new(
            vec![
                Message::user("first".into()),
                Message {
                    role: Role::Assistant,
                    content: vec![ContentBlock::Text {
                        text: "reply".into(),
                    }],
                },
            ],
            Some(Arc::clone(&shared)),
        );

        compact(&provider, &model, &mut history, &event_tx).unwrap();

        let msgs = history.as_slice();
        assert_eq!(msgs.len(), 2);
        assert!(matches!(msgs[0].role, Role::User));
        assert!(matches!(msgs[1].role, Role::Assistant));

        let shared_msgs = shared.lock().unwrap();
        assert_eq!(msgs.len(), shared_msgs.len());
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
            stop_reason: Some("tool_use".into()),
        }
    }

    fn run_with_interrupt(
        provider: MockProvider,
        interrupt_rx: &Receiver<String>,
    ) -> (Vec<Message>, Vec<Envelope>) {
        let model = default_model();
        let input = AgentInput {
            message: "hello".into(),
            mode: AgentMode::Build,
            pending_plan: None,
        };
        let mut history = History::new(Vec::new(), None);
        let (event_tx, event_rx) = mpsc::channel();
        let tools = serde_json::json!([]);

        let _ = run(
            &provider,
            &model,
            input,
            &mut history,
            "system",
            &event_tx,
            &tools,
            None,
            Some(interrupt_rx),
        );
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
        let (interrupt_tx, interrupt_rx) = mpsc::channel();
        interrupt_tx.send("fix the bug".into()).unwrap();

        let provider = MockProvider::new(vec![
            tool_call_response("glob", "t1"),
            text_response("end_turn"),
        ]);
        let (history, events) = run_with_interrupt(provider, &interrupt_rx);

        assert!(events.iter().any(|e| {
            matches!(e.event, AgentEvent::InterruptConsumed { ref message } if message == "fix the bug")
        }));
        assert!(has_interrupt_in_history(&history));
    }

    #[test]
    fn no_interrupt_when_channel_empty() {
        let (_interrupt_tx, interrupt_rx) = mpsc::channel();

        let provider = MockProvider::new(vec![
            tool_call_response("glob", "t1"),
            text_response("end_turn"),
        ]);
        let (history, events) = run_with_interrupt(provider, &interrupt_rx);

        assert!(!has_interrupt_event(&events));
        assert!(!has_interrupt_in_history(&history));
    }

    #[test]
    fn interrupt_consumed_during_text_only_response() {
        let (interrupt_tx, interrupt_rx) = mpsc::channel();
        interrupt_tx.send("new task".into()).unwrap();

        let provider =
            MockProvider::new(vec![text_response("end_turn"), text_response("end_turn")]);
        let (history, events) = run_with_interrupt(provider, &interrupt_rx);

        assert!(events.iter().any(|e| {
            matches!(e.event, AgentEvent::InterruptConsumed { ref message } if message == "new task")
        }));
        assert!(has_interrupt_in_history(history.as_slice()));
    }

    #[test]
    fn history_tool_pair_atomic_commit() {
        let shared: SharedHistory = Arc::new(Mutex::new(Vec::new()));
        let mut history = History::new(Vec::new(), Some(Arc::clone(&shared)));

        history.push(Message::user("go".into()));
        history.commit();

        let assistant_msg = Message {
            role: Role::Assistant,
            content: vec![ContentBlock::ToolUse {
                id: "t1".into(),
                name: "glob".into(),
                input: serde_json::json!({}),
            }],
        };
        history.push(assistant_msg);
        assert_eq!(shared.lock().unwrap().len(), 1, "tool_use not yet visible");

        let result_msg =
            Message::tool_results(vec![ToolDoneEvent::error("t1".into(), "cancelled")]);
        history.push(result_msg);
        history.commit();
        assert_eq!(shared.lock().unwrap().len(), 3, "both visible after commit");
    }

    #[test]
    fn history_replace_syncs_to_shared() {
        let shared: SharedHistory = Arc::new(Mutex::new(Vec::new()));
        let mut history =
            History::new(vec![Message::user("old".into())], Some(Arc::clone(&shared)));

        history.replace(vec![
            Message::user("new1".into()),
            Message::user("new2".into()),
        ]);

        let shared_msgs = shared.lock().unwrap();
        assert_eq!(shared_msgs.len(), 2);
        assert_eq!(history.as_slice().len(), 2);
    }
}

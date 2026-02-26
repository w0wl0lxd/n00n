use std::collections::VecDeque;
use std::fs;
use std::path::Path;
use std::process::Command;
use std::sync::mpsc::Sender;

use tracing::{info, warn};

use serde_json::Value;

use crate::template::Vars;
use crate::tools::{ToolCall, ToolContext};
use crate::{
    AgentError, AgentEvent, AgentInput, AgentMode, Envelope, Message, TokenUsage, ToolDoneEvent,
};
use maki_providers::Model;
use maki_providers::provider::Provider;

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

    let cwd = vars.apply("{cwd}");
    let agents_path = Path::new(cwd.as_ref()).join(AGENTS_MD);
    if let Ok(content) = fs::read_to_string(&agents_path) {
        out.push_str(&format!(
            "\n\nProject instructions ({AGENTS_MD}):\n{content}"
        ));
    }

    if let AgentMode::Plan(plan_path) = mode {
        let plan_vars = Vars::new().set("{plan_path}", plan_path);
        out.push_str(&plan_vars.apply(crate::prompt::PLAN_PROMPT));
    }

    out
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

fn execute_tools(tool_calls: &[ParsedToolCall], ctx: &ToolContext) -> Vec<ToolDoneEvent> {
    std::thread::scope(|s| {
        let handles: Vec<_> = tool_calls
            .iter()
            .map(|parsed| {
                let tx = ctx.event_tx.clone();
                let tool_ctx = ToolContext {
                    tool_use_id: Some(&parsed.id),
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
                    ToolDoneEvent::error(parsed.id.clone(), "tool thread panicked")
                })
            })
            .collect()
    })
}

pub fn run(
    provider: &dyn Provider,
    model: &Model,
    input: AgentInput,
    history: &mut Vec<Message>,
    system: &str,
    event_tx: &Sender<Envelope>,
    tools: &Value,
) -> Result<(), AgentError> {
    let user_message = input.effective_message();
    history.push(Message::user(user_message.clone()));
    let ctx = ToolContext {
        provider,
        model,
        event_tx,
        mode: &input.mode,
        tool_use_id: None,
    };
    let mut total_usage = TokenUsage::default();
    let mut num_turns: u32 = 0;
    let mut recent_calls = RecentCalls::new();

    loop {
        let response = provider.stream_message(model, history, system, tools, event_tx)?;
        num_turns += 1;

        let has_tools = response.message.has_tool_calls();

        info!(
            input_tokens = response.usage.input,
            output_tokens = response.usage.output,
            cache_creation = response.usage.cache_creation,
            cache_read = response.usage.cache_read,
            has_tools,
            "API response received"
        );

        event_tx.send(
            AgentEvent::TurnComplete {
                message: response.message.clone(),
                usage: response.usage.clone(),
                model: model.id.clone(),
            }
            .into(),
        )?;

        total_usage += response.usage;

        if !has_tools {
            let truncated = response.stop_reason.as_deref() == Some("max_tokens");
            history.push(response.message);

            if truncated && num_turns <= MAX_CONTINUATION_TURNS {
                warn!(num_turns, "response truncated (max_tokens), re-prompting");
                continue;
            }

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
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;
    use std::sync::mpsc;

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
        let mut history = Vec::new();
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

    fn max_tokens_responses(n: u32) -> Vec<StreamResponse> {
        (0..n).map(|_| text_response("max_tokens")).collect()
    }

    #[test_case(&["end_turn"],                 1, Some("end_turn")  ; "end_turn_completes")]
    #[test_case(&["max_tokens", "end_turn"],   2, Some("end_turn")  ; "max_tokens_continues")]
    fn turn_counting(stops: &[&str], expected_turns: u32, expected_stop: Option<&str>) {
        let responses = stops.iter().map(|s| text_response(s)).collect();
        let provider = MockProvider::new(responses);
        let (turns, stop_reason) = run_agent(&provider);
        assert_eq!(turns, expected_turns);
        assert_eq!(stop_reason.as_deref(), expected_stop);
    }

    #[test]
    fn max_tokens_gives_up_after_limit() {
        let provider = MockProvider::new(max_tokens_responses(MAX_CONTINUATION_TURNS + 1));
        let (turns, stop_reason) = run_agent(&provider);
        assert_eq!(turns, MAX_CONTINUATION_TURNS + 1);
        assert_eq!(stop_reason.as_deref(), Some("max_tokens"));
    }
}

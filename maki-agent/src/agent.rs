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
    ToolOutput,
};
use maki_providers::Model;
use maki_providers::provider::Provider;

const AGENTS_MD: &str = "AGENTS.md";
const DOOM_LOOP_THRESHOLD: usize = 3;
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

fn is_doom_loop(name: &str, input: &Value, recent: &VecDeque<(String, Value)>) -> bool {
    recent.len() >= DOOM_LOOP_THRESHOLD - 1
        && recent
            .iter()
            .rev()
            .take(DOOM_LOOP_THRESHOLD - 1)
            .all(|(n, i)| n == name && i == input)
}

fn parse_tool_calls<'a>(
    tool_uses: impl Iterator<Item = (&'a str, &'a str, &'a serde_json::Value)>,
    event_tx: &Sender<Envelope>,
    recent_calls: &mut VecDeque<(String, Value)>,
) -> (Vec<ParsedToolCall>, Vec<ToolDoneEvent>) {
    let mut parsed = Vec::new();
    let mut errors = Vec::new();

    for (id, name, input) in tool_uses {
        if is_doom_loop(name, input, recent_calls) {
            warn!(tool = %name, "doom loop detected, skipping execution");
            errors.push(ToolDoneEvent {
                id: id.to_owned(),
                tool: "unknown",
                output: ToolOutput::Plain(DOOM_LOOP_MESSAGE.into()),
                is_error: true,
            });
        } else {
            match ToolCall::from_api(name, input) {
                Ok(call) => parsed.push(ParsedToolCall {
                    id: id.to_owned(),
                    call,
                }),
                Err(e) => {
                    let msg = format!("failed to parse tool {name}: {e}");
                    warn!(tool = %name, error = %e, "failed to parse tool call");
                    let _ = event_tx.send(
                        AgentEvent::Error {
                            message: msg.clone(),
                        }
                        .into(),
                    );
                    errors.push(ToolDoneEvent {
                        id: id.to_owned(),
                        tool: "unknown",
                        output: ToolOutput::Plain(msg),
                        is_error: true,
                    });
                }
            }
        }
        recent_calls.push_back((name.to_owned(), input.clone()));
        if recent_calls.len() > DOOM_LOOP_THRESHOLD {
            recent_calls.pop_front();
        }
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
                h.join().unwrap_or_else(|_| ToolDoneEvent {
                    id: parsed.id.clone(),
                    tool: "unknown",
                    output: ToolOutput::Plain("tool thread panicked".into()),
                    is_error: true,
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
    let mut recent_calls: VecDeque<(String, Value)> = VecDeque::new();

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
            history.push(response.message);
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

        let (parsed, errors) =
            parse_tool_calls(response.message.tool_uses(), event_tx, &mut recent_calls);

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
    use super::*;
    use test_case::test_case;

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

    fn recent(entries: &[(&str, Value)]) -> VecDeque<(String, Value)> {
        entries
            .iter()
            .map(|(n, v)| (n.to_string(), v.clone()))
            .collect()
    }

    #[test]
    fn doom_loop_triggers_after_threshold() {
        let input = serde_json::json!({"path": "/tmp/a.txt"});
        assert!(!is_doom_loop("read", &input, &VecDeque::new()));
        assert!(!is_doom_loop(
            "read",
            &input,
            &recent(&[("read", input.clone())])
        ));
        assert!(is_doom_loop(
            "read",
            &input,
            &recent(&[("read", input.clone()), ("read", input.clone())])
        ));
    }

    #[test]
    fn doom_loop_no_false_positives() {
        let input = serde_json::json!({"path": "/a"});
        let other = serde_json::json!({"path": "/b"});

        // different input breaks chain
        assert!(!is_doom_loop(
            "read",
            &input,
            &recent(&[("read", input.clone()), ("read", other.clone())])
        ));
        // different tool name breaks chain
        assert!(!is_doom_loop(
            "grep",
            &input,
            &recent(&[("glob", input.clone()), ("glob", input.clone())])
        ));
        // interrupted chain in tail
        assert!(!is_doom_loop(
            "bash",
            &input,
            &recent(&[
                ("bash", input.clone()),
                ("bash", other),
                ("bash", input.clone())
            ])
        ));
    }
}

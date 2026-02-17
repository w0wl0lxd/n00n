use std::env;
use std::fs;
use std::path::Path;
use std::process::Command;
use std::sync::mpsc::Sender;

use tracing::{info, warn};

use serde_json::Value;

use crate::tools::{ToolCall, ToolContext};
use crate::{AgentError, AgentEvent, AgentInput, AgentMode, Message, TokenUsage, ToolDoneEvent};
use maki_providers::Model;
use maki_providers::provider::Provider;

const AGENTS_MD: &str = "AGENTS.md";

pub fn build_system_prompt(cwd: &str, mode: &AgentMode, model: &Model) -> String {
    let mut out = crate::prompt::base_prompt(model.family()).to_string();

    out.push_str(&format!(
        "\n\nEnvironment:\n- Working directory: {cwd}\n- Platform: {}\n- Date: {}",
        env::consts::OS,
        current_date(),
    ));

    let agents_path = Path::new(cwd).join(AGENTS_MD);
    if let Ok(content) = fs::read_to_string(&agents_path) {
        out.push_str(&format!(
            "\n\nProject instructions ({AGENTS_MD}):\n{content}"
        ));
    }

    if let AgentMode::Plan(plan_path) = mode {
        out.push_str(&crate::prompt::PLAN_PROMPT.replace("{plan_path}", plan_path));
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

fn parse_tool_calls<'a>(
    tool_uses: impl Iterator<Item = (&'a str, &'a str, &'a serde_json::Value)>,
    event_tx: &Sender<AgentEvent>,
) -> Vec<ParsedToolCall> {
    tool_uses
        .filter_map(|(id, name, input)| match ToolCall::from_api(name, input) {
            Ok(call) => Some(ParsedToolCall {
                id: id.to_owned(),
                call,
            }),
            Err(e) => {
                warn!(tool = %name, error = %e, "failed to parse tool call");
                let _ = event_tx.send(AgentEvent::Error {
                    message: format!("failed to parse tool {name}: {e}"),
                });
                None
            }
        })
        .collect()
}

fn execute_tools(tool_calls: &[ParsedToolCall], ctx: &ToolContext) -> Vec<(String, ToolDoneEvent)> {
    std::thread::scope(|s| {
        let handles: Vec<_> = tool_calls
            .iter()
            .map(|parsed| {
                let tx = ctx.event_tx.clone();
                s.spawn(move || {
                    let output = parsed.call.execute(ctx);
                    let _ = tx.send(AgentEvent::ToolDone(output.clone()));
                    output
                })
            })
            .collect();

        tool_calls
            .iter()
            .zip(handles)
            .map(|(parsed, h)| {
                let output = h.join().unwrap_or_else(|_| ToolDoneEvent {
                    tool: "unknown",
                    content: "tool thread panicked".into(),
                    is_error: true,
                });
                (parsed.id.clone(), output)
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
    event_tx: &Sender<AgentEvent>,
    tools_override: Option<Value>,
) -> Result<(), AgentError> {
    history.push(Message::user(input.effective_message()));
    let tools = tools_override.unwrap_or_else(ToolCall::definitions);
    let ctx = ToolContext {
        provider,
        model,
        event_tx,
        mode: &input.mode,
    };
    let mut total_usage = TokenUsage::default();
    let mut num_turns: u32 = 0;

    loop {
        let response = provider.stream_message(model, history, system, &tools, event_tx)?;
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

        event_tx.send(AgentEvent::TurnComplete {
            message: response.message.clone(),
            usage: response.usage.clone(),
            model: model.id.clone(),
        })?;

        total_usage += response.usage;

        if !has_tools {
            history.push(response.message);
            event_tx.send(AgentEvent::Done {
                usage: total_usage,
                num_turns,
                stop_reason: response.stop_reason,
            })?;
            break;
        }

        let parsed = parse_tool_calls(response.message.tool_uses(), event_tx);

        history.push(response.message);

        for p in &parsed {
            event_tx.send(AgentEvent::ToolStart(p.call.start_event()))?;
        }

        let tool_results = execute_tools(&parsed, &ctx);
        let tool_msg = Message::tool_results(tool_results);
        event_tx.send(AgentEvent::ToolResultsSubmitted {
            message: tool_msg.clone(),
        })?;
        history.push(tool_msg);
    }

    Ok(())
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
        let prompt = build_system_prompt("/tmp", mode, &default_model());
        assert_eq!(prompt.contains("Plan Mode"), expect_plan);
        if expect_plan {
            assert!(prompt.contains(PLAN_PATH));
        }
    }
}

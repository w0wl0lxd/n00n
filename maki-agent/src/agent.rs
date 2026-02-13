use std::env;
use std::fs;
use std::path::Path;
use std::process::Command;
use std::sync::mpsc::Sender;

use tracing::info;

use crate::client;
use crate::{AgentError, AgentEvent, Message, ToolOutput};

const AGENTS_MD: &str = "AGENTS.md";

const SYSTEM_PROMPT_STATIC: &str = "\
You are Maki, a coding assistant. You help with software engineering tasks.
- Use tools to interact with the filesystem and execute commands
- Read files before editing them
- Be concise
- When done, summarize what you did";

pub fn build_system_prompt(cwd: &str) -> String {
    let mut prompt = SYSTEM_PROMPT_STATIC.to_string();
    prompt.push_str(&format!(
        "\n\nEnvironment:\n- Working directory: {cwd}\n- Platform: {}\n- Date: {}",
        env::consts::OS,
        current_date(),
    ));

    let agents_path = Path::new(cwd).join(AGENTS_MD);
    if let Ok(content) = fs::read_to_string(&agents_path) {
        prompt.push_str(&format!(
            "\n\nProject instructions ({AGENTS_MD}):\n{content}"
        ));
    }

    prompt
}

fn current_date() -> String {
    let output = Command::new("date").arg("+%Y-%m-%d").output();
    match output {
        Ok(o) => String::from_utf8_lossy(&o.stdout).trim().to_string(),
        Err(_) => "unknown".to_string(),
    }
}

pub fn run(
    user_msg: String,
    history: &mut Vec<Message>,
    system: &str,
    event_tx: &Sender<AgentEvent>,
) -> Result<(), AgentError> {
    history.push(Message::user(user_msg));

    loop {
        let response = client::stream_message(history, system, event_tx)?;

        info!(
            input_tokens = response.input_tokens,
            output_tokens = response.output_tokens,
            tool_count = response.tool_calls.len(),
            "API response received"
        );

        history.push(response.message);

        if response.tool_calls.is_empty() {
            event_tx.send(AgentEvent::Done {
                input_tokens: response.input_tokens,
                output_tokens: response.output_tokens,
            })?;
            break;
        }

        for pending in &response.tool_calls {
            event_tx.send(AgentEvent::ToolStart {
                name: pending.call.name().to_string(),
                input: pending.call.input_summary(),
            })?;
        }

        let outputs: Vec<_> = std::thread::scope(|s| {
            response
                .tool_calls
                .iter()
                .map(|p| s.spawn(|| p.call.execute()))
                .collect::<Vec<_>>()
                .into_iter()
                .map(|h| {
                    h.join()
                        .unwrap_or_else(|_| ToolOutput::err("tool thread panicked".into()))
                })
                .collect()
        });

        let tool_results = response
            .tool_calls
            .iter()
            .zip(outputs)
            .map(|(pending, output)| {
                event_tx.send(AgentEvent::ToolDone {
                    name: pending.call.name().to_string(),
                    output: output.content.clone(),
                })?;
                Ok((pending.id.clone(), output))
            })
            .collect::<Result<Vec<_>, AgentError>>()?;
        history.push(Message::tool_results(tool_results));
    }

    Ok(())
}

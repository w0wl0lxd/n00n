use std::env;
use std::fs;
use std::path::Path;
use std::process::Command;
use std::sync::mpsc::Sender;

use tracing::{debug, info};

use crate::client;
use crate::{AgentError, AgentEvent, Message};

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

        for pending in response.tool_calls {
            let name = pending.call.name().to_string();

            event_tx.send(AgentEvent::ToolStart {
                name: name.clone(),
                input: pending.call.input_summary(),
            })?;

            debug!(tool = %name, "executing tool");
            let output = pending.call.execute();
            let output_content = output.content.clone();

            history.push(Message::tool_result(pending.id, output));

            event_tx.send(AgentEvent::ToolDone {
                name,
                output: output_content,
            })?;
        }
    }

    Ok(())
}

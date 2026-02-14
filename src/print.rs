//! Non-interactive (headless) mode: `maki "prompt" --print`.
//!
//! # Claude Code compatibility
//!
//! `--print` and `--output-format text|json|stream-json` match Claude Code on
//! purpose. Tools and scripts that consume Claude Code output should work with
//! ours unchanged.
//!
//! Rules:
//! - JSON fields in `PrintResult` must be a strict subset of Claude Code's.
//!   Don't add maki-specific fields.
//! - `StreamJson` is JSONL, one object per line, same shape as Claude Code.
//! - `Text` prints the raw response, nothing else.
//!
//! We can adopt new fields when Claude Code adds them, but we don't invent our
//! own. Check Claude Code's docs/source before changing anything here.

use std::env;
use std::io::{self, Read};
use std::sync::mpsc;
use std::thread;
use std::time::Instant;

use clap::ValueEnum;
use color_eyre::Result;
use maki_agent::pricing::SONNET_4;
use maki_agent::{AgentEvent, AgentInput, AgentMode, TokenUsage, agent};
use serde::Serialize;
use tracing::error;
use uuid::Uuid;

#[derive(Clone, ValueEnum)]
pub enum OutputFormat {
    Text,
    Json,
    StreamJson,
}

#[derive(Serialize)]
struct PrintResult {
    #[serde(rename = "type")]
    result_type: &'static str,
    subtype: &'static str,
    is_error: bool,
    duration_ms: u128,
    num_turns: u32,
    result: String,
    stop_reason: Option<String>,
    session_id: String,
    total_cost_usd: f64,
    usage: TokenUsage,
}

pub fn run(prompt_arg: Option<String>, format: OutputFormat) -> Result<()> {
    let prompt = match prompt_arg {
        Some(p) => p,
        None => {
            let mut buf = String::new();
            io::stdin().read_to_string(&mut buf)?;
            buf
        }
    };

    let cwd = env::current_dir()?.to_string_lossy().to_string();
    let mode = AgentMode::Build;
    let system = agent::build_system_prompt(&cwd, &mode);

    let (event_tx, event_rx) = mpsc::channel::<AgentEvent>();
    let input = AgentInput {
        message: prompt,
        mode,
        pending_plan: None,
    };

    let session_id = Uuid::new_v4().to_string();
    let start = Instant::now();

    thread::spawn(move || {
        let mut history = Vec::new();
        if let Err(e) = agent::run(input, &mut history, &system, &event_tx) {
            error!(error = %e, "agent error");
            let _ = event_tx.send(AgentEvent::Error {
                message: e.to_string(),
            });
        }
    });

    let mut result_text = String::new();
    let mut is_error = false;
    let mut num_turns: u32 = 0;
    let mut usage = TokenUsage::default();
    let mut stop_reason: Option<String> = None;

    for event in event_rx {
        if let OutputFormat::StreamJson = format {
            let done = matches!(event, AgentEvent::Done { .. });
            println!("{}", serde_json::to_string(&event)?);
            if done {
                break;
            }
            continue;
        }

        match &event {
            AgentEvent::TextDelta { text } => {
                result_text.push_str(text);
            }
            AgentEvent::Done {
                usage: u,
                num_turns: turns,
                stop_reason: sr,
            } => {
                num_turns = *turns;
                usage = u.clone();
                stop_reason = sr.clone();
            }
            AgentEvent::Error { message } => {
                is_error = true;
                result_text = message.clone();
            }
            _ => {}
        }

        if matches!(event, AgentEvent::Done { .. } | AgentEvent::Error { .. }) {
            break;
        }
    }

    let duration_ms = start.elapsed().as_millis();

    match format {
        OutputFormat::Text => {
            print!("{result_text}");
        }
        OutputFormat::Json => {
            let total_cost_usd = usage.cost(&SONNET_4);
            let result = PrintResult {
                result_type: "result",
                subtype: if is_error { "error" } else { "success" },
                is_error,
                duration_ms,
                num_turns,
                result: result_text,
                stop_reason,
                session_id,
                total_cost_usd,
                usage,
            };
            println!("{}", serde_json::to_string(&result)?);
        }
        OutputFormat::StreamJson => {}
    }

    Ok(())
}

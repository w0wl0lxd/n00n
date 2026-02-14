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
use maki_agent::client::MODEL;
use maki_agent::pricing::SONNET_4;
use maki_agent::{AgentEvent, AgentInput, AgentMode, TokenUsage, agent};
use serde::Serialize;
use serde_json::Value;
use tracing::error;
use uuid::Uuid;

const TOOLS: &[&str] = &["bash", "read", "write", "glob", "grep", "todowrite"];

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

#[derive(Serialize)]
struct InitEvent<'a> {
    #[serde(rename = "type")]
    event_type: &'static str,
    subtype: &'static str,
    cwd: &'a str,
    session_id: &'a str,
    tools: &'static [&'static str],
    model: &'static str,
}

#[derive(Serialize)]
struct AssistantEvent<'a> {
    #[serde(rename = "type")]
    event_type: &'static str,
    message: AssistantMessage<'a>,
    session_id: &'a str,
}

#[derive(Serialize)]
struct AssistantMessage<'a> {
    model: &'static str,
    role: &'static str,
    content: &'a Value,
    usage: &'a TokenUsage,
}

#[derive(Serialize)]
struct UserEvent<'a> {
    #[serde(rename = "type")]
    event_type: &'static str,
    message: UserMessage<'a>,
    session_id: &'a str,
}

#[derive(Serialize)]
struct UserMessage<'a> {
    role: &'static str,
    content: &'a Value,
}

enum VerboseOutput {
    StreamJson,
    Json(Vec<Value>),
}

impl VerboseOutput {
    fn emit(&mut self, value: &impl Serialize) -> Result<()> {
        match self {
            Self::StreamJson => println!("{}", serde_json::to_string(value)?),
            Self::Json(events) => events.push(serde_json::to_value(value)?),
        }
        Ok(())
    }
}

pub fn run(prompt_arg: Option<String>, format: OutputFormat, verbose: bool) -> Result<()> {
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

    let is_stream_json = matches!(format, OutputFormat::StreamJson);
    let mut verbose_out = verbose.then(|| match format {
        OutputFormat::StreamJson => VerboseOutput::StreamJson,
        _ => VerboseOutput::Json(Vec::new()),
    });

    if let Some(out) = &mut verbose_out {
        out.emit(&InitEvent {
            event_type: "system",
            subtype: "init",
            cwd: &cwd,
            session_id: &session_id,
            tools: TOOLS,
            model: MODEL,
        })?;
    }

    let mut result_text = String::new();
    let mut is_error = false;
    let mut num_turns: u32 = 0;
    let mut usage = TokenUsage::default();
    let mut stop_reason: Option<String> = None;

    for event in event_rx {
        if verbose_out.is_none() && is_stream_json {
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
                if is_stream_json {
                    println!("{}", serde_json::to_string(&event)?);
                }
            }
            AgentEvent::ToolStart(_) | AgentEvent::ToolDone(_) => {
                if is_stream_json {
                    println!("{}", serde_json::to_string(&event)?);
                }
            }
            AgentEvent::TurnComplete {
                message,
                usage: turn_usage,
                model,
            } => {
                if let Some(out) = &mut verbose_out {
                    let content_value = serde_json::to_value(&message.content)?;
                    out.emit(&AssistantEvent {
                        event_type: "assistant",
                        message: AssistantMessage {
                            model,
                            role: "assistant",
                            content: &content_value,
                            usage: turn_usage,
                        },
                        session_id: &session_id,
                    })?;
                }
            }
            AgentEvent::ToolResultsSubmitted { message } => {
                if let Some(out) = &mut verbose_out {
                    let content_value = serde_json::to_value(&message.content)?;
                    out.emit(&UserEvent {
                        event_type: "user",
                        message: UserMessage {
                            role: "user",
                            content: &content_value,
                        },
                        session_id: &session_id,
                    })?;
                }
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
        }

        if matches!(event, AgentEvent::Done { .. } | AgentEvent::Error { .. }) {
            break;
        }
    }

    let duration_ms = start.elapsed().as_millis();
    let total_cost_usd = usage.cost(&SONNET_4);

    match format {
        OutputFormat::Text => {
            print!("{result_text}");
        }
        OutputFormat::Json | OutputFormat::StreamJson => {
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
            match verbose_out {
                Some(VerboseOutput::Json(mut events)) => {
                    events.push(serde_json::to_value(&result)?);
                    println!("{}", serde_json::to_string(&events)?);
                }
                Some(VerboseOutput::StreamJson) => println!("{}", serde_json::to_string(&result)?),
                None if is_stream_json => {}
                None => println!("{}", serde_json::to_string(&result)?),
            }
        }
    }

    Ok(())
}

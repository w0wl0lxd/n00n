//! Non-interactive (headless) mode: `n00n "prompt" --print`.
//!
//! Wire format intentionally matches Claude Code so existing scripts work
//! unchanged. Keep `PrintResult` fields a strict subset of theirs. `StreamJson`
//! is JSONL with the same shape, `Text` prints the raw response only.
//!
//! We adopt new fields when Claude Code adds them but never invent our own.
//! Check their docs before changing anything here.

use std::io::{self, Read};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use clap::ValueEnum;
use color_eyre::Result;
use color_eyre::eyre::{Context, eyre};
use n00n_agent::headless::{HeadlessHandle, HeadlessParams};
use n00n_agent::tools::QUESTION_TOOL_NAME;
use n00n_agent::{AgentConfig, AgentEvent, Envelope, ImageSource, PermissionsConfig};
use n00n_lua::EventHandle;
use n00n_providers::model::Model;
use n00n_providers::{OpenAiOptions, StopReason, TokenUsage};
use n00n_storage::id::SessionRef;
use serde::Serialize;
use serde_json::Value;

const AGENT_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(5);
pub(crate) const LOCAL_TOKEN_USAGE_NOTE: &str = "usage.input is fresh input; usage.cache_read and usage.cache_creation are cached context. Local token counts are not ChatGPT subscription quota.";

// Fails fast: silently dropping an image the caller explicitly attached
// would be worse than erroring.
fn load_images(paths: &[PathBuf]) -> Result<Vec<ImageSource>> {
    paths
        .iter()
        .map(|path| {
            let media_type = n00n_ui::image::media_type_for(path)
                .ok_or_else(|| eyre!("unsupported image type: {}", path.display()))?;
            n00n_ui::image::load_file_image(path, media_type)
                .map_err(|e| eyre!("failed to load image: {e}"))
        })
        .collect()
}

#[derive(Clone, Copy, ValueEnum)]
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
    stop_reason: Option<StopReason>,
    session_id: SessionRef,
    total_cost_usd: f64,
    usage: TokenUsage,
    usage_note: &'static str,
}

#[derive(Serialize)]
struct InitEvent<'a> {
    #[serde(rename = "type")]
    event_type: &'static str,
    subtype: &'static str,
    cwd: &'a str,
    session_id: &'a SessionRef,
    tools: &'a [String],
    model: &'a str,
}

#[derive(Serialize)]
struct AssistantEvent<'a> {
    #[serde(rename = "type")]
    event_type: &'static str,
    message: AssistantMessage<'a>,
    session_id: &'a SessionRef,
    #[serde(skip_serializing_if = "Option::is_none")]
    parent_tool_use_id: Option<&'a str>,
}

#[derive(Serialize)]
struct AssistantMessage<'a> {
    model: &'a str,
    role: &'static str,
    content: &'a Value,
    usage: &'a TokenUsage,
}

#[derive(Serialize)]
struct UserEvent<'a> {
    #[serde(rename = "type")]
    event_type: &'static str,
    message: UserMessage<'a>,
    session_id: &'a SessionRef,
    #[serde(skip_serializing_if = "Option::is_none")]
    parent_tool_use_id: Option<&'a str>,
}

#[derive(Serialize)]
struct UserMessage<'a> {
    role: &'static str,
    content: &'a Value,
}

#[derive(Serialize)]
struct RetryEvent<'a> {
    #[serde(rename = "type")]
    event_type: &'static str,
    subtype: &'static str,
    attempt: u32,
    retry_delay_ms: u64,
    error: &'a str,
    session_id: &'a SessionRef,
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

pub struct PrintArgs<'a> {
    pub prompt_arg: Option<String>,
    pub image_paths: &'a [PathBuf],
    pub format: OutputFormat,
    pub verbose: bool,
    pub config: AgentConfig,
    pub permissions_config: PermissionsConfig,
    pub timeouts: n00n_providers::Timeouts,
    pub openai_options: OpenAiOptions,
    pub lua_handle: Option<&'a EventHandle>,
    pub fast: bool,
    pub workflow: bool,
}

struct PrintState<'a> {
    verbose_out: &'a mut Option<VerboseOutput>,
    result_text: &'a mut String,
    result_checkpoint: &'a mut usize,
    is_error: &'a mut bool,
    num_turns: &'a mut u32,
    usage: &'a mut TokenUsage,
    stop_reason: &'a mut Option<StopReason>,
    session_id: &'a SessionRef,
}

struct ResultSummary {
    is_error: bool,
    duration_ms: u128,
    num_turns: u32,
    stop_reason: Option<StopReason>,
    session_id: SessionRef,
    usage: TokenUsage,
    total_cost_usd: f64,
}

fn load_inputs(
    prompt_arg: Option<String>,
    image_paths: &[PathBuf],
) -> Result<(String, Vec<ImageSource>)> {
    let prompt = if let Some(p) = prompt_arg {
        p
    } else {
        let mut buf = String::new();
        io::stdin().read_to_string(&mut buf).context("read stdin")?;
        buf
    };
    let images = load_images(image_paths)?;
    Ok((prompt, images))
}

fn init_verbose_output(format: OutputFormat, verbose: bool) -> Option<VerboseOutput> {
    match format {
        OutputFormat::StreamJson => Some(VerboseOutput::StreamJson),
        _ if verbose => Some(VerboseOutput::Json(Vec::new())),
        _ => None,
    }
}

fn emit_init_event(
    verbose_out: &mut Option<VerboseOutput>,
    cwd: &str,
    session_id: &SessionRef,
    tool_names: &[String],
    model_id: &str,
) -> Result<()> {
    if let Some(out) = verbose_out {
        out.emit(&InitEvent {
            event_type: "system",
            subtype: "init",
            cwd,
            session_id,
            tools: tool_names,
            model: model_id,
        })?;
    }
    Ok(())
}

pub fn run(model: &Model, args: PrintArgs<'_>) -> Result<()> {
    let PrintArgs {
        prompt_arg,
        image_paths,
        format,
        verbose,
        config,
        permissions_config,
        timeouts,
        openai_options,
        lua_handle,
        fast,
        workflow,
    } = args;

    let (prompt, images) = load_inputs(prompt_arg, image_paths)?;

    let prompt_slots = lua_handle.map_or_else(Default::default, EventHandle::collect_prompt_slots);

    let cwd = std::env::current_dir().unwrap_or_else(|_| ".".into());
    let (mcp_handle, mcp_config_errors) = smol::block_on(n00n_agent::mcp::start(&cwd));
    if !mcp_config_errors.is_empty() {
        eprintln!("MCP config error: {mcp_config_errors}");
    }

    let handle = n00n_agent::headless::spawn(HeadlessParams {
        model: model.clone(),
        config,
        permissions_config,
        timeouts,
        openai_options,
        prompt,
        images,
        prompt_slots,
        excluded_tools: vec![QUESTION_TOOL_NAME],
        mcp_handle,
        initial_wd: cwd,
        fast,
        workflow,
    });

    let HeadlessHandle {
        event_rx,
        tool_names,
        session_id,
        cwd,
        task,
    } = handle;
    let start = Instant::now();

    let mut verbose_out = init_verbose_output(format, verbose);
    emit_init_event(&mut verbose_out, &cwd, &session_id, &tool_names, &model.id)?;

    let mut result_text = String::new();
    let mut result_checkpoint = 0;
    let mut is_error = false;
    let mut num_turns: u32 = 0;
    let mut usage = TokenUsage::default();
    let mut stop_reason: Option<StopReason> = None;

    let mut state = PrintState {
        verbose_out: &mut verbose_out,
        result_text: &mut result_text,
        result_checkpoint: &mut result_checkpoint,
        is_error: &mut is_error,
        num_turns: &mut num_turns,
        usage: &mut usage,
        stop_reason: &mut stop_reason,
        session_id: &session_id,
    };

    while let Ok(envelope) = smol::block_on(event_rx.recv_async()) {
        let Envelope {
            ref event,
            ref subagent,
            ..
        } = envelope;
        let parent_tool_use_id = subagent.as_ref().map(|s| s.parent_tool_use_id.as_str());
        if handle_print_event(event, parent_tool_use_id, &mut state)? {
            break;
        }
    }
    smol::block_on(async {
        futures_lite::future::or(task, async {
            smol::Timer::after(AGENT_SHUTDOWN_TIMEOUT).await;
        })
        .await;
    });

    let duration_ms = start.elapsed().as_millis();
    let total_cost_usd = usage.cost(&model.pricing, fast);
    output_result(
        format,
        std::mem::take(&mut verbose_out),
        std::mem::take(&mut result_text),
        ResultSummary {
            is_error,
            duration_ms,
            num_turns,
            stop_reason,
            session_id,
            usage,
            total_cost_usd,
        },
    )
}

fn handle_print_event(
    event: &AgentEvent,
    parent_tool_use_id: Option<&str>,
    state: &mut PrintState<'_>,
) -> Result<bool> {
    match event {
        AgentEvent::TextDelta { text } => {
            if parent_tool_use_id.is_none() {
                state.result_text.push_str(text);
            }
        }
        AgentEvent::ThinkingDelta { .. }
        | AgentEvent::ToolPending { .. }
        | AgentEvent::ToolStart(_)
        | AgentEvent::ToolOutput { .. }
        | AgentEvent::ToolDone(_)
        | AgentEvent::QueueItemConsumed { .. }
        | AgentEvent::AutoCompacting
        | AgentEvent::CompactionDone
        | AgentEvent::AuthRequired
        | AgentEvent::PermissionRequest { .. }
        | AgentEvent::SubagentInputRequired { .. }
        | AgentEvent::SubagentHistory { .. }
        | AgentEvent::ToolSnapshot { .. }
        | AgentEvent::ToolHeaderSnapshot { .. }
        | AgentEvent::LiveToolBuf { .. }
        | AgentEvent::Nudge
        | AgentEvent::PromptProgress { .. } => {}
        AgentEvent::Retry {
            attempt,
            message,
            delay_ms,
        } => {
            if parent_tool_use_id.is_none() {
                state.result_text.truncate(*state.result_checkpoint);
            }
            if let Some(out) = state.verbose_out {
                out.emit(&RetryEvent {
                    event_type: "system",
                    subtype: "api_retry",
                    attempt: *attempt,
                    retry_delay_ms: *delay_ms,
                    error: message,
                    session_id: state.session_id,
                })?;
            }
        }
        AgentEvent::TurnComplete(tc) => {
            if parent_tool_use_id.is_none() {
                *state.result_checkpoint = state.result_text.len();
            }
            if let Some(out) = state.verbose_out {
                let content_value = serde_json::to_value(&tc.message.content)?;
                out.emit(&AssistantEvent {
                    event_type: "assistant",
                    message: AssistantMessage {
                        model: &tc.model,
                        role: "assistant",
                        content: &content_value,
                        usage: &tc.usage,
                    },
                    session_id: state.session_id,
                    parent_tool_use_id,
                })?;
            }
        }
        AgentEvent::ToolResultsSubmitted { message } => {
            if let Some(out) = state.verbose_out {
                let content_value = serde_json::to_value(&message.content)?;
                out.emit(&UserEvent {
                    event_type: "user",
                    message: UserMessage {
                        role: "user",
                        content: &content_value,
                    },
                    session_id: state.session_id,
                    parent_tool_use_id,
                })?;
            }
        }
        AgentEvent::Done {
            usage: u,
            num_turns: turns,
            stop_reason: sr,
        } => {
            *state.num_turns = *turns;
            *state.usage = *u;
            *state.stop_reason = *sr;
            return Ok(true);
        }
        AgentEvent::Error { message } => {
            *state.is_error = true;
            state.result_text.clone_from(message);
            return Ok(true);
        }
    }
    Ok(false)
}

fn output_result(
    format: OutputFormat,
    verbose_out: Option<VerboseOutput>,
    result_text: String,
    summary: ResultSummary,
) -> Result<()> {
    let ResultSummary {
        is_error,
        duration_ms,
        num_turns,
        stop_reason,
        session_id,
        usage,
        total_cost_usd,
    } = summary;

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
                usage_note: LOCAL_TOKEN_USAGE_NOTE,
            };
            match verbose_out {
                Some(VerboseOutput::Json(mut events)) => {
                    events.push(serde_json::to_value(&result)?);
                    println!("{}", serde_json::to_string(&events)?);
                }
                _ => println!("{}", serde_json::to_string(&result)?),
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use n00n_providers::TokenUsage;

    const PRINT_RESULT_FIELDS: &[&str] = &[
        "type",
        "subtype",
        "is_error",
        "num_turns",
        "result",
        "stop_reason",
        "session_id",
        "total_cost_usd",
        "usage",
        "usage_note",
        "duration_ms",
    ];
    const INIT_EVENT_FIELDS: &[&str] = &["type", "subtype", "cwd", "session_id", "tools", "model"];
    const RETRY_EVENT_FIELDS: &[&str] = &[
        "type",
        "subtype",
        "attempt",
        "retry_delay_ms",
        "error",
        "session_id",
    ];

    #[test]
    fn wire_format_required_fields() {
        let result = PrintResult {
            result_type: "result",
            subtype: "success",
            is_error: false,
            duration_ms: 1234,
            num_turns: 2,
            result: "done".into(),
            stop_reason: Some(StopReason::EndTurn),
            session_id: SessionRef::generate(),
            total_cost_usd: 0.003,
            usage: TokenUsage::default(),
            usage_note: LOCAL_TOKEN_USAGE_NOTE,
        };
        let json: Value = serde_json::to_value(&result).unwrap();
        for field in PRINT_RESULT_FIELDS {
            assert!(json.get(field).is_some(), "PrintResult missing: {field}");
        }

        let sid = SessionRef::generate();
        let init = InitEvent {
            event_type: "system",
            subtype: "init",
            cwd: "/tmp",
            session_id: &sid,
            tools: &["bash".into(), "read".into()],
            model: "test-model",
        };
        let json: Value = serde_json::to_value(&init).unwrap();
        for field in INIT_EVENT_FIELDS {
            assert!(json.get(field).is_some(), "InitEvent missing: {field}");
        }

        let retry = RetryEvent {
            event_type: "system",
            subtype: "api_retry",
            attempt: 2,
            retry_delay_ms: 3000,
            error: "rate_limit",
            session_id: &sid,
        };
        let json: Value = serde_json::to_value(&retry).unwrap();
        for field in RETRY_EVENT_FIELDS {
            assert!(json.get(field).is_some(), "RetryEvent missing: {field}");
        }
    }
}

use std::env;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use async_lock::Mutex;
use color_eyre::Result;
use color_eyre::eyre::Context;
use flume::Sender;
use futures_lite::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, split};
use n00n_agent::headless;
use n00n_agent::tools::ToolRegistry;
use n00n_agent::{AgentEvent, AgentInput, AgentMode as RuntimeAgentMode, Envelope};
use n00n_config::{load_env_files, load_permissions};
use n00n_lua::PluginHost;
use n00n_providers::ThinkingConfig;
use n00n_storage::StateDir;
use serde::{Deserialize, Serialize};
use smol::net::unix::{UnixListener, UnixStream};

use crate::cli::AgentMode as CliAgentMode;
use crate::setup;

fn workflow_from_mode(mode: CliAgentMode) -> bool {
    matches!(mode, CliAgentMode::Team | CliAgentMode::Workflow)
}

const MAX_AGENT_ID_LEN: usize = 64;

fn validate_agent_id(id: &str) -> Result<()> {
    if id.is_empty() {
        return Err(color_eyre::eyre::eyre!("agent id cannot be empty"));
    }
    if id.len() > MAX_AGENT_ID_LEN {
        return Err(color_eyre::eyre::eyre!(
            "agent id must be {MAX_AGENT_ID_LEN} characters or fewer"
        ));
    }
    if !id
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return Err(color_eyre::eyre::eyre!(
            "agent id must contain only ASCII letters, digits, hyphens, and underscores"
        ));
    }
    Ok(())
}

async fn write_line<W: AsyncWriteExt + Unpin>(writer: &mut W, line: &str) -> Result<()> {
    writer
        .write_all(line.as_bytes())
        .await
        .wrap_err("failed to write line")?;
    writer
        .write_all(b"\n")
        .await
        .wrap_err("failed to write newline")?;
    writer.flush().await.wrap_err("failed to flush writer")?;
    Ok(())
}

pub fn run(
    prompt: &str,
    model_arg: Option<&str>,
    mode: CliAgentMode,
    _goal: Option<&str>,
    json: bool,
    yolo: bool,
    no_jit: bool,
) -> Result<()> {
    let storage = StateDir::resolve().context("resolve data directory")?;
    n00n_providers::model_registry::load_from_storage(&storage);

    let cwd = env::current_dir().unwrap_or_else(|_| ".".into());
    load_env_files(&cwd);

    let mut plugin_host = PluginHost::with_jit(Arc::clone(ToolRegistry::global_arc()), !no_jit)
        .context("initialize lua plugin host")?;

    let raw_config = plugin_host
        .load_init_files(&cwd)
        .context("load init.lua files")?;

    let mut config = raw_config
        .unwrap_or_else(Default::default)
        .into_config(false)
        .context("invalid config")?;
    config.permissions = load_permissions(&cwd);

    setup::init_logging(&config.storage);

    if yolo || config.always_yolo {
        config.permissions.yolo = true;
    }
    config.validate()?;

    plugin_host
        .load_builtins(&config.plugins)
        .context("load builtin plugins")?;

    let timeouts = n00n_providers::Timeouts {
        connect: config.provider.connect_timeout,
        low_speed: config.provider.low_speed_timeout,
        stream: config.provider.stream_timeout,
    };

    let model = setup::resolve_model(model_arg, &config.provider, &storage)?;
    setup::install_panic_log_hook();

    let (mcp_handle, _mcp_config_errors) = smol::block_on(n00n_agent::mcp::start(
        &cwd,
        config.agent.mcp_tool_desc_max_chars,
    ));

    let prompt_slots = plugin_host
        .event_handle()
        .map_or_else(Default::default, |h| h.collect_prompt_slots());

    let headless_params = headless::HeadlessParams {
        model,
        config: config.agent,
        permissions_config: config.permissions,
        timeouts,
        openai_options: n00n_providers::OpenAiOptions::from(&config.provider),
        prompt: prompt.to_string(),
        images: Vec::new(),
        prompt_slots,
        excluded_tools: Vec::new(),
        mcp_handle,
        initial_wd: cwd,
        fast: false,
        workflow: workflow_from_mode(mode),
    };

    let handle = headless::spawn(headless_params);

    // Collect events and final result
    let mut final_output = String::new();
    let mut final_usage = None;
    let mut stop_reason = String::from("completed");

    while let Ok(event) = handle.event_rx.recv() {
        match event.event {
            n00n_agent::AgentEvent::TextDelta { text } => {
                final_output.push_str(&text);
            }
            n00n_agent::AgentEvent::Done { usage, .. } => {
                final_usage = Some(usage);
            }
            n00n_agent::AgentEvent::Error { message } => {
                stop_reason = format!("error: {message}");
                eprintln!("Error: {message}");
            }
            _ => {}
        }
    }

    smol::block_on(handle.task);

    if json {
        let output = serde_json::json!({
            "session_id": handle.session_id,
            "output": final_output,
            "usage": final_usage,
            "stop_reason": stop_reason,
        });
        let json_output = serde_json::to_string_pretty(&output)?;
        println!("{json_output}");
    } else {
        println!("{final_output}");
    }

    Ok(())
}

#[derive(Debug, Serialize, Deserialize)]
struct AgentState {
    id: String,
    session_id: String,
    socket_path: String,
    pid: u32,
    status: String,
    prompt: String,
    model: String,
    created_at: u64,
    updated_at: u64,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "cmd", rename_all = "snake_case")]
enum ClientCommand {
    Message { text: String },
    Pause,
    Resume,
    Stop,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ServerEvent {
    TextDelta {
        text: String,
    },
    ToolOutput {
        id: String,
        content: String,
    },
    Error {
        message: String,
    },
    Done {
        text: String,
        usage: serde_json::Value,
        #[serde(skip_serializing_if = "Option::is_none")]
        error: Option<String>,
    },
}

const AGENTS_SUBDIR: &str = "agents";
const STATE_FILE: &str = "agent.json";
const SOCKET_FILE: &str = "control.sock";

fn agent_dir(state_dir: &StateDir, agent_id: &str) -> Result<PathBuf> {
    validate_agent_id(agent_id)?;
    let agents_dir = state_dir.ensure_subdir(AGENTS_SUBDIR)?;
    Ok(agents_dir.join(agent_id))
}

fn socket_path(state_dir: &StateDir, agent_id: &str) -> Result<PathBuf> {
    Ok(agent_dir(state_dir, agent_id)?.join(SOCKET_FILE))
}

fn state_file_path(state_dir: &StateDir, agent_id: &str) -> Result<PathBuf> {
    Ok(agent_dir(state_dir, agent_id)?.join(STATE_FILE))
}

fn write_agent_state(state_dir: &StateDir, state: &AgentState) -> Result<()> {
    let path = state_file_path(state_dir, &state.id)?;
    let data = serde_json::to_vec_pretty(state).wrap_err("failed to serialize agent state")?;
    n00n_storage::atomic_write(&path, &data).wrap_err("failed to write agent state")?;
    Ok(())
}

fn read_agent_state(state_dir: &StateDir, agent_id: &str) -> Result<AgentState> {
    let path = state_file_path(state_dir, agent_id)?;
    let data = fs::read_to_string(&path).wrap_err("failed to read agent state")?;
    let state: AgentState = serde_json::from_str(&data).wrap_err("failed to parse agent state")?;
    Ok(state)
}

fn list_agent_states(state_dir: &StateDir) -> Result<Vec<AgentState>> {
    let agents_dir = state_dir.ensure_subdir(AGENTS_SUBDIR)?;
    let mut states = Vec::new();

    for entry in fs::read_dir(&agents_dir).wrap_err("failed to read agents directory")? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            let state_path = path.join(STATE_FILE);
            if state_path.exists() {
                let data =
                    fs::read_to_string(&state_path).wrap_err("failed to read agent state")?;
                let state: AgentState =
                    serde_json::from_str(&data).wrap_err("failed to parse agent state")?;
                states.push(state);
            }
        }
    }

    states.sort_by_key(|a| std::cmp::Reverse(a.updated_at));
    Ok(states)
}

fn now_epoch() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

pub fn server(
    prompt: &str,
    model_arg: Option<&str>,
    mode: CliAgentMode,
    agent_id: Option<String>,
    yolo: bool,
    no_jit: bool,
) -> Result<()> {
    let storage = StateDir::resolve().context("resolve data directory")?;
    n00n_providers::model_registry::load_from_storage(&storage);

    let cwd = env::current_dir().unwrap_or_else(|_| ".".into());
    load_env_files(&cwd);

    let mut plugin_host = PluginHost::with_jit(Arc::clone(ToolRegistry::global_arc()), !no_jit)
        .context("initialize lua plugin host")?;

    let raw_config = plugin_host
        .load_init_files(&cwd)
        .context("load init.lua files")?;

    let mut config = raw_config
        .unwrap_or_else(Default::default)
        .into_config(false)
        .context("invalid config")?;
    config.permissions = load_permissions(&cwd);

    setup::init_logging(&config.storage);

    if yolo || config.always_yolo {
        config.permissions.yolo = true;
    }
    config.validate()?;

    plugin_host
        .load_builtins(&config.plugins)
        .context("load builtin plugins")?;

    let timeouts = n00n_providers::Timeouts {
        connect: config.provider.connect_timeout,
        low_speed: config.provider.low_speed_timeout,
        stream: config.provider.stream_timeout,
    };

    let model = setup::resolve_model(model_arg, &config.provider, &storage)?;
    let model_spec = model.spec();
    setup::install_panic_log_hook();

    let (mcp_handle, _mcp_config_errors) = smol::block_on(n00n_agent::mcp::start(
        &cwd,
        config.agent.mcp_tool_desc_max_chars,
    ));

    let prompt_slots = plugin_host
        .event_handle()
        .map_or_else(Default::default, |h| h.collect_prompt_slots());

    let interactive_params = headless::InteractiveParams {
        model,
        config: config.agent,
        permissions_config: config.permissions,
        timeouts,
        openai_options: n00n_providers::OpenAiOptions::from(&config.provider),
        prompt_slots: Arc::new(prompt_slots),
        excluded_tools: Vec::new(),
        mcp_handle,
        initial_wd: cwd,
        session_id: None,
        initial_history: Vec::new(),
        yolo,
        system_prompt_override: None,
        append_system_prompt: None,
        workflow: workflow_from_mode(mode),
    };

    let handle = headless::spawn_interactive(interactive_params);

    let agent_id =
        agent_id.unwrap_or_else(|| handle.session_id.to_string().chars().take(12).collect());

    let agent_dir_path = agent_dir(&storage, &agent_id)?;
    fs::create_dir_all(&agent_dir_path).wrap_err("failed to create agent directory")?;
    fs::set_permissions(&agent_dir_path, fs::Permissions::from_mode(0o700))
        .wrap_err("failed to set agent directory permissions")?;

    let socket_path_value = socket_path(&storage, &agent_id)?;

    let mut state = AgentState {
        id: agent_id.clone(),
        session_id: handle.session_id.to_string(),
        socket_path: socket_path_value.to_string_lossy().into_owned(),
        pid: std::process::id(),
        status: "running".to_string(),
        prompt: prompt.to_string(),
        model: model_spec,
        created_at: now_epoch(),
        updated_at: now_epoch(),
    };

    write_agent_state(&storage, &state)?;

    let listener = UnixListener::bind(&socket_path_value).wrap_err("failed to bind socket")?;
    fs::set_permissions(&socket_path_value, fs::Permissions::from_mode(0o600))
        .wrap_err("failed to set socket permissions")?;

    let message_lock = Arc::new(Mutex::new(()));
    let paused = Arc::new(AtomicBool::new(false));

    if !prompt.is_empty() {
        let _lock = smol::block_on(message_lock.lock());
        state.status = "working".to_string();
        write_agent_state(&storage, &state)?;

        let _ = handle.input_tx.send(AgentInput {
            message: prompt.to_string(),
            mode: RuntimeAgentMode::Build,
            images: Vec::new(),
            preamble: Vec::new(),
            thinking: ThinkingConfig::default(),
            fast: false,
            workflow: workflow_from_mode(mode),
            prompt: None,
        });

        // Wait for the initial run to complete.
        wait_for_run_done(&handle.event_rx);

        state.status = "running".to_string();
        state.updated_at = now_epoch();
        write_agent_state(&storage, &state)?;
    }

    smol::block_on(async {
        while let Ok(stream) = listener.accept().await {
            let stream = stream.0;
            let input_tx = handle.input_tx.clone();
            let event_rx = handle.event_rx.clone();
            let cancel_tx = handle.cancel_tx.clone();
            let message_lock = Arc::clone(&message_lock);
            let paused = Arc::clone(&paused);
            let storage_clone = storage.clone();
            let agent_id_clone = agent_id.clone();
            let mode_clone = mode;

            smol::spawn(async move {
                if let Err(e) = handle_connection(
                    stream,
                    input_tx,
                    event_rx,
                    cancel_tx,
                    message_lock,
                    paused,
                    &storage_clone,
                    &agent_id_clone,
                    mode_clone,
                )
                .await
                {
                    eprintln!("Connection error: {e}");
                }
            })
            .detach();
        }
    });

    Ok(())
}

fn wait_for_run_done(event_rx: &flume::Receiver<Envelope>) {
    while let Ok(envelope) = event_rx.recv() {
        if matches!(
            envelope.event,
            AgentEvent::Done { .. } | AgentEvent::Error { .. }
        ) {
            break;
        }
    }
}

async fn handle_connection(
    stream: UnixStream,
    input_tx: Sender<AgentInput>,
    event_rx: flume::Receiver<Envelope>,
    cancel_tx: Sender<()>,
    message_lock: Arc<Mutex<()>>,
    paused: Arc<AtomicBool>,
    storage: &StateDir,
    agent_id: &str,
    mode: CliAgentMode,
) -> Result<()> {
    let (reader, mut writer) = split(stream);
    let mut reader = BufReader::new(reader);

    let mut line = String::new();
    reader
        .read_line(&mut line)
        .await
        .wrap_err("failed to read command")?;
    if line.is_empty() {
        return Ok(());
    }

    let cmd: ClientCommand = serde_json::from_str(&line).wrap_err("failed to parse command")?;

    match cmd {
        ClientCommand::Message { text } => {
            let _lock = message_lock.lock().await;

            if paused.load(Ordering::Relaxed) {
                let response = serde_json::json!({"ok": false, "error": "agent is paused"});
                let response_json = serde_json::to_string(&response)?;
                write_line(&mut writer, &response_json)
                    .await
                    .wrap_err("failed to write response")?;
                return Ok(());
            }

            let mut state = read_agent_state(storage, agent_id)?;
            state.status = "working".to_string();
            state.updated_at = now_epoch();
            write_agent_state(storage, &state)?;

            while event_rx.try_recv().is_ok() {}

            input_tx
                .send(AgentInput {
                    message: text.clone(),
                    mode: RuntimeAgentMode::Build,
                    images: Vec::new(),
                    preamble: Vec::new(),
                    thinking: ThinkingConfig::default(),
                    fast: false,
                    workflow: workflow_from_mode(mode),
                    prompt: None,
                })
                .wrap_err("failed to send input")?;

            let mut current_run_id: Option<u64> = None;
            let mut accumulated_text = String::new();
            let mut final_usage: Option<serde_json::Value> = None;
            let mut error_message: Option<String> = None;

            while let Ok(envelope) = event_rx.recv_async().await {
                if current_run_id.is_none() {
                    current_run_id = Some(envelope.run_id);
                }

                if current_run_id != Some(envelope.run_id) {
                    continue;
                }

                match envelope.event {
                    AgentEvent::TextDelta { text } => {
                        accumulated_text.push_str(&text);
                        let event = ServerEvent::TextDelta { text };
                        let json =
                            serde_json::to_string(&event).wrap_err("failed to serialize event")?;
                        write_line(&mut writer, &json)
                            .await
                            .wrap_err("failed to write event")?;
                    }
                    AgentEvent::ToolOutput { id, content } => {
                        let event = ServerEvent::ToolOutput { id, content };
                        let json =
                            serde_json::to_string(&event).wrap_err("failed to serialize event")?;
                        write_line(&mut writer, &json)
                            .await
                            .wrap_err("failed to write event")?;
                    }
                    AgentEvent::Error { message } => {
                        error_message = Some(message.clone());
                        let event = ServerEvent::Error { message };
                        let json =
                            serde_json::to_string(&event).wrap_err("failed to serialize event")?;
                        write_line(&mut writer, &json)
                            .await
                            .wrap_err("failed to write event")?;
                    }
                    AgentEvent::Done { usage, .. } => {
                        final_usage = Some(
                            serde_json::to_value(usage).unwrap_or_else(|_| serde_json::Value::Null),
                        );
                        break;
                    }
                    _ => {}
                }
            }

            let event = ServerEvent::Done {
                text: accumulated_text,
                usage: final_usage.unwrap_or_else(|| serde_json::Value::Null),
                error: error_message,
            };
            let json = serde_json::to_string(&event).wrap_err("failed to serialize event")?;
            write_line(&mut writer, &json)
                .await
                .wrap_err("failed to write event")?;

            let mut state = read_agent_state(storage, agent_id)?;
            state.status = "running".to_string();
            state.updated_at = now_epoch();
            write_agent_state(storage, &state)?;
        }
        ClientCommand::Pause => {
            paused.store(true, Ordering::Relaxed);

            let mut state = read_agent_state(storage, agent_id)?;
            state.status = "paused".to_string();
            state.updated_at = now_epoch();
            write_agent_state(storage, &state)?;

            let response = serde_json::json!({ "ok": true });
            let response_json = serde_json::to_string(&response)?;
            write_line(&mut writer, &response_json)
                .await
                .wrap_err("failed to write response")?;
        }
        ClientCommand::Resume => {
            paused.store(false, Ordering::Relaxed);

            let mut state = read_agent_state(storage, agent_id)?;
            state.status = "running".to_string();
            state.updated_at = now_epoch();
            write_agent_state(storage, &state)?;

            let response = serde_json::json!({ "ok": true });
            let response_json = serde_json::to_string(&response)?;
            write_line(&mut writer, &response_json)
                .await
                .wrap_err("failed to write response")?;
        }
        ClientCommand::Stop => {
            let mut state = read_agent_state(storage, agent_id)?;
            state.status = "stopping".to_string();
            write_agent_state(storage, &state)?;

            let _ = cancel_tx.send(());

            let response = serde_json::json!({ "ok": true });
            let response_json = serde_json::to_string(&response)?;
            write_line(&mut writer, &response_json)
                .await
                .wrap_err("failed to write response")?;

            let agent_dir_path = agent_dir(storage, agent_id)?;
            if let Err(e) = fs::remove_file(&state.socket_path) {
                tracing::warn!(error = %e, path = %state.socket_path, "failed to remove agent socket");
            }
            if let Err(e) = fs::remove_dir_all(&agent_dir_path) {
                tracing::warn!(error = %e, path = %agent_dir_path.display(), "failed to remove agent state directory");
            }

            std::process::exit(0);
        }
    }

    Ok(())
}

pub fn message_client(id: &str, text: &str, json: bool) -> Result<()> {
    let storage = StateDir::resolve().wrap_err("failed to resolve state directory")?;
    let state = read_agent_state(&storage, id).wrap_err("failed to read agent state")?;

    let stream = smol::block_on(UnixStream::connect(&state.socket_path))
        .wrap_err("failed to connect to agent socket")?;

    let cmd = ClientCommand::Message {
        text: text.to_string(),
    };
    let cmd_json = serde_json::to_string(&cmd).wrap_err("failed to serialize command")?;

    smol::block_on(async {
        let (reader, mut writer) = split(stream);
        let mut reader = BufReader::new(reader);

        write_line(&mut writer, &cmd_json)
            .await
            .wrap_err("failed to send command")?;

        let mut line = String::new();

        while let Ok(n) = reader.read_line(&mut line).await {
            if n == 0 {
                break;
            }

            if json {
                print!("{line}");
            } else if let Ok(event) = serde_json::from_str::<ServerEvent>(&line) {
                match event {
                    ServerEvent::TextDelta { text } => {
                        print!("{text}");
                    }
                    ServerEvent::Done {
                        error: Some(message),
                        ..
                    } => {
                        eprintln!("\nError: {message}");
                        return Err(color_eyre::eyre::eyre!(message));
                    }
                    ServerEvent::Done { .. } => {
                        println!();
                    }
                    ServerEvent::Error { message } => {
                        eprintln!("Error: {message}");
                        return Err(color_eyre::eyre::eyre!(message));
                    }
                    ServerEvent::ToolOutput { .. } => {}
                }
            }
            line.clear();
        }

        Ok(())
    })
}

pub fn stop_client(id: &str) -> Result<()> {
    let storage = StateDir::resolve().wrap_err("failed to resolve state directory")?;

    let Ok(state) = read_agent_state(&storage, id) else {
        let agent_dir_path = agent_dir(&storage, id)?;
        let _ = fs::remove_dir_all(&agent_dir_path);
        eprintln!("Agent {id} not found, cleaned up directory");
        return Ok(());
    };

    let stream = smol::block_on(UnixStream::connect(&state.socket_path))
        .wrap_err("failed to connect to agent socket")?;

    let cmd = ClientCommand::Stop;
    let cmd_json = serde_json::to_string(&cmd).wrap_err("failed to serialize command")?;

    smol::block_on(async {
        let (reader, mut writer) = split(stream);
        let mut reader = BufReader::new(reader);

        write_line(&mut writer, &cmd_json)
            .await
            .wrap_err("failed to send command")?;

        let mut line = String::new();
        let _ = reader
            .read_line(&mut line)
            .await
            .wrap_err("failed to read response")?;

        let response: serde_json::Value =
            serde_json::from_str(&line).wrap_err("failed to parse response")?;

        match response.get("ok").and_then(serde_json::Value::as_bool) {
            Some(true) => println!("Agent {id} stopped"),
            _ => eprintln!("Failed to stop agent {id}"),
        }

        Ok(())
    })
}

pub fn list_client(json: bool) -> Result<()> {
    let storage = StateDir::resolve().wrap_err("failed to resolve state directory")?;
    let states = list_agent_states(&storage)?;

    if json {
        let output =
            serde_json::to_string_pretty(&states).wrap_err("failed to serialize agent list")?;
        println!("{output}");
    } else {
        if states.is_empty() {
            println!("No background agents running");
            return Ok(());
        }

        println!("Background agents:");
        for state in &states {
            let prompt_preview = if state.prompt.len() > 50 {
                format!("{}...", &state.prompt[..50])
            } else {
                state.prompt.clone()
            };
            println!(
                "  {} - {} - {} - {} - {}",
                state.id, state.status, state.model, prompt_preview, state.updated_at
            );
        }
    }

    Ok(())
}

pub fn status_client(id: &str, json: bool) -> Result<()> {
    let storage = StateDir::resolve().wrap_err("failed to resolve state directory")?;
    let state = read_agent_state(&storage, id).wrap_err("failed to read agent state")?;

    if json {
        let output =
            serde_json::to_string_pretty(&state).wrap_err("failed to serialize agent status")?;
        println!("{output}");
    } else {
        println!("Agent: {}", state.id);
        println!("  session:  {}", state.session_id);
        println!("  status:   {}", state.status);
        println!("  model:    {}", state.model);
        println!("  prompt:   {}", state.prompt);
        println!("  socket:   {}", state.socket_path);
        println!("  pid:      {}", state.pid);
        println!("  created:  {}", state.created_at);
        println!("  updated:  {}", state.updated_at);
    }

    Ok(())
}

pub fn pause_client(id: &str) -> Result<()> {
    control_command_client(id, &ClientCommand::Pause, "paused")
}

pub fn resume_client(id: &str) -> Result<()> {
    control_command_client(id, &ClientCommand::Resume, "resumed")
}

fn control_command_client(id: &str, command: &ClientCommand, success_label: &str) -> Result<()> {
    let storage = StateDir::resolve().wrap_err("failed to resolve state directory")?;
    let state = read_agent_state(&storage, id).wrap_err("failed to read agent state")?;
    let stream = smol::block_on(UnixStream::connect(&state.socket_path))
        .wrap_err("failed to connect to agent socket")?;

    let cmd_json = serde_json::to_string(&command).wrap_err("failed to serialize command")?;

    smol::block_on(async {
        let (reader, mut writer) = split(stream);
        let mut reader = BufReader::new(reader);

        write_line(&mut writer, &cmd_json)
            .await
            .wrap_err("failed to send command")?;

        let mut line = String::new();
        let _ = reader
            .read_line(&mut line)
            .await
            .wrap_err("failed to read response")?;

        let response: serde_json::Value =
            serde_json::from_str(&line).wrap_err("failed to parse response")?;

        match response.get("ok").and_then(serde_json::Value::as_bool) {
            Some(true) => println!("Agent {id} {success_label}"),
            _ => eprintln!("Failed to update agent {id}"),
        }

        Ok(())
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn agent_state_serialization_roundtrip() {
        let state = AgentState {
            id: "test-agent".to_string(),
            session_id: "session-123".to_string(),
            socket_path: "/tmp/test.sock".to_string(),
            pid: 12345,
            status: "running".to_string(),
            prompt: "test prompt".to_string(),
            model: "anthropic/claude-3-opus".to_string(),
            created_at: 1_234_567_890,
            updated_at: 1_234_567_900,
        };

        let json = serde_json::to_string(&state).unwrap();
        let decoded: AgentState = serde_json::from_str(&json).unwrap();

        assert_eq!(decoded.id, state.id);
        assert_eq!(decoded.session_id, state.session_id);
        assert_eq!(decoded.socket_path, state.socket_path);
        assert_eq!(decoded.pid, state.pid);
        assert_eq!(decoded.status, state.status);
        assert_eq!(decoded.prompt, state.prompt);
        assert_eq!(decoded.model, state.model);
        assert_eq!(decoded.created_at, state.created_at);
        assert_eq!(decoded.updated_at, state.updated_at);
    }

    #[test]
    fn client_command_message_serialization() {
        let cmd = ClientCommand::Message {
            text: "hello".to_string(),
        };
        let json = serde_json::to_string(&cmd).unwrap();
        let decoded: ClientCommand = serde_json::from_str(&json).unwrap();

        assert!(matches!(
            decoded,
            ClientCommand::Message { text } if text == "hello"
        ));
    }

    #[test]
    fn client_command_stop_serialization() {
        let cmd = ClientCommand::Stop;
        let json = serde_json::to_string(&cmd).unwrap();
        let decoded: ClientCommand = serde_json::from_str(&json).unwrap();

        assert!(matches!(decoded, ClientCommand::Stop));
    }

    #[test]
    fn server_event_text_delta_serialization() {
        let event = ServerEvent::TextDelta {
            text: "hello".to_string(),
        };
        let json = serde_json::to_string(&event).unwrap();
        let decoded: ServerEvent = serde_json::from_str(&json).unwrap();

        assert!(matches!(
            decoded,
            ServerEvent::TextDelta { text } if text == "hello"
        ));
    }

    #[test]
    fn server_event_done_serialization() {
        let event = ServerEvent::Done {
            text: "result".to_string(),
            usage: serde_json::json!({ "total_tokens": 100 }),
            error: None,
        };
        let json = serde_json::to_string(&event).unwrap();
        let decoded: ServerEvent = serde_json::from_str(&json).unwrap();

        assert!(matches!(
            decoded,
            ServerEvent::Done { text, .. } if text == "result"
        ));
    }

    #[test]
    fn list_agent_states_empty_directory() {
        let tmp = TempDir::new().unwrap();
        let state_dir = StateDir::from_path(tmp.path().to_path_buf());
        let _agents_dir = state_dir.ensure_subdir(AGENTS_SUBDIR).unwrap();

        let states = list_agent_states(&state_dir).unwrap();
        assert!(states.is_empty());
    }

    #[test]
    fn list_agent_states_with_entries() {
        let tmp = TempDir::new().unwrap();
        let state_dir = StateDir::from_path(tmp.path().to_path_buf());
        let agents_root = state_dir.ensure_subdir(AGENTS_SUBDIR).unwrap();

        let agent1_path = agents_root.join("agent1");
        fs::create_dir_all(&agent1_path).unwrap();
        let first_state = AgentState {
            id: "agent1".to_string(),
            session_id: "session1".to_string(),
            socket_path: "/tmp/agent1.sock".to_string(),
            pid: 1,
            status: "running".to_string(),
            prompt: "prompt1".to_string(),
            model: "model1".to_string(),
            created_at: 100,
            updated_at: 200,
        };
        let data1 = serde_json::to_vec_pretty(&first_state).unwrap();
        n00n_storage::atomic_write(&agent1_path.join(STATE_FILE), &data1).unwrap();

        let agent2_path = agents_root.join("agent2");
        fs::create_dir_all(&agent2_path).unwrap();
        let second_state = AgentState {
            id: "agent2".to_string(),
            session_id: "session2".to_string(),
            socket_path: "/tmp/agent2.sock".to_string(),
            pid: 2,
            status: "running".to_string(),
            prompt: "prompt2".to_string(),
            model: "model2".to_string(),
            created_at: 50,
            updated_at: 300,
        };
        let data2 = serde_json::to_vec_pretty(&second_state).unwrap();
        n00n_storage::atomic_write(&agent2_path.join(STATE_FILE), &data2).unwrap();

        let all = list_agent_states(&state_dir).unwrap();
        assert_eq!(all.len(), 2);
        assert_eq!(all[0].id, "agent2");
        assert_eq!(all[1].id, "agent1");
    }

    #[test]
    fn validate_agent_id_accepts_safe_ids() {
        assert!(validate_agent_id("my-agent_1").is_ok());
        assert!(validate_agent_id("a").is_ok());
        assert!(validate_agent_id(&"x".repeat(64)).is_ok());
    }

    #[test]
    fn validate_agent_id_rejects_unsafe_ids() {
        assert!(validate_agent_id("").is_err());
        assert!(validate_agent_id("a/b").is_err());
        assert!(validate_agent_id("..").is_err());
        assert!(validate_agent_id("../../../etc/passwd").is_err());
        assert!(validate_agent_id("a b").is_err());
        assert!(validate_agent_id(&"x".repeat(65)).is_err());
    }

    #[test]
    fn agent_dir_rejects_path_traversal_id() {
        let tmp = TempDir::new().unwrap();
        let state_dir = StateDir::from_path(tmp.path().to_path_buf());
        assert!(agent_dir(&state_dir, "../escape").is_err());
    }
}

use std::env;
use std::sync::Arc;

use color_eyre::Result;
use color_eyre::eyre::Context;

use n00n_agent::headless;
use n00n_agent::tools::ToolRegistry;
use n00n_config::{load_env_files, load_permissions};
use n00n_lua::PluginHost;
use n00n_storage::StateDir;

use crate::setup;

pub fn run(
    prompt: &str,
    model_arg: Option<&str>,
    mode: AgentMode,
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
        workflow: matches!(mode, AgentMode::Team | AgentMode::Workflow),
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

#[derive(clap::ValueEnum, Clone, Copy, Debug)]
pub enum AgentMode {
    Research,
    General,
    Task,
    Team,
    Workflow,
}

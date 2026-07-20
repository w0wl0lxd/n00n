use std::env;
use std::sync::Arc;

use color_eyre::Result;
use color_eyre::eyre::Context;

use n00n_agent::tools::ToolRegistry;
use n00n_config::{load_env_files, load_permissions};
use n00n_lua::PluginHost;
use n00n_storage::StateDir;

use crate::setup;

pub fn run(model_arg: Option<String>, yolo: bool, no_jit: bool) -> Result<()> {
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
        .unwrap_or_default()
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

    let model = setup::resolve_model(model_arg.as_deref(), &config.provider, &storage)?;
    setup::install_panic_log_hook();

    let (mcp_handle, _mcp_config_errors) = smol::block_on(n00n_agent::mcp::start(&cwd));

    let prompt_slots = plugin_host
        .event_handle()
        .map(|h| h.collect_prompt_slots())
        .unwrap_or_default();

    n00n_acp::run(n00n_acp::AcpParams {
        model,
        config: config.agent,
        permissions_config: config.permissions,
        timeouts,
        initial_wd: cwd,
        mcp_handle,
        prompt_slots: Arc::new(prompt_slots),
        yolo,
    })
}

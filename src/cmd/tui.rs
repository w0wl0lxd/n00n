use std::env;
use std::io::{self, IsTerminal, Read};
use std::path::Path;
use std::sync::{Arc, RwLock};
use std::thread::{self, JoinHandle};

use color_eyre::Result;
use color_eyre::eyre::Context;

use n00n_agent::command::{self, CustomCommand};
use n00n_agent::tools::ToolRegistry;
use n00n_config::providers::ProvidersConfig;
use n00n_config::{Config, load_env_files, load_permissions};
use n00n_lua::PluginHost;
use n00n_providers::Message;
use n00n_providers::model::Model;
use n00n_providers::model_registry::ModelRegistry;
use n00n_storage::StateDir;
use n00n_storage::id::n00nId;
use n00n_storage::sessions::RetentionBudget;
use n00n_ui::{AppSession, RunOutcome};

use crate::cli::{Cli, normalize_tool_name};
use crate::setup;

const CONFIG_FALLBACK_WARNING: &str = "config reload failed, using previous config";
const MODEL_FALLBACK_WARNING: &str = "model resolution failed, keeping previous model";

/// One generation of the app: everything torn down and rebuilt on `/reload`.
/// Dropping it joins the Lua thread via `PluginHost::drop`.
struct Stack {
    plugin_host: PluginHost,
    config: Config,
    commands: Vec<CustomCommand>,
    model: Model,
    model_registry: Arc<RwLock<ModelRegistry>>,
    needs_login: bool,
}

impl Stack {
    fn timeouts(&self) -> n00n_providers::Timeouts {
        n00n_providers::Timeouts {
            connect: self.config.provider.connect_timeout,
            low_speed: self.config.provider.low_speed_timeout,
            stream: self.config.provider.stream_timeout,
        }
    }
}

/// Background teardown of the previous generation. `defer` keeps the slow
/// drop (a Lua thread join, capped at 2s in `PluginHost::drop`) off the
/// `/reload` hot path. Joining on replace and on drop covers every exit
/// path, including `?` unwinds, so no VM is abandoned mid-shutdown and at
/// most one teardown is ever in flight.
#[derive(Default)]
struct Teardown(Option<JoinHandle<()>>);

impl Teardown {
    fn defer(&mut self, work: impl FnOnce() + Send + 'static) {
        self.join();
        self.0 = Some(thread::spawn(work));
    }

    fn join(&mut self) {
        if let Some(handle) = self.0.take()
            && handle.join().is_err()
        {
            tracing::warn!("background teardown panicked");
        }
    }
}

impl Drop for Teardown {
    fn drop(&mut self) {
        self.join();
    }
}

fn discover_commands(disable: bool) -> Vec<CustomCommand> {
    if disable {
        return Vec::new();
    }
    let cwd = env::current_dir().unwrap_or_else(|_| ".".into());
    command::discover_commands(&cwd)
}

fn load_config(plugin_host: &PluginHost, cli: &Cli, cwd: &Path) -> Result<Config> {
    let raw_config = plugin_host
        .load_init_files(cwd)
        .context("load init.lua files")?;

    let mut config = raw_config
        .unwrap_or_else(Default::default)
        .into_config(cli.plugin_flags.no_rtk)
        .context("invalid config")?;
    config.permissions = load_permissions(cwd);

    if cli.permission_flags.yolo || config.always_yolo {
        config.permissions.yolo = true;
    }
    if !cli.allowed_tools.is_empty() {
        config.agent.allowed_tools = cli
            .allowed_tools
            .iter()
            .map(|t| normalize_tool_name(t))
            .collect::<Result<Vec<_>>>()?;
    }
    if !cli.disallowed_tools.is_empty() {
        config.agent.disabled_tools.extend(
            cli.disallowed_tools
                .iter()
                .filter_map(|t| normalize_tool_name(t).ok()),
        );
    }
    config.agent.fusion.enabled = super::resolve_fusion_opt_in(
        cli.fusion,
        config.always_fusion,
        config.agent.fusion.enabled,
    );
    config.validate()?;
    Ok(config)
}

fn config_or_fallback(
    loaded: Result<Config>,
    fallback: Option<Config>,
    warnings: &mut Vec<String>,
) -> Result<Config> {
    match (loaded, fallback) {
        (Ok(config), _) => Ok(config),
        (Err(e), Some(last_good)) => {
            warnings.push(format!("{CONFIG_FALLBACK_WARNING}: {e:#}"));
            Ok(last_good)
        }
        (Err(e), None) => Err(e),
    }
}

fn select_startup_model(
    resolved: Result<Model>,
    reload_fallback: Option<Model>,
    interactive_fallback: bool,
    recent_model: Option<Model>,
    detected_model: Option<Model>,
    warnings: &mut Vec<String>,
    model_registry: &Arc<RwLock<ModelRegistry>>,
) -> Result<(Model, bool)> {
    match (resolved, reload_fallback) {
        (Ok(model), _)
            if interactive_fallback
                && !n00n_providers::provider::provider_available(&model.provider) =>
        {
            if let Some(fallback) = recent_model {
                warnings.push(format!(
                    "provider '{}' is unavailable; using recent model '{}'",
                    model.provider,
                    fallback.spec()
                ));
                Ok((fallback, false))
            } else if let Some(detected) = detected_model {
                warnings.push(format!(
                    "provider '{}' is unavailable; using auto-detected model '{}'",
                    model.provider,
                    detected.spec()
                ));
                Ok((detected, false))
            } else {
                Ok((model, true))
            }
        }
        (Ok(model), _) => Ok((model, false)),
        (Err(error), Some(last_model)) => {
            warnings.push(format!("{MODEL_FALLBACK_WARNING}: {error:#}"));
            Ok((last_model, false))
        }
        (Err(error), None) if interactive_fallback => {
            if let Some(fallback) = recent_model {
                warnings.push(format!(
                    "model resolution failed; using recent model '{}': {error:#}",
                    fallback.spec()
                ));
                Ok((fallback, false))
            } else if let Some(detected) = detected_model {
                warnings.push(format!(
                    "model resolution failed; using auto-detected model '{}': {error:#}",
                    detected.spec()
                ));
                Ok((detected, false))
            } else {
                setup::placeholder_model(model_registry)
                    .map(|placeholder| (placeholder, true))
                    .map_err(|_| error)
            }
        }
        (Err(error), None) => Err(error),
    }
}

/// The one construction path for a generation: first startup passes
/// `fallback: None` (fail-fast); `/reload` passes the last-good config and
/// model so a broken config reopens the UI with a warning instead of exiting.
fn build_stack(
    cli: &Cli,
    cwd: &Path,
    storage: &StateDir,
    model_registry: &Arc<RwLock<ModelRegistry>>,
    fallback: Option<(Config, Model)>,
) -> Result<(Stack, Vec<String>)> {
    let mut warnings = Vec::new();

    let mut plugin_host = if cli.plugin_flags.no_plugins {
        PluginHost::disabled()
    } else {
        PluginHost::with_jit(
            Arc::clone(ToolRegistry::global_arc()),
            !cli.plugin_flags.no_jit,
        )
        .context("initialize lua plugin host")?
    };

    let (fallback_config, fallback_model) = fallback.unzip();
    let reloading = fallback_model.is_some();
    let config = config_or_fallback(
        load_config(&plugin_host, cli, cwd),
        fallback_config,
        &mut warnings,
    )?;
    plugin_host
        .set_search_config(Arc::new(config.search.clone()))
        .context("configure lua search services")?;
    if let Err(e) = plugin_host.load_builtins(&config.plugins) {
        let e = color_eyre::eyre::Report::from(e).wrap_err("load builtin plugins");
        if reloading {
            warnings.push(format!("{e:#}"));
        } else {
            return Err(e);
        }
    }

    let commands = discover_commands(cli.plugin_flags.no_commands);
    let providers_toml = ProvidersConfig::load();

    let model_result = setup::resolve_model_with_fusion(
        cli.model.as_deref(),
        &config.provider,
        &providers_toml,
        storage,
        Some(&config.agent.fusion),
        model_registry,
    );
    let explicit = cli.model.is_some();
    let interactive_fallback = !cli.run_flags.print && !explicit;
    let (recent_model, detected_model) = if interactive_fallback {
        (
            setup::fallback_to_recent_model(storage, model_registry),
            setup::auto_detect_model(&providers_toml, model_registry),
        )
    } else {
        (None, None)
    };
    let (model, needs_login) = select_startup_model(
        model_result,
        fallback_model,
        interactive_fallback,
        recent_model,
        detected_model,
        &mut warnings,
        model_registry,
    )?;

    Ok((
        Stack {
            plugin_host,
            config,
            commands,
            model,
            model_registry: Arc::clone(model_registry),
            needs_login,
        },
        warnings,
    ))
}

fn resolve_session(
    continue_session: bool,
    session_id: Option<&str>,
    model: &str,
    cwd: &str,
    storage: &StateDir,
    retention_budget: RetentionBudget,
) -> Result<AppSession> {
    if let Some(raw) = session_id {
        let id: n00nId = raw
            .parse()
            .map_err(|e| color_eyre::eyre::eyre!("invalid session id {raw:?}: {e}"))?;
        return AppSession::load_with_retention(id, storage, retention_budget, message_tool_ids)
            .map_err(|e| color_eyre::eyre::eyre!("{e}"));
    }
    if continue_session {
        match AppSession::latest_with_retention(cwd, storage, retention_budget, message_tool_ids) {
            Ok(Some(session)) => return Ok(session),
            Ok(None) => {
                tracing::info!("no previous session found for this directory, starting new");
            }
            Err(e) => {
                tracing::warn!(error = %e, "failed to load latest session, starting new");
            }
        }
    }
    Ok(AppSession::new(model, cwd))
}

fn message_tool_ids(message: &Message) -> Vec<String> {
    message
        .tool_uses()
        .map(|(id, _, _)| id.to_owned())
        .collect()
}

fn read_initial_prompt(cli_prompt: Option<String>) -> Result<Option<String>> {
    match cli_prompt {
        Some(p) => Ok(Some(p)),
        None if !io::stdin().is_terminal() => {
            let mut buf = String::new();
            io::stdin().read_to_string(&mut buf).context("read stdin")?;
            Ok(Some(buf))
        }
        None => Ok(None),
    }
}

pub fn run(cli: Cli) -> Result<()> {
    let storage = StateDir::resolve().context("resolve data directory")?;
    let model_registry = Arc::new(RwLock::new(ModelRegistry::default()));
    model_registry
        .write()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .load_from_storage(&storage);

    let cwd = env::current_dir().unwrap_or_else(|_| ".".into());

    load_env_files(&cwd);
    warn_stale_config_toml(&cwd);

    let (stack, startup_warnings) = build_stack(&cli, &cwd, &storage, &model_registry, None)?;
    let openai_options = n00n_providers::OpenAiOptions::from(&stack.config.provider);

    setup::init_logging(&stack.config.storage);
    setup::install_panic_log_hook();

    if cli.is_sdk_mode() {
        return run_sdk_mode(cli, stack, openai_options);
    }
    if cli.run_flags.print {
        return run_print_mode(cli, stack, openai_options);
    }

    run_ui_loop(&cli, stack, startup_warnings, &storage, &cwd)
}

fn run_sdk_mode(
    cli: Cli,
    stack: Stack,
    openai_options: n00n_providers::OpenAiOptions,
) -> Result<()> {
    let fast = stack.config.always_fast && stack.model.supports_fast();
    let event_handle = stack.plugin_host.event_handle();
    let prompt_slots = event_handle
        .as_ref()
        .map_or_else(
            || Ok(n00n_agent::prompt::ResolvedSlots::default()),
            n00n_lua::EventHandle::try_collect_prompt_slots,
        )
        .context("collect plugin prompt slots")?;
    let state_persistence = event_handle
        .map(|handle| Arc::new(handle) as Arc<dyn n00n_agent::headless::SessionStatePersistence>);
    let timeouts = stack.timeouts();
    crate::sdk_mode::run(crate::sdk_mode::SdkParams {
        cli,
        model: stack.model,
        config: Arc::new(stack.config.agent),
        permissions_config: stack.config.permissions,
        timeouts,
        openai_options,
        prompt_slots,
        state_persistence,
        fast,
        workflow: stack.config.always_workflow,
    })
    .context("run sdk mode")
}

fn run_print_mode(
    cli: Cli,
    stack: Stack,
    openai_options: n00n_providers::OpenAiOptions,
) -> Result<()> {
    let fast = stack.config.always_fast && stack.model.supports_fast();
    let timeouts = stack.timeouts();
    crate::print::run(
        &stack.model,
        crate::print::PrintArgs {
            prompt_arg: cli.initial_prompt,
            image_paths: &cli.images,
            format: cli.output_format,
            verbose: cli.run_flags.verbose,
            config: stack.config.agent,
            permissions_config: stack.config.permissions,
            timeouts,
            openai_options,
            lua_handle: stack.plugin_host.event_handle().as_ref(),
            fast,
            workflow: stack.config.always_workflow,
        },
    )
    .context("run print mode")
}

fn run_ui_loop(
    cli: &Cli,
    mut stack: Stack,
    mut warnings: Vec<String>,
    storage: &StateDir,
    cwd: &std::path::Path,
) -> Result<()> {
    if !io::stdout().is_terminal() {
        color_eyre::eyre::bail!(
            "interactive UI requires terminal output; use `n00n --print` in non-interactive mode"
        );
    }
    let cwd_str = cwd.to_string_lossy().into_owned();
    let mut tabs = vec![resolve_session(
        cli.session_flags.continue_session,
        cli.session.as_deref(),
        &stack.model.spec(),
        &cwd_str,
        storage,
        stack.config.storage.retention_budget(),
    )?];
    let mut focused = 0;
    let mut initial_prompt = read_initial_prompt(cli.initial_prompt.clone())?;
    let mut teardown = Teardown::default();

    loop {
        let openai_options = n00n_providers::OpenAiOptions::from(&stack.config.provider);
        for session in &mut tabs {
            if session.messages.is_empty() {
                session.meta.fast |= stack.config.always_fast;
                session.meta.workflow |= stack.config.always_workflow;
                if let Some(thinking) = stack.config.always_thinking {
                    session.meta.thinking = Some(thinking);
                }
            }
        }
        let focused_tab = &tabs[focused];
        let model = if focused_tab.messages.is_empty() {
            stack.model.clone()
        } else {
            setup::available_model_from_spec(&focused_tab.model, &stack.model_registry)
                .unwrap_or_else(|| stack.model.clone())
        };

        // Bind daemon.sock for this UI generation so CLI `n00n agent list`
        // unions live TUI sessions. Dropped on exit / before `/reload`.
        let daemon = stack
            .plugin_host
            .ui_action_tx()
            .and_then(|tx| crate::cmd::tui_bridge::try_spawn(storage.path(), tx));

        let outcome = n00n_ui::run(
            n00n_ui::EventLoopParams {
                model,
                needs_login: stack.needs_login,
                commands: std::mem::take(&mut stack.commands),
                sessions: std::mem::take(&mut tabs),
                focused,
                startup_warnings: std::mem::take(&mut warnings),
                storage: storage.clone(),
                config: stack.config.agent.clone(),
                ui_config: stack.config.ui.clone(),
                input_history_size: stack.config.storage.input_history_size,
                retention_budget: stack.config.storage.retention_budget(),
                permissions: Arc::new(n00n_agent::permissions::PermissionManager::new(
                    stack.config.permissions.clone(),
                    cwd.to_path_buf(),
                )),
                timeouts: stack.timeouts(),
                openai_options,
                exit_on_done: cli.run_flags.exit_on_done,
                lua_command_reader: stack.plugin_host.command_reader(),
                keymap_reader: stack.plugin_host.keymap_reader(),
                hint_reader: stack.plugin_host.hint_reader(),
                ui_action_rx: stack.plugin_host.ui_action_rx(),
                lua_event_handle: stack.plugin_host.event_handle(),
            },
            initial_prompt.take(),
        )
        .context("run UI")?;

        drop(daemon);

        match outcome {
            RunOutcome::Exit { session_id, code } => {
                if let Some(session_id) = session_id {
                    eprintln!("Resume session:\n\n  n00n -s {session_id}");
                }
                if code != 0 {
                    teardown.join();
                    std::process::exit(code);
                }
                return Ok(());
            }
            RunOutcome::Reload {
                tabs: reloaded,
                focused: f,
            } => {
                let (new_tabs, new_focused) = handle_reload(
                    cli,
                    cwd,
                    storage,
                    &mut stack,
                    &mut warnings,
                    &mut teardown,
                    reloaded,
                    f,
                )?;
                tabs = new_tabs;
                focused = new_focused;
            }
        }
    }
}

fn handle_reload(
    cli: &Cli,
    cwd: &std::path::Path,
    storage: &StateDir,
    stack: &mut Stack,
    warnings: &mut Vec<String>,
    teardown: &mut Teardown,
    reloaded: Vec<AppSession>,
    f: usize,
) -> Result<(Vec<AppSession>, usize)> {
    let started = std::time::Instant::now();
    let last_good = (stack.config.clone(), stack.model.clone());
    let cwd_str = cwd.to_string_lossy().into_owned();
    // Shut the old host down first so nothing can repopulate
    // the registry after the clear: its senders disconnect, the
    // watchdog aborts in-flight callbacks, and only this thread
    // issues loads. The old VM then shares nothing with the new
    // stack, so its slow join (up to 2s) can run on a
    // background thread.
    stack.plugin_host.begin_shutdown();
    ToolRegistry::global().clear_lua();
    let plugin_host = std::mem::replace(&mut stack.plugin_host, PluginHost::disabled());
    teardown.defer(move || drop(plugin_host));

    let (new_stack, new_warnings) =
        build_stack(cli, cwd, storage, &stack.model_registry, Some(last_good))
            .context("reload with fallback should not fail")?;
    let tabs = if reloaded.is_empty() {
        vec![AppSession::new(&new_stack.model.spec(), &cwd_str)]
    } else {
        reloaded
    };
    *stack = new_stack;
    *warnings = new_warnings;
    let focused = f.min(tabs.len() - 1);
    tracing::info!(
        elapsed_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or_else(|_| u64::MAX),
        tabs = tabs.len(),
        "reload: rebuilt plugins and config"
    );
    Ok((tabs, focused))
}

fn warn_stale_config_toml(cwd: &std::path::Path) {
    let stale_paths = [
        n00n_config::global_config_dir().map(|d| d.join("config.toml")),
        Some(cwd.join(".n00n/config.toml")),
    ];
    for path in stale_paths.into_iter().flatten() {
        if path.is_file() {
            tracing::warn!(
                path = %path.display(),
                "config.toml found but no longer used. Migrate to init.lua. See https://github.com/w0wl0lxd/n00n/docs/configuration/"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use color_eyre::eyre::eyre;
    use n00n_config::RawConfig;
    use std::sync::atomic::{AtomicBool, Ordering};

    /// `second_saw_first` requires both joins: `defer` joining the first
    /// closure before spawning the second, and `Drop` joining the second
    /// before the assert reads the flag.
    #[test]
    fn teardown_defer_joins_previous_and_drop_joins_last() {
        let first_done = Arc::new(AtomicBool::new(false));
        let second_saw_first = Arc::new(AtomicBool::new(false));
        let mut teardown = Teardown::default();

        let set = Arc::clone(&first_done);
        teardown.defer(move || set.store(true, Ordering::Release));

        let read = Arc::clone(&first_done);
        let record = Arc::clone(&second_saw_first);
        teardown.defer(move || record.store(read.load(Ordering::Acquire), Ordering::Release));

        drop(teardown);
        assert!(second_saw_first.load(Ordering::Acquire));
    }

    #[test]
    fn teardown_swallows_panic_and_keeps_working() {
        let prev_hook = std::panic::take_hook();
        std::panic::set_hook(Box::new(|_| {}));

        let after_panic_ran = Arc::new(AtomicBool::new(false));
        let mut teardown = Teardown::default();
        teardown.defer(|| panic!("intentional"));
        let set = Arc::clone(&after_panic_ran);
        teardown.defer(move || set.store(true, Ordering::Release));
        drop(teardown);

        std::panic::set_hook(prev_hook);
        assert!(after_panic_ran.load(Ordering::Acquire));
    }

    fn test_config() -> Config {
        RawConfig::default()
            .into_config(false)
            .expect("default config")
    }

    #[test]
    fn broken_config_with_fallback_uses_last_good_and_warns() {
        let mut last_good = test_config();
        last_good.always_fast = true;
        let mut warnings = Vec::new();

        let config = config_or_fallback(Err(eyre!("boom")), Some(last_good), &mut warnings)
            .expect("fallback config");

        assert!(config.always_fast);
        assert_eq!(warnings.len(), 1);
        assert!(
            warnings[0].starts_with(CONFIG_FALLBACK_WARNING),
            "{warnings:?}"
        );
        assert!(warnings[0].contains("boom"), "{warnings:?}");
    }

    #[test]
    fn broken_config_without_fallback_is_fatal() {
        let mut warnings = Vec::new();
        let Err(err) = config_or_fallback(Err(eyre!("boom")), None, &mut warnings) else {
            panic!("expected error without fallback");
        };
        assert!(err.to_string().contains("boom"));
        assert!(warnings.is_empty());
    }

    #[test]
    fn resolution_failure_uses_recent_model_and_warns() {
        let recent = Model::from_spec(
            &n00n_providers::model_registry::test_registry(),
            "codex/gpt-5.6-sol",
        )
        .unwrap();
        let mut warnings = Vec::new();

        let (model, needs_login) = select_startup_model(
            Err(eyre!("no provider")),
            None,
            true,
            Some(recent.clone()),
            None,
            &mut warnings,
            &n00n_providers::model_registry::test_registry(),
        )
        .unwrap();

        assert_eq!(model.spec(), recent.spec());
        assert!(!needs_login);
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains(&recent.spec()));
    }

    #[test]
    fn fresh_install_uses_placeholder_and_opens_login() {
        let mut warnings = Vec::new();

        let (model, needs_login) = select_startup_model(
            Err(eyre!("no provider")),
            None,
            true,
            None,
            None,
            &mut warnings,
            &n00n_providers::model_registry::test_registry(),
        )
        .unwrap();

        assert!(needs_login);
        assert!(warnings.is_empty());
        assert_eq!(model.tier, n00n_providers::model::ModelTier::Strong);
    }

    #[test]
    fn noninteractive_resolution_failure_remains_fatal() {
        let mut warnings = Vec::new();

        let error = select_startup_model(
            Err(eyre!("explicit provider unavailable")),
            None,
            false,
            None,
            None,
            &mut warnings,
            &n00n_providers::model_registry::test_registry(),
        )
        .unwrap_err();

        assert!(error.to_string().contains("explicit provider unavailable"));
        assert!(warnings.is_empty());
    }
}

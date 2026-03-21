//! Single-threaded ratatui event loop; the agent runs on smol tasks in a separate thread.
//! `AgentHandles` bundles all flume channels to the agent. `dispatch()` processes
//! `Action`s returned by `App::update()`. Scroll and drag events are coalesced from
//! the queue to avoid jank.

pub mod animation;
pub mod app;
pub mod chat;
mod components;
mod highlight;
mod image;
mod markdown;
#[cfg(feature = "demo")]
mod mock;
mod render_worker;
mod selection;
pub mod splash;
mod storage_writer;
mod text_buffer;
mod theme;

use std::collections::HashMap;
use std::io::stdout;
use std::mem;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use arc_swap::{ArcSwap, ArcSwapOption};
use color_eyre::Result;
use color_eyre::eyre::Context;
use crossterm::ExecutableCommand;
use crossterm::event::{
    self, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture, Event, MouseButton,
    MouseEvent as CtMouseEvent, MouseEventKind,
};
use crossterm::terminal::{self, EnterAlternateScreen, LeaveAlternateScreen};
use futures_lite::future;
use maki_agent::ToolOutput;
use maki_agent::agent;
use maki_agent::mcp::McpManager;
use maki_agent::mcp::config::{McpServerInfo, persist_enabled};
use maki_agent::skill::Skill;
use maki_agent::template;
use maki_agent::tools::ToolCall;
use maki_agent::{
    Agent, AgentConfig, AgentEvent, AgentInput, AgentParams, AgentRunParams, CancelToken,
    CancelTrigger, Envelope, EventSender, ExtractedCommand, History,
};
use maki_config::UiConfig;
use maki_providers::AgentError;
use maki_providers::Message;
use maki_providers::Model;
use maki_providers::TokenUsage;
use maki_providers::provider::{Provider, fetch_all_models, from_model};
use maki_storage::DataDir;
use tracing::{error, info, warn};

pub type AppSession = maki_storage::sessions::Session<Message, TokenUsage, ToolOutput>;

use app::shell::{ShellEvent, spawn_shell};
use app::{App, Msg};
use chat::history_to_display;
use components::Action;

const ANIMATION_INTERVAL_MS: u64 = 16;
const IDLE_POLL_INTERVAL_MS: u64 = 100;

struct TerminalGuard;

impl TerminalGuard {
    fn init() -> Result<(Self, ratatui::DefaultTerminal)> {
        let terminal = ratatui::init();
        stdout().execute(EnableBracketedPaste)?;
        stdout().execute(EnableMouseCapture)?;
        Ok((Self, terminal))
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        stdout().execute(DisableMouseCapture).ok();
        stdout().execute(event::DisableBracketedPaste).ok();
        ratatui::restore();
    }
}

#[allow(clippy::too_many_arguments)]
pub fn run(
    model: Model,
    skills: Vec<Skill>,
    session: AppSession,
    storage: DataDir,
    config: AgentConfig,
    ui_config: UiConfig,
    input_history_size: usize,
    #[cfg(feature = "demo")] demo: bool,
) -> Result<String> {
    let (_guard, mut terminal) = TerminalGuard::init()?;
    let event_loop = EventLoop::new(
        &mut terminal,
        model,
        skills,
        session,
        storage,
        config,
        ui_config,
        input_history_size,
        #[cfg(feature = "demo")]
        demo,
    )?;
    event_loop.run()
}

struct EventLoop<'t> {
    terminal: &'t mut ratatui::DefaultTerminal,
    app: App,
    handles: AgentHandles,
    provider: Arc<dyn Provider>,
    model: Model,
    skills: Arc<[Skill]>,
    config: AgentConfig,
    shell_tx: flume::Sender<ShellEvent>,
    shell_rx: flume::Receiver<ShellEvent>,
    warn_rx: flume::Receiver<String>,
    storage_writer: Arc<storage_writer::StorageWriter>,
    _model_fetch_task: smol::Task<()>,
}

impl<'t> EventLoop<'t> {
    #[allow(clippy::too_many_arguments)]
    fn new(
        terminal: &'t mut ratatui::DefaultTerminal,
        model: Model,
        skills: Vec<Skill>,
        session: AppSession,
        storage: DataDir,
        config: AgentConfig,
        ui_config: UiConfig,
        input_history_size: usize,
        #[cfg(feature = "demo")] demo: bool,
    ) -> Result<Self> {
        let available_models: Arc<ArcSwapOption<Vec<String>>> = Arc::new(ArcSwapOption::empty());
        let available_models_bg = Arc::clone(&available_models);
        let (warn_tx, warn_rx) = flume::unbounded::<String>();
        let _model_fetch_task = smol::spawn(async move {
            let warn_tx = warn_tx;
            fetch_all_models(|batch| {
                for w in batch.warnings {
                    let _ = warn_tx.try_send(w);
                }
                if batch.models.is_empty() {
                    return;
                }
                let mut merged = available_models_bg
                    .load()
                    .as_deref()
                    .cloned()
                    .unwrap_or_default();
                merged.extend(batch.models);
                available_models_bg.store(Some(Arc::new(merged)));
            })
            .await;
        });

        let storage_writer = Arc::new(storage_writer::StorageWriter::new(storage.clone()));
        let mcp_state = McpState {
            disabled: Vec::new(),
            infos: Arc::new(ArcSwap::from_pointee(Vec::new())),
            pids: Arc::new(Mutex::new(Vec::new())),
        };

        let (shell_tx, shell_rx) = flume::unbounded::<ShellEvent>();

        std::thread::spawn(highlight::warmup);

        let resumed = !session.messages.is_empty();
        let initial_history = session.messages.clone();
        let mut app = App::new(
            model.spec(),
            model.pricing.clone(),
            model.context_window,
            session,
            storage,
            available_models,
            Arc::clone(&mcp_state.infos),
            Arc::clone(&storage_writer),
            ui_config,
            input_history_size,
        );
        #[cfg(feature = "demo")]
        if demo {
            app.status = components::Status::Streaming;
            app.run_id = 1;
            for event in mock::mock_events() {
                match event {
                    mock::MockEvent::User(text) => app.main_chat().push_user_message(&text),
                    mock::MockEvent::Error(text) => {
                        app.main_chat().push(components::DisplayMessage::new(
                            components::DisplayRole::Error,
                            text,
                        ));
                    }
                    mock::MockEvent::Flush => app.flush_all_chats(),
                    mock::MockEvent::Agent(envelope) => {
                        app.update(Msg::Agent(envelope));
                    }
                }
            }
            app.flush_all_chats();
            if let Some(idx) = app.chat_index_for(mock::question_tool_id()) {
                app.set_demo_questions(idx, mock::mock_questions());
            }
            app.status = components::Status::Idle;
        }
        let provider: Arc<dyn Provider> = Arc::from(from_model(&model).context("create provider")?);
        let skills: Arc<[Skill]> = Arc::from(skills);
        let handles = spawn_agent(
            &provider,
            &model,
            initial_history,
            &skills,
            config,
            mcp_state,
        );
        handles.apply_to_app(&mut app);
        if resumed {
            app.token_usage = app.session.token_usage;
            *handles
                .tool_outputs
                .lock()
                .unwrap_or_else(|e| e.into_inner()) = app.session.tool_outputs.clone();
            let display_msgs = history_to_display(
                &app.session.messages,
                &app.session.tool_outputs,
                app.ui_config.tool_output_lines,
            );
            app.main_chat().load_messages(display_msgs);
            app.todo_panel.restore(&app.session.tool_outputs);
        }

        Ok(Self {
            terminal,
            app,
            handles,
            provider,
            model,
            skills,
            config,
            shell_tx,
            shell_rx,
            warn_rx,
            storage_writer,
            _model_fetch_task,
        })
    }

    fn run(mut self) -> Result<String> {
        loop {
            self.tick();
            self.terminal.draw(|f| self.app.view(f))?;
            let had_agent_msg = self.drain_channels();

            if self.app.should_quit {
                return Ok(self.shutdown());
            }

            self.poll_and_handle_input(had_agent_msg)?;
        }
    }

    fn tick(&mut self) {
        self.app.tick_edge_scroll();
        self.app.tick_error_expiry();
        self.app.poll_image_paste();
        self.app.btw_modal.poll();
    }

    fn drain_channels(&mut self) -> bool {
        while let Ok(event) = self.shell_rx.try_recv() {
            self.app.handle_shell_event(event);
        }

        let mut had_agent_msg = false;
        while let Ok(envelope) = self.handles.agent_rx.try_recv() {
            had_agent_msg = true;
            let actions = self.app.update(Msg::Agent(Box::new(envelope)));
            self.dispatch(actions);
        }

        while let Ok(warning) = self.warn_rx.try_recv() {
            self.app.flash(warning);
        }

        had_agent_msg
    }

    fn poll_and_handle_input(&mut self, had_agent_msg: bool) -> Result<()> {
        let poll_duration = if had_agent_msg {
            Duration::ZERO
        } else if self.app.is_animating() {
            Duration::from_millis(ANIMATION_INTERVAL_MS)
        } else {
            Duration::from_millis(IDLE_POLL_INTERVAL_MS)
        };

        if !event::poll(poll_duration)? {
            return Ok(());
        }

        if let Some(msg) = self.translate_input()? {
            let actions = self.app.update(msg);
            self.dispatch(actions);
        }
        Ok(())
    }

    fn translate_input(&mut self) -> Result<Option<Msg>> {
        let raw = event::read()?;
        match raw {
            Event::Key(key) => Ok(Some(Msg::Key(key))),
            Event::Paste(text) => Ok(Some(Msg::Paste(text))),
            Event::Mouse(mouse) => Ok(self.translate_mouse(mouse)),
            _ => Ok(None),
        }
    }

    fn translate_mouse(&mut self, mouse: CtMouseEvent) -> Option<Msg> {
        match mouse.kind {
            MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => {
                let (scroll, extra) = aggregate_scroll(
                    mouse.column,
                    mouse.row,
                    scroll_delta(mouse.kind, self.app.ui_config.mouse_scroll_lines),
                    self.app.ui_config.mouse_scroll_lines,
                );
                if let Some(extra) = extra {
                    let actions = self.app.update(scroll);
                    self.dispatch(actions);
                    Some(extra)
                } else {
                    Some(scroll)
                }
            }
            MouseEventKind::Drag(MouseButton::Left) => {
                let (drag, extra) = coalesce_drag(mouse);
                let actions = self.app.update(Msg::Mouse(drag));
                self.dispatch(actions);
                extra
            }
            _ => Some(Msg::Mouse(mouse)),
        }
    }

    fn dispatch(&mut self, actions: Vec<Action>) {
        for action in actions {
            self.handle_action(action);
        }
    }

    fn handle_action(&mut self, action: Action) {
        match action {
            Action::SendMessage(mut input) => {
                input.preamble = self.app.shell.drain_results();
                let cmd = AgentCommand::Run(input, self.app.run_id);
                if self.handles.cmd_tx.try_send(cmd).is_err() {
                    self.handles.respawn(
                        Vec::new(),
                        &self.provider,
                        &self.model,
                        &self.skills,
                        self.config,
                        &mut self.app,
                    );
                }
            }
            Action::CancelAgent => {
                let _ = self.handles.cmd_tx.try_send(AgentCommand::Cancel);
            }
            Action::NewSession => {
                self.handles.respawn(
                    Vec::new(),
                    &self.provider,
                    &self.model,
                    &self.skills,
                    self.config,
                    &mut self.app,
                );
            }
            Action::LoadSession(loaded) => {
                self.handles.respawn(
                    loaded.messages,
                    &self.provider,
                    &self.model,
                    &self.skills,
                    self.config,
                    &mut self.app,
                );
                *self
                    .handles
                    .tool_outputs
                    .lock()
                    .unwrap_or_else(|e| e.into_inner()) = loaded.tool_outputs;
            }
            Action::ChangeModel(spec) => self.change_model(spec),
            Action::Compact => {
                let _ = self
                    .handles
                    .cmd_tx
                    .try_send(AgentCommand::Compact(self.app.run_id));
            }
            Action::ToggleMcp(server_name, enabled) => {
                toggle_disabled(&mut self.handles.mcp.disabled, &server_name, enabled);
                let _ = self
                    .handles
                    .cmd_tx
                    .try_send(AgentCommand::ToggleMcp(server_name, enabled));
            }
            Action::ShellCommand {
                id,
                command,
                visible,
            } => {
                let (trigger, cancel) = CancelToken::new();
                self.app.shell.add_trigger(trigger);
                spawn_shell(
                    command,
                    id,
                    visible,
                    self.shell_tx.clone(),
                    cancel,
                    self.config,
                );
            }
            Action::OpenEditor(path) => {
                if let Err(e) = open_in_editor(&path, self.terminal) {
                    self.app.flash(e);
                }
            }
            Action::Btw(question) => {
                self.app
                    .start_btw(question, Arc::clone(&self.provider), self.model.clone());
            }
            Action::Quit => {}
        }
    }

    fn change_model(&mut self, spec: String) {
        match Model::from_spec(&spec) {
            Ok(new_model) => match from_model(&new_model) {
                Ok(new_provider) => {
                    self.app.update_model(&new_model);
                    let history = self
                        .handles
                        .history
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .clone();
                    self.provider = Arc::from(new_provider);
                    self.model = new_model;
                    self.handles.respawn(
                        history,
                        &self.provider,
                        &self.model,
                        &self.skills,
                        self.config,
                        &mut self.app,
                    );
                }
                Err(e) => self.app.flash(format!("Failed to create provider: {e}")),
            },
            Err(e) => self.app.flash(format!("Invalid model: {e}")),
        }
    }

    fn shutdown(mut self) -> String {
        let session_id = self.app.session.id.clone();
        maki_agent::mcp::kill_process_groups(
            &self
                .handles
                .mcp
                .pids
                .lock()
                .unwrap_or_else(|e| e.into_inner()),
        );
        self.app.cmd_tx = None;
        self.app.answer_tx = None;
        drop(self.app);
        self.handles.shutdown(Duration::from_secs(3));
        match Arc::try_unwrap(self.storage_writer) {
            Ok(writer) => writer.shutdown(Duration::from_secs(3)),
            Err(_) => {
                warn!("storage writer has outstanding references, skipping graceful shutdown")
            }
        }
        session_id
    }
}

pub(crate) enum AgentCommand {
    Run(AgentInput, u64),
    Compact(u64),
    Cancel,
    ToggleMcp(String, bool),
}

#[derive(Clone, Default)]
struct McpState {
    disabled: Vec<String>,
    infos: Arc<ArcSwap<Vec<McpServerInfo>>>,
    pids: Arc<Mutex<Vec<u32>>>,
}

struct AgentHandles {
    cmd_tx: flume::Sender<AgentCommand>,
    agent_rx: flume::Receiver<Envelope>,
    answer_tx: flume::Sender<String>,
    history: Arc<Mutex<Vec<Message>>>,
    tool_outputs: Arc<Mutex<HashMap<String, ToolOutput>>>,
    mcp: McpState,
    task: smol::Task<()>,
}

impl AgentHandles {
    fn apply_to_app(&self, app: &mut App) {
        app.answer_tx = Some(self.answer_tx.clone());
        app.cmd_tx = Some(self.cmd_tx.clone());
        app.shared_history = Some(Arc::clone(&self.history));
        app.shared_tool_outputs = Some(Arc::clone(&self.tool_outputs));
    }

    fn cancel(self) {
        let _ = self.cmd_tx.try_send(AgentCommand::Cancel);
    }

    fn respawn(
        &mut self,
        history: Vec<Message>,
        provider: &Arc<dyn Provider>,
        model: &Model,
        skills: &Arc<[Skill]>,
        config: AgentConfig,
        app: &mut App,
    ) {
        let mcp = self.mcp.clone();
        let old = mem::replace(
            self,
            spawn_agent(provider, model, history, skills, config, mcp),
        );
        old.cancel();
        self.apply_to_app(app);
    }

    fn shutdown(self, timeout: Duration) {
        let _ = self.cmd_tx.try_send(AgentCommand::Cancel);
        let task = self.task;
        drop((self.cmd_tx, self.agent_rx, self.answer_tx));
        info!("waiting for agent to finish (timeout {timeout:?})");
        smol::block_on(async {
            let finished = futures_lite::future::or(
                async {
                    task.await;
                    true
                },
                async {
                    smol::Timer::after(timeout).await;
                    false
                },
            )
            .await;
            if !finished {
                warn!("agent did not finish within {timeout:?}, forcing shutdown");
            }
        });
    }
}

fn spawn_agent(
    provider: &Arc<dyn Provider>,
    model: &Model,
    initial_history: Vec<Message>,
    skills: &Arc<[Skill]>,
    config: AgentConfig,
    mcp_state: McpState,
) -> AgentHandles {
    let (agent_tx, agent_rx) = flume::unbounded::<Envelope>();
    let (cmd_tx, cmd_rx) = flume::unbounded::<AgentCommand>();
    let (answer_tx, answer_rx) = flume::unbounded::<String>();
    let (ecmd_tx, ecmd_rx) = flume::unbounded::<ExtractedCommand>();
    let shared_history: Arc<Mutex<Vec<Message>>> = Arc::new(Mutex::new(initial_history.clone()));
    let shared_history_inner = Arc::clone(&shared_history);
    let shared_tool_outputs: Arc<Mutex<HashMap<String, ToolOutput>>> =
        Arc::new(Mutex::new(HashMap::new()));
    let model = model.clone();
    let provider = Arc::clone(provider);
    let skills = Arc::clone(skills);
    let mcp_infos = Arc::clone(&mcp_state.infos);
    let mcp_pids = Arc::clone(&mcp_state.pids);
    let initial_disabled = mcp_state.disabled.clone();

    let task = smol::spawn(async move {
        let answer_mutex = Arc::new(async_lock::Mutex::new(answer_rx));
        let vars = template::env_vars();
        let cwd_owned = vars.apply("{cwd}").into_owned();
        let cwd_path = PathBuf::from(&cwd_owned);
        let (instructions, loaded_instructions) =
            smol::unblock(move || agent::load_instruction_files(&cwd_owned)).await;
        let mut tools =
            ToolCall::definitions(&vars, &skills, model.family.supports_tool_examples());

        let mcp_config = maki_agent::mcp::config::load_config(&cwd_path);
        let mut disabled: Vec<String> = initial_disabled;
        disabled.sort_unstable();
        disabled.dedup();

        if !mcp_config.is_empty() {
            mcp_infos.store(Arc::new(mcp_config.preliminary_infos(&disabled)));
        }

        let mcp_manager = McpManager::start_with_config(mcp_config).await;

        if let Some(ref mgr) = mcp_manager {
            mgr.extend_tools(&mut tools, &disabled);
            mcp_infos.store(Arc::new(mgr.server_infos(&disabled)));
            *mcp_pids.lock().unwrap_or_else(|e| e.into_inner()) = mgr.child_pids();
        }

        let cancel_trigger: Arc<Mutex<Option<CancelTrigger>>> = Arc::new(Mutex::new(None));
        let cancel_trigger_fwd = Arc::clone(&cancel_trigger);

        let (toggle_tx, toggle_rx) = flume::unbounded::<(String, bool)>();

        smol::spawn(async move {
            while let Ok(cmd) = cmd_rx.recv_async().await {
                let extracted = match cmd {
                    AgentCommand::Run(input, run_id) => ExtractedCommand::Interrupt(input, run_id),
                    AgentCommand::Cancel => {
                        if let Some(trigger) = cancel_trigger_fwd
                            .lock()
                            .unwrap_or_else(|e| e.into_inner())
                            .take()
                        {
                            trigger.cancel();
                        }
                        ExtractedCommand::Cancel
                    }
                    AgentCommand::Compact(run_id) => ExtractedCommand::Compact(run_id),
                    AgentCommand::ToggleMcp(server_name, enabled) => {
                        let _ = toggle_tx.try_send((server_name, enabled));
                        continue;
                    }
                };
                if ecmd_tx.try_send(extracted).is_err() {
                    break;
                }
            }
        })
        .detach();

        let mut ecmd_rx = ecmd_rx;
        let mut history = History::new(initial_history);
        let mut min_run_id = 0u64;

        enum LoopEvent {
            Cmd(ExtractedCommand),
            Toggle(String, bool),
        }

        loop {
            let event = future::race(
                async { ecmd_rx.recv_async().await.ok().map(LoopEvent::Cmd) },
                async {
                    toggle_rx
                        .recv_async()
                        .await
                        .ok()
                        .map(|(s, e)| LoopEvent::Toggle(s, e))
                },
            )
            .await;

            let Some(event) = event else { break };

            let cmd = match event {
                LoopEvent::Toggle(server_name, enabled) => {
                    toggle_disabled(&mut disabled, &server_name, enabled);
                    let mut new_tools = ToolCall::definitions(
                        &vars,
                        &skills,
                        model.family.supports_tool_examples(),
                    );
                    if let Some(ref mcp) = mcp_manager {
                        mcp.extend_tools(&mut new_tools, &disabled);
                        let infos = mcp.server_infos(&disabled);
                        if let Some(info) = infos.iter().find(|i| i.name == server_name) {
                            let path = info.config_path.clone();
                            let name = server_name.clone();
                            smol::spawn(async move {
                                if let Err(e) = smol::unblock(move || persist_enabled(&path, &name, enabled)).await {
                                    tracing::warn!(error = %e, server = %server_name, "failed to persist MCP toggle");
                                }
                            })
                            .detach();
                        }
                        mcp_infos.store(Arc::new(infos));
                    }
                    tools = new_tools;
                    continue;
                }
                LoopEvent::Cmd(cmd) => cmd,
            };

            let (event_tx, current_run_id) = match &cmd {
                ExtractedCommand::Interrupt(_, run_id) | ExtractedCommand::Compact(run_id)
                    if *run_id >= min_run_id =>
                {
                    (EventSender::new(agent_tx.clone(), *run_id), *run_id)
                }
                _ => continue,
            };
            let result = match cmd {
                ExtractedCommand::Compact(_) => {
                    let r = agent::compact(&*provider, &model, &mut history, &event_tx).await;
                    *shared_history_inner
                        .lock()
                        .unwrap_or_else(|e| e.into_inner()) = history.as_slice().to_vec();
                    r
                }
                ExtractedCommand::Cancel | ExtractedCommand::Ignore => unreachable!(),
                ExtractedCommand::Interrupt(mut input, _) => {
                    for msg in mem::take(&mut input.preamble) {
                        history.push(msg);
                    }
                    let system = agent::build_system_prompt(&vars, &input.mode, &instructions);
                    let (trigger, cancel) = CancelToken::new();
                    *cancel_trigger.lock().unwrap_or_else(|e| e.into_inner()) = Some(trigger);
                    let agent = Agent::new(
                        AgentParams {
                            provider: Arc::clone(&provider),
                            model: model.clone(),
                            skills: Arc::clone(&skills),
                            config,
                        },
                        AgentRunParams {
                            history: mem::replace(&mut history, History::new(Vec::new())),
                            system,
                            event_tx,
                            tools: tools.clone(),
                        },
                    )
                    .with_loaded_instructions(loaded_instructions.clone())
                    .with_user_response_rx(Arc::clone(&answer_mutex))
                    .with_cmd_rx(ecmd_rx)
                    .with_cancel(cancel)
                    .with_mcp(mcp_manager.clone());
                    let outcome = agent.run(input).await;
                    *cancel_trigger.lock().unwrap_or_else(|e| e.into_inner()) = None;
                    history = outcome.history;
                    *shared_history_inner
                        .lock()
                        .unwrap_or_else(|e| e.into_inner()) = history.as_slice().to_vec();
                    ecmd_rx = outcome.cmd_rx.expect("cmd_rx was set");
                    if matches!(outcome.result, Err(AgentError::Cancelled)) {
                        min_run_id = current_run_id + 1;
                    }
                    outcome.result
                }
            };
            match result {
                Ok(()) => {}
                Err(AgentError::Cancelled) => {
                    let event_tx = EventSender::new(agent_tx.clone(), current_run_id);
                    let _ = event_tx.send(AgentEvent::Done {
                        usage: TokenUsage::default(),
                        num_turns: 0,
                        stop_reason: None,
                    });
                }
                Err(e) => {
                    error!(error = %e, "agent error");
                    let event_tx = EventSender::new(agent_tx.clone(), current_run_id);
                    let _ = event_tx.send(AgentEvent::Error {
                        message: e.user_message(),
                    });
                }
            }
        }
    });

    AgentHandles {
        cmd_tx,
        agent_rx,
        answer_tx,
        history: shared_history,
        tool_outputs: shared_tool_outputs,
        mcp: mcp_state,
        task,
    }
}

fn suspend_terminal() {
    terminal::disable_raw_mode().ok();
    stdout().execute(DisableMouseCapture).ok();
    stdout().execute(event::DisableBracketedPaste).ok();
    stdout().execute(LeaveAlternateScreen).ok();
}

fn resume_terminal(terminal: &mut ratatui::DefaultTerminal) {
    stdout().execute(EnterAlternateScreen).ok();
    stdout().execute(EnableBracketedPaste).ok();
    stdout().execute(EnableMouseCapture).ok();
    terminal::enable_raw_mode().ok();
    let _ = terminal.clear();
}

fn open_in_editor(path: &Path, terminal: &mut ratatui::DefaultTerminal) -> Result<(), String> {
    let editor = std::env::var("VISUAL")
        .or_else(|_| std::env::var("EDITOR"))
        .map_err(|_| "Set $VISUAL or $EDITOR to open files".to_string())?;

    suspend_terminal();

    let result = std::process::Command::new(&editor)
        .arg(path)
        .stdin(std::process::Stdio::inherit())
        .stdout(std::process::Stdio::inherit())
        .stderr(std::process::Stdio::inherit())
        .status();

    resume_terminal(terminal);

    match result {
        Ok(status) if !status.success() => Err(format!(
            "{editor} exited with {status} - set $VISUAL or $EDITOR"
        )),
        Err(e) => Err(format!(
            "Failed to open {editor}: {e} - set $VISUAL or $EDITOR"
        )),
        Ok(_) => Ok(()),
    }
}

fn toggle_disabled(disabled: &mut Vec<String>, name: &str, enabled: bool) {
    if enabled {
        disabled.retain(|s| s != name);
    } else if !disabled.contains(&name.to_owned()) {
        disabled.push(name.to_owned());
    }
}

fn scroll_delta(kind: MouseEventKind, lines: u32) -> i32 {
    if kind == MouseEventKind::ScrollUp {
        lines as i32
    } else {
        -(lines as i32)
    }
}

fn aggregate_scroll(
    column: u16,
    row: u16,
    mut delta: i32,
    scroll_lines: u32,
) -> (Msg, Option<Msg>) {
    while event::poll(Duration::ZERO).unwrap_or(false) {
        if let Ok(Event::Mouse(next)) = event::read() {
            match next.kind {
                MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => {
                    delta += scroll_delta(next.kind, scroll_lines);
                }
                _ => return (Msg::Scroll { column, row, delta }, Some(Msg::Mouse(next))),
            }
        } else {
            break;
        }
    }
    (Msg::Scroll { column, row, delta }, None)
}

fn coalesce_drag(mut latest: CtMouseEvent) -> (CtMouseEvent, Option<Msg>) {
    while event::poll(Duration::ZERO).unwrap_or(false) {
        if let Ok(Event::Mouse(next)) = event::read() {
            if matches!(next.kind, MouseEventKind::Drag(MouseButton::Left)) {
                latest = next;
            } else {
                return (latest, Some(Msg::Mouse(next)));
            }
        } else {
            break;
        }
    }
    (latest, None)
}

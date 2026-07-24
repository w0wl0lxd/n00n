//! Multi-session supervisor: every session owns an `App` + `AgentHandles` and
//! keeps draining agent events while backgrounded; only the focused session
//! renders and receives input. `SpawnCtx` carries the shared resources needed
//! to spawn session runtimes at any point.
//!
//! Terminal input arrives on a channel (see [`InputReader`]), so the loop
//! waits on every event source at once and wakes the moment a plugin action,
//! agent event, or keypress arrives instead of sleeping in `event::poll`.

use std::sync::Arc;
use std::time::{Duration, Instant};

use arc_swap::{ArcSwap, ArcSwapOption};
use color_eyre::Result;
use color_eyre::eyre::{Context, eyre};

use crossterm::event::{
    Event, KeyEventKind, MouseButton, MouseEvent as CtMouseEvent, MouseEventKind,
};
use n00n_agent::command::CustomCommand;
use n00n_agent::permissions::PermissionManager;
use n00n_agent::{AgentConfig, CancelToken, McpCommand, McpConfigErrors, McpHandle, mcp};
use n00n_config::UiConfig;
use n00n_lua::{
    EventHandle, HintReader, KeymapReader, LuaCommandReader, SessionReply, SessionRequest, UiAction,
};
use n00n_providers::Timeouts;
use n00n_providers::provider::{
    Provider, fetch_all_models, from_model_fallback_with_openai_options,
    from_model_with_openai_options,
};
use n00n_providers::{Message, Model, OpenAiOptions};
use n00n_storage::StateDir;
use n00n_storage::StorageError;
use n00n_storage::id::{N00nId, N00nIdParseError, SessionRef};
use n00n_storage::sessions::{SessionError, TranscriptEntry, normalize_title};
use serde_json::json;
use tracing::warn;

use crate::AppSession;
use crate::agent::{AgentCommand, AgentHandles, ModelSlot, shared_queue::QueueItem};
use crate::app::shell::{ShellEvent, spawn_shell};
use crate::app::{App, AppInit, Msg, QueuedMessage, SubmitOutcome};
use crate::components::input::Submission;
use crate::components::usage_modal::UsageFetchState;
use crate::components::{
    Action, DisplayMessage, DisplayRole, ExitRequest, Status, SubmissionDispatch,
};
use crate::input::InputReader;

use crate::storage_writer::StorageWriter;
use crate::terminal;
use crate::terminal_image;
use ratatui_image::picker::Picker;

const ANIMATION_INTERVAL_MS: u64 = 16;
const IDLE_POLL_INTERVAL_MS: u64 = 100;
const PERIODIC_SAVE_INTERVAL: Duration = Duration::from_secs(1);
/// Max events handled per frame so a flood cannot starve rendering.
const DRAIN_BUDGET: usize = 256;
const AGENT_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(3);
const DELETE_FOCUSED_ERR: &str = "cannot delete the focused session";
const NOT_LIVE_ERR: &str = "session not live";

/// Tabs carry their in-memory sessions so `/reload` reopens them without a
/// disk round-trip; `session_has_content` tells which ones were saved.
pub(crate) struct ShutdownReport {
    pub exit: ExitRequest,
    pub tabs: Vec<AppSession>,
    pub focused: usize,
}

pub struct EventLoopParams {
    pub model: Model,
    pub needs_login: bool,
    pub commands: Vec<CustomCommand>,
    pub sessions: Vec<AppSession>,
    pub focused: usize,
    pub startup_warnings: Vec<String>,
    pub storage: StateDir,
    pub config: AgentConfig,
    pub ui_config: UiConfig,
    pub input_history_size: usize,
    pub permissions: Arc<PermissionManager>,
    pub timeouts: Timeouts,
    pub openai_options: OpenAiOptions,
    pub exit_on_done: bool,
    pub lua_command_reader: LuaCommandReader,
    pub keymap_reader: KeymapReader,
    pub hint_reader: HintReader,
    pub ui_action_rx: Option<flume::Receiver<UiAction>>,
    pub lua_event_handle: Option<EventHandle>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum SessionStatus {
    Working,
    NeedsInput,
    Idle,
}

impl SessionStatus {
    fn of(app: &App) -> Self {
        if app.awaiting_input() {
            Self::NeedsInput
        } else if app.status == Status::Streaming {
            Self::Working
        } else {
            Self::Idle
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Working => "working",
            Self::NeedsInput => "needs_input",
            Self::Idle => "idle",
        }
    }
}

fn parse_session_id(id: &str) -> Result<N00nId, String> {
    id.parse().map_err(|e: N00nIdParseError| e.to_string())
}

struct SessionRuntime {
    app: App,
    handles: AgentHandles,
    shell_tx: flume::Sender<ShellEvent>,
    shell_rx: flume::Receiver<ShellEvent>,
    last_status: SessionStatus,
}

impl SessionRuntime {
    fn id(&self) -> N00nId {
        self.app.state.session.id
    }
}

/// Everything needed to bring up a new session runtime after startup.
struct SpawnCtx {
    storage: StateDir,
    config: AgentConfig,
    ui_config: UiConfig,
    input_history_size: usize,
    /// Prototype only: every runtime forks its own manager so session
    /// rules stay per-session.
    permissions: Arc<PermissionManager>,
    timeouts: Timeouts,
    openai_options: OpenAiOptions,
    custom_commands: Arc<[CustomCommand]>,
    lua_command_reader: LuaCommandReader,
    keymap_reader: KeymapReader,
    hint_reader: HintReader,
    lua_event_handle: Option<EventHandle>,
    mcp_handle: Option<McpHandle>,
    mcp_config_errors: McpConfigErrors,
    model_slot: Arc<ArcSwap<ModelSlot>>,
    available_models: Arc<ArcSwapOption<Vec<String>>>,
    storage_writer: Arc<StorageWriter>,
    picker: Arc<Picker>,
}

impl SpawnCtx {
    fn spawn_runtime(&self, session: AppSession) -> SessionRuntime {
        let resumed = crate::app::session_has_content(&session);
        let permissions = Arc::new(self.permissions.fork());
        let handles = AgentHandles::spawn(
            &self.model_slot,
            session.messages.clone(),
            session.transcript.clone(),
            self.config.clone(),
            self.ui_config.tool_output_lines,
            &permissions,
            Some(SessionRef::from(session.id)),
            self.timeouts,
            self.openai_options,
            self.lua_event_handle.clone(),
            self.mcp_handle.clone(),
            self.mcp_config_errors.clone(),
        );
        let mut app = App::new(AppInit {
            model: self.model_slot.load().model.clone(),
            session,
            storage: self.storage.clone(),
            available_models: Arc::clone(&self.available_models),
            mcp_reader: handles.mcp_reader(),
            mcp_config_errors: handles.mcp_config_errors.clone(),
            lua_command_reader: self.lua_command_reader.clone(),
            keymap_reader: self.keymap_reader.clone(),
            hint_reader: self.hint_reader.clone(),
            storage_writer: Arc::clone(&self.storage_writer),
            ui_config: self.ui_config.clone(),
            input_history_size: self.input_history_size,
            permissions,
            custom_commands: Arc::clone(&self.custom_commands),
            picker: Arc::clone(&self.picker),
        });
        app.lua_event_handle.clone_from(&self.lua_event_handle);
        handles.apply_to_app(&mut app);
        if resumed {
            restore_session(&mut app, &handles);
        }
        let (shell_tx, shell_rx) = flume::unbounded::<ShellEvent>();
        SessionRuntime {
            app,
            handles,
            shell_tx,
            shell_rx,
            last_status: SessionStatus::Idle,
        }
    }
}

pub(crate) struct EventLoop<'t> {
    terminal: &'t mut ratatui::DefaultTerminal,
    sessions: Vec<SessionRuntime>,
    focused: usize,
    ctx: SpawnCtx,
    input: InputReader,
    warn_rx: flume::Receiver<String>,
    warn_tx: flume::Sender<String>,
    ui_action_rx: Option<flume::Receiver<UiAction>>,
    submission_persist_tx: flume::Sender<SubmissionPersistence>,
    submission_persist_rx: flume::Receiver<SubmissionPersistence>,
    post_draw_submissions: Vec<(N00nId, SubmissionDispatch)>,
    last_save: Instant,
    _model_fetch_task: smol::Task<()>,
    /// Set when UI state changed and a fresh frame must be painted. Draws are
    /// gated on this (or active animation) so we don't re-diff the whole
    /// buffer on every idle tick. Resize also sets it.
    dirty: bool,
}

/// One item from any of the event loop's sources; `None` from `next_wake`
/// means the wait timed out (animation/idle tick).
struct SubmissionPersistence {
    session_id: N00nId,
    dispatch: SubmissionDispatch,
    result: Result<(), SessionError>,
}

enum Wake {
    Input(Event),
    InputGone,
    Ui(UiAction),
    Agent(usize, Box<n00n_agent::Envelope>),
    Shell(usize, ShellEvent),
    SubmissionPersisted(SubmissionPersistence),
    Warn(String),
}

struct DrainScheduler {
    prefer_input: bool,
}

impl Default for DrainScheduler {
    fn default() -> Self {
        Self { prefer_input: true }
    }
}

impl DrainScheduler {
    fn next<T>(
        &mut self,
        mut input: impl FnMut() -> Option<T>,
        mut other: impl FnMut() -> Option<T>,
    ) -> Option<T> {
        let (is_input, item) = if self.prefer_input {
            input()
                .map(|item| (true, item))
                .or_else(|| other().map(|item| (false, item)))
        } else {
            other()
                .map(|item| (false, item))
                .or_else(|| input().map(|item| (true, item)))
        }?;
        self.prefer_input = !is_input;
        Some(item)
    }
}

struct BackgroundModels {
    available: Arc<ArcSwapOption<Vec<String>>>,
    warn_rx: flume::Receiver<String>,
    warn_tx: flume::Sender<String>,
    task: smol::Task<()>,
}

fn merge_batch(
    available: &Arc<ArcSwapOption<Vec<String>>>,
    batch: n00n_providers::provider::ModelBatch,
    warn_tx: &flume::Sender<String>,
) {
    for w in batch.warnings {
        let _ = warn_tx.try_send(w);
    }
    if batch.models.is_empty() {
        return;
    }
    let mut merged = available
        .load()
        .as_deref()
        .cloned()
        .unwrap_or_else(Vec::new);
    for spec in &batch.models {
        if !merged.contains(spec) {
            merged.push(spec.clone());
        }
    }
    available.store(Some(Arc::new(merged)));
}

fn spawn_model_fetch(
    model_slot: &Arc<ArcSwap<ModelSlot>>,
    timeouts: Timeouts,
    openai_options: OpenAiOptions,
) -> BackgroundModels {
    let available: Arc<ArcSwapOption<Vec<String>>> = Arc::new(ArcSwapOption::empty());
    let bg = Arc::clone(&available);
    let (warn_tx, warn_rx) = flume::unbounded::<String>();
    let warn_tx_bg = warn_tx.clone();
    let model_slot = Arc::clone(model_slot);
    let task = smol::spawn(async move {
        let warn_tx = warn_tx_bg;
        let done = Box::new(move || {
            let spec = model_slot.load().model.spec();
            let mut resolved = match Model::from_spec(&spec) {
                Ok(m) => m,
                Err(e) => {
                    warn!(spec = %spec, error = %e, "failed to resolve model after discovery");
                    return;
                }
            };
            let provider = match from_model_with_openai_options(
                &mut resolved,
                timeouts,
                openai_options,
            ) {
                Ok(p) => p,
                Err(e) => {
                    warn!(spec = %spec, error = %e, "failed to create provider after discovery");
                    return;
                }
            };
            model_slot.store(Arc::new(ModelSlot {
                model: resolved,
                provider: Arc::from(provider),
            }));
        });
        fetch_all_models(|batch| merge_batch(&bg, batch, &warn_tx), Some(done)).await;
    });
    BackgroundModels {
        available,
        warn_rx,
        warn_tx,
        task,
    }
}

fn restore_session(app: &mut App, handles: &AgentHandles) {
    app.permissions
        .load_session_rules(crate::app::session_state::stored_to_rules(
            &app.state.session.meta.session_rules,
        ));
    (*handles
        .tool_outputs
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner))
    .clone_from(&app.state.session.tool_outputs);
    app.restore_display();
    for w in app.state.warnings.drain(..) {
        app.status_bar.flash(w);
    }
}

impl<'t> EventLoop<'t> {
    pub(crate) fn new(
        terminal: &'t mut ratatui::DefaultTerminal,
        params: EventLoopParams,
    ) -> Result<Self> {
        static PROCESS_WARMUP: std::sync::Once = std::sync::Once::new();

        let EventLoopParams {
            mut model,
            needs_login,
            commands,
            sessions,
            focused,
            startup_warnings,
            storage,
            config,
            ui_config,
            input_history_size,
            permissions,
            timeouts,
            openai_options,
            exit_on_done,
            lua_command_reader,
            keymap_reader,
            hint_reader,
            ui_action_rx,
            lua_event_handle,
        } = params;

        PROCESS_WARMUP.call_once(|| {
            std::thread::spawn(crate::highlight::warmup);
            crate::update::spawn_check();
        });

        let storage_writer = Arc::new(StorageWriter::new(storage.clone())?);
        let cwd = std::env::current_dir().unwrap_or_else(|_| ".".into());
        let (mcp_handle, mcp_config_errors) =
            smol::block_on(mcp::start(&cwd, config.mcp_tool_desc_max_chars));

        let provider: Arc<dyn Provider> = if needs_login {
            Arc::from(from_model_fallback_with_openai_options(
                &mut model,
                timeouts,
                openai_options,
            ))
        } else {
            Arc::from(
                from_model_with_openai_options(&mut model, timeouts, openai_options)
                    .context("create provider")?,
            )
        };
        let model_slot = Arc::new(ArcSwap::from_pointee(ModelSlot {
            model: model.clone(),
            provider,
        }));
        let bg = spawn_model_fetch(&model_slot, timeouts, openai_options);

        let picker = Arc::new(terminal_image::picker());

        let ctx = SpawnCtx {
            storage,
            config,
            ui_config,
            input_history_size,
            permissions,
            timeouts,
            openai_options,
            custom_commands: Arc::from(commands),
            lua_command_reader,
            keymap_reader,
            hint_reader,
            lua_event_handle,
            mcp_handle,
            mcp_config_errors,
            model_slot,
            available_models: bg.available,
            storage_writer,
            picker,
        };

        let mut runtimes: Vec<SessionRuntime> = sessions
            .into_iter()
            .map(|session| ctx.spawn_runtime(session))
            .collect();
        if runtimes.is_empty() {
            return Err(eyre!("event loop needs at least one session"));
        }
        let focused = focused.min(runtimes.len() - 1);
        let app = &mut runtimes[focused].app;
        app.exit_on_done = exit_on_done;
        if needs_login {
            app.login_picker.open(app.storage.clone());
        }
        if !ctx.mcp_config_errors.is_empty() {
            let msg = format!("MCP config error: {}", ctx.mcp_config_errors);
            app.flash(msg);
        }
        for w in startup_warnings {
            app.flash(w);
        }

        let (submission_persist_tx, submission_persist_rx) = flume::unbounded();
        Ok(Self {
            terminal,
            sessions: runtimes,
            focused,
            ctx,
            input: InputReader::spawn()?,
            warn_rx: bg.warn_rx,
            warn_tx: bg.warn_tx,
            ui_action_rx,
            submission_persist_tx,
            submission_persist_rx,
            post_draw_submissions: Vec::new(),
            last_save: Instant::now(),
            _model_fetch_task: bg.task,
            dirty: true,
        })
    }

    fn focused_app(&mut self) -> &mut App {
        &mut self.sessions[self.focused].app
    }

    pub(crate) fn run(mut self, initial_prompt: Option<String>) -> Result<ShutdownReport> {
        if let Some(prompt) = initial_prompt {
            let sub = Submission {
                text: prompt,
                images: Vec::new(),
            };
            let actions = self.focused_app().handle_submit(sub);
            self.dispatch(self.focused, actions);
        }
        let result = loop {
            self.tick();
            if let Err(e) = self.drain_channels() {
                break Err(e);
            }
            let should_draw = self.dirty
                || self.sessions[self.focused].app.is_animating()
                || !self.post_draw_submissions.is_empty();
            let app = &mut self.sessions[self.focused].app;
            if should_draw {
                if let Err(e) = draw_then_post_terminal(self.terminal, |f| app.view(f), || {}) {
                    break Err(e.into());
                }
                self.dirty = false;
                self.after_terminal_draw();
            }

            if let Some(i) = self
                .sessions
                .iter()
                .position(|rt| rt.app.exit_request != ExitRequest::None)
            {
                // A backgrounded session can finish an `exit_on_done` turn;
                // focus it so shutdown reports its exit code and id.
                self.focused = i;
                break Ok(());
            }

            let timeout = if self.sessions[self.focused].app.is_animating() {
                Duration::from_millis(ANIMATION_INTERVAL_MS)
            } else {
                Duration::from_millis(IDLE_POLL_INTERVAL_MS)
            };
            if let Some(wake) = self.next_wake(timeout)
                && let Err(e) = self.handle_wake(wake)
            {
                break Err(e);
            }
        };
        // Fatal errors still save every session, kill MCP process groups,
        // and drain the storage writer before the process exits.
        let report = self.shutdown();
        result.map(|()| report)
    }

    /// Wait for the next event from any source, or time out so animations
    /// and periodic polls keep running. Already-pending input wins before
    /// joining the fair selector.
    fn next_wake(&self, timeout: Duration) -> Option<Wake> {
        self.try_input_wake().or_else(|| self.select_wake(timeout))
    }

    fn try_input_wake(&self) -> Option<Wake> {
        self.input.receiver().try_recv().ok().map(Wake::Input)
    }

    fn select_wake(&self, timeout: Duration) -> Option<Wake> {
        let mut sel = flume::Selector::new().recv(self.input.receiver(), |res| match res {
            Ok(ev) => Some(Wake::Input(ev)),
            Err(_) => Some(Wake::InputGone),
        });
        if let Some(rx) = self
            .ui_action_rx
            .as_ref()
            .filter(|rx| !rx.is_disconnected())
        {
            sel = sel.recv(rx, |res| res.ok().map(Wake::Ui));
        }
        sel = sel.recv(&self.warn_rx, |res| res.ok().map(Wake::Warn));
        sel = sel.recv(&self.submission_persist_rx, |res| {
            res.ok().map(Wake::SubmissionPersisted)
        });
        for (i, rt) in self.sessions.iter().enumerate() {
            if !rt.handles.agent_rx.is_disconnected() {
                sel = sel.recv(&rt.handles.agent_rx, move |res| {
                    res.ok().map(|env| Wake::Agent(i, Box::new(env)))
                });
            }
            sel = sel.recv(&rt.shell_rx, move |res| {
                res.ok().map(|ev| Wake::Shell(i, ev))
            });
        }
        sel.wait_timeout(timeout).ok().flatten()
    }

    fn next_non_input_wake(&self) -> Option<Wake> {
        let mut sel = flume::Selector::new();
        if let Some(rx) = self
            .ui_action_rx
            .as_ref()
            .filter(|rx| !rx.is_disconnected())
        {
            sel = sel.recv(rx, |res| res.ok().map(Wake::Ui));
        }
        sel = sel.recv(&self.warn_rx, |res| res.ok().map(Wake::Warn));
        sel = sel.recv(&self.submission_persist_rx, |res| {
            res.ok().map(Wake::SubmissionPersisted)
        });
        for (i, rt) in self.sessions.iter().enumerate() {
            if !rt.handles.agent_rx.is_disconnected() {
                sel = sel.recv(&rt.handles.agent_rx, move |res| {
                    res.ok().map(|env| Wake::Agent(i, Box::new(env)))
                });
            }
            sel = sel.recv(&rt.shell_rx, move |res| {
                res.ok().map(|ev| Wake::Shell(i, ev))
            });
        }
        sel.wait_timeout(Duration::ZERO).ok().flatten()
    }

    fn handle_wake(&mut self, wake: Wake) -> Result<()> {
        self.dirty = true;
        match wake {
            Wake::Input(ev) => self.handle_input(ev),
            Wake::InputGone => return Err(eyre!("terminal input reader stopped")),
            Wake::Ui(action) => self.handle_ui_action(action),
            Wake::Agent(i, envelope) => self.handle_agent(i, envelope),
            Wake::Shell(i, event) => self.sessions[i].app.handle_shell_event(event),
            Wake::SubmissionPersisted(completion) => self.handle_submission_persisted(completion),
            Wake::Warn(warning) => self.focused_app().flash(warning),
        }
        Ok(())
    }

    fn tick(&mut self) {
        for (i, rt) in self.sessions.iter_mut().enumerate() {
            rt.app.float_mgr.tick();
            if i != self.focused {
                continue;
            }
            rt.app.tick_edge_scroll();
            rt.app.tick_error_expiry();
            rt.app.poll_image_paste();
            rt.app.btw_modal.poll();
            rt.app.status_bar.poll_branch_update();
            rt.app.mcp_picker.refresh();
        }
        self.tick_periodic_save();
    }

    fn tick_periodic_save(&mut self) {
        if self.last_save.elapsed() < PERIODIC_SAVE_INTERVAL {
            return;
        }
        let app = &mut self.sessions[self.focused].app;
        if app.status != Status::Streaming {
            return;
        }
        app.save_session();
        self.last_save = Instant::now();
    }

    fn handle_agent(&mut self, idx: usize, envelope: Box<n00n_agent::Envelope>) {
        let actions = self.sessions[idx].app.update(Msg::Agent(envelope));
        self.dispatch(idx, actions);
    }

    fn drain_channels(&mut self) -> Result<()> {
        // Leftovers beyond the budget are picked up right after the next draw.
        let mut scheduler = DrainScheduler::default();
        for _ in 0..DRAIN_BUDGET {
            let Some(wake) =
                scheduler.next(|| self.try_input_wake(), || self.next_non_input_wake())
            else {
                break;
            };
            self.handle_wake(wake)?;
        }

        for rt in &mut self.sessions {
            if rt.app.status == Status::Streaming && rt.handles.agent_rx.is_disconnected() {
                rt.app.status = Status::error("agent stopped unexpectedly".into());
                self.dirty = true;
            }
        }

        let slot_model = self.ctx.model_slot.load();
        let spec = slot_model.model.spec();
        for rt in &mut self.sessions {
            if rt.app.state.session.model != spec {
                rt.app.update_model(&slot_model.model);
                self.dirty = true;
            }
        }
        drop(slot_model);

        self.emit_status_changes();
        Ok(())
    }

    fn handle_ui_action(&mut self, action: UiAction) {
        match action {
            UiAction::Flash(msg) => {
                self.focused_app().flash(msg);
            }
            UiAction::OpenEditor { path, reply_tx } => {
                let code = self.open_editor(self.focused, &path);
                let _ = reply_tx.send(code);
            }
            UiAction::OpenWin {
                buf,
                config,
                focus,
                event_tx,
                cmd_rx,
            } => {
                let app = self.focused_app();
                app.float_mgr.open(buf, config, focus, event_tx, cmd_rx);
                if focus {
                    app.transition_plan(&crate::app::mode::PlanTrigger::InteractivePrompt);
                }
            }
            UiAction::PickModel { current, reply_tx } => {
                self.focused_app()
                    .pick_model_for_lua(current.as_deref(), reply_tx);
                self.handle_action(self.focused, Action::RefreshModels);
            }
            UiAction::Session { req, reply_tx } => {
                self.handle_session_request(req, reply_tx);
            }
        }
    }

    /// Exits with the editor's status code; `-1` (flashed on the session's
    /// app) when the editor could not be launched.
    fn open_editor(&mut self, idx: usize, path: &std::path::Path) -> i32 {
        let result = match self.input.pause() {
            Ok(_pause) => terminal::open_in_editor(path, self.terminal),
            Err(e) => Err(e),
        };
        match result {
            Ok(code) => code,
            Err(e) => {
                self.sessions[idx].app.flash(e);
                -1
            }
        }
    }

    fn emit_status_changes(&mut self) {
        let Some(handle) = self.ctx.lua_event_handle.as_ref() else {
            return;
        };
        for (i, rt) in self.sessions.iter_mut().enumerate() {
            let status = SessionStatus::of(&rt.app);
            if status == rt.last_status {
                continue;
            }
            rt.last_status = status;
            handle.fire_autocmd(
                "SessionStatusChanged",
                json!({
                    "session_id": rt.id(),
                    "title": rt.app.state.session.title,
                    "status": status.as_str(),
                    "focused": i == self.focused,
                }),
            );
        }
    }

    /// `List` replies from a background task (the scan can be slow); every
    /// other request is answered synchronously by the event loop, which owns
    /// the live runtimes.
    fn handle_session_request(
        &mut self,
        req: SessionRequest,
        reply_tx: flume::Sender<SessionReply>,
    ) {
        match req {
            SessionRequest::List => {
                let storage = self.ctx.storage.clone();
                smol::unblock(move || {
                    let cwd =
                        std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
                    let reply = AppSession::list(&cwd.to_string_lossy(), &storage)
                        .map_err(|e| e.to_string())
                        .and_then(|list| serde_json::to_value(list).map_err(|e| e.to_string()));
                    let _ = reply_tx.send(reply);
                })
                .detach();
            }
            // Deletes run on the storage writer thread after any queued
            // flushes, so the loop never blocks on disk and a queued save
            // cannot resurrect the files.
            SessionRequest::Delete { id } => {
                let id = match parse_session_id(&id) {
                    Ok(id) => id,
                    Err(e) => {
                        let _ = reply_tx.send(Err(e));
                        return;
                    }
                };
                if let Some(i) = self.position(id) {
                    if i == self.focused {
                        let _ = reply_tx.send(Err(DELETE_FOCUSED_ERR.into()));
                        return;
                    }
                    let rt = self.remove_runtime(i);
                    rt.handles.cancel();
                }
                self.ctx.storage_writer.delete(id, move |res| {
                    let reply = match res {
                        Ok(()) | Err(SessionError::Storage(StorageError::NotFound(_))) => {
                            Ok(json!(true))
                        }
                        Err(e) => Err(e.to_string()),
                    };
                    let _ = reply_tx.send(reply);
                });
            }
            SessionRequest::Live => {
                let list: Vec<_> = self
                    .sessions
                    .iter()
                    .enumerate()
                    .map(|(i, rt)| {
                        json!({
                            "id": rt.id(),
                            "title": rt.app.state.session.title,
                            "status": SessionStatus::of(&rt.app).as_str(),
                            "updated_at": rt.app.state.session.updated_at,
                            "focused": i == self.focused,
                        })
                    })
                    .collect();
                let _ = reply_tx.send(Ok(json!(list)));
            }
            SessionRequest::Status { id } => {
                let reply = parse_session_id(&id).and_then(|id| {
                    let idx = self
                        .position(id)
                        .ok_or_else(|| format!("{NOT_LIVE_ERR}: {id}"))?;
                    let rt = &self.sessions[idx];
                    let history = rt.handles.history.load();
                    let output = history.iter().rev().find_map(|message| {
                        matches!(message.role, n00n_providers::Role::Assistant)
                            .then(|| message.first_text_content())
                            .flatten()
                    });
                    Ok(json!({
                        "id": rt.id(),
                        "title": rt.app.state.session.title,
                        "status": SessionStatus::of(&rt.app).as_str(),
                        "updated_at": rt.app.state.session.updated_at,
                        "focused": idx == self.focused,
                        "output": output,
                    }))
                });
                let _ = reply_tx.send(reply);
            }
            SessionRequest::Current => {
                let _ = reply_tx.send(Ok(json!(self.sessions[self.focused].id())));
            }
            SessionRequest::New { prompt, focus } => {
                let session = {
                    let slot = self.ctx.model_slot.load();
                    let cwd = std::env::current_dir().unwrap_or_else(|_| ".".into());
                    AppSession::new(&slot.model.spec(), &cwd.to_string_lossy())
                };
                let idx = self.push_runtime(self.ctx.spawn_runtime(session));
                let id = self.sessions[idx].id();
                if let Some(prompt) = prompt {
                    let _ = self.submit_text(idx, prompt);
                }
                if focus {
                    self.set_focus(idx);
                }
                let _ = reply_tx.send(Ok(json!(id)));
            }
            SessionRequest::Prompt { id, text } => {
                let idx = match id {
                    None => Ok(self.focused),
                    Some(id) => parse_session_id(&id).and_then(|id| {
                        self.position(id)
                            .ok_or_else(|| format!("{NOT_LIVE_ERR}: {id}"))
                    }),
                };
                let _ = reply_tx.send(idx.and_then(|idx| self.submit_text(idx, text)));
            }
            SessionRequest::Cancel { id } => {
                let reply = parse_session_id(&id).and_then(|id| {
                    let idx = self
                        .position(id)
                        .ok_or_else(|| format!("{NOT_LIVE_ERR}: {id}"))?;
                    if SessionStatus::of(&self.sessions[idx].app) == SessionStatus::Idle {
                        return Err(format!("session is idle: {id}"));
                    }
                    let actions = self.sessions[idx].app.cancel_current_run();
                    self.dispatch(idx, actions);
                    Ok(json!(true))
                });
                let _ = reply_tx.send(reply);
            }
            SessionRequest::Focus { id } => {
                let reply = parse_session_id(&id)
                    .and_then(|id| self.focus_session(id))
                    .map(|()| json!(true));
                let _ = reply_tx.send(reply);
            }
            SessionRequest::SetTitle { id, title } => {
                let title = normalize_title(&title);
                let reply = (|| {
                    let id = parse_session_id(&id)?;
                    if let Some(i) = self.position(id) {
                        let app = &mut self.sessions[i].app;
                        app.state.session.title = title;
                        app.save_session();
                    } else {
                        let mut session =
                            AppSession::load(id, &self.ctx.storage).map_err(|e| e.to_string())?;
                        session.title = title;
                        session.updated_at = n00n_storage::now_epoch();
                        self.ctx.storage_writer.send(Box::new(session));
                    }
                    Ok(json!(true))
                })();
                let _ = reply_tx.send(reply);
            }
        }
    }

    fn submit_text(&mut self, idx: usize, text: String) -> SessionReply {
        let msg = QueuedMessage {
            text,
            images: Vec::new(),
        };
        match self.sessions[idx].app.submit_background_prompt(msg) {
            SubmitOutcome::Started(actions) => {
                self.dispatch(idx, actions);
                Ok(json!("started"))
            }
            SubmitOutcome::Queued => Ok(json!("queued")),
            SubmitOutcome::Rejected(e) => Err(e.into()),
        }
    }

    fn position(&self, id: N00nId) -> Option<usize> {
        self.sessions.iter().position(|rt| rt.id() == id)
    }

    /// The single place that removes a runtime: keeps `focused` pointing at
    /// the same session afterwards. The focused runtime itself is never
    /// removable, so `sessions` stays non-empty.
    fn remove_runtime(&mut self, idx: usize) -> SessionRuntime {
        debug_assert_ne!(idx, self.focused);
        let rt = self.sessions.remove(idx);
        if idx < self.focused {
            self.focused -= 1;
        }
        rt
    }

    fn push_runtime(&mut self, rt: SessionRuntime) -> usize {
        self.sessions.push(rt);
        self.sessions.len() - 1
    }

    fn set_focus(&mut self, idx: usize) {
        if idx == self.focused {
            return;
        }
        self.sessions[self.focused].app.save_session();
        self.focused = idx;
    }

    /// Focus a live session, or bring a stored one up: in place when the
    /// focused session is a blank idle one (nothing worth keeping), otherwise
    /// as a new runtime so the session you came from stays live.
    fn focus_session(&mut self, id: N00nId) -> Result<(), String> {
        if let Some(i) = self.position(id) {
            self.set_focus(i);
            return Ok(());
        }
        let focused = &mut self.sessions[self.focused];
        if SessionStatus::of(&focused.app) == SessionStatus::Idle && !focused.app.has_content() {
            let actions = focused.app.load_session(id);
            self.dispatch(self.focused, actions);
            return Ok(());
        }
        let session = AppSession::load(id, &self.ctx.storage)
            .map_err(|e| format!("Failed to load session: {e}"))?;
        let idx = self.push_runtime(self.ctx.spawn_runtime(session));
        self.set_focus(idx);
        Ok(())
    }

    /// Handles one input event plus any leftover produced while coalescing
    /// bursts of scroll/drag events.
    fn handle_input(&mut self, raw: Event) {
        let mut pending = Some(raw);
        while let Some(ev) = pending.take() {
            let (msg, leftover) = self.translate(ev);
            if let Some(msg) = msg {
                let actions = self.sessions[self.focused].app.update(msg);
                self.dispatch(self.focused, actions);
            }
            pending = leftover;
        }
    }

    fn translate(&mut self, raw: Event) -> (Option<Msg>, Option<Event>) {
        match raw {
            Event::Resize(..) => {
                self.dirty = true;
                (None, None)
            }
            Event::Key(key) if key.kind == KeyEventKind::Press => (Some(Msg::Key(key)), None),
            Event::Paste(text) => (Some(Msg::Paste(text)), None),
            Event::Mouse(mouse) => self.translate_mouse(mouse),
            _ => (None, None),
        }
    }

    fn translate_mouse(&mut self, mouse: CtMouseEvent) -> (Option<Msg>, Option<Event>) {
        match mouse.kind {
            MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => {
                let scroll_lines = self.focused_app().ui_config.mouse_scroll_lines;
                let (msg, leftover) = self.aggregate_scroll(mouse, scroll_lines);
                (Some(msg), leftover)
            }
            MouseEventKind::Drag(MouseButton::Left) => {
                let (drag, leftover) = self.coalesce_drag(mouse);
                (Some(Msg::Mouse(drag)), leftover)
            }
            _ => (Some(Msg::Mouse(mouse)), None),
        }
    }

    /// Sums queued scroll events into one delta; the first non-scroll event
    /// drained along the way is returned so it isn't lost.
    fn aggregate_scroll(&self, first: CtMouseEvent, scroll_lines: u32) -> (Msg, Option<Event>) {
        let mut delta = scroll_delta(first.kind, scroll_lines);
        let mut leftover = None;
        while let Ok(next) = self.input.receiver().try_recv() {
            match next {
                Event::Mouse(m)
                    if matches!(
                        m.kind,
                        MouseEventKind::ScrollUp | MouseEventKind::ScrollDown
                    ) =>
                {
                    delta += scroll_delta(m.kind, scroll_lines);
                }
                other => {
                    leftover = Some(other);
                    break;
                }
            }
        }
        (
            Msg::Scroll {
                column: first.column,
                row: first.row,
                delta,
            },
            leftover,
        )
    }

    /// Keeps only the newest queued drag position; the first non-drag event
    /// drained along the way is returned so it isn't lost.
    fn coalesce_drag(&self, mut latest: CtMouseEvent) -> (CtMouseEvent, Option<Event>) {
        let mut leftover = None;
        while let Ok(next) = self.input.receiver().try_recv() {
            match next {
                Event::Mouse(m) if matches!(m.kind, MouseEventKind::Drag(MouseButton::Left)) => {
                    latest = m;
                }
                other => {
                    leftover = Some(other);
                    break;
                }
            }
        }
        (latest, leftover)
    }

    fn dispatch(&mut self, idx: usize, actions: Vec<Action>) {
        for action in actions {
            match action {
                Action::SendMessage(dispatch) if dispatch.paint_required => {
                    self.post_draw_submissions
                        .push((self.sessions[idx].id(), *dispatch));
                }
                Action::SendMessage(dispatch) => {
                    self.handle_action(idx, Action::SendMessage(dispatch));
                }
                action => self.handle_action(idx, action),
            }
        }
    }

    /// The optimistic user bubble must have completed a terminal draw before
    /// persistence or provider dispatch can begin.
    fn after_terminal_draw(&mut self) {
        let painted_session = self.sessions[self.focused].id();
        for (_, dispatch) in
            take_painted_submissions(&mut self.post_draw_submissions, painted_session)
        {
            self.handle_action(self.focused, Action::SendMessage(Box::new(dispatch)));
        }
    }

    fn respawn_agent(
        &mut self,
        idx: usize,
        history: Vec<Message>,
        transcript: Vec<TranscriptEntry<Message>>,
    ) {
        let rt = &mut self.sessions[idx];
        let lua_handle = rt.app.lua_event_handle.clone();
        let permissions = Arc::clone(&rt.app.permissions);
        rt.handles.respawn(
            history,
            transcript,
            &self.ctx.model_slot,
            self.ctx.config.clone(),
            self.ctx.ui_config.tool_output_lines,
            &permissions,
            &mut rt.app,
            lua_handle,
        );
    }

    fn handle_submission_persisted(&mut self, completion: SubmissionPersistence) {
        let Some(idx) = self.position(completion.session_id) else {
            return;
        };
        let rt = &mut self.sessions[idx];
        if completion.result.is_err() {
            rt.app
                .handle_submission_persistence_failure(&completion.dispatch);
            return;
        }
        if !rt.app.accepts_submission_persistence(&completion.dispatch) {
            rt.app
                .queue
                .remove_submission(completion.dispatch.submission_id);
            return;
        }
        let submission_id = completion.dispatch.submission_id;
        if !rt
            .app
            .queue
            .mark_submission_ready(submission_id, completion.dispatch.input)
        {
            rt.app.queue.remove_submission(submission_id);
        }
    }

    fn handle_action(&mut self, idx: usize, action: Action) {
        match action {
            Action::SendMessage(mut dispatch) => {
                let rt = &mut self.sessions[idx];
                if !rt.app.stage_submission_preamble(&mut dispatch) {
                    rt.app.queue.remove_submission(dispatch.submission_id);
                    return;
                }
                let session_id = rt.app.state.session.id;
                let snapshot = rt.app.session_snapshot();
                let completion_tx = self.submission_persist_tx.clone();
                self.ctx
                    .storage_writer
                    .persist(Box::new(snapshot), move |result| {
                        let _ = completion_tx.send(SubmissionPersistence {
                            session_id,
                            dispatch: *dispatch,
                            result,
                        });
                    });
            }
            Action::CancelAgent { run_id } => {
                let _ = self.sessions[idx]
                    .handles
                    .cmd_tx
                    .try_send(AgentCommand::Cancel { run_id });
            }
            Action::CancelSubagent { tool_use_id } => {
                let _ = self.sessions[idx]
                    .handles
                    .cmd_tx
                    .try_send(AgentCommand::CancelSubagent { tool_use_id });
            }
            Action::NewSession => {
                self.respawn_agent(idx, Vec::new(), Vec::new());
            }
            Action::LoadSession(loaded) => {
                let loaded = *loaded;
                if loaded.model_spec != self.ctx.model_slot.load().model.spec()
                    && let Ok(mut new_model) = Model::from_spec(&loaded.model_spec)
                    && let Ok(new_provider) = from_model_with_openai_options(
                        &mut new_model,
                        self.ctx.timeouts,
                        self.ctx.openai_options,
                    )
                {
                    self.sessions[idx].app.usage_slot.store(None);
                    self.ctx.model_slot.store(Arc::new(ModelSlot {
                        model: new_model,
                        provider: Arc::from(new_provider),
                    }));
                }
                self.respawn_agent(idx, loaded.messages, loaded.transcript);
                *self.sessions[idx]
                    .handles
                    .tool_outputs
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner) = loaded.tool_outputs;
            }
            Action::ChangeModel(spec) => self.change_model(&spec),
            Action::RefreshProvider { slug } => self.refresh_provider(&slug),
            Action::AssignTier(spec, tier) => {
                n00n_providers::model_registry::set_and_persist(spec, tier, &self.ctx.storage);
            }
            Action::UnassignTier(spec, tier) => {
                n00n_providers::model_registry::unset_and_persist(&spec, tier, &self.ctx.storage);
            }
            Action::Compact => {
                let rt = &mut self.sessions[idx];
                let run_id = rt.app.run_id;
                rt.handles.queue.push(QueueItem::Compact { run_id });
            }
            Action::ToggleMcp(server_name, enabled) => {
                self.sessions[idx].handles.send_mcp(McpCommand::Toggle {
                    server: server_name,
                    enabled,
                });
            }
            Action::ShellCommand {
                id,
                command,
                visible,
            } => {
                let rt = &mut self.sessions[idx];
                let (trigger, cancel) = CancelToken::new();
                rt.app.shell.add_trigger(trigger);
                spawn_shell(
                    command,
                    id,
                    visible,
                    rt.shell_tx.clone(),
                    cancel,
                    &self.ctx.config,
                );
            }
            Action::OpenEditor(path) => {
                self.open_editor(idx, &path);
            }
            Action::EditInputInEditor => {
                let current_text = self.sessions[idx].app.input_box.buffer.value();
                let result = match self.input.pause() {
                    Ok(_pause) => terminal::edit_temp_content(&current_text, self.terminal),
                    Err(e) => Err(e),
                };
                match result {
                    Ok(edited) => self.sessions[idx].app.input_box.set_input(&edited),
                    Err(e) => self.sessions[idx].app.flash(e),
                }
            }
            Action::Btw(question) => {
                let slot = self.ctx.model_slot.load();
                self.sessions[idx].app.start_btw(
                    &question,
                    Arc::clone(&slot.provider),
                    slot.model.clone(),
                );
            }
            Action::Suspend => match self.input.pause() {
                Ok(_pause) => terminal::suspend(self.terminal),
                Err(e) => self.sessions[idx].app.flash(e),
            },
            Action::RefreshModels => self.refresh_models(),
            Action::RefreshUsage => self.refresh_usage(),
        }
    }

    fn change_model(&mut self, spec: &str) {
        match Model::from_spec(spec) {
            Ok(mut new_model) => match from_model_with_openai_options(
                &mut new_model,
                self.ctx.timeouts,
                self.ctx.openai_options,
            ) {
                Ok(new_provider) => {
                    let app = self.focused_app();
                    app.update_model(&new_model);
                    app.record_recent_model(spec);
                    app.usage_slot.store(None);
                    self.ctx.model_slot.store(Arc::new(ModelSlot {
                        model: new_model,
                        provider: Arc::from(new_provider),
                    }));
                }
                Err(e) => {
                    let msg = format!("Failed to create provider: {e}");
                    self.focused_app()
                        .main_chat()
                        .push(DisplayMessage::new(DisplayRole::Error, msg.clone()));
                    self.focused_app().flash(msg);
                }
            },
            Err(e) => {
                let msg = format!("Invalid model: {e}");
                self.focused_app()
                    .main_chat()
                    .push(DisplayMessage::new(DisplayRole::Error, msg.clone()));
                self.focused_app().flash(msg);
            }
        }
    }

    fn refresh_models(&self) {
        let available = Arc::clone(&self.ctx.available_models);
        let warn_tx = self.warn_tx.clone();
        available.store(None);
        smol::spawn(async move {
            fetch_all_models(|batch| merge_batch(&available, batch, &warn_tx), None).await;
        })
        .detach();
    }

    fn refresh_usage(&mut self) {
        let provider = Arc::clone(&self.ctx.model_slot.load().provider);
        let slot = Arc::clone(&self.focused_app().usage_slot);
        slot.store(Some(Arc::new(UsageFetchState::Loading)));
        smol::spawn(async move {
            let state = match provider.fetch_usage().await {
                Ok(Some(usage)) => UsageFetchState::Ready(usage),
                Ok(None) => UsageFetchState::Unsupported,
                Err(e) => UsageFetchState::Error(e.user_message()),
            };
            slot.store(Some(Arc::new(state)));
        })
        .detach();
    }

    fn refresh_provider(&mut self, slug: &str) {
        let mut model = self.ctx.model_slot.load().model.clone();
        if model.provider.to_string() == slug {
            if let Ok(provider) =
                n00n_providers::provider::from_model(&mut model, self.ctx.timeouts)
            {
                self.focused_app().usage_slot.store(None);
                self.ctx.model_slot.store(Arc::new(ModelSlot {
                    model,
                    provider: Arc::from(provider),
                }));
            }
        } else if let Some(builtin) = n00n_config::providers::builtin_provider(slug) {
            self.change_model(builtin.default_model);
        }
    }

    fn preserve_post_draw_submissions(&mut self) {
        for (session_id, dispatch) in std::mem::take(&mut self.post_draw_submissions)
            .into_iter()
            .rev()
        {
            let Some(idx) = self.position(session_id) else {
                warn!(%session_id, "paint-gated submission lost its session before shutdown");
                continue;
            };
            self.sessions[idx]
                .app
                .preserve_submission_for_shutdown(dispatch);
        }
    }

    fn shutdown(mut self) -> ShutdownReport {
        self.preserve_post_draw_submissions();
        let exit = self.sessions[self.focused].app.exit_request;
        if let Some(ref h) = self.ctx.mcp_handle {
            mcp::kill_process_groups(&h.reader().load().pids);
        }
        for rt in &self.sessions {
            let _ = rt.handles.cmd_tx.try_send(AgentCommand::CancelAll);
        }
        let mut tabs = Vec::with_capacity(self.sessions.len());
        let mut agent_tasks = Vec::with_capacity(self.sessions.len());
        for rt in self.sessions.drain(..) {
            let SessionRuntime {
                mut app, handles, ..
            } = rt;
            app.save_session();
            // `app` drops at the end of this iteration, closing the
            // channels the agent loop waits on, so `join_all` can finish.
            tabs.push(app.state.session);
            agent_tasks.push(handles.into_task());
        }
        crate::agent::join_all(agent_tasks, AGENT_SHUTDOWN_TIMEOUT);
        if let Some(ref h) = self.ctx.mcp_handle {
            smol::block_on(h.shutdown());
        }
        match Arc::try_unwrap(self.ctx.storage_writer) {
            Ok(writer) => writer.shutdown(AGENT_SHUTDOWN_TIMEOUT),
            Err(_) => {
                warn!("storage writer has outstanding references, skipping graceful shutdown");
            }
        }
        ShutdownReport {
            exit,
            tabs,
            focused: self.focused,
        }
    }
}

fn draw_then_post_terminal<B>(
    terminal: &mut ratatui::Terminal<B>,
    draw: impl FnOnce(&mut ratatui::Frame<'_>),
    after_draw: impl FnOnce(),
) -> Result<(), B::Error>
where
    B: ratatui::backend::Backend,
{
    terminal.draw(draw)?;
    after_draw();
    Ok(())
}

fn take_painted_submissions<T>(
    pending: &mut Vec<(N00nId, T)>,
    painted_session: N00nId,
) -> Vec<(N00nId, T)> {
    let submissions = std::mem::take(pending);
    let mut ready = Vec::new();
    for (session_id, submission) in submissions {
        if session_id == painted_session {
            ready.push((session_id, submission));
        } else {
            pending.push((session_id, submission));
        }
    }
    ready
}

fn scroll_delta(kind: MouseEventKind, lines: u32) -> i32 {
    let lines = crate::cast::u32_to_isize(lines);
    let n = i32::try_from(lines).unwrap_or_else(|_| i32::MAX);
    if kind == MouseEventKind::ScrollUp {
        n
    } else {
        -n
    }
}

#[cfg(test)]
mod tests {
    use super::{DRAIN_BUDGET, DrainScheduler, draw_then_post_terminal, take_painted_submissions};
    use n00n_storage::id::N00nId;
    use ratatui::{
        Terminal,
        backend::{Backend, ClearType, TestBackend, WindowSize},
        buffer::Cell,
        layout::{Position, Size},
        widgets::Paragraph,
    };
    use std::io;

    struct FailingBackend(TestBackend);

    fn infallible<T>(result: Result<T, std::convert::Infallible>) -> T {
        match result {
            Ok(value) => value,
            Err(error) => match error {},
        }
    }

    impl Backend for FailingBackend {
        type Error = io::Error;
        fn draw<'a, I>(&mut self, _content: I) -> io::Result<()>
        where
            I: Iterator<Item = (u16, u16, &'a Cell)>,
        {
            Err(io::Error::other("deterministic draw failure"))
        }

        fn hide_cursor(&mut self) -> io::Result<()> {
            infallible(self.0.hide_cursor());
            Ok(())
        }

        fn show_cursor(&mut self) -> io::Result<()> {
            infallible(self.0.show_cursor());
            Ok(())
        }

        fn get_cursor_position(&mut self) -> io::Result<Position> {
            Ok(infallible(self.0.get_cursor_position()))
        }

        fn set_cursor_position<P: Into<Position>>(&mut self, position: P) -> io::Result<()> {
            infallible(self.0.set_cursor_position(position));
            Ok(())
        }

        fn clear(&mut self) -> io::Result<()> {
            infallible(self.0.clear());
            Ok(())
        }

        fn clear_region(&mut self, clear_type: ClearType) -> io::Result<()> {
            infallible(self.0.clear_region(clear_type));
            Ok(())
        }

        fn size(&self) -> io::Result<Size> {
            Ok(infallible(self.0.size()))
        }

        fn window_size(&mut self) -> io::Result<WindowSize> {
            Ok(infallible(self.0.window_size()))
        }

        fn flush(&mut self) -> io::Result<()> {
            infallible(self.0.flush());
            Ok(())
        }
    }

    #[derive(Debug, PartialEq, Eq)]
    enum Source {
        Input(usize),
        Agent(usize),
    }

    #[test]
    fn painted_submission_waits_for_its_session_after_focus_switch() {
        let first = N00nId::generate();
        let second = N00nId::generate();
        let mut pending = vec![(first, "first"), (second, "second")];

        let released = take_painted_submissions(&mut pending, second);

        assert_eq!(released, vec![(second, "second")]);
        assert_eq!(pending, vec![(first, "first")]);
        assert_eq!(
            take_painted_submissions(&mut pending, first),
            vec![(first, "first")]
        );
    }

    #[test]
    fn post_draw_hook_runs_after_terminal_buffer_is_painted() {
        let mut terminal = Terminal::new(TestBackend::new(20, 1)).expect("test terminal");
        let painted = std::cell::Cell::new(false);
        let persistence_started = std::cell::Cell::new(false);

        draw_then_post_terminal(
            &mut terminal,
            |frame| {
                frame.render_widget(Paragraph::new("bubble"), frame.area());
                painted.set(true);
            },
            || {
                persistence_started.set(true);
                assert!(painted.get());
            },
        )
        .expect("draw succeeds");

        assert!(persistence_started.get());
        assert_eq!(
            terminal.backend().buffer().cell((0, 0)).unwrap().symbol(),
            "b"
        );
    }

    #[test]
    fn terminal_draw_failure_does_not_release_post_draw_work() {
        let mut terminal =
            Terminal::new(FailingBackend(TestBackend::new(20, 1))).expect("test terminal");
        let post_draw_ran = std::cell::Cell::new(false);

        let result = draw_then_post_terminal(
            &mut terminal,
            |frame| frame.render_widget(Paragraph::new("bubble"), frame.area()),
            || post_draw_ran.set(true),
        );

        assert!(result.is_err());
        assert!(!post_draw_ran.get());
    }

    #[test]
    fn drain_prioritizes_input_and_preserves_fair_bounded_progress() {
        let (input_tx, input_rx) = flume::unbounded();
        let (agent_tx, agent_rx) = flume::unbounded();
        for i in 0..DRAIN_BUDGET {
            input_tx.send(i).expect("input receiver remains connected");
            agent_tx.send(i).expect("agent receiver remains connected");
        }

        let mut scheduler = DrainScheduler::default();
        let drained: Vec<_> = (0..DRAIN_BUDGET)
            .filter_map(|_| {
                scheduler.next(
                    || input_rx.try_recv().ok().map(Source::Input),
                    || agent_rx.try_recv().ok().map(Source::Agent),
                )
            })
            .collect();

        assert_eq!(drained.first(), Some(&Source::Input(0)));
        assert_eq!(
            drained
                .iter()
                .filter(|source| matches!(source, Source::Input(_)))
                .count(),
            DRAIN_BUDGET / 2
        );
        assert_eq!(
            drained
                .iter()
                .filter(|source| matches!(source, Source::Agent(_)))
                .count(),
            DRAIN_BUDGET / 2
        );
        assert_eq!(input_rx.len() + agent_rx.len(), DRAIN_BUDGET);
    }
}

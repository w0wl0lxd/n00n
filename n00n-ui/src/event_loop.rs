//! Multi-session supervisor: every session owns an `App` + `AgentHandles` and
//! keeps draining agent events while backgrounded; only the focused session
//! renders and receives input. `SpawnCtx` carries the shared resources needed
//! to spawn session runtimes at any point.
//!
//! Terminal input arrives on a channel (see [`InputReader`]), so the loop
//! waits on every event source at once and wakes the moment a plugin action,
//! agent event, or keypress arrives instead of sleeping in `event::poll`.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use arc_swap::{ArcSwap, ArcSwapOption};
use color_eyre::Result;
use color_eyre::eyre::{Context, eyre};

use crossterm::event::{
    Event, KeyEventKind, MouseButton, MouseEvent as CtMouseEvent, MouseEventKind,
};
use n00n_agent::command::CustomCommand;
use n00n_agent::permissions::PermissionManager;
use n00n_agent::{
    AgentConfig, CancelToken, McpCommand, McpConfigErrors, McpHandle, ToolOutput, mcp,
};
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
use n00n_storage::id::{SessionRef, n00nId, n00nIdParseError};
use n00n_storage::sessions::{SessionError, SessionLifecycle, TranscriptEntry, normalize_title};
use serde_json::{Value, json};
use tracing::warn;

use crate::AppSession;
use crate::agent::{AgentCommand, AgentHandles, ModelSlot, shared_queue::QueueItem};
use crate::app::shell::{ShellEvent, spawn_shell};
use crate::app::{
    AgentSessionEntry, App, AppInit, Msg, QueuedMessage, SubmitOutcome, TaskStatus, paused_team_run,
};
use crate::components::input::Submission;
use crate::components::usage_modal::UsageFetchState;
use crate::components::{
    Action, DisplayMessage, DisplayRole, ExitRequest, Status, SubmissionDispatch,
};
use crate::input::InputReader;

use crate::color_compat;
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
const STORAGE_WRITER_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(5);
const DELETE_FOCUSED_ERR: &str = "cannot delete the focused session";
const NOT_LIVE_ERR: &str = "session not live";
const MAX_LIVE_SESSIONS: usize = 64;
const MAX_SESSION_DEPTH: usize = 8;

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

fn parse_session_id(id: &str) -> Result<n00nId, String> {
    id.parse().map_err(|e: n00nIdParseError| e.to_string())
}

fn task_status(lifecycle: SessionLifecycle) -> TaskStatus {
    match lifecycle {
        SessionLifecycle::Running => TaskStatus::Running,
        SessionLifecycle::WaitingInput => TaskStatus::WaitingInput,
        SessionLifecycle::Paused => TaskStatus::Paused,
        SessionLifecycle::Interrupted => TaskStatus::Interrupted,
        SessionLifecycle::Failed => TaskStatus::Error,
        SessionLifecycle::Cancelled => TaskStatus::Cancelled,
        SessionLifecycle::Idle | SessionLifecycle::Succeeded => TaskStatus::Done,
    }
}

fn stored_task_status(lifecycle: SessionLifecycle) -> TaskStatus {
    match lifecycle {
        SessionLifecycle::Running | SessionLifecycle::WaitingInput => TaskStatus::Interrupted,
        lifecycle => task_status(lifecycle),
    }
}

fn reconcile_restored_lifecycle(session: &mut AppSession) -> bool {
    if matches!(
        session.meta.lifecycle,
        SessionLifecycle::Running | SessionLifecycle::WaitingInput
    ) {
        session.meta.lifecycle = SessionLifecycle::Interrupted;
        true
    } else {
        false
    }
}
fn shutdown_lifecycle(
    lifecycle: SessionLifecycle,
    team_resume_queued: bool,
    history: &[Message],
) -> SessionLifecycle {
    if lifecycle == SessionLifecycle::Running
        && (team_resume_queued || paused_team_run(history).is_some())
    {
        SessionLifecycle::Paused
    } else {
        lifecycle
    }
}

fn session_depth(id: n00nId, parents: &HashMap<n00nId, Option<n00nId>>) -> Option<usize> {
    let mut current = id;
    let mut depth = 0usize;
    let mut seen = HashSet::new();
    while seen.insert(current) {
        let parent = parents.get(&current)?;
        let Some(parent) = parent else {
            return Some(depth);
        };
        depth = depth.saturating_add(1);
        current = *parent;
    }
    None
}

fn is_descendant(
    candidate: n00nId,
    ancestor: n00nId,
    parents: &HashMap<n00nId, Option<n00nId>>,
) -> bool {
    let mut current = candidate;
    let mut seen = HashSet::new();
    while seen.insert(current) {
        let Some(parent) = parents.get(&current).copied().flatten() else {
            return false;
        };
        if parent == ancestor {
            return true;
        }
        current = parent;
    }
    false
}

fn session_root(id: n00nId, parents: &HashMap<n00nId, Option<n00nId>>) -> Option<n00nId> {
    let mut current = id;
    let mut seen = HashSet::new();
    while seen.insert(current) {
        let parent = parents.get(&current)?;
        let Some(parent) = parent else {
            return Some(current);
        };
        current = *parent;
    }
    None
}
fn owned_session_root(
    id: n00nId,
    explicit_roots: &HashMap<n00nId, Option<n00nId>>,
    parents: &HashMap<n00nId, Option<n00nId>>,
) -> Option<n00nId> {
    let Some(root) = explicit_roots.get(&id).copied().flatten() else {
        return session_root(id, parents);
    };
    if parents.get(&root).copied().flatten().is_some() {
        return None;
    }
    let mut current = id;
    let mut seen = HashSet::new();
    while seen.insert(current) {
        if current == root {
            return Some(root);
        }
        match parents.get(&current).copied().flatten() {
            Some(parent) => current = parent,
            None => return (!parents.contains_key(&current)).then_some(root),
        }
    }
    None
}

#[derive(Clone)]
struct StoredAgentSession {
    entry: AgentSessionEntry,
    parent_id: Option<n00nId>,
    root_id: Option<n00nId>,
}

struct SessionRuntime {
    app: App,
    handles: AgentHandles,
    shell_tx: flume::Sender<ShellEvent>,
    shell_rx: flume::Receiver<ShellEvent>,
    last_status: SessionStatus,
}

impl SessionRuntime {
    fn id(&self) -> n00nId {
        self.app.state.session.id
    }
}
#[derive(Clone)]
struct RuntimeDescriptor {
    id: n00nId,
    title: String,
    lifecycle: SessionLifecycle,
    parent_id: Option<n00nId>,
    root_id: Option<n00nId>,
    updated_at: u64,
    focused: bool,
    cwd: String,
}

impl RuntimeDescriptor {
    fn from_runtime(runtime: &SessionRuntime, focused: bool) -> Self {
        Self {
            id: runtime.id(),
            title: runtime.app.state.session.title.clone(),
            lifecycle: runtime.app.state.session.meta.lifecycle,
            parent_id: runtime.app.state.session.meta.parent_id,
            root_id: runtime.app.state.session.meta.root_id,
            updated_at: runtime.app.state.session.updated_at,
            focused,
            cwd: runtime.app.state.session.cwd.clone(),
        }
    }

    fn agent_entry(&self) -> AgentSessionEntry {
        AgentSessionEntry {
            id: self.id,
            name: self.title.clone(),
            status: task_status(self.lifecycle),
        }
    }

    fn control_json(&self) -> Value {
        json!({
            "id": self.id,
            "title": self.title,
            "status": match self.lifecycle {
                SessionLifecycle::Running => "working",
                SessionLifecycle::WaitingInput => "needs_input",
                SessionLifecycle::Idle
                | SessionLifecycle::Paused
                | SessionLifecycle::Interrupted
                | SessionLifecycle::Succeeded
                | SessionLifecycle::Failed
                | SessionLifecycle::Cancelled => "idle",
            },
            "lifecycle": self.lifecycle,
            "updated_at": self.updated_at,
            "focused": self.focused,
            "parent_id": self.parent_id,
            "root_id": self.root_id,
            "cwd": self.cwd,
        })
    }

    fn control_status_json(&self, output: Option<&str>, paused_team: Option<Value>) -> Value {
        let mut descriptor = self.control_json();
        if let Some(object) = descriptor.as_object_mut() {
            object.insert(
                "output".to_owned(),
                output.map_or(Value::Null, |text| Value::String(text.to_owned())),
            );
            object.insert(
                "paused_team".to_owned(),
                paused_team.map_or(Value::Null, std::convert::identity),
            );
        }
        descriptor
    }
}

fn project_agent_sessions(
    active_id: n00nId,
    stored: &[StoredAgentSession],
    live: &[RuntimeDescriptor],
) -> Vec<AgentSessionEntry> {
    let mut parents: HashMap<_, _> = stored
        .iter()
        .map(|session| (session.entry.id, session.parent_id))
        .collect();
    parents.extend(
        live.iter()
            .map(|descriptor| (descriptor.id, descriptor.parent_id)),
    );
    let mut explicit_roots: HashMap<_, _> = stored
        .iter()
        .map(|session| (session.entry.id, session.root_id))
        .collect();
    explicit_roots.extend(
        live.iter()
            .map(|descriptor| (descriptor.id, descriptor.root_id)),
    );
    let Some(active_root) = owned_session_root(active_id, &explicit_roots, &parents) else {
        return Vec::new();
    };
    let mut agents: HashMap<_, _> = stored
        .iter()
        .filter(|session| session.parent_id.is_some())
        .map(|session| (session.entry.id, session.entry.clone()))
        .collect();
    for descriptor in live
        .iter()
        .filter(|descriptor| descriptor.parent_id.is_some())
    {
        agents.insert(descriptor.id, descriptor.agent_entry());
    }
    let mut agents: Vec<_> = agents
        .into_values()
        .filter(|agent| {
            agent.id != active_id
                && owned_session_root(agent.id, &explicit_roots, &parents) == Some(active_root)
        })
        .collect();
    agents.sort_unstable_by_key(|agent| *agent.id.as_bytes());
    agents
}

fn continued_subagent_session(
    parent: &AppSession,
    model: &str,
    name: &str,
    messages: Vec<Message>,
) -> AppSession {
    let mut session = AppSession::new(model, &parent.cwd);
    session.title = normalize_title(&format!("continued: {name}"));
    session.meta.parent_id = Some(parent.id);
    session.meta.root_id = Some(
        parent
            .meta
            .root_id
            .map_or(parent.id, std::convert::identity),
    );
    session.transcript = messages
        .iter()
        .cloned()
        .map(TranscriptEntry::Message)
        .collect();
    session.messages = messages;
    session
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
        let initial_plan_path = session.meta.plan_path.as_ref().map(PathBuf::from);
        let handles = AgentHandles::spawn(
            &self.model_slot,
            session.messages.clone(),
            session.transcript.clone(),
            initial_plan_path,
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
    stored_agent_sessions: Vec<StoredAgentSession>,
    focused: usize,
    ctx: SpawnCtx,
    input: InputReader,
    warn_rx: flume::Receiver<String>,
    warn_tx: flume::Sender<String>,
    ui_action_rx: Option<flume::Receiver<UiAction>>,
    submission_persist_tx: flume::Sender<SubmissionPersistence>,
    submission_persist_rx: flume::Receiver<SubmissionPersistence>,
    post_draw_submissions: Vec<(n00nId, SubmissionDispatch)>,
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
    session_id: n00nId,
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

        let mut stored_by_id = HashMap::new();
        let session_cwds: HashSet<_> = sessions.iter().map(|session| session.cwd.clone()).collect();
        for session_cwd in session_cwds {
            match AppSession::list(&session_cwd, &storage) {
                Ok(summaries) => {
                    for summary in summaries {
                        let lifecycle = if matches!(
                            summary.lifecycle,
                            SessionLifecycle::Running | SessionLifecycle::WaitingInput
                        ) {
                            match AppSession::load(summary.id, &storage) {
                                Ok(mut session) => {
                                    reconcile_restored_lifecycle(&mut session);
                                    storage_writer.send(Box::new(session));
                                    SessionLifecycle::Interrupted
                                }
                                Err(error) => {
                                    warn!(%error, session_id = %summary.id, "failed to reconcile interrupted session");
                                    summary.lifecycle
                                }
                            }
                        } else {
                            summary.lifecycle
                        };
                        stored_by_id.insert(
                            summary.id,
                            StoredAgentSession {
                                entry: AgentSessionEntry {
                                    id: summary.id,
                                    name: summary.title,
                                    status: stored_task_status(lifecycle),
                                },
                                parent_id: summary.parent_id,
                                root_id: summary.root_id,
                            },
                        );
                    }
                }
                Err(error) => {
                    warn!(%error, cwd = %session_cwd, "failed to list stored agent sessions");
                }
            }
        }
        let stored_agent_sessions = stored_by_id.into_values().collect();

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
            .map(|mut session| {
                if reconcile_restored_lifecycle(&mut session) {
                    ctx.storage_writer.send(Box::new(session.clone()));
                }
                ctx.spawn_runtime(session)
            })
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
            stored_agent_sessions,
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
    fn runtime_descriptors(&self) -> Vec<RuntimeDescriptor> {
        self.sessions
            .iter()
            .enumerate()
            .map(|(index, runtime)| RuntimeDescriptor::from_runtime(runtime, index == self.focused))
            .collect()
    }

    pub(crate) fn run(mut self, initial_prompt: Option<String>) -> Result<ShutdownReport> {
        if let Some(prompt) = initial_prompt {
            let sub = Submission {
                text: prompt,
                images: Vec::new(),
                control: false,
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
        match wake {
            Wake::Input(ev) => self.handle_input(ev),
            Wake::InputGone => return Err(eyre!("terminal input reader stopped")),
            Wake::Ui(action) => {
                self.handle_ui_action(action);
                self.dirty = true;
            }
            Wake::Agent(i, envelope) => {
                self.handle_agent(i, envelope);
                self.dirty = true;
            }
            Wake::Shell(i, event) => {
                self.sessions[i].app.handle_shell_event(event);
                self.dirty = true;
            }
            Wake::SubmissionPersisted(completion) => {
                self.handle_submission_persisted(completion);
                self.dirty = true;
            }
            Wake::Warn(warning) => {
                self.focused_app().flash(warning);
                self.dirty = true;
            }
        }
        Ok(())
    }

    fn tick(&mut self) {
        self.sync_agent_sessions();
        let mut focused_changed = false;
        for (i, rt) in self.sessions.iter_mut().enumerate() {
            rt.app.float_mgr.tick();
            if i != self.focused {
                continue;
            }
            rt.app.tick_edge_scroll();
            focused_changed |= rt.app.tick_error_expiry();
            focused_changed |= !rt.app.image_paste_rx.is_empty();
            rt.app.poll_image_paste();
            rt.app.btw_modal.poll();
            focused_changed |= rt.app.status_bar.clear_expired_hint();
            focused_changed |= rt.app.status_bar.poll_branch_update();
            rt.app.mcp_picker.refresh();
        }
        self.dirty |= focused_changed;
        self.tick_periodic_save();
    }

    fn sync_agent_sessions(&mut self) {
        let descriptors = self.runtime_descriptors();
        for runtime in &mut self.sessions {
            runtime.app.set_agent_sessions(project_agent_sessions(
                runtime.id(),
                &self.stored_agent_sessions,
                &descriptors,
            ));
        }
    }

    fn tick_periodic_save(&mut self) {
        if self.last_save.elapsed() < PERIODIC_SAVE_INTERVAL {
            return;
        }
        for rt in &mut self.sessions {
            if should_save_periodically(&rt.app.status) {
                rt.app.save_session();
            }
        }
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
                rt.app.state.session.meta.lifecycle = SessionLifecycle::Failed;
                rt.app.save_session();
                self.dirty = true;
            }
        }

        let slot_model = self.ctx.model_slot.load();
        let spec = slot_model.model.spec();
        for rt in &mut self.sessions {
            if rt.app.state.session.model != spec
                || rt.app.state.model.context_window != slot_model.model.context_window
            {
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
                self.stored_agent_sessions
                    .retain(|session| session.entry.id != id);
                self.sync_agent_sessions();
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
                    .runtime_descriptors()
                    .iter()
                    .map(RuntimeDescriptor::control_json)
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
                    let paused_team = paused_team_run(&history);
                    Ok(RuntimeDescriptor::from_runtime(rt, idx == self.focused)
                        .control_status_json(output, paused_team))
                });
                let _ = reply_tx.send(reply);
            }
            SessionRequest::Current => {
                let _ = reply_tx.send(Ok(json!(self.sessions[self.focused].id())));
            }
            SessionRequest::New {
                prompt,
                focus,
                parent_id,
            } => {
                if self.sessions.len() >= MAX_LIVE_SESSIONS {
                    let _ = reply_tx.send(Err(format!(
                        "live session limit reached ({MAX_LIVE_SESSIONS})"
                    )));
                    return;
                }
                let mut session = {
                    let slot = self.ctx.model_slot.load();
                    let cwd = std::env::current_dir().unwrap_or_else(|_| ".".into());
                    AppSession::new(&slot.model.spec(), &cwd.to_string_lossy())
                };
                let parent_id = match parent_id {
                    Some(id) => match parse_session_id(&id) {
                        Ok(id) => Some(id),
                        Err(error) => {
                            let _ = reply_tx.send(Err(error));
                            return;
                        }
                    },
                    None => None,
                };
                if let Some(parent_id) = parent_id {
                    let Some(parent_position) = self.position(parent_id) else {
                        let _ = reply_tx.send(Err(format!("{NOT_LIVE_ERR}: {parent_id}")));
                        return;
                    };
                    let parents: HashMap<_, _> = self
                        .sessions
                        .iter()
                        .map(|runtime| (runtime.id(), runtime.app.state.session.meta.parent_id))
                        .collect();
                    let Some(parent_depth) = session_depth(parent_id, &parents) else {
                        let _ = reply_tx.send(Err("invalid session parent chain".into()));
                        return;
                    };
                    if parent_depth >= MAX_SESSION_DEPTH {
                        let _ = reply_tx.send(Err(format!(
                            "session depth limit reached ({MAX_SESSION_DEPTH})"
                        )));
                        return;
                    }
                    session.meta.root_id = Some(
                        self.sessions[parent_position]
                            .app
                            .state
                            .session
                            .meta
                            .root_id
                            .map_or(parent_id, std::convert::identity),
                    );
                }
                session.meta.parent_id = parent_id;
                let idx = self.push_runtime(self.ctx.spawn_runtime(session));
                let id = self.sessions[idx].id();
                if let Some(prompt) = prompt
                    && let Err(error) = self.submit_text(idx, prompt, false, false)
                {
                    let runtime = self.remove_runtime(idx);
                    runtime.handles.cancel();
                    let _ = reply_tx.send(Err(error));
                    return;
                }
                if focus {
                    self.set_focus(idx);
                }
                self.sync_agent_sessions();
                let _ = reply_tx.send(Ok(json!(id)));
            }
            SessionRequest::Prompt {
                id,
                text,
                steer,
                control,
            } => {
                let idx = match id {
                    None => Ok(self.focused),
                    Some(id) => parse_session_id(&id).and_then(|id| {
                        self.position(id)
                            .ok_or_else(|| format!("{NOT_LIVE_ERR}: {id}"))
                    }),
                };
                let _ =
                    reply_tx.send(idx.and_then(|idx| self.submit_text(idx, text, steer, control)));
            }
            SessionRequest::Cancel { id } => {
                let reply = parse_session_id(&id).and_then(|id| {
                    if self.position(id).is_none() {
                        return Err(format!("{NOT_LIVE_ERR}: {id}"));
                    }
                    if self.cancel_session_tree(id) == 0 {
                        return Err(format!("session tree is idle: {id}"));
                    }
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

    fn submit_text(
        &mut self,
        idx: usize,
        text: String,
        steer: bool,
        control: bool,
    ) -> SessionReply {
        let msg = QueuedMessage {
            text,
            images: Vec::new(),
            control,
        };
        let outcome = if steer {
            self.sessions[idx].app.submit_control_prompt(msg)
        } else {
            self.sessions[idx].app.submit_background_prompt(msg)
        };
        match outcome {
            SubmitOutcome::Started(actions) => {
                self.dispatch(idx, actions);
                Ok(json!("started"))
            }
            SubmitOutcome::Queued => Ok(json!("queued")),
            SubmitOutcome::Rejected(e) => Err(e.into()),
        }
    }

    fn position(&self, id: n00nId) -> Option<usize> {
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

    fn ensure_live_session(&mut self, id: n00nId) -> Result<usize, String> {
        if let Some(position) = self.position(id) {
            return Ok(position);
        }
        if self.sessions.len() >= MAX_LIVE_SESSIONS {
            return Err(format!("live session limit reached ({MAX_LIVE_SESSIONS})"));
        }
        let mut session = AppSession::load(id, &self.ctx.storage)
            .map_err(|error| format!("Failed to load session: {error}"))?;
        if reconcile_restored_lifecycle(&mut session) {
            self.ctx.storage_writer.send(Box::new(session.clone()));
        }
        Ok(self.push_runtime(self.ctx.spawn_runtime(session)))
    }

    fn cancel_session_tree(&mut self, id: n00nId) -> usize {
        let parents: HashMap<_, _> = self
            .sessions
            .iter()
            .map(|runtime| (runtime.id(), runtime.app.state.session.meta.parent_id))
            .collect();
        let targets: Vec<_> = self
            .sessions
            .iter()
            .enumerate()
            .filter(|(_, runtime)| {
                (runtime.id() == id || is_descendant(runtime.id(), id, &parents))
                    && matches!(
                        runtime.app.state.session.meta.lifecycle,
                        SessionLifecycle::Running | SessionLifecycle::WaitingInput
                    )
            })
            .map(|(index, _)| index)
            .collect();
        let cancelled = targets.len();
        for position in targets {
            let actions = self.sessions[position].app.cancel_current_run();
            self.dispatch(position, actions);
        }
        cancelled
    }

    fn resume_session(&mut self, id: n00nId) -> Result<(), String> {
        let position = self.ensure_live_session(id)?;
        let resume_id = {
            let app = &self.sessions[position].app;
            (app.state.session.meta.lifecycle == SessionLifecycle::Paused)
                .then(|| paused_team_run(&app.state.session.messages))
                .flatten()
                .and_then(|payload| {
                    payload
                        .get("run_id")
                        .and_then(Value::as_str)
                        .map(str::to_owned)
                })
                .ok_or_else(|| "This session has no resumable team run".to_owned())?
        };
        let runtime = &mut self.sessions[position];
        runtime.app.run_id += 1;
        let run_id = runtime.app.run_id;
        runtime.app.status = Status::Streaming;
        runtime.app.state.session.meta.lifecycle = SessionLifecycle::Running;
        runtime.app.save_session();
        runtime
            .handles
            .queue
            .push(QueueItem::ResumeTeam { run_id, resume_id });
        Ok(())
    }

    /// Focus a live session, or bring a stored one up: in place when the
    /// focused session is a blank idle one (nothing worth keeping), otherwise
    /// as a new runtime so the session you came from stays live.
    fn focus_session(&mut self, id: n00nId) -> Result<(), String> {
        if let Some(i) = self.position(id) {
            self.set_focus(i);
            return Ok(());
        }
        let focused = &mut self.sessions[self.focused];
        if SessionStatus::of(&focused.app) == SessionStatus::Idle && !focused.app.has_content() {
            let actions = focused.app.load_session(id);
            if reconcile_restored_lifecycle(&mut focused.app.state.session) {
                focused.app.save_session();
            }
            self.dispatch(self.focused, actions);
            return Ok(());
        }
        let idx = self.ensure_live_session(id)?;
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
            Event::Key(key) if key.kind == KeyEventKind::Press => {
                self.dirty = true;
                (Some(Msg::Key(key)), None)
            }
            Event::Paste(text) => {
                self.dirty = true;
                (Some(Msg::Paste(text)), None)
            }
            Event::Mouse(mouse) => self.translate_mouse(mouse),
            _ => (None, None),
        }
    }

    fn translate_mouse(&mut self, mouse: CtMouseEvent) -> (Option<Msg>, Option<Event>) {
        match mouse.kind {
            MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => {
                self.dirty = true;
                let scroll_lines = self.focused_app().ui_config.mouse_scroll_lines;
                let (msg, leftover) = self.aggregate_scroll(mouse, scroll_lines);
                (Some(msg), leftover)
            }
            MouseEventKind::Drag(MouseButton::Left) => {
                self.dirty = true;
                let (drag, leftover) = self.coalesce_drag(mouse);
                (Some(Msg::Mouse(drag)), leftover)
            }
            MouseEventKind::Moved => {
                self.dirty |= self.focused_app().ui_config.mascot;
                (Some(Msg::Mouse(mouse)), None)
            }
            _ => {
                self.dirty = true;
                (Some(Msg::Mouse(mouse)), None)
            }
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
            Action::FocusSession { id } => {
                if let Err(error) = self.focus_session(id) {
                    self.focused_app().flash(error);
                }
            }
            Action::CancelSession { id } => {
                self.cancel_session_tree(id);
            }
            Action::ResumeSession { id } => {
                if let Err(error) = self.resume_session(id) {
                    self.focused_app().flash(error);
                }
            }
            Action::ContinueSubagent { name, messages } => {
                let model = self.ctx.model_slot.load().model.spec();
                let session = continued_subagent_session(
                    &self.sessions[idx].app.state.session,
                    &model,
                    &name,
                    messages,
                );
                let position = self.push_runtime(self.ctx.spawn_runtime(session));
                self.sessions[position].app.save_session();
                self.set_focus(position);
            }
            Action::NewSession => {
                self.respawn_agent(idx, Vec::new(), Vec::new());
                if let Some(pending) = self.sessions[idx].app.pending_plan_submit.take() {
                    let actions = {
                        let app = &mut self.sessions[idx].app;
                        if let Some((content, path)) = pending.plan {
                            app.main_chat()
                                .push(DisplayMessage::plan(content, path.clone()));
                            app.state.plan =
                                crate::app::PlanState::Ready(std::path::PathBuf::from(path));
                        }
                        app.run_id += 1;
                        app.start_from_queue(&pending.message)
                    };
                    self.dispatch(idx, actions);
                }
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
        let mut pending_sessions = Vec::with_capacity(self.sessions.len());
        let mut agent_tasks = Vec::with_capacity(self.sessions.len());
        for rt in self.sessions.drain(..) {
            let SessionRuntime {
                mut app, handles, ..
            } = rt;
            app.state.session.meta.lifecycle = shutdown_lifecycle(
                app.state.session.meta.lifecycle,
                handles.queue.has_team_resume(),
                &app.state.session.messages,
            );
            let snapshot = app.session_snapshot();
            pending_sessions.push((
                snapshot,
                app.shared_history.clone(),
                app.shared_transcript.clone(),
                app.shared_tool_outputs.clone(),
            ));
            agent_tasks.push(handles.into_task());
        }
        crate::agent::join_all(agent_tasks, AGENT_SHUTDOWN_TIMEOUT);
        let mut tabs = Vec::with_capacity(pending_sessions.len());
        for (mut session, history, transcript, tool_outputs) in pending_sessions {
            sync_agent_mirrors(
                &mut session,
                history.as_ref(),
                transcript.as_ref(),
                tool_outputs.as_ref(),
            );
            self.ctx.storage_writer.send(Box::new(session.clone()));
            tabs.push(session);
        }
        if let Some(ref h) = self.ctx.mcp_handle {
            smol::block_on(h.shutdown());
        }
        match Arc::try_unwrap(self.ctx.storage_writer) {
            Ok(writer) => writer.shutdown(STORAGE_WRITER_SHUTDOWN_TIMEOUT),
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
    terminal.draw(|f| {
        draw(f);
        color_compat::downgrade_if_needed(f.buffer_mut());
    })?;
    after_draw();
    Ok(())
}

fn take_painted_submissions<T>(
    pending: &mut Vec<(n00nId, T)>,
    painted_session: n00nId,
) -> Vec<(n00nId, T)> {
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

fn sync_agent_mirrors(
    session: &mut AppSession,
    history: Option<&Arc<ArcSwap<Vec<Message>>>>,
    transcript: Option<&n00n_agent::SharedTranscript>,
    tool_outputs: Option<&Arc<Mutex<HashMap<String, ToolOutput>>>>,
) {
    if let Some(history) = history {
        session.messages.clone_from(&history.load());
    }
    if let Some(transcript) = transcript {
        session.transcript.clone_from(&transcript.load());
        session.set_transcript_revision(None);
    }
    if let Some(tool_outputs) = tool_outputs {
        session.tool_outputs.clone_from(
            &tool_outputs
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
        );
    }
}

fn should_save_periodically(status: &Status) -> bool {
    matches!(status, Status::Streaming)
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
    use super::{
        DRAIN_BUDGET, DrainScheduler, RuntimeDescriptor, StoredAgentSession,
        continued_subagent_session, draw_then_post_terminal, is_descendant, owned_session_root,
        paused_team_run, project_agent_sessions, reconcile_restored_lifecycle, session_depth,
        session_root, should_save_periodically, shutdown_lifecycle, stored_task_status,
        sync_agent_mirrors, take_painted_submissions,
    };
    use crate::{
        app::{AgentSessionEntry, TaskStatus},
        components::Status,
    };
    use arc_swap::ArcSwap;
    use n00n_providers::{ContentBlock, Message, Role};
    use n00n_storage::{id::n00nId, sessions::SessionLifecycle};
    use ratatui::{
        Terminal,
        backend::{Backend, ClearType, TestBackend, WindowSize},
        buffer::Cell,
        layout::{Position, Size},
        widgets::Paragraph,
    };
    use std::collections::{HashMap, HashSet};
    use std::{io, sync::Arc};

    const TEAM_TOOL_NAME: &str = "team";

    #[test]
    fn session_root_scopes_entries_to_one_tree() {
        let root = n00nId::generate();
        let child = n00nId::generate();
        let grandchild = n00nId::generate();
        let unrelated = n00nId::generate();
        let parents = HashMap::from([
            (root, None),
            (child, Some(root)),
            (grandchild, Some(child)),
            (unrelated, None),
        ]);

        assert_eq!(session_root(child, &parents), Some(root));
        assert_eq!(session_root(grandchild, &parents), Some(root));
        assert_eq!(session_root(unrelated, &parents), Some(unrelated));
        assert_eq!(session_depth(root, &parents), Some(0));
        assert_eq!(session_depth(grandchild, &parents), Some(2));
        assert!(is_descendant(grandchild, root, &parents));
        assert!(!is_descendant(child, unrelated, &parents));

        let cycle = HashMap::from([(root, Some(child)), (child, Some(root))]);
        assert_eq!(session_depth(root, &cycle), None);
        assert_eq!(session_root(root, &cycle), None);
    }
    #[test]
    fn durable_roots_recover_missing_parents_without_cross_root_leaks() {
        let root_a = n00nId::generate();
        let root_b = n00nId::generate();
        let child = n00nId::generate();
        let missing_parent = n00nId::generate();
        let parents = HashMap::from([
            (root_a, None),
            (root_b, None),
            (child, Some(missing_parent)),
        ]);
        let owned = HashMap::from([(child, Some(root_a))]);
        assert_eq!(owned_session_root(child, &owned, &parents), Some(root_a));

        let forged = HashMap::from([(child, Some(root_b))]);
        let complete_chain = HashMap::from([(root_a, None), (root_b, None), (child, Some(root_a))]);
        assert_eq!(owned_session_root(child, &forged, &complete_chain), None);

        let cycle_peer = n00nId::generate();
        let cycle = HashMap::from([
            (root_a, None),
            (child, Some(cycle_peer)),
            (cycle_peer, Some(child)),
        ]);
        assert_eq!(owned_session_root(child, &owned, &cycle), None);
    }

    #[test]
    fn concurrent_roots_project_only_owned_live_and_restored_descendants() {
        let root_a = n00nId::generate();
        let root_b = n00nId::generate();
        let live_a = n00nId::generate();
        let stored_a = n00nId::generate();
        let child_b = n00nId::generate();
        let malformed = n00nId::generate();
        let descriptor = |id, parent_id, root_id| RuntimeDescriptor {
            id,
            title: id.to_string(),
            lifecycle: SessionLifecycle::Running,
            parent_id,
            root_id,
            updated_at: 1,
            focused: false,
            cwd: "/project".into(),
        };
        let live = vec![
            descriptor(root_a, None, None),
            descriptor(root_b, None, None),
            descriptor(live_a, Some(root_a), Some(root_a)),
            descriptor(child_b, Some(root_b), Some(root_b)),
            descriptor(malformed, Some(n00nId::generate()), None),
        ];
        let stored = vec![StoredAgentSession {
            entry: AgentSessionEntry {
                id: stored_a,
                name: "restored reviewer".into(),
                status: TaskStatus::Interrupted,
            },
            parent_id: Some(root_a),
            root_id: Some(root_a),
        }];

        let primary_projection = project_agent_sessions(root_a, &stored, &live);
        assert_eq!(
            primary_projection
                .iter()
                .map(|entry| entry.id)
                .collect::<HashSet<_>>(),
            HashSet::from([live_a, stored_a])
        );
        let child_projection = project_agent_sessions(live_a, &stored, &live);
        assert_eq!(
            child_projection
                .iter()
                .map(|entry| entry.id)
                .collect::<Vec<_>>(),
            [stored_a]
        );
        let unrelated_projection = project_agent_sessions(root_b, &stored, &live);
        assert_eq!(
            unrelated_projection
                .iter()
                .map(|entry| entry.id)
                .collect::<Vec<_>>(),
            [child_b]
        );
    }

    #[test]
    fn continued_subagent_branches_exact_transcript_with_parent_ownership() {
        let mut parent = crate::AppSession::new("test/model", "/project");
        let root = n00nId::generate();
        parent.meta.root_id = Some(root);
        let messages = vec![
            Message::user("review this".into()),
            Message {
                role: Role::Assistant,
                content: vec![ContentBlock::Text {
                    text: "finding".into(),
                }],
                ..Message::default()
            },
        ];

        let continued =
            continued_subagent_session(&parent, "test/model", "reviewer", messages.clone());

        assert_eq!(continued.meta.parent_id, Some(parent.id));
        assert_eq!(continued.meta.root_id, Some(root));
        assert_eq!(continued.messages.len(), messages.len());
        assert_eq!(
            continued.messages[0].first_text_content(),
            Some("review this")
        );
        assert_eq!(continued.messages[1].first_text_content(), Some("finding"));
        assert_eq!(continued.transcript.len(), 2);
        assert_eq!(continued.meta.lifecycle, SessionLifecycle::Idle);
    }

    #[test]
    fn stored_active_lifecycle_is_reported_as_interrupted() {
        let mut session = crate::AppSession::new("test/model", "/tmp");
        session.meta.lifecycle = SessionLifecycle::Running;
        assert!(reconcile_restored_lifecycle(&mut session));
        assert_eq!(session.meta.lifecycle, SessionLifecycle::Interrupted);
        assert!(!reconcile_restored_lifecycle(&mut session));

        assert_eq!(
            stored_task_status(SessionLifecycle::Running),
            crate::app::TaskStatus::Interrupted
        );
        assert_eq!(
            stored_task_status(SessionLifecycle::WaitingInput),
            crate::app::TaskStatus::Interrupted
        );
        assert_eq!(
            stored_task_status(SessionLifecycle::Paused),
            crate::app::TaskStatus::Paused
        );
    }

    #[test]
    fn shutdown_syncs_sanitized_agent_history() {
        let mut session = crate::AppSession::new("test/model", "/tmp");
        let history = Arc::new(ArcSwap::from_pointee(vec![Message::user(
            "cancelled".into(),
        )]));

        sync_agent_mirrors(&mut session, Some(&history), None, None);

        assert_eq!(session.messages.len(), 1);
        assert_eq!(session.messages[0].first_text_content(), Some("cancelled"));
    }

    #[test]
    fn shutdown_mirrors_never_cross_contaminate_independent_roots() {
        let mut session_a = crate::AppSession::new("test/model", "/tmp");
        let mut session_b = crate::AppSession::new("test/model", "/tmp");
        let history_a = Arc::new(ArcSwap::from_pointee(vec![Message::user("root-a".into())]));
        let history_b = Arc::new(ArcSwap::from_pointee(vec![Message::user("root-b".into())]));

        sync_agent_mirrors(&mut session_a, Some(&history_a), None, None);
        sync_agent_mirrors(&mut session_b, Some(&history_b), None, None);

        assert_eq!(session_a.messages[0].first_text_content(), Some("root-a"));
        assert_eq!(session_b.messages[0].first_text_content(), Some("root-b"));
    }

    #[test]
    fn paused_team_run_requires_matching_team_tool_call() {
        let tool_result = Message {
            role: Role::User,
            content: vec![ContentBlock::ToolResult {
                tool_use_id: "tool-1".into(),
                content: r#"{"paused":true,"run_id":"run-1"}"#.into(),
                is_error: true,
            }],
            ..Default::default()
        };
        assert!(paused_team_run(std::slice::from_ref(&tool_result)).is_none());

        let tool_call = Message {
            role: Role::Assistant,
            content: vec![ContentBlock::ToolUse {
                id: "tool-1".into(),
                name: TEAM_TOOL_NAME.into(),
                input: serde_json::json!({}),
            }],
            ..Default::default()
        };
        let paused = paused_team_run(&[tool_call, tool_result]).expect("paused team payload");
        assert_eq!(paused["run_id"], "run-1");
    }

    #[test]
    fn shutdown_keeps_inflight_team_checkpoint_resumable() {
        let history = vec![
            Message {
                role: Role::Assistant,
                content: vec![ContentBlock::ToolUse {
                    id: "tool-1".into(),
                    name: TEAM_TOOL_NAME.into(),
                    input: serde_json::json!({}),
                }],
                ..Default::default()
            },
            Message {
                role: Role::User,
                content: vec![ContentBlock::ToolResult {
                    tool_use_id: "tool-1".into(),
                    content: r#"{"paused":true,"run_id":"run-1"}"#.into(),
                    is_error: true,
                }],
                ..Default::default()
            },
        ];
        assert_eq!(
            shutdown_lifecycle(SessionLifecycle::Running, false, &history),
            SessionLifecycle::Paused
        );
        assert_eq!(
            shutdown_lifecycle(SessionLifecycle::Running, true, &[]),
            SessionLifecycle::Paused
        );
        assert_eq!(
            shutdown_lifecycle(SessionLifecycle::Failed, true, &history),
            SessionLifecycle::Failed
        );
    }

    #[test]
    fn paused_team_run_ignores_malformed_team_json() {
        let tool_call = Message {
            role: Role::Assistant,
            content: vec![ContentBlock::ToolUse {
                id: "tool-1".into(),
                name: TEAM_TOOL_NAME.into(),
                input: serde_json::json!({}),
            }],
            ..Default::default()
        };
        let tool_result = Message {
            role: Role::User,
            content: vec![ContentBlock::ToolResult {
                tool_use_id: "tool-1".into(),
                content: r#"{"paused":true,"run_id":"#.into(),
                is_error: true,
            }],
            ..Default::default()
        };
        assert!(paused_team_run(&[tool_call, tool_result]).is_none());
    }

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
    fn periodic_save_skips_unchanged_idle_sessions() {
        assert!(!should_save_periodically(&Status::Idle));
        assert!(!should_save_periodically(&Status::error("failed".into())));
        assert!(should_save_periodically(&Status::Streaming));
    }

    #[test]
    fn painted_submission_waits_for_its_session_after_focus_switch() {
        let first = n00nId::generate();
        let second = n00nId::generate();
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

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex as SyncMutex};

use async_lock::Mutex;
use flume::Receiver;
use n00n_providers::Message;
use n00n_providers::OpenAiOptions;
use n00n_providers::System;
use n00n_providers::Timeouts;
use n00n_providers::TokenUsage;
use n00n_providers::model::Model;
use n00n_providers::provider::{self, Provider};
use n00n_storage::id::{SessionRef, n00nId};
use n00n_storage::sessions::{
    CompactionStateError, SESSIONS_DIR, Session, SessionError, StoredMode,
    StoredSessionStateSnapshot, TranscriptEntry,
};
use n00n_storage::{StateDir, StorageError};
use serde_json::Value;
use tracing::{error, warn};

use crate::agent::{self, History};
use crate::cancel::{CancelMap, CancelToken};
use crate::permissions::PermissionManager;
use crate::prompt::ResolvedSlots;
use crate::template;
use crate::tools::{FileReadTracker, SessionIdentity, ToolAudience, ToolFilter, ToolRegistry};
use crate::{
    Agent, AgentConfig, AgentEvent, AgentInput, AgentMode, AgentParams, AgentRunParams, Envelope,
    EventSender, ImageSource, McpHandle, McpSession, PermissionsConfig, ToolOutput,
    ToolOutputLines,
};

type StoredSession = Session<Message, TokenUsage, ToolOutput>;

const NON_UTF8_PLAN_PATH_ERR: &str = "plan path must be valid UTF-8";
const INITIAL_STATE_REVISION: u64 = 0;

pub trait SessionStatePersistence: Send + Sync {
    /// Hydrates the runtime for one session and returns an ownership lease.
    ///
    /// # Errors
    /// Returns an error when the persisted snapshot cannot be restored.
    fn hydrate(
        &self,
        identity: &SessionIdentity,
        snapshot: Option<StoredSessionStateSnapshot>,
    ) -> Result<u64, String>;

    /// Captures the runtime state for one session revision.
    ///
    /// # Errors
    /// Returns an error when runtime state cannot be serialized.
    fn capture(
        &self,
        identity: &SessionIdentity,
        revision: u64,
    ) -> Result<StoredSessionStateSnapshot, String>;

    /// Resolves prompt slots that depend on the hydrated session state.
    ///
    /// # Errors
    /// Returns an error when the session-bound prompt context is unavailable.
    fn prompt_slots(&self, _identity: &SessionIdentity) -> Result<Option<ResolvedSlots>, String> {
        Ok(None)
    }

    /// Drops state owned by a session when its matching lease is released.
    ///
    /// # Errors
    /// Returns an error when runtime state cannot be discarded.
    fn drop_owner(&self, owner: n00nId, lease: u64) -> Result<(), String>;
}
fn resolved_prompt_slots(
    persistence: Option<&Arc<dyn SessionStatePersistence>>,
    identity: &SessionIdentity,
    fallback: Arc<ResolvedSlots>,
) -> Arc<ResolvedSlots> {
    let Some(persistence) = persistence else {
        return fallback;
    };
    match persistence.prompt_slots(identity) {
        Ok(Some(slots)) => Arc::new(slots),
        Ok(None) => fallback,
        Err(error) => {
            warn!(%error, "session prompt slots unavailable; using startup slots");
            fallback
        }
    }
}

fn state_revision_or_initial(snapshot: Option<&StoredSessionStateSnapshot>) -> u64 {
    let Some(snapshot) = snapshot else {
        return INITIAL_STATE_REVISION;
    };
    let Some(revision) = snapshot.state_revision() else {
        return INITIAL_STATE_REVISION;
    };
    revision
}

fn session_identity(session: &StoredSession) -> Result<SessionIdentity, String> {
    let session_id = SessionRef::from(session.id);
    match (session.meta.parent_id, session.meta.root_session_id) {
        (Some(_), None) => Err(format!(
            "session {} has parent metadata without a root session",
            session.id
        )),
        (_, Some(root_id)) if root_id != session.id => Ok(SessionIdentity::child(
            session_id,
            SessionRef::from(root_id),
        )),
        _ => Ok(SessionIdentity::root(session_id)),
    }
}

fn outer_compaction_revision(transcript: &[TranscriptEntry<Message>]) -> Option<u64> {
    match transcript.first() {
        Some(TranscriptEntry::Compaction { state_revision, .. }) => *state_revision,
        _ => None,
    }
}

fn hydration_snapshot(
    session: &StoredSession,
) -> Result<Option<StoredSessionStateSnapshot>, String> {
    let Some(revision) = outer_compaction_revision(&session.transcript) else {
        return Ok(session.meta.state_snapshot.clone());
    };
    let latest = session.meta.state_snapshot.clone();
    if state_revision_or_initial(latest.as_ref()) >= revision {
        return Ok(latest);
    }
    match session.meta.compaction_state_at(revision) {
        Ok(snapshot) => Ok(Some(snapshot.clone())),
        Err(
            error @ (CompactionStateError::MissingRevision { .. }
            | CompactionStateError::FutureRevision { .. }),
        ) => {
            warn!(
                session_id = %session.id,
                checkpoint_revision = revision,
                %error,
                "compaction state checkpoint unavailable; using latest plugin state"
            );
            Ok(latest)
        }
        Err(error) => Err(format!(
            "cannot restore compaction state revision {revision} for session {}: {error}",
            session.id
        )),
    }
}

fn update_turn_metadata(
    session: &mut StoredSession,
    mode: &AgentMode,
    plan_path: Option<&Path>,
) -> Result<(), &'static str> {
    let (stored_mode, stored_plan_path) = match mode {
        AgentMode::Build => (StoredMode::Build, plan_path),
        AgentMode::Plan(path) => (StoredMode::Plan, Some(path.as_path())),
        AgentMode::Research => (StoredMode::Research, None),
    };
    let stored_plan_path = stored_plan_path
        .map(|path| {
            path.to_str()
                .map(str::to_owned)
                .ok_or(NON_UTF8_PLAN_PATH_ERR)
        })
        .transpose()?;
    session.meta.mode = Some(stored_mode);
    session.meta.plan_path = stored_plan_path;
    session.meta.revision = session.meta.revision.saturating_add(1);
    Ok(())
}

struct SessionStore {
    dir: StateDir,
    session: StoredSession,
    state_persistence: Option<Arc<dyn SessionStatePersistence>>,
    identity: SessionIdentity,
    state_lease: Option<u64>,
    state_revision: u64,
}

impl SessionStore {
    fn open(
        session_id: n00nId,
        cwd: &str,
        model_spec: &str,
        mode: &AgentMode,
        state_persistence: Option<Arc<dyn SessionStatePersistence>>,
    ) -> Result<Option<Self>, String> {
        let Some(dir) = StateDir::resolve()
            .map_err(|error| {
                warn!(%error, "state dir unavailable; session will not be persisted");
            })
            .ok()
        else {
            return Ok(None);
        };
        Self::open_in_with_state(dir, session_id, cwd, model_spec, mode, state_persistence)
            .map(Some)
    }

    #[cfg(test)]
    fn open_in(
        dir: StateDir,
        session_id: n00nId,
        cwd: &str,
        model_spec: &str,
        mode: &AgentMode,
    ) -> Self {
        Self::open_in_with_state(dir, session_id, cwd, model_spec, mode, None)
            .expect("new test session must open")
    }

    fn open_in_with_state(
        dir: StateDir,
        session_id: n00nId,
        cwd: &str,
        model_spec: &str,
        mode: &AgentMode,
        state_persistence: Option<Arc<dyn SessionStatePersistence>>,
    ) -> Result<Self, String> {
        let session_path = dir
            .ensure_subdir(SESSIONS_DIR)
            .map_err(|error| format!("open sessions directory: {error}"))?
            .join(format!("{session_id}.jsonl"));
        let session_existed = session_path.exists();
        let (session, is_new) = match StoredSession::load(session_id, &dir) {
            Ok(session) => (session, false),
            Err(SessionError::Storage(StorageError::NotFound(_))) if !session_existed => {
                let mut session = StoredSession::new(model_spec, cwd);
                session.id = session_id;
                (session, true)
            }
            Err(error) => return Err(format!("load session {session_id}: {error}")),
        };
        let identity = session_identity(&session)?;
        let mut store = Self {
            dir,
            session,
            state_persistence,
            identity,
            state_lease: None,
            state_revision: INITIAL_STATE_REVISION,
        };
        store.hydrate_plugin_state();
        if is_new {
            if let Err(error) = store.update_turn_metadata(mode, None) {
                warn!(error, "session metadata was not persisted");
            } else {
                store.save();
            }
        }
        Ok(store)
    }

    fn state_revision(&self) -> u64 {
        self.state_revision
    }

    fn hydrate_plugin_state(&mut self) {
        let snapshot = match hydration_snapshot(&self.session) {
            Ok(snapshot) => snapshot,
            Err(error) => {
                warn!(session_id = %self.session.id, %error, "failed to select plugin session state; using latest snapshot");
                self.session.meta.state_snapshot.clone()
            }
        };
        self.state_revision = state_revision_or_initial(snapshot.as_ref());
        let Some(state_persistence) = &self.state_persistence else {
            return;
        };
        match state_persistence.hydrate(&self.identity, snapshot) {
            Ok(lease) => self.state_lease = Some(lease),
            Err(error) => {
                warn!(session_id = %self.session.id, %error, "failed to restore plugin session state; continuing without hydrated state");
            }
        }
    }

    fn capture_plugin_state(&mut self) {
        let Some(state_persistence) = &self.state_persistence else {
            return;
        };
        let persisted_revision =
            state_revision_or_initial(self.session.meta.state_snapshot.as_ref());
        let revision = self
            .session
            .meta
            .revision
            .max(self.state_revision.saturating_add(1))
            .max(persisted_revision.saturating_add(1));
        match state_persistence.capture(&self.identity, revision) {
            Ok(snapshot) => {
                self.state_revision = state_revision_or_initial(Some(&snapshot));
                self.session.meta.state_snapshot = Some(snapshot);
            }
            Err(error) => {
                warn!(session_id = %self.session.id, %error, "failed to capture plugin session state");
            }
        }
    }

    fn save(&mut self) {
        self.capture_plugin_state();
        if let Err(e) = self.session.save(&self.dir) {
            warn!(error = %e, session_id = %self.session.id, "failed to persist session");
        }
    }

    fn compaction_snapshot(
        &self,
        session: &StoredSession,
        revision: u64,
    ) -> Result<Option<StoredSessionStateSnapshot>, String> {
        match session.meta.compaction_state_at(revision) {
            Ok(_) => return Ok(None),
            Err(
                CompactionStateError::MissingRevision { .. }
                | CompactionStateError::FutureRevision { .. },
            ) => {}
            Err(error) => return Err(error.to_string()),
        }

        let snapshot = if let Some(state_persistence) = &self.state_persistence {
            state_persistence.capture(&self.identity, revision)?
        } else if let Some(mut snapshot) = session.meta.state_snapshot.clone() {
            snapshot
                .set_state_revision(revision)
                .map_err(|error| error.to_string())?;
            snapshot
        } else {
            StoredSessionStateSnapshot::new(revision)
        };
        if snapshot.state_revision() != Some(revision) {
            return Err(format!(
                "captured plugin state revision does not match compaction revision {revision}"
            ));
        }
        Ok(Some(snapshot))
    }

    fn update_turn_metadata(
        &mut self,
        mode: &AgentMode,
        plan_path: Option<&Path>,
    ) -> Result<(), &'static str> {
        update_turn_metadata(&mut self.session, mode, plan_path)
    }

    fn record_turn_started(
        &mut self,
        mode: &AgentMode,
        plan_path: Option<&Path>,
    ) -> Result<(), &'static str> {
        self.update_turn_metadata(mode, plan_path)?;
        self.save();
        Ok(())
    }

    fn checkpoint_compaction(
        &mut self,
        messages: &[Message],
        transcript: &[TranscriptEntry<Message>],
        model_spec: &str,
        revision: u64,
    ) -> Result<(), String> {
        if outer_compaction_revision(transcript) != Some(revision) {
            return Err(format!(
                "compaction boundary revision does not match checkpoint revision {revision}"
            ));
        }
        let mut candidate = self.session.clone();
        candidate.messages = messages.to_vec();
        candidate.transcript = transcript.to_vec();
        model_spec.clone_into(&mut candidate.model);
        candidate.update_title_if_default();
        let quarantined = matches!(
            candidate.meta.compaction_state_at(revision),
            Err(CompactionStateError::UnsupportedSchemaVersion { .. }
                | CompactionStateError::InvalidEnvelope)
        );
        if quarantined {
            warn!(session_id = %candidate.id, checkpoint_revision = revision, "compaction checkpoint metadata is unusable; compacting without an exact checkpoint");
        } else if let Some(snapshot) = self.compaction_snapshot(&candidate, revision)? {
            candidate
                .meta
                .checkpoint_compaction_state(snapshot.clone())
                .map_err(|error| error.to_string())?;
            if state_revision_or_initial(candidate.meta.state_snapshot.as_ref()) <= revision {
                candidate.meta.state_snapshot = Some(snapshot);
            }
        }
        candidate
            .save(&self.dir)
            .map_err(|error| error.to_string())?;
        self.state_revision = self.state_revision.max(revision);
        self.session = candidate;
        Ok(())
    }

    fn record_turn(
        &mut self,
        messages: &[Message],
        transcript: &[TranscriptEntry<Message>],
        model_spec: String,
        mode: &AgentMode,
        plan_path: Option<&Path>,
    ) -> Result<(), String> {
        let mut candidate = self.session.clone();
        update_turn_metadata(&mut candidate, mode, plan_path).map_err(str::to_owned)?;
        candidate.messages = messages.to_vec();
        candidate.transcript = transcript.to_vec();
        candidate.model = model_spec;
        candidate.update_title_if_default();

        self.session = candidate;
        self.capture_plugin_state();
        self.session
            .save(&self.dir)
            .map_err(|error| error.to_string())
    }
}

impl Drop for SessionStore {
    fn drop(&mut self) {
        let (Some(state_persistence), Some(lease)) = (&self.state_persistence, self.state_lease)
        else {
            return;
        };
        if let Err(error) = state_persistence.drop_owner(self.session.id, lease) {
            warn!(session_id = %self.session.id, %error, "failed to drop plugin session state");
        }
    }
}

pub struct HeadlessParams {
    pub model: Model,
    pub config: Arc<AgentConfig>,
    pub permissions_config: PermissionsConfig,
    pub timeouts: Timeouts,
    pub openai_options: OpenAiOptions,
    pub prompt: String,
    pub images: Vec<ImageSource>,
    pub prompt_slots: ResolvedSlots,
    pub state_persistence: Option<Arc<dyn SessionStatePersistence>>,
    pub excluded_tools: Vec<&'static str>,
    pub mcp_handle: Option<McpHandle>,
    pub initial_wd: PathBuf,
    pub fast: bool,
    pub workflow: bool,
    pub mode: AgentMode,
}

pub struct HeadlessHandle {
    pub event_rx: Receiver<Envelope>,
    pub tool_names: Vec<String>,
    pub session_id: SessionRef,
    pub cwd: String,
    pub task: smol::Task<()>,
}

struct AgentSetup {
    vars: template::Vars,
    instructions: agent::Instructions,
    tools: Value,
    tool_filter: ToolFilter,
}

fn setup(
    model: &Model,
    config: &Arc<AgentConfig>,
    excluded_tools: &[&'static str],
    workflow: bool,
) -> AgentSetup {
    let vars = template::env_vars();
    let instructions = agent::load_instructions(&vars.apply("{cwd}"));
    let (tools, tool_filter) = tool_definitions(
        &vars,
        model,
        config,
        excluded_tools,
        workflow,
        ToolRegistry::global(),
    );

    AgentSetup {
        vars,
        instructions,
        tools,
        tool_filter,
    }
}

fn tool_definitions(
    vars: &template::Vars,
    model: &Model,
    config: &Arc<AgentConfig>,
    excluded_tools: &[&'static str],
    workflow: bool,
    registry: &ToolRegistry,
) -> (Value, ToolFilter) {
    crate::tools::runtime_tool_definitions(registry, vars, config, model, excluded_tools, workflow)
}

#[must_use]
pub fn spawn(mut params: HeadlessParams) -> HeadlessHandle {
    let working_dir = params.initial_wd.to_string_lossy().into_owned();
    let mode = params.mode.clone();
    let AgentSetup {
        vars,
        instructions,
        tools,
        tool_filter,
    } = setup(
        &params.model,
        &params.config,
        &params.excluded_tools,
        params.workflow,
    );

    let startup_prompt_slots = Arc::new(std::mem::take(&mut params.prompt_slots));
    let tool_names = extract_tool_names(&tools);

    let (raw_tx, event_rx) = flume::unbounded::<Envelope>();

    let session_id = n00nId::generate();
    let session_ref = SessionRef::from(session_id);
    let session_ref_clone = session_ref.clone();
    let session_cwd = working_dir.clone();
    let fast = params.fast;
    let workflow = params.workflow;
    let task = smol::spawn({
        let mcp_shutdown = params.mcp_handle.clone();
        let working_dir_path = params.initial_wd.clone();
        async move {
            let event_tx = EventSender::new(raw_tx, 0);
            let mut model = params.model;
            let provider: Arc<dyn Provider> =
                Arc::from(provider::from_model_fallback_with_openai_options(
                    &mut model,
                    params.timeouts,
                    params.openai_options.clone(),
                ));
            let error_tx = event_tx.clone();
            let mut history = History::new(Vec::new());
            let model_spec = model.spec();
            let mut session_store = match SessionStore::open(
                session_ref_clone.id(),
                &session_cwd,
                &model_spec,
                &mode,
                params.state_persistence.clone(),
            ) {
                Ok(store) => store,
                Err(message) => {
                    let _ = error_tx.send(AgentEvent::Error { message });
                    if let Some(handle) = mcp_shutdown {
                        handle.shutdown().await;
                    }
                    return;
                }
            };
            let plan_path = mode.plan_path().map(PathBuf::from);
            if let Some(store) = &mut session_store
                && let Err(message) = store.record_turn_started(&mode, plan_path.as_deref())
            {
                let _ = error_tx.send(AgentEvent::Error {
                    message: message.into(),
                });
                if let Some(handle) = mcp_shutdown {
                    handle.shutdown().await;
                }
                return;
            }
            let state_revision = session_store
                .as_ref()
                .map_or(INITIAL_STATE_REVISION, SessionStore::state_revision);
            let identity = session_store.as_ref().map_or_else(
                || SessionIdentity::root(session_ref_clone.clone()),
                |store| store.identity.clone(),
            );
            let prompt_slots = resolved_prompt_slots(
                params.state_persistence.as_ref(),
                &identity,
                startup_prompt_slots,
            );
            let system =
                agent::build_system_prompt(&vars, &mode, &instructions.text, &prompt_slots, &model);
            let session_store = Arc::new(SyncMutex::new(session_store));
            let checkpoint_store = Arc::clone(&session_store);
            let checkpoint_model_spec = model_spec.clone();
            let mut agent = Agent::new(
                AgentParams {
                    provider,
                    model,
                    config: params.config,
                    tool_output_lines: ToolOutputLines::default(),
                    permissions: Arc::new(PermissionManager::new(
                        params.permissions_config,
                        working_dir_path,
                    )),
                    identity: Some(identity.clone()),
                    timeouts: params.timeouts,
                    openai_options: params.openai_options,
                    file_tracker: FileReadTracker::fresh(),
                    prompt_slots: Arc::clone(&prompt_slots),
                    subagent_cancels: Arc::new(CancelMap::new()),
                    registry: Arc::clone(ToolRegistry::global_arc()),
                    audience: ToolAudience::MAIN,
                    state_revision: Some(state_revision),
                },
                AgentRunParams {
                    history: &mut history,
                    system,
                    event_tx,
                    tools,
                    tool_filter,
                },
            )
            .with_loaded_instructions(instructions.loaded)
            .with_compaction_checkpoint(move |history, revision| {
                let mut guard = checkpoint_store
                    .lock()
                    .map_err(|error| format!("session persistence lock poisoned: {error}"))?;
                let Some(store) = guard.as_mut() else {
                    return Ok(());
                };
                store.checkpoint_compaction(
                    history.as_slice(),
                    history.transcript(),
                    &checkpoint_model_spec,
                    revision,
                )
            })
            .with_mcp(params.mcp_handle.clone().map(|h| McpSession::new(h, &[])));

            let result = agent
                .run(AgentInput {
                    message: params.prompt,
                    mode: mode.clone(),
                    images: params.images,
                    preamble: Vec::new(),
                    thinking: n00n_providers::ThinkingConfig::default(),
                    fast,
                    workflow,
                    control: false,
                    prompt: None,
                    plan_path: plan_path.clone(),
                })
                .await;
            drop(agent);

            match session_store.lock() {
                Ok(mut guard) => {
                    if let Some(store) = guard.as_mut()
                        && let Err(error) = store.record_turn(
                            history.as_slice(),
                            history.transcript(),
                            model_spec,
                            &mode,
                            plan_path.as_deref(),
                        )
                    {
                        warn!(error, "session metadata was not persisted");
                    }
                }
                Err(error) => warn!(%error, "session persistence lock poisoned"),
            }

            if let Err(e) = result {
                error!(error = %e, "agent error");
                let _ = error_tx.send(AgentEvent::Error {
                    message: e.user_message(),
                });
            }

            if let Some(handle) = mcp_shutdown {
                handle.shutdown().await;
            }
        }
    });

    HeadlessHandle {
        event_rx,
        tool_names,
        session_id: session_ref,
        cwd: working_dir,
        task,
    }
}

pub struct InteractiveParams {
    pub model: Model,
    pub config: Arc<AgentConfig>,
    pub permissions_config: PermissionsConfig,
    pub timeouts: Timeouts,
    pub openai_options: OpenAiOptions,
    pub prompt_slots: Arc<ResolvedSlots>,
    pub state_persistence: Option<Arc<dyn SessionStatePersistence>>,
    pub excluded_tools: Vec<&'static str>,
    pub mcp_handle: Option<McpHandle>,
    pub initial_wd: PathBuf,
    pub session_id: Option<SessionRef>,
    pub initial_history: Vec<Message>,
    pub initial_transcript: Vec<TranscriptEntry<Message>>,
    pub yolo: bool,
    pub system_prompt_override: Option<String>,
    pub append_system_prompt: Option<String>,
    pub workflow: bool,
    pub mode: AgentMode,
}

pub struct InteractiveHandle {
    pub event_rx: Receiver<Envelope>,
    pub tool_names: Vec<String>,
    pub input_tx: flume::Sender<AgentInput>,
    pub answer_tx: flume::Sender<String>,
    pub cancel_tx: flume::Sender<()>,
    pub shutdown_tx: flume::Sender<()>,
    pub model_tx: flume::Sender<Model>,
    pub session_id: SessionRef,
    pub permissions: Arc<PermissionManager>,
    pub task: smol::Task<()>,
}

#[must_use]
pub fn spawn_interactive(params: InteractiveParams) -> InteractiveHandle {
    let AgentSetup {
        vars,
        instructions,
        mut tools,
        tool_filter,
    } = setup(
        &params.model,
        &params.config,
        &params.excluded_tools,
        params.workflow,
    );

    let tool_names = extract_tool_names(&tools);

    let (raw_tx, event_rx) = flume::unbounded::<Envelope>();
    let (input_tx, input_rx) = flume::unbounded::<AgentInput>();
    let (answer_tx, answer_rx) = flume::unbounded::<String>();
    let (cancel_tx, cancel_rx) = flume::bounded::<()>(1);
    let (shutdown_tx, shutdown_rx) = flume::bounded::<()>(1);
    let (model_tx, model_rx) = flume::unbounded::<Model>();

    let (session_id, session_ref) = if let Some(w) = params.session_id.clone() {
        (w.id(), w)
    } else {
        let id = n00nId::generate();
        (id, SessionRef::from(id))
    };

    let working_dir = params.initial_wd.to_string_lossy().into_owned();
    let store = SessionStore::open(
        session_id,
        &working_dir,
        &params.model.spec(),
        &params.mode,
        params.state_persistence.clone(),
    );
    let permissions = Arc::new(PermissionManager::new(
        params.permissions_config.clone(),
        params.initial_wd.clone(),
    ));
    if params.yolo {
        permissions.set_yolo(true);
    }

    let answer_rx = Arc::new(Mutex::new(answer_rx));
    let file_tracker = FileReadTracker::fresh();

    let session_ref_clone = session_ref.clone();
    let task = smol::spawn({
        let permissions = Arc::clone(&permissions);
        async move {
            let mut model = params.model;
            let mut provider: Arc<dyn Provider> =
                Arc::from(provider::from_model_fallback_with_openai_options(
                    &mut model,
                    params.timeouts,
                    params.openai_options.clone(),
                ));

            let store = match store {
                Ok(store) => store,
                Err(message) => {
                    let _ = EventSender::new(raw_tx.clone(), 0).send(AgentEvent::Error { message });
                    if let Some(handle) = params.mcp_handle.clone() {
                        handle.shutdown().await;
                    }
                    return;
                }
            };
            let identity = store.as_ref().map_or_else(
                || SessionIdentity::root(session_ref_clone.clone()),
                |store| store.identity.clone(),
            );
            let mut history = History::restored_with_transcript(
                params.initial_history,
                params.initial_transcript,
            );
            let mut state_revision = store
                .as_ref()
                .map_or(INITIAL_STATE_REVISION, SessionStore::state_revision);
            let store = Arc::new(SyncMutex::new(store));
            let mut run_id: u64 = 0;
            let mut tool_filter = tool_filter.clone();

            loop {
                let Some((input, cancel, cancel_task)) =
                    next_interactive_run(&input_rx, &cancel_rx, &shutdown_rx).await
                else {
                    break;
                };
                let event_tx = EventSender::new(raw_tx.clone(), run_id);
                let error_tx = event_tx.clone();
                let turn_mode = input.mode.clone();
                let turn_plan_path = input.plan_path.clone();
                let turn_start = store
                    .lock()
                    .map_err(|error| format!("session persistence lock poisoned: {error}"))
                    .and_then(|mut guard| {
                        let Some(store) = guard.as_mut() else {
                            return Ok(None);
                        };
                        store
                            .record_turn_started(&turn_mode, turn_plan_path.as_deref())
                            .map_err(str::to_owned)?;
                        Ok(Some(store.state_revision()))
                    });
                match turn_start {
                    Ok(Some(revision)) => state_revision = revision,
                    Ok(None) => {}
                    Err(message) => {
                        let _ = error_tx.send(AgentEvent::Error { message });
                        run_id += 1;
                        cancel_task.cancel().await;
                        continue;
                    }
                }

                if let Some(mut new_model) = model_rx.try_iter().last()
                    && new_model.spec() != model.spec()
                {
                    provider = Arc::from(provider::from_model_fallback_with_openai_options(
                        &mut new_model,
                        params.timeouts,
                        params.openai_options.clone(),
                    ));
                    let (new_tools, new_filter) = tool_definitions(
                        &vars,
                        &new_model,
                        &params.config,
                        &params.excluded_tools,
                        params.workflow,
                        ToolRegistry::global(),
                    );
                    tools = new_tools;
                    tool_filter = new_filter;
                    model = new_model;
                }

                let prompt_slots = resolved_prompt_slots(
                    params.state_persistence.as_ref(),
                    &identity,
                    Arc::clone(&params.prompt_slots),
                );
                let mut system = if let Some(override_) = params.system_prompt_override.as_deref() {
                    System::from(override_)
                } else {
                    agent::build_system_prompt(
                        &vars,
                        &input.mode,
                        &instructions.text,
                        &prompt_slots,
                        &model,
                    )
                };
                if let Some(append) = &params.append_system_prompt {
                    system.push_static(format!("\n{append}"));
                }
                if matches!(input.mode, AgentMode::Build)
                    && let Some(plan_path) = input.plan_path.as_deref()
                    && let Err(e) = agent::append_build_plan_prompt(&mut system, plan_path)
                {
                    error!(error = %e, "failed to append build plan prompt");
                    run_id += 1;
                    if event_tx
                        .send(AgentEvent::Error {
                            message: e.user_message(),
                        })
                        .is_err()
                    {
                        error!("event receiver closed while reporting plan prompt error");
                        cancel_task.cancel().await;
                        break;
                    }
                    cancel_task.cancel().await;
                    continue;
                }

                while answer_rx.lock().await.try_recv().is_ok() {}

                let checkpoint_model_spec = model.spec();
                let checkpoint_store = Arc::clone(&store);
                let mut agent = Agent::new(
                    AgentParams {
                        provider: Arc::clone(&provider),
                        model: model.clone(),
                        config: Arc::clone(&params.config),
                        tool_output_lines: ToolOutputLines::default(),
                        permissions: Arc::clone(&permissions),
                        identity: Some(identity.clone()),
                        timeouts: params.timeouts,
                        openai_options: params.openai_options.clone(),
                        file_tracker: Arc::clone(&file_tracker),
                        prompt_slots: Arc::clone(&prompt_slots),
                        subagent_cancels: Arc::new(CancelMap::new()),
                        registry: Arc::clone(ToolRegistry::global_arc()),
                        audience: ToolAudience::MAIN,
                        state_revision: Some(state_revision),
                    },
                    AgentRunParams {
                        history: &mut history,
                        system,
                        event_tx,
                        tools: tools.clone(),
                        tool_filter: tool_filter.clone(),
                    },
                )
                .with_loaded_instructions(instructions.loaded.clone())
                .with_compaction_checkpoint(move |history, revision| {
                    let mut guard = checkpoint_store
                        .lock()
                        .map_err(|error| format!("session persistence lock poisoned: {error}"))?;
                    let Some(store) = guard.as_mut() else {
                        return Ok(());
                    };
                    store.checkpoint_compaction(
                        history.as_slice(),
                        history.transcript(),
                        &checkpoint_model_spec,
                        revision,
                    )
                })
                .with_user_response_rx(Arc::clone(&answer_rx))
                .with_cancel(cancel)
                .with_mcp(params.mcp_handle.clone().map(|h| McpSession::new(h, &[])));

                let result = agent.run(input).await;
                if let Some(compaction_revision) = agent.state_revision() {
                    state_revision = compaction_revision;
                }
                drop(agent);
                cancel_task.cancel().await;

                if let Err(ref e) = result {
                    error!(error = %e, "agent error");
                    let _ = error_tx.send(AgentEvent::Error {
                        message: e.user_message(),
                    });
                }

                match store.lock() {
                    Ok(mut guard) => {
                        if let Some(store) = guard.as_mut() {
                            if let Err(error) = store.record_turn(
                                history.as_slice(),
                                history.transcript(),
                                model.spec(),
                                &turn_mode,
                                turn_plan_path.as_deref(),
                            ) {
                                warn!(error, "session metadata was not persisted");
                            }
                            state_revision = store.state_revision();
                        }
                    }
                    Err(error) => warn!(%error, "session persistence lock poisoned"),
                }
                run_id += 1;
            }

            if let Some(handle) = params.mcp_handle {
                handle.shutdown().await;
            }
        }
    });

    InteractiveHandle {
        event_rx,
        tool_names,
        input_tx,
        answer_tx,
        cancel_tx,
        shutdown_tx,
        model_tx,
        session_id: session_ref,
        permissions,
        task,
    }
}

async fn next_interactive_run<T>(
    input_rx: &Receiver<T>,
    cancel_rx: &Receiver<()>,
    shutdown_rx: &Receiver<()>,
) -> Option<(T, CancelToken, smol::Task<()>)> {
    enum RunBoundary<T> {
        Input(Result<T, flume::RecvError>),
        Cancel(Result<(), flume::RecvError>),
        Shutdown,
    }

    let mut cancel_open = true;
    loop {
        while cancel_rx.try_recv().is_ok() {}
        let boundary = if cancel_open {
            futures_lite::future::or(
                async {
                    let _ = shutdown_rx.recv_async().await;
                    RunBoundary::Shutdown
                },
                futures_lite::future::or(
                    async { RunBoundary::Cancel(cancel_rx.recv_async().await) },
                    async { RunBoundary::Input(input_rx.recv_async().await) },
                ),
            )
            .await
        } else {
            futures_lite::future::or(
                async {
                    let _ = shutdown_rx.recv_async().await;
                    RunBoundary::Shutdown
                },
                async { RunBoundary::Input(input_rx.recv_async().await) },
            )
            .await
        };

        match boundary {
            RunBoundary::Input(Ok(input)) => {
                let (cancel, cancel_task) = cancellation_for_run(cancel_rx);
                return Some((input, cancel, cancel_task));
            }
            RunBoundary::Input(Err(_)) | RunBoundary::Shutdown => return None,
            RunBoundary::Cancel(Ok(())) => {}
            RunBoundary::Cancel(Err(_)) => cancel_open = false,
        }
    }
}

fn cancellation_for_run(cancel_rx: &Receiver<()>) -> (CancelToken, smol::Task<()>) {
    let (trigger, cancel) = CancelToken::new();
    let cancel_rx = cancel_rx.clone();
    let task = smol::spawn(async move {
        if cancel_rx.recv_async().await.is_ok() {
            trigger.cancel();
        }
    });
    (cancel, task)
}

fn extract_tool_names(tools: &Value) -> Vec<String> {
    tools.as_array().map_or_else(Vec::new, |arr| {
        arr.iter()
            .filter_map(|t| t["name"].as_str().map(String::from))
            .collect()
    })
}

#[cfg(test)]
mod tests {
    use n00n_storage::sessions::{TranscriptEntry, generate_title};
    use std::{slice::from_ref, sync::Mutex};
    use tempfile::TempDir;

    use super::*;

    const SESSION_ID: &str = "01965087-4c71-7f00-8000-000000000000";
    const CWD: &str = "/project";
    const MODEL_SPEC: &str = "anthropic/claude-test";

    const PLUGIN: &str = "todo_write";

    #[derive(Default)]
    struct StatePersistenceProbe {
        hydrated_revisions: Mutex<Vec<Option<u64>>>,
        hydrated_snapshots: Mutex<Vec<Option<StoredSessionStateSnapshot>>>,
        captured_revisions: Mutex<Vec<u64>>,
        captured_identities: Mutex<Vec<SessionIdentity>>,
        dropped_owners: Mutex<Vec<(n00nId, u64)>>,
        prompt_identities: Mutex<Vec<SessionIdentity>>,
        prompt_content: Mutex<Option<String>>,
        next_lease: std::sync::atomic::AtomicU64,
        fail_capture: std::sync::atomic::AtomicBool,
        fail_hydrate: std::sync::atomic::AtomicBool,
    }

    impl SessionStatePersistence for StatePersistenceProbe {
        fn hydrate(
            &self,
            _identity: &SessionIdentity,
            snapshot: Option<StoredSessionStateSnapshot>,
        ) -> Result<u64, String> {
            if self.fail_hydrate.load(std::sync::atomic::Ordering::Relaxed) {
                return Err("hydrate failed".into());
            }
            self.hydrated_revisions.lock().unwrap().push(
                snapshot
                    .as_ref()
                    .and_then(StoredSessionStateSnapshot::state_revision),
            );
            self.hydrated_snapshots.lock().unwrap().push(snapshot);
            Ok(self
                .next_lease
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed))
        }

        fn capture(
            &self,
            identity: &SessionIdentity,
            revision: u64,
        ) -> Result<StoredSessionStateSnapshot, String> {
            self.captured_revisions.lock().unwrap().push(revision);
            self.captured_identities
                .lock()
                .unwrap()
                .push(identity.clone());
            if self.fail_capture.load(std::sync::atomic::Ordering::Relaxed) {
                return Err("capture failed".into());
            }
            let mut snapshot = StoredSessionStateSnapshot::new(revision);
            snapshot
                .set_plugin_state(
                    PLUGIN,
                    1,
                    n00n_storage::sessions::StoredStateScope::Root,
                    serde_json::json!({"todos": []}),
                )
                .unwrap();
            snapshot
                .set_plugin_state(
                    PLUGIN,
                    1,
                    n00n_storage::sessions::StoredStateScope::Session,
                    serde_json::json!({"draft": "child"}),
                )
                .unwrap();
            Ok(snapshot)
        }

        fn prompt_slots(
            &self,
            identity: &SessionIdentity,
        ) -> Result<Option<ResolvedSlots>, String> {
            self.prompt_identities
                .lock()
                .unwrap()
                .push(identity.clone());
            let Some(content) = self.prompt_content.lock().unwrap().clone() else {
                return Ok(None);
            };
            let mut slots = ResolvedSlots::default();
            slots.insert(
                crate::prompt::PromptId::System,
                crate::prompt::Slot::AfterInstructions,
                crate::prompt::SlotEntry {
                    plugin: Arc::from(PLUGIN),
                    content,
                },
            );
            Ok(Some(slots))
        }

        fn drop_owner(&self, owner: n00nId, lease: u64) -> Result<(), String> {
            self.dropped_owners.lock().unwrap().push((owner, lease));
            Ok(())
        }
    }

    fn session_id() -> n00nId {
        SESSION_ID.parse().unwrap()
    }

    fn store_in(tmp: &TempDir) -> SessionStore {
        SessionStore::open_in(
            StateDir::from_path(tmp.path().to_path_buf()),
            session_id(),
            CWD,
            MODEL_SPEC,
            &AgentMode::Plan(PathBuf::from("plan.md")),
        )
    }

    fn load(tmp: &TempDir) -> StoredSession {
        StoredSession::load(session_id(), &StateDir::from_path(tmp.path().to_path_buf())).unwrap()
    }

    #[test]
    fn corrupt_session_load_is_propagated_without_overwriting_history() {
        let tmp = TempDir::new().unwrap();
        drop(store_in(&tmp));
        let dir = StateDir::from_path(tmp.path().to_path_buf());
        let sessions_dir = dir
            .ensure_subdir(n00n_storage::sessions::SESSIONS_DIR)
            .unwrap();
        let path = sessions_dir.join(format!("{}.jsonl", session_id()));
        let persisted = std::fs::read(&path).unwrap();
        let corrupt_history = &persisted[..5];
        std::fs::write(&path, corrupt_history).unwrap();

        let Err(error) = SessionStore::open_in_with_state(
            dir,
            session_id(),
            CWD,
            MODEL_SPEC,
            &AgentMode::Build,
            None,
        ) else {
            panic!("corrupt session unexpectedly opened");
        };

        assert!(error.contains("load session"));
        assert_eq!(std::fs::read(path).unwrap(), corrupt_history);
    }

    #[test]
    fn idle_cancellation_does_not_cancel_the_next_run() {
        smol::block_on(async {
            let (input_tx, input_rx) = flume::bounded(1);
            let (cancel_tx, cancel_rx) = flume::bounded(1);
            let (_shutdown_tx, shutdown_rx) = flume::bounded(1);
            cancel_tx.send(()).unwrap();
            input_tx.send(7).unwrap();

            let (input, cancel, cancel_task) =
                next_interactive_run(&input_rx, &cancel_rx, &shutdown_rx)
                    .await
                    .unwrap();

            assert_eq!(input, 7);
            assert!(!cancel.is_cancelled());

            cancel_tx.send_async(()).await.unwrap();
            cancel.cancelled().await;
            assert!(cancel.is_cancelled());
            cancel_task.await;
        });
    }

    #[test]
    fn cancellation_after_input_dequeue_cancels_run_during_setup() {
        smol::block_on(async {
            let (input_tx, input_rx) = flume::bounded(1);
            let (cancel_tx, cancel_rx) = flume::bounded(1);
            let (_shutdown_tx, shutdown_rx) = flume::bounded(1);
            input_tx.send(7).unwrap();

            let (input, cancel, cancel_task) =
                next_interactive_run(&input_rx, &cancel_rx, &shutdown_rx)
                    .await
                    .unwrap();
            assert_eq!(input, 7);

            cancel_tx.send_async(()).await.unwrap();
            cancel.cancelled().await;
            assert!(cancel.is_cancelled());
            cancel_task.await;
        });
    }

    #[test_case::test_case(())]
    fn resolved_prompt_slots_uses_requested_session(_case: ()) {
        let probe = Arc::new(StatePersistenceProbe::default());
        *probe.prompt_content.lock().unwrap() = Some("restored todo".into());
        let persistence: Arc<dyn SessionStatePersistence> = Arc::clone(&probe) as Arc<_>;
        let identity = SessionIdentity::root(SessionRef::generate());

        let slots = resolved_prompt_slots(
            Some(&persistence),
            &identity,
            Arc::new(ResolvedSlots::default()),
        );

        assert_eq!(
            probe.prompt_identities.lock().unwrap().as_slice(),
            from_ref(&identity)
        );
        assert_eq!(
            slots
                .get(
                    crate::prompt::PromptId::System,
                    crate::prompt::Slot::AfterInstructions,
                )
                .iter()
                .map(|entry| entry.content.as_str())
                .collect::<Vec<_>>(),
            ["restored todo"]
        );
    }

    #[test]
    fn new_session_is_loadable_before_first_turn() {
        let tmp = TempDir::new().unwrap();
        store_in(&tmp);
        let loaded = load(&tmp);
        assert_eq!(loaded.id, session_id());
        assert_eq!(loaded.cwd, CWD);
        assert_eq!(loaded.model, MODEL_SPEC);
        assert_eq!(loaded.meta.mode, Some(StoredMode::Plan));
        assert_eq!(loaded.meta.plan_path.as_deref(), Some("plan.md"));
        assert!(loaded.messages.is_empty());
    }

    #[test]
    fn record_turn_persists_messages_and_title() {
        let tmp = TempDir::new().unwrap();
        let mut store = store_in(&tmp);
        let messages = vec![Message::user("fix the login bug".into())];
        store
            .record_turn(
                &messages,
                &[],
                MODEL_SPEC.into(),
                &AgentMode::Plan(PathBuf::from("plan.md")),
                None,
            )
            .unwrap();

        let loaded = load(&tmp);
        assert_eq!(loaded.messages.len(), 1);
        assert_eq!(loaded.title, generate_title(&messages));
    }

    #[test_case::test_case(())]
    fn turn_mode_transitions_update_restored_metadata(_case: ()) {
        let tmp = TempDir::new().unwrap();
        let mut store = store_in(&tmp);
        let build_plan = PathBuf::from("approved-plan.md");

        store
            .record_turn_started(&AgentMode::Build, Some(&build_plan))
            .unwrap();
        let loaded = load(&tmp);
        assert_eq!(loaded.meta.mode, Some(StoredMode::Build));
        assert_eq!(loaded.meta.plan_path.as_deref(), Some("approved-plan.md"));

        store
            .record_turn(
                &[],
                &[],
                MODEL_SPEC.into(),
                &AgentMode::Research,
                Some(&build_plan),
            )
            .unwrap();
        let loaded = load(&tmp);
        assert_eq!(loaded.meta.mode, Some(StoredMode::Research));
        assert!(loaded.meta.plan_path.is_none());
    }

    #[test_case::test_case(())]
    fn reopening_resumes_existing_session(_case: ()) {
        let tmp = TempDir::new().unwrap();
        let mut store = store_in(&tmp);
        store
            .record_turn(
                &[Message::user("first prompt".into())],
                &[],
                MODEL_SPEC.into(),
                &AgentMode::Plan(PathBuf::from("plan.md")),
                None,
            )
            .unwrap();
        drop(store);

        let mut store = store_in(&tmp);
        assert_eq!(store.session.messages.len(), 1);

        let messages = vec![
            Message::user("first prompt".into()),
            Message::user("second prompt".into()),
        ];
        store
            .record_turn(
                &messages,
                &[],
                "other/model".into(),
                &AgentMode::Plan(PathBuf::from("plan.md")),
                None,
            )
            .unwrap();

        let loaded = load(&tmp);
        assert_eq!(loaded.messages.len(), 2);
        assert_eq!(loaded.model, "other/model");
    }

    #[test]
    fn record_turn_persists_recursive_transcript() {
        let tmp = TempDir::new().unwrap();
        let mut store = store_in(&tmp);
        let original = Message::user("original prompt".into());
        let compact_prompt = Message::user("What did we do so far?".into());
        let summary = Message::assistant("summary".into());
        let transcript = vec![
            TranscriptEntry::Compaction {
                entries: vec![TranscriptEntry::Compaction {
                    entries: vec![TranscriptEntry::Message(original)],
                    generated_summary: None,
                    state_revision: Some(3),
                }],
                generated_summary: Some(summary.clone()),
                state_revision: Some(9),
            },
            TranscriptEntry::GeneratedMessage(compact_prompt.clone()),
            TranscriptEntry::GeneratedMessage(summary.clone()),
        ];
        let messages = [compact_prompt, summary];

        store
            .record_turn(
                &messages,
                &transcript,
                MODEL_SPEC.into(),
                &AgentMode::Build,
                None,
            )
            .unwrap();

        let loaded = load(&tmp);
        let resumed = History::restored_with_transcript(loaded.messages, loaded.transcript);
        assert_eq!(
            serde_json::to_value(resumed.transcript()).unwrap(),
            serde_json::to_value(transcript).unwrap()
        );
    }

    #[cfg(unix)]
    #[test_case::test_case(())]
    fn non_utf8_plan_path_is_rejected_without_mutating_metadata(_case: ()) {
        use std::ffi::OsString;
        use std::os::unix::ffi::OsStringExt as _;

        let tmp = TempDir::new().unwrap();
        let mut store = store_in(&tmp);
        let original = store.session.meta.clone();
        let path = PathBuf::from(OsString::from_vec(vec![0xff]));

        assert_eq!(
            store.update_turn_metadata(&AgentMode::Plan(path), None),
            Err(NON_UTF8_PLAN_PATH_ERR)
        );

        assert_eq!(store.session.meta.mode, original.mode);
        assert_eq!(store.session.meta.plan_path, original.plan_path);
    }

    #[test]
    fn session_store_hydrates_and_captures_plugin_state() {
        let tmp = TempDir::new().unwrap();
        let dir = StateDir::from_path(tmp.path().to_path_buf());
        let mut persisted = StoredSession::new(MODEL_SPEC, CWD);
        persisted.id = session_id();
        let mut snapshot = StoredSessionStateSnapshot::new(7);
        snapshot
            .set_plugin_state(
                PLUGIN,
                1,
                n00n_storage::sessions::StoredStateScope::Root,
                serde_json::json!({"todos": [{"content": "resume", "status": "pending"}]}),
            )
            .unwrap();
        persisted.meta.state_snapshot = Some(snapshot);
        persisted.save(&dir).unwrap();

        let probe = Arc::new(StatePersistenceProbe::default());
        let state_persistence: Arc<dyn SessionStatePersistence> = Arc::clone(&probe) as Arc<_>;
        let mut store = SessionStore::open_in_with_state(
            dir.clone(),
            session_id(),
            CWD,
            MODEL_SPEC,
            &AgentMode::Build,
            Some(state_persistence),
        )
        .unwrap();
        assert_eq!(*probe.hydrated_revisions.lock().unwrap(), vec![Some(7)]);

        store
            .record_turn(&[], &[], MODEL_SPEC.into(), &AgentMode::Build, None)
            .unwrap();
        assert_eq!(*probe.captured_revisions.lock().unwrap(), vec![8]);
        assert_eq!(
            StoredSession::load(session_id(), &dir)
                .unwrap()
                .meta
                .state_snapshot
                .as_ref()
                .and_then(StoredSessionStateSnapshot::state_revision),
            Some(8)
        );
        drop(store);
        assert_eq!(
            *probe.dropped_owners.lock().unwrap(),
            vec![(session_id(), 0)]
        );
    }

    #[test]
    fn reopening_hydrates_latest_snapshot_after_outer_compaction() {
        let tmp = TempDir::new().unwrap();
        let dir = StateDir::from_path(tmp.path().to_path_buf());
        let mut persisted = StoredSession::new(MODEL_SPEC, CWD);
        persisted.id = session_id();
        persisted.transcript = vec![TranscriptEntry::Compaction {
            entries: vec![TranscriptEntry::Compaction {
                entries: Vec::new(),
                generated_summary: None,
                state_revision: Some(3),
            }],
            generated_summary: None,
            state_revision: Some(9),
        }];
        persisted
            .meta
            .checkpoint_compaction_state(StoredSessionStateSnapshot::new(3))
            .unwrap();
        persisted
            .meta
            .checkpoint_compaction_state(StoredSessionStateSnapshot::new(9))
            .unwrap();
        persisted.meta.state_snapshot = Some(StoredSessionStateSnapshot::new(12));
        persisted.save(&dir).unwrap();

        let probe = Arc::new(StatePersistenceProbe::default());
        let state_persistence: Arc<dyn SessionStatePersistence> = Arc::clone(&probe) as Arc<_>;
        let store = SessionStore::open_in_with_state(
            dir,
            session_id(),
            CWD,
            MODEL_SPEC,
            &AgentMode::Build,
            Some(state_persistence),
        )
        .unwrap();

        assert_eq!(*probe.hydrated_revisions.lock().unwrap(), vec![Some(12)]);
        assert_eq!(store.state_revision(), 12);
    }

    #[test]
    fn turn_persistence_does_not_backfill_old_compaction_boundary() {
        let tmp = TempDir::new().unwrap();
        let dir = StateDir::from_path(tmp.path().to_path_buf());
        let mut persisted = StoredSession::new(MODEL_SPEC, CWD);
        persisted.id = session_id();
        persisted.meta.state_snapshot = Some(StoredSessionStateSnapshot::new(4));
        persisted.save(&dir).unwrap();

        let probe = Arc::new(StatePersistenceProbe::default());
        let state_persistence: Arc<dyn SessionStatePersistence> = Arc::clone(&probe) as Arc<_>;
        let mut store = SessionStore::open_in_with_state(
            dir.clone(),
            session_id(),
            CWD,
            MODEL_SPEC,
            &AgentMode::Build,
            Some(state_persistence),
        )
        .unwrap();
        let transcript = vec![TranscriptEntry::Compaction {
            entries: Vec::new(),
            generated_summary: None,
            state_revision: Some(7),
        }];

        store
            .record_turn(&[], &transcript, MODEL_SPEC.into(), &AgentMode::Build, None)
            .unwrap();
        store
            .record_turn(&[], &transcript, MODEL_SPEC.into(), &AgentMode::Build, None)
            .unwrap();

        assert_eq!(*probe.captured_revisions.lock().unwrap(), vec![5, 6]);
        let loaded = StoredSession::load(session_id(), &dir).unwrap();
        assert!(matches!(
            loaded.meta.compaction_state_at(7),
            Err(CompactionStateError::FutureRevision { .. }
                | CompactionStateError::MissingRevision { .. })
        ));
        assert_eq!(
            loaded
                .meta
                .state_snapshot
                .as_ref()
                .and_then(StoredSessionStateSnapshot::state_revision),
            Some(6)
        );
    }

    #[test]
    fn missing_compaction_checkpoint_falls_back_to_latest_snapshot() {
        let tmp = TempDir::new().unwrap();
        let dir = StateDir::from_path(tmp.path().to_path_buf());
        let mut persisted = StoredSession::new(MODEL_SPEC, CWD);
        persisted.id = session_id();
        persisted.transcript = vec![TranscriptEntry::Compaction {
            entries: Vec::new(),
            generated_summary: None,
            state_revision: Some(7),
        }];
        persisted.meta.state_snapshot = Some(StoredSessionStateSnapshot::new(6));
        persisted.save(&dir).unwrap();

        let probe = Arc::new(StatePersistenceProbe::default());
        let state_persistence: Arc<dyn SessionStatePersistence> = Arc::clone(&probe) as Arc<_>;
        let store = SessionStore::open_in_with_state(
            dir,
            session_id(),
            CWD,
            MODEL_SPEC,
            &AgentMode::Build,
            Some(state_persistence),
        )
        .unwrap();

        assert_eq!(*probe.hydrated_revisions.lock().unwrap(), vec![Some(6)]);
        assert_eq!(store.state_revision(), 6);
    }
    #[test]
    fn hydration_failure_does_not_abort_session_open() {
        let tmp = TempDir::new().unwrap();
        let dir = StateDir::from_path(tmp.path().to_path_buf());
        let mut persisted = StoredSession::new(MODEL_SPEC, CWD);
        persisted.id = session_id();
        persisted.meta.state_snapshot = Some(StoredSessionStateSnapshot::new(6));
        persisted.save(&dir).unwrap();

        let probe = Arc::new(StatePersistenceProbe::default());
        probe
            .fail_hydrate
            .store(true, std::sync::atomic::Ordering::Relaxed);
        let state_persistence: Arc<dyn SessionStatePersistence> = Arc::clone(&probe) as Arc<_>;
        let store = SessionStore::open_in_with_state(
            dir,
            session_id(),
            CWD,
            MODEL_SPEC,
            &AgentMode::Build,
            Some(state_persistence),
        )
        .unwrap();

        assert_eq!(store.state_revision(), 6);
        assert!(store.state_lease.is_none());
    }

    #[test]
    fn compaction_checkpoint_is_durable_when_checkpoint_call_returns() {
        let tmp = TempDir::new().unwrap();
        let dir = StateDir::from_path(tmp.path().to_path_buf());
        let probe = Arc::new(StatePersistenceProbe::default());
        let state_persistence: Arc<dyn SessionStatePersistence> = Arc::clone(&probe) as Arc<_>;
        let mut store = SessionStore::open_in_with_state(
            dir.clone(),
            session_id(),
            CWD,
            MODEL_SPEC,
            &AgentMode::Build,
            Some(state_persistence),
        )
        .unwrap();
        let transcript = vec![TranscriptEntry::Compaction {
            entries: vec![TranscriptEntry::Message(Message::user("before".into()))],
            generated_summary: Some(Message::assistant("summary".into())),
            state_revision: Some(1),
        }];

        store
            .checkpoint_compaction(&[], &transcript, MODEL_SPEC, 1)
            .unwrap();

        let persisted = StoredSession::load(session_id(), &dir).unwrap();
        assert_eq!(
            serde_json::to_value(persisted.transcript).unwrap(),
            serde_json::to_value(transcript).unwrap()
        );
        assert_eq!(
            persisted
                .meta
                .compaction_state_at(1)
                .unwrap()
                .state_revision(),
            Some(1)
        );
    }

    #[test]
    fn unusable_checkpoint_metadata_does_not_block_compaction() {
        let tmp = TempDir::new().unwrap();
        let dir = StateDir::from_path(tmp.path().to_path_buf());
        let mut persisted = StoredSession::new(MODEL_SPEC, CWD);
        persisted.id = session_id();
        persisted.meta = serde_json::from_value(serde_json::json!({
            "compaction_state_checkpoints": {
                "schema_version": 2,
                "opaque": true
            }
        }))
        .unwrap();
        persisted.save(&dir).unwrap();
        let mut store = SessionStore::open_in_with_state(
            dir.clone(),
            session_id(),
            CWD,
            MODEL_SPEC,
            &AgentMode::Build,
            None,
        )
        .unwrap();
        let transcript = vec![TranscriptEntry::Compaction {
            entries: Vec::new(),
            generated_summary: None,
            state_revision: Some(1),
        }];

        store
            .checkpoint_compaction(&[], &transcript, MODEL_SPEC, 1)
            .unwrap();

        let loaded = StoredSession::load(session_id(), &dir).unwrap();
        assert_eq!(
            serde_json::to_value(loaded.transcript).unwrap(),
            serde_json::to_value(transcript).unwrap()
        );
        assert!(matches!(
            loaded.meta.compaction_state_at(1),
            Err(CompactionStateError::UnsupportedSchemaVersion { found: 2, .. })
        ));
    }

    #[test]
    fn resumed_child_hydrates_matching_root_and_child_checkpoint() {
        let tmp = TempDir::new().unwrap();
        let dir = StateDir::from_path(tmp.path().to_path_buf());
        let root_id = n00nId::generate();
        let mut persisted = StoredSession::new(MODEL_SPEC, CWD);
        persisted.id = session_id();
        persisted.meta.parent_id = Some(n00nId::generate());
        persisted.meta.root_session_id = Some(root_id);
        persisted.meta.state_snapshot = Some(StoredSessionStateSnapshot::new(4));
        persisted.save(&dir).unwrap();

        let probe = Arc::new(StatePersistenceProbe::default());
        let state_persistence: Arc<dyn SessionStatePersistence> = Arc::clone(&probe) as Arc<_>;
        let mut store = SessionStore::open_in_with_state(
            dir.clone(),
            session_id(),
            CWD,
            MODEL_SPEC,
            &AgentMode::Build,
            Some(state_persistence),
        )
        .unwrap();
        let transcript = vec![TranscriptEntry::Compaction {
            entries: Vec::new(),
            generated_summary: None,
            state_revision: Some(5),
        }];
        store
            .checkpoint_compaction(&[], &transcript, MODEL_SPEC, 5)
            .unwrap();
        assert!(
            probe
                .captured_identities
                .lock()
                .unwrap()
                .iter()
                .any(|identity| {
                    identity.session_id().id() == session_id()
                        && identity.root_session_id().id() == root_id
                })
        );
        let durable_checkpoint = StoredSession::load(session_id(), &dir)
            .unwrap()
            .meta
            .compaction_state_at(5)
            .unwrap()
            .clone();
        let durable_json = serde_json::to_value(&durable_checkpoint).unwrap();
        assert!(durable_json["plugins"][PLUGIN]["root"].is_object());
        assert!(durable_json["plugins"][PLUGIN]["session"].is_object());
        drop(store);

        let restart_probe = Arc::new(StatePersistenceProbe::default());
        let state_persistence: Arc<dyn SessionStatePersistence> =
            Arc::clone(&restart_probe) as Arc<_>;
        let restarted = SessionStore::open_in_with_state(
            dir,
            session_id(),
            CWD,
            MODEL_SPEC,
            &AgentMode::Build,
            Some(state_persistence),
        )
        .unwrap();

        assert_eq!(restarted.state_revision(), 5);
        assert_eq!(
            restart_probe.hydrated_snapshots.lock().unwrap().as_slice(),
            &[Some(durable_checkpoint)]
        );
    }

    #[test]
    fn newer_latest_snapshot_ignores_stale_checkpoint_set() {
        let tmp = TempDir::new().unwrap();
        let dir = StateDir::from_path(tmp.path().to_path_buf());
        let mut persisted = StoredSession::new(MODEL_SPEC, CWD);
        persisted.id = session_id();
        persisted.transcript = vec![TranscriptEntry::Compaction {
            entries: Vec::new(),
            generated_summary: None,
            state_revision: Some(9),
        }];
        persisted
            .meta
            .checkpoint_compaction_state(StoredSessionStateSnapshot::new(3))
            .unwrap();
        persisted.meta.state_snapshot = Some(StoredSessionStateSnapshot::new(12));
        persisted.save(&dir).unwrap();

        let store = SessionStore::open_in_with_state(
            dir,
            session_id(),
            CWD,
            MODEL_SPEC,
            &AgentMode::Build,
            None,
        )
        .unwrap();

        assert_eq!(store.state_revision(), 12);
    }

    #[test]
    fn failed_plugin_state_capture_preserves_persisted_snapshot() {
        let tmp = TempDir::new().unwrap();
        let dir = StateDir::from_path(tmp.path().to_path_buf());
        let mut persisted = StoredSession::new(MODEL_SPEC, CWD);
        persisted.id = session_id();
        persisted.meta.state_snapshot = Some(StoredSessionStateSnapshot::new(11));
        persisted.save(&dir).unwrap();

        let probe = Arc::new(StatePersistenceProbe::default());
        probe
            .fail_capture
            .store(true, std::sync::atomic::Ordering::Relaxed);
        let state_persistence: Arc<dyn SessionStatePersistence> = Arc::clone(&probe) as Arc<_>;
        let mut store = SessionStore::open_in_with_state(
            dir.clone(),
            session_id(),
            CWD,
            MODEL_SPEC,
            &AgentMode::Build,
            Some(state_persistence),
        )
        .unwrap();
        store
            .record_turn(&[], &[], MODEL_SPEC.into(), &AgentMode::Build, None)
            .unwrap();

        assert_eq!(
            StoredSession::load(session_id(), &dir)
                .unwrap()
                .meta
                .state_snapshot
                .as_ref()
                .and_then(StoredSessionStateSnapshot::state_revision),
            Some(11)
        );
    }

    #[test]
    fn extract_tool_names_filters_valid_entries() {
        let tools = serde_json::json!([{"name": "read"}, {"type": "function"}, {"name": "bash"}]);
        assert_eq!(extract_tool_names(&tools), vec!["read", "bash"]);
    }
}

use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_lock::Mutex;
use flume::Receiver;
use n00n_providers::Message;
use n00n_providers::OpenAiOptions;
use n00n_providers::System;
use n00n_providers::Timeouts;
use n00n_providers::TokenUsage;
use n00n_providers::model::Model;
use n00n_providers::provider::{self, Provider};
use n00n_storage::StateDir;
use n00n_storage::id::{SessionRef, n00nId};
use n00n_storage::sessions::{Session, StoredMode, StoredSessionStateSnapshot};
use serde_json::Value;
use tracing::{error, warn};

use crate::agent::{self, History};
use crate::cancel::{CancelMap, CancelToken};
use crate::permissions::PermissionManager;
use crate::prompt::ResolvedSlots;
use crate::template;
use crate::tools::{
    DescriptionContext, FileReadTracker, SessionIdentity, ToolAudience, ToolFilter, ToolRegistry,
};
use crate::{
    Agent, AgentConfig, AgentEvent, AgentInput, AgentMode, AgentParams, AgentRunParams, Envelope,
    EventSender, ImageSource, McpHandle, McpSession, PermissionsConfig, ToolOutput,
    ToolOutputLines,
};

type StoredSession = Session<Message, TokenUsage, ToolOutput>;

const NON_UTF8_PLAN_PATH_ERR: &str = "plan path must be valid UTF-8";
const INITIAL_STATE_REVISION: u64 = 0;

pub trait SessionStatePersistence: Send + Sync {
    /// # Errors
    /// Returns an error when the runtime cannot restore the snapshot.
    fn hydrate(
        &self,
        identity: &SessionIdentity,
        snapshot: Option<StoredSessionStateSnapshot>,
    ) -> Result<u64, String>;

    /// # Errors
    /// Returns an error when the runtime cannot capture its current state.
    fn capture(
        &self,
        identity: &SessionIdentity,
        revision: u64,
    ) -> Result<StoredSessionStateSnapshot, String>;

    /// # Errors
    /// Returns an error when the runtime cannot remove the owner state.
    fn drop_owner(&self, owner: n00nId, lease: u64) -> Result<(), String>;
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

struct SessionStore {
    dir: StateDir,
    session: StoredSession,
    state_persistence: Option<Arc<dyn SessionStatePersistence>>,
    identity: SessionIdentity,
    state_lease: Option<u64>,
}

impl SessionStore {
    fn open(
        session_id: n00nId,
        cwd: &str,
        model_spec: &str,
        mode: &AgentMode,
        state_persistence: Option<Arc<dyn SessionStatePersistence>>,
    ) -> Option<Self> {
        let dir = StateDir::resolve()
            .map_err(|e| warn!(error = %e, "state dir unavailable; session will not be persisted"))
            .ok()?;
        Some(Self::open_in_with_state(
            dir,
            session_id,
            cwd,
            model_spec,
            mode,
            state_persistence,
        ))
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
    }

    fn open_in_with_state(
        dir: StateDir,
        session_id: n00nId,
        cwd: &str,
        model_spec: &str,
        mode: &AgentMode,
        state_persistence: Option<Arc<dyn SessionStatePersistence>>,
    ) -> Self {
        let mut is_new = false;
        let session = if let Ok(session) = StoredSession::load(session_id, &dir) {
            session
        } else {
            is_new = true;
            let mut session = StoredSession::new(model_spec, cwd);
            session.id = session_id;
            session
        };
        let identity = SessionIdentity::root(SessionRef::from(session_id));
        let mut store = Self {
            dir,
            session,
            state_persistence,
            identity,
            state_lease: None,
        };
        store.hydrate_plugin_state();
        if is_new {
            if let Err(error) = store.update_turn_metadata(mode, None) {
                warn!(error, "session metadata was not persisted");
            } else {
                store.save();
            }
        }
        store
    }

    fn hydrate_plugin_state(&mut self) {
        let Some(state_persistence) = &self.state_persistence else {
            return;
        };
        match state_persistence.hydrate(&self.identity, self.session.meta.state_snapshot.clone()) {
            Ok(lease) => self.state_lease = Some(lease),
            Err(error) => {
                warn!(session_id = %self.session.id, %error, "failed to restore plugin session state");
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
            .max(persisted_revision.saturating_add(1));
        match state_persistence.capture(&self.identity, revision) {
            Ok(snapshot) => self.session.meta.state_snapshot = Some(snapshot),
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

    fn update_turn_metadata(
        &mut self,
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
        self.session.meta.mode = Some(stored_mode);
        self.session.meta.plan_path = stored_plan_path;
        self.session.meta.revision = self.session.meta.revision.saturating_add(1);
        Ok(())
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

    fn record_turn(
        &mut self,
        messages: &[Message],
        model_spec: String,
        mode: &AgentMode,
        plan_path: Option<&Path>,
    ) -> Result<(), &'static str> {
        self.update_turn_metadata(mode, plan_path)?;
        self.session.messages = messages.to_vec();
        self.session.model = model_spec;
        self.session.update_title_if_default();
        self.save();
        Ok(())
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
    let filter = ToolFilter::from_config(config, model, excluded_tools);
    let ctx = DescriptionContext {
        filter: &filter,
        audience: ToolAudience::MAIN,
        workflow,
    };
    let tools = registry.definitions_active(
        vars,
        &ctx,
        model.supports_tool_examples(),
        &crate::tools::default_active_tools(),
    );

    (tools, filter)
}

#[must_use]
pub fn spawn(params: HeadlessParams) -> HeadlessHandle {
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

    let system = agent::build_system_prompt(
        &vars,
        &mode,
        &instructions.text,
        &params.prompt_slots,
        &params.model,
    );

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
                    params.openai_options,
                ));
            let error_tx = event_tx.clone();
            let mut history = History::new(Vec::new());
            let model_spec = model.spec();
            let mut session_store = SessionStore::open(
                session_ref_clone.id(),
                &session_cwd,
                &model_spec,
                &mode,
                params.state_persistence,
            );
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
                    session_id: Some(session_ref_clone.clone()),
                    timeouts: params.timeouts,
                    openai_options: params.openai_options,
                    file_tracker: FileReadTracker::fresh(),
                    prompt_slots: Arc::new(params.prompt_slots),
                    subagent_cancels: Arc::new(CancelMap::new()),
                    registry: Arc::clone(ToolRegistry::global_arc()),
                    audience: ToolAudience::MAIN,
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
            .with_mcp(params.mcp_handle.clone().map(|h| McpSession::new(h, &[])));

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

            if let Some(store) = &mut session_store
                && let Err(error) =
                    store.record_turn(history.as_slice(), model_spec, &mode, plan_path.as_deref())
            {
                warn!(error, "session metadata was not persisted");
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
                    params.openai_options,
                ));

            let mut store = store;
            let mut history = History::restored(params.initial_history);
            let mut run_id: u64 = 0;
            let mut tool_filter = tool_filter.clone();

            while let Ok(input) = input_rx.recv_async().await {
                let event_tx = EventSender::new(raw_tx.clone(), run_id);
                let error_tx = event_tx.clone();
                let turn_mode = input.mode.clone();
                let turn_plan_path = input.plan_path.clone();
                if let Some(store) = &mut store
                    && let Err(message) =
                        store.record_turn_started(&turn_mode, turn_plan_path.as_deref())
                {
                    let _ = error_tx.send(AgentEvent::Error {
                        message: message.into(),
                    });
                    run_id += 1;
                    continue;
                }

                if let Some(mut new_model) = model_rx.try_iter().last()
                    && new_model.spec() != model.spec()
                {
                    provider = Arc::from(provider::from_model_fallback_with_openai_options(
                        &mut new_model,
                        params.timeouts,
                        params.openai_options,
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

                let mut system = if let Some(override_) = params.system_prompt_override.as_deref() {
                    System::from(override_)
                } else {
                    agent::build_system_prompt(
                        &vars,
                        &input.mode,
                        &instructions.text,
                        &params.prompt_slots,
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
                        break;
                    }
                    continue;
                }

                let (trigger, cancel) = CancelToken::new();
                let cancel_task = smol::spawn({
                    let cancel_rx = cancel_rx.clone();
                    async move {
                        if cancel_rx.recv_async().await.is_ok() {
                            trigger.cancel();
                        }
                    }
                });

                while answer_rx.lock().await.try_recv().is_ok() {}

                let mut agent = Agent::new(
                    AgentParams {
                        provider: Arc::clone(&provider),
                        model: model.clone(),
                        config: Arc::clone(&params.config),
                        tool_output_lines: ToolOutputLines::default(),
                        permissions: Arc::clone(&permissions),
                        session_id: Some(session_ref_clone.clone()),
                        timeouts: params.timeouts,
                        openai_options: params.openai_options,
                        file_tracker: Arc::clone(&file_tracker),
                        prompt_slots: Arc::clone(&params.prompt_slots),
                        subagent_cancels: Arc::new(CancelMap::new()),
                        registry: Arc::clone(ToolRegistry::global_arc()),
                        audience: ToolAudience::MAIN,
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
                .with_user_response_rx(Arc::clone(&answer_rx))
                .with_cancel(cancel)
                .with_mcp(params.mcp_handle.clone().map(|h| McpSession::new(h, &[])));

                let result = agent.run(input).await;
                drop(agent);
                cancel_task.cancel().await;

                if let Err(ref e) = result {
                    error!(error = %e, "agent error");
                    let _ = error_tx.send(AgentEvent::Error {
                        message: e.user_message(),
                    });
                }

                if let Some(store) = &mut store
                    && let Err(error) = store.record_turn(
                        history.as_slice(),
                        model.spec(),
                        &turn_mode,
                        turn_plan_path.as_deref(),
                    )
                {
                    warn!(error, "session metadata was not persisted");
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
        model_tx,
        session_id: session_ref,
        permissions,
        task,
    }
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
    use n00n_storage::sessions::generate_title;
    use tempfile::TempDir;

    use super::*;

    const SESSION_ID: &str = "01965087-4c71-7f00-8000-000000000000";
    const CWD: &str = "/project";
    const MODEL_SPEC: &str = "anthropic/claude-test";

    const PLUGIN: &str = "todo_write";

    #[derive(Default)]
    struct StatePersistenceProbe {
        hydrated_revisions: std::sync::Mutex<Vec<Option<u64>>>,
        captured_revisions: std::sync::Mutex<Vec<u64>>,
        dropped_owners: std::sync::Mutex<Vec<(n00nId, u64)>>,
        next_lease: std::sync::atomic::AtomicU64,
        fail_capture: std::sync::atomic::AtomicBool,
    }

    impl SessionStatePersistence for StatePersistenceProbe {
        fn hydrate(
            &self,
            _identity: &SessionIdentity,
            snapshot: Option<StoredSessionStateSnapshot>,
        ) -> Result<u64, String> {
            self.hydrated_revisions.lock().unwrap().push(
                snapshot
                    .as_ref()
                    .and_then(StoredSessionStateSnapshot::state_revision),
            );
            Ok(self
                .next_lease
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed))
        }

        fn capture(
            &self,
            _identity: &SessionIdentity,
            revision: u64,
        ) -> Result<StoredSessionStateSnapshot, String> {
            self.captured_revisions.lock().unwrap().push(revision);
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
            Ok(snapshot)
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
                "other/model".into(),
                &AgentMode::Plan(PathBuf::from("plan.md")),
                None,
            )
            .unwrap();

        let loaded = load(&tmp);
        assert_eq!(loaded.messages.len(), 2);
        assert_eq!(loaded.model, "other/model");
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
        );
        assert_eq!(*probe.hydrated_revisions.lock().unwrap(), vec![Some(7)]);

        store
            .record_turn(&[], MODEL_SPEC.into(), &AgentMode::Build, None)
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
        );
        store
            .record_turn(&[], MODEL_SPEC.into(), &AgentMode::Build, None)
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

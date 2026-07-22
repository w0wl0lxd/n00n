use std::path::PathBuf;
use std::sync::Arc;

use async_lock::Mutex;
use flume::Receiver;
use n00n_providers::Message;
use n00n_providers::OpenAiOptions;
use n00n_providers::Timeouts;
use n00n_providers::TokenUsage;
use n00n_providers::model::Model;
use n00n_providers::provider::{self, Provider};
use n00n_storage::StateDir;
use n00n_storage::id::{N00nId, SessionRef};
use n00n_storage::sessions::Session;
use serde_json::Value;
use tracing::{error, warn};

use crate::agent::{self, History};
use crate::cancel::{CancelMap, CancelToken};
use crate::permissions::PermissionManager;
use crate::prompt::ResolvedSlots;
use crate::template;
use crate::tools::{
    ActiveTools, DescriptionContext, FileReadTracker, ToolAudience, ToolFilter, ToolRegistry,
};
use crate::{
    Agent, AgentConfig, AgentError, AgentEvent, AgentInput, AgentMode, AgentParams, AgentRunParams,
    Envelope, EventSender, ImageSource, LoadedInstructions, McpHandle, PermissionsConfig,
    ToolOutput, ToolOutputLines,
};

type StoredSession = Session<Message, TokenUsage, ToolOutput>;

struct SessionStore {
    dir: StateDir,
    session: StoredSession,
}

impl SessionStore {
    fn open(session_id: N00nId, cwd: &str, model_spec: &str) -> Option<Self> {
        let dir = StateDir::resolve()
            .map_err(|e| warn!(error = %e, "state dir unavailable; session will not be persisted"))
            .ok()?;
        Some(Self::open_in(dir, session_id, cwd, model_spec))
    }

    fn open_in(dir: StateDir, session_id: N00nId, cwd: &str, model_spec: &str) -> Self {
        if let Ok(session) = StoredSession::load(session_id, &dir) {
            Self { dir, session }
        } else {
            let mut session = StoredSession::new(model_spec, cwd);
            session.id = session_id;
            let mut store = Self { dir, session };
            store.save();
            store
        }
    }

    fn save(&mut self) {
        if let Err(e) = self.session.save(&self.dir) {
            warn!(error = %e, session_id = %self.session.id, "failed to persist session");
        }
    }

    fn record_turn(&mut self, messages: &[Message], model_spec: String) {
        self.session.messages = messages.to_vec();
        self.session.model = model_spec;
        self.session.update_title_if_default();
        self.save();
    }
}

pub struct HeadlessParams {
    pub model: Model,
    pub config: AgentConfig,
    pub permissions_config: PermissionsConfig,
    pub timeouts: Timeouts,
    pub openai_options: OpenAiOptions,
    pub prompt: String,
    pub images: Vec<ImageSource>,
    pub prompt_slots: ResolvedSlots,
    pub excluded_tools: Vec<&'static str>,
    pub mcp_handle: Option<McpHandle>,
    pub initial_wd: PathBuf,
    pub fast: bool,
    pub workflow: bool,
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

struct HeadlessAgentContext {
    raw_tx: flume::Sender<Envelope>,
    params: HeadlessParams,
    session_ref: SessionRef,
    session_cwd: String,
    system: String,
    tools: Value,
    tool_filter: ToolFilter,
    loaded_instructions: LoadedInstructions,
    mode: AgentMode,
    fast: bool,
    workflow: bool,
}

struct InteractiveAgentContext {
    raw_tx: flume::Sender<Envelope>,
    input_rx: flume::Receiver<AgentInput>,
    cancel_rx: flume::Receiver<()>,
    model_rx: flume::Receiver<Model>,
    answer_rx: Arc<Mutex<flume::Receiver<String>>>,
    file_tracker: Arc<FileReadTracker>,
    params: InteractiveParams,
    vars: template::Vars,
    instructions: agent::Instructions,
    tools: Value,
    tool_filter: ToolFilter,
    session_id: N00nId,
    working_dir: String,
    session_ref: SessionRef,
    permissions: Arc<PermissionManager>,
}

struct ModelChangeContext<'a> {
    model_rx: &'a flume::Receiver<Model>,
    model: &'a mut Model,
    provider: &'a mut Arc<dyn Provider>,
    tools: &'a mut Value,
    tool_filter: &'a mut ToolFilter,
    vars: &'a template::Vars,
    config: &'a AgentConfig,
    excluded_tools: &'a [&'static str],
    mcp_handle: Option<&'a McpHandle>,
    workflow: bool,
    timeouts: Timeouts,
    openai_options: OpenAiOptions,
}

struct SingleTurnContext<'a> {
    input: AgentInput,
    history: &'a mut History,
    model: &'a Model,
    config: &'a AgentConfig,
    provider: &'a Arc<dyn Provider>,
    permissions: &'a Arc<PermissionManager>,
    session_ref: &'a SessionRef,
    timeouts: Timeouts,
    openai_options: OpenAiOptions,
    file_tracker: &'a Arc<FileReadTracker>,
    prompt_slots: &'a Arc<ResolvedSlots>,
    tools: &'a Value,
    tool_filter: &'a ToolFilter,
    loaded_instructions: &'a LoadedInstructions,
    answer_rx: &'a Arc<Mutex<flume::Receiver<String>>>,
    cancel_rx: &'a flume::Receiver<()>,
    system: String,
    event_tx: EventSender,
    mcp_handle: Option<McpHandle>,
}

fn setup(
    model: &Model,
    config: &AgentConfig,
    excluded_tools: &[&'static str],
    mcp_handle: Option<&McpHandle>,
    workflow: bool,
) -> AgentSetup {
    let vars = template::env_vars();
    let instructions = agent::load_instructions(&vars.apply("{cwd}"));
    let (tools, tool_filter) = tool_definitions(
        &vars,
        model,
        config,
        excluded_tools,
        mcp_handle,
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
    config: &AgentConfig,
    excluded_tools: &[&'static str],
    mcp_handle: Option<&McpHandle>,
    workflow: bool,
    registry: &ToolRegistry,
) -> (Value, ToolFilter) {
    let filter = ToolFilter::from_config(config, model, excluded_tools);
    let ctx = DescriptionContext {
        filter: &filter,
        audience: ToolAudience::MAIN,
        workflow,
    };
    let mut tools = registry.definitions_active(
        vars,
        &ctx,
        model.supports_tool_examples(),
        &ActiveTools::default(),
    );

    if let Some(handle) = mcp_handle {
        handle.extend_tools(&mut tools);
    }

    (tools, filter)
}

#[must_use]
pub fn spawn(params: HeadlessParams) -> HeadlessHandle {
    let working_dir = params.initial_wd.to_string_lossy().into_owned();
    let mode = AgentMode::Build;
    let AgentSetup {
        vars,
        instructions,
        tools,
        tool_filter,
    } = setup(
        &params.model,
        &params.config,
        &params.excluded_tools,
        params.mcp_handle.as_ref(),
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

    let session_id = N00nId::generate();
    let session_ref = SessionRef::from(session_id);
    let session_ref_clone = session_ref.clone();
    let session_cwd = working_dir.clone();
    let fast = params.fast;
    let workflow = params.workflow;
    let ctx = HeadlessAgentContext {
        raw_tx,
        params,
        session_ref: session_ref_clone,
        session_cwd,
        system,
        tools,
        tool_filter,
        loaded_instructions: instructions.loaded,
        mode,
        fast,
        workflow,
    };
    let task = smol::spawn(run_headless_agent(ctx));

    HeadlessHandle {
        event_rx,
        tool_names,
        session_id: session_ref,
        cwd: working_dir,
        task,
    }
}

async fn run_headless_agent(ctx: HeadlessAgentContext) {
    let HeadlessAgentContext {
        raw_tx,
        params,
        session_ref,
        session_cwd,
        system,
        tools,
        tool_filter,
        loaded_instructions,
        mode,
        fast,
        workflow,
    } = ctx;

    let mcp_shutdown = params.mcp_handle.clone();
    let working_dir_path = params.initial_wd.clone();
    let event_tx = EventSender::new(raw_tx, 0);
    let mut model = params.model;
    let provider: Arc<dyn Provider> = match provider::from_model_async_with_openai_options(
        &mut model,
        params.timeouts,
        params.openai_options,
    )
    .await
    {
        Ok(p) => Arc::from(p),
        Err(e) => {
            error!(error = %e, "provider error");
            let _ = event_tx.send(AgentEvent::Error {
                message: e.user_message(),
            });
            return;
        }
    };
    let error_tx = event_tx.clone();
    let mut history = History::new(Vec::new());
    let model_spec = model.spec();
    let mut session_store = SessionStore::open(session_ref.id(), &session_cwd, &model_spec);
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
            session_id: Some(session_ref.clone()),
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
    .with_loaded_instructions(loaded_instructions)
    .with_mcp(params.mcp_handle);

    let result = agent
        .run(AgentInput {
            message: params.prompt,
            mode,
            images: params.images,
            preamble: Vec::new(),
            thinking: n00n_providers::ThinkingConfig::default(),
            fast,
            workflow,
            prompt: None,
        })
        .await;
    drop(agent);

    if let Some(store) = &mut session_store {
        store.record_turn(history.as_slice(), model_spec);
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

pub struct InteractiveParams {
    pub model: Model,
    pub config: AgentConfig,
    pub permissions_config: PermissionsConfig,
    pub timeouts: Timeouts,
    pub openai_options: OpenAiOptions,
    pub prompt_slots: Arc<ResolvedSlots>,
    pub excluded_tools: Vec<&'static str>,
    pub mcp_handle: Option<McpHandle>,
    pub initial_wd: PathBuf,
    pub session_id: Option<SessionRef>,
    pub initial_history: Vec<Message>,
    pub yolo: bool,
    pub system_prompt_override: Option<String>,
    pub append_system_prompt: Option<String>,
    pub workflow: bool,
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
        tools,
        tool_filter,
    } = setup(
        &params.model,
        &params.config,
        &params.excluded_tools,
        params.mcp_handle.as_ref(),
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
        let id = N00nId::generate();
        (id, SessionRef::from(id))
    };

    let working_dir = params.initial_wd.to_string_lossy().into_owned();
    let permissions = Arc::new(PermissionManager::new(
        params.permissions_config.clone(),
        params.initial_wd.clone(),
    ));
    if params.yolo {
        permissions.toggle_yolo();
    }

    let answer_rx = Arc::new(Mutex::new(answer_rx));
    let file_tracker = FileReadTracker::fresh();

    let session_ref_clone = session_ref.clone();
    let permissions_clone = Arc::clone(&permissions);
    let ctx = InteractiveAgentContext {
        raw_tx,
        input_rx,
        cancel_rx,
        model_rx,
        answer_rx,
        file_tracker,
        params,
        vars,
        instructions,
        tools,
        tool_filter,
        session_id,
        working_dir,
        session_ref: session_ref_clone,
        permissions: permissions_clone,
    };
    let task = smol::spawn(run_interactive_agent(ctx));

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

async fn run_interactive_agent(mut ctx: InteractiveAgentContext) {
    let mut model = ctx.params.model.clone();
    let Some(mut provider) = initialize_provider(
        &mut model,
        ctx.params.timeouts,
        ctx.params.openai_options,
        &ctx.raw_tx,
    )
    .await
    else {
        return;
    };
    let mut store = SessionStore::open(ctx.session_id, &ctx.working_dir, &model.spec());
    let mut history = History::restored(ctx.params.initial_history.clone());
    let mut run_id: u64 = 0;
    let mcp_handle = ctx.params.mcp_handle.clone();

    run_interactive_loop(
        &mut ctx,
        mcp_handle.as_ref(),
        &mut model,
        &mut provider,
        &mut store,
        &mut history,
        &mut run_id,
    )
    .await;

    if let Some(handle) = mcp_handle {
        handle.shutdown().await;
    }
}

async fn run_interactive_loop(
    ctx: &mut InteractiveAgentContext,
    mcp_handle: Option<&McpHandle>,
    model: &mut Model,
    provider: &mut Arc<dyn Provider>,
    store: &mut Option<SessionStore>,
    history: &mut History,
    run_id: &mut u64,
) {
    let tools = &mut ctx.tools;
    let tool_filter = &mut ctx.tool_filter;

    while let Ok(input) = ctx.input_rx.recv_async().await {
        let event_tx = EventSender::new(ctx.raw_tx.clone(), *run_id);

        if let Err(e) = handle_model_change(ModelChangeContext {
            model_rx: &ctx.model_rx,
            model,
            provider,
            tools,
            tool_filter,
            vars: &ctx.vars,
            config: &ctx.params.config,
            excluded_tools: &ctx.params.excluded_tools,
            mcp_handle,
            workflow: ctx.params.workflow,
            timeouts: ctx.params.timeouts,
            openai_options: ctx.params.openai_options,
        })
        .await
        {
            error!(error = %e, "provider error");
            let _ = event_tx.send(AgentEvent::Error {
                message: e.user_message(),
            });
            *run_id += 1;
            continue;
        }

        let system = build_system_prompt(
            &ctx.vars,
            &input.mode,
            &ctx.instructions.text,
            &ctx.params.prompt_slots,
            ctx.params.system_prompt_override.as_ref(),
            ctx.params.append_system_prompt.as_ref(),
            model,
        );

        let result = run_single_turn(SingleTurnContext {
            input,
            history,
            model,
            config: &ctx.params.config,
            provider,
            permissions: &ctx.permissions,
            session_ref: &ctx.session_ref,
            timeouts: ctx.params.timeouts,
            openai_options: ctx.params.openai_options,
            file_tracker: &ctx.file_tracker,
            prompt_slots: &ctx.params.prompt_slots,
            tools,
            tool_filter,
            loaded_instructions: &ctx.instructions.loaded,
            answer_rx: &ctx.answer_rx,
            cancel_rx: &ctx.cancel_rx,
            system,
            event_tx: event_tx.clone(),
            mcp_handle: mcp_handle.cloned(),
        })
        .await;

        if let Err(ref e) = result {
            error!(error = %e, "agent error");
            let _ = event_tx.send(AgentEvent::Error {
                message: e.user_message(),
            });
        }

        if let Some(store) = store {
            store.record_turn(history.as_slice(), model.spec());
        }
        *run_id += 1;
    }
}

async fn initialize_provider(
    model: &mut Model,
    timeouts: Timeouts,
    openai_options: OpenAiOptions,
    raw_tx: &flume::Sender<Envelope>,
) -> Option<Arc<dyn Provider>> {
    match provider::from_model_async_with_openai_options(model, timeouts, openai_options).await {
        Ok(p) => Some(Arc::from(p)),
        Err(e) => {
            error!(error = %e, "provider error");
            let _ = EventSender::new(raw_tx.clone(), 0).send(AgentEvent::Error {
                message: e.user_message(),
            });
            None
        }
    }
}

async fn run_single_turn(ctx: SingleTurnContext<'_>) -> Result<(), AgentError> {
    let SingleTurnContext {
        input,
        history,
        model,
        config,
        provider,
        permissions,
        session_ref,
        timeouts,
        openai_options,
        file_tracker,
        prompt_slots,
        tools,
        tool_filter,
        loaded_instructions,
        answer_rx,
        cancel_rx,
        system,
        event_tx,
        mcp_handle,
    } = ctx;

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
            provider: Arc::clone(provider),
            model: model.clone(),
            config: config.clone(),
            tool_output_lines: ToolOutputLines::default(),
            permissions: Arc::clone(permissions),
            session_id: Some(session_ref.clone()),
            timeouts,
            openai_options,
            file_tracker: Arc::clone(file_tracker),
            prompt_slots: Arc::clone(prompt_slots),
            subagent_cancels: Arc::new(CancelMap::new()),
            registry: Arc::clone(ToolRegistry::global_arc()),
            audience: ToolAudience::MAIN,
        },
        AgentRunParams {
            history,
            system,
            event_tx,
            tools: tools.clone(),
            tool_filter: tool_filter.clone(),
        },
    )
    .with_loaded_instructions(loaded_instructions.clone())
    .with_user_response_rx(Arc::clone(answer_rx))
    .with_cancel(cancel)
    .with_mcp(mcp_handle);

    let result = agent.run(input).await;
    drop(agent);
    cancel_task.cancel().await;
    result
}

async fn handle_model_change(
    ctx: ModelChangeContext<'_>,
) -> Result<(), n00n_providers::AgentError> {
    let ModelChangeContext {
        model_rx,
        model,
        provider,
        tools,
        tool_filter,
        vars,
        config,
        excluded_tools,
        mcp_handle,
        workflow,
        timeouts,
        openai_options,
    } = ctx;

    if let Some(mut new_model) = model_rx.try_iter().last()
        && new_model.spec() != model.spec()
    {
        let p = provider::from_model_async_with_openai_options(
            &mut new_model,
            timeouts,
            openai_options,
        )
        .await?;

        *provider = Arc::from(p);
        let (new_tools, new_filter) = tool_definitions(
            vars,
            &new_model,
            config,
            excluded_tools,
            mcp_handle,
            workflow,
            ToolRegistry::global(),
        );
        *tools = new_tools;
        *tool_filter = new_filter;
        *model = new_model;
    }
    Ok(())
}

fn build_system_prompt(
    vars: &template::Vars,
    mode: &AgentMode,
    instructions_text: &str,
    prompt_slots: &Arc<ResolvedSlots>,
    system_prompt_override: Option<&String>,
    append_system_prompt: Option<&String>,
    model: &Model,
) -> String {
    let mut system = system_prompt_override.cloned().unwrap_or_else(|| {
        agent::build_system_prompt(vars, mode, instructions_text, prompt_slots, model)
    });
    if let Some(append) = append_system_prompt {
        system.push('\n');
        system.push_str(append);
    }
    system
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

    fn session_id() -> N00nId {
        SESSION_ID.parse().unwrap()
    }

    fn store_in(tmp: &TempDir) -> SessionStore {
        SessionStore::open_in(
            StateDir::from_path(tmp.path().to_path_buf()),
            session_id(),
            CWD,
            MODEL_SPEC,
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
        assert!(loaded.messages.is_empty());
    }

    #[test]
    fn record_turn_persists_messages_and_title() {
        let tmp = TempDir::new().unwrap();
        let mut store = store_in(&tmp);
        let messages = vec![Message::user("fix the login bug".into())];
        store.record_turn(&messages, MODEL_SPEC.into());

        let loaded = load(&tmp);
        assert_eq!(loaded.messages.len(), 1);
        assert_eq!(loaded.title, generate_title(&messages));
    }

    #[test]
    fn reopening_resumes_existing_session() {
        let tmp = TempDir::new().unwrap();
        let mut store = store_in(&tmp);
        store.record_turn(&[Message::user("first prompt".into())], MODEL_SPEC.into());
        drop(store);

        let mut store = store_in(&tmp);
        assert_eq!(store.session.messages.len(), 1);

        let messages = vec![
            Message::user("first prompt".into()),
            Message::user("second prompt".into()),
        ];
        store.record_turn(&messages, "other/model".into());

        let loaded = load(&tmp);
        assert_eq!(loaded.messages.len(), 2);
        assert_eq!(loaded.model, "other/model");
    }

    #[test]
    fn extract_tool_names_filters_valid_entries() {
        let tools = serde_json::json!([{"name": "read"}, {"type": "function"}, {"name": "bash"}]);
        assert_eq!(extract_tool_names(&tools), vec!["read", "bash"]);
    }
}

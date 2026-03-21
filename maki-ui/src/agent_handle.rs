use std::collections::HashMap;
use std::mem;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use arc_swap::ArcSwap;
use futures_lite::future;
use maki_agent::agent;
use maki_agent::mcp::McpManager;
use maki_agent::mcp::config::{McpServerInfo, persist_enabled};
use maki_agent::skill::Skill;
use maki_agent::template;
use maki_agent::tools::ToolCall;
use maki_agent::{
    Agent, AgentConfig, AgentEvent, AgentInput, AgentParams, AgentRunParams, CancelToken,
    CancelTrigger, Envelope, EventSender, ExtractedCommand, History, ToolOutput,
};
use maki_providers::provider::Provider;
use maki_providers::{AgentError, Message, Model, TokenUsage};
use tracing::{error, info, warn};

use crate::app::App;

pub(crate) enum AgentCommand {
    Run(AgentInput, u64),
    Compact(u64),
    Cancel,
    ToggleMcp(String, bool),
}

#[derive(Clone, Default)]
pub(crate) struct McpState {
    pub(crate) disabled: Vec<String>,
    pub(crate) infos: Arc<ArcSwap<Vec<McpServerInfo>>>,
    pub(crate) pids: Arc<Mutex<Vec<u32>>>,
}

pub(crate) struct AgentHandles {
    pub(crate) cmd_tx: flume::Sender<AgentCommand>,
    pub(crate) agent_rx: flume::Receiver<Envelope>,
    pub(crate) answer_tx: flume::Sender<String>,
    pub(crate) history: Arc<Mutex<Vec<Message>>>,
    pub(crate) tool_outputs: Arc<Mutex<HashMap<String, ToolOutput>>>,
    pub(crate) mcp: McpState,
    task: smol::Task<()>,
}

impl AgentHandles {
    pub(crate) fn apply_to_app(&self, app: &mut App) {
        app.answer_tx = Some(self.answer_tx.clone());
        app.cmd_tx = Some(self.cmd_tx.clone());
        app.shared_history = Some(Arc::clone(&self.history));
        app.shared_tool_outputs = Some(Arc::clone(&self.tool_outputs));
    }

    pub(crate) fn cancel(self) {
        let _ = self.cmd_tx.try_send(AgentCommand::Cancel);
    }

    pub(crate) fn respawn(
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

    pub(crate) fn shutdown(self, timeout: Duration) {
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

pub(crate) fn toggle_disabled(disabled: &mut Vec<String>, name: &str, enabled: bool) {
    if enabled {
        disabled.retain(|s| s != name);
    } else if !disabled.contains(&name.to_owned()) {
        disabled.push(name.to_owned());
    }
}

pub(crate) fn spawn_agent(
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

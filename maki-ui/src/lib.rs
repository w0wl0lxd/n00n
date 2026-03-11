pub mod animation;
pub mod app;
pub mod chat;
mod components;
mod highlight;
mod markdown;
#[cfg(feature = "demo")]
mod mock;
mod render_worker;
mod selection;
mod text_buffer;
mod theme;

use std::collections::HashMap;
use std::io::stdout;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use color_eyre::Result;
use color_eyre::eyre::Context;
use crossterm::ExecutableCommand;
use crossterm::event::{
    self, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture, Event, MouseButton,
    MouseEvent as CtMouseEvent, MouseEventKind,
};
use crossterm::terminal::{self, EnterAlternateScreen, LeaveAlternateScreen};
use maki_agent::ToolOutput;
use maki_agent::agent;
use maki_agent::skill::Skill;
use maki_agent::template;
use maki_agent::{
    Agent, AgentEvent, AgentInput, CancelToken, CancelTrigger, Envelope, EventSender,
    ExtractedCommand, History,
};
use maki_providers::AgentError;
use maki_providers::Message;
use maki_providers::Model;
use maki_providers::TokenUsage;
use maki_providers::provider::Provider;
use maki_storage::DataDir;
use tracing::error;

pub type AppSession = maki_storage::sessions::Session<Message, TokenUsage, ToolOutput>;

use app::{App, Msg};
use chat::history_to_display;
use components::Action;

const MOUSE_SCROLL_LINES: i32 = 3;

const ANIMATION_INTERVAL_MS: u64 = 8;
const EVENT_POLL_INTERVAL_MS: u64 = 8;

pub fn run(
    model: Model,
    skills: Vec<Skill>,
    session: AppSession,
    storage: DataDir,
    #[cfg(feature = "demo")] demo: bool,
) -> Result<String> {
    let mut terminal = ratatui::init();
    stdout().execute(EnterAlternateScreen)?;
    stdout().execute(EnableBracketedPaste)?;
    stdout().execute(EnableMouseCapture)?;
    terminal::enable_raw_mode()?;

    let session_id = run_event_loop(
        &mut terminal,
        model,
        skills,
        session,
        storage,
        #[cfg(feature = "demo")]
        demo,
    );

    terminal::disable_raw_mode()?;
    stdout().execute(DisableMouseCapture)?;
    stdout().execute(event::DisableBracketedPaste)?;
    stdout().execute(LeaveAlternateScreen)?;
    ratatui::restore();

    session_id
}

fn run_event_loop(
    terminal: &mut ratatui::DefaultTerminal,
    model: Model,
    skills: Vec<Skill>,
    session: AppSession,
    storage: DataDir,
    #[cfg(feature = "demo")] demo: bool,
) -> Result<String> {
    let resumed = !session.messages.is_empty();
    let initial_history = session.messages.clone();
    let mut app = App::new(
        model.spec(),
        model.pricing.clone(),
        model.context_window,
        session,
        storage,
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
                    app.update(Msg::Agent(Box::new(envelope)));
                }
            }
        }
        app.flush_all_chats();
        if let Some(idx) = app.chat_index_for(mock::question_tool_id()) {
            app.set_demo_questions(idx, mock::mock_questions());
        }
        app.status = components::Status::Idle;
    }
    let provider: Arc<dyn Provider> =
        Arc::from(maki_providers::provider::from_model(&model).context("create provider")?);
    let skills: Arc<[Skill]> = Arc::from(skills);
    let mut handles = spawn_agent(&provider, &model, initial_history, &skills);
    handles.apply_to_app(&mut app);
    if resumed {
        app.token_usage = app.session.token_usage;
        *handles.tool_outputs.lock().unwrap() = app.session.tool_outputs.clone();
        let display_msgs = history_to_display(&app.session.messages, &app.session.tool_outputs);
        app.main_chat().load_messages(display_msgs);
    }

    loop {
        app.tick_edge_scroll();
        terminal.draw(|f| app.view(f))?;

        let mut had_agent_msg = false;
        while let Ok(envelope) = handles.agent_rx.try_recv() {
            had_agent_msg = true;
            dispatch(
                app.update(Msg::Agent(Box::new(envelope))),
                &mut handles,
                &provider,
                &model,
                &skills,
                &mut app,
            );
        }

        if app.should_quit {
            return Ok(app.session.id.clone());
        }

        let poll_duration = if had_agent_msg {
            Duration::ZERO
        } else if app.is_animating() {
            Duration::from_millis(ANIMATION_INTERVAL_MS)
        } else {
            Duration::from_millis(EVENT_POLL_INTERVAL_MS)
        };

        if event::poll(poll_duration)? {
            let msg = match event::read()? {
                Event::Key(key) => Msg::Key(key),
                Event::Paste(text) => Msg::Paste(text),
                Event::Mouse(mouse) => match mouse.kind {
                    MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => {
                        let (scroll, extra) =
                            aggregate_scroll(mouse.column, mouse.row, scroll_delta(mouse.kind));
                        if let Some(extra) = extra {
                            dispatch(
                                app.update(scroll),
                                &mut handles,
                                &provider,
                                &model,
                                &skills,
                                &mut app,
                            );
                            extra
                        } else {
                            scroll
                        }
                    }
                    MouseEventKind::Drag(MouseButton::Left) => {
                        let (drag, extra) = coalesce_drag(mouse);
                        dispatch(
                            app.update(Msg::Mouse(drag)),
                            &mut handles,
                            &provider,
                            &model,
                            &skills,
                            &mut app,
                        );
                        if let Some(extra) = extra {
                            extra
                        } else {
                            continue;
                        }
                    }
                    _ => Msg::Mouse(mouse),
                },
                _ => continue,
            };
            dispatch(
                app.update(msg),
                &mut handles,
                &provider,
                &model,
                &skills,
                &mut app,
            );
        }
    }
}

pub(crate) enum AgentCommand {
    Run(AgentInput, u64),
    Compact(u64),
    Cancel,
}

struct AgentHandles {
    cmd_tx: flume::Sender<AgentCommand>,
    agent_rx: flume::Receiver<Envelope>,
    answer_tx: flume::Sender<String>,
    history: Arc<Mutex<Vec<Message>>>,
    tool_outputs: Arc<Mutex<HashMap<String, maki_agent::ToolOutput>>>,
}

impl AgentHandles {
    fn apply_to_app(&self, app: &mut App) {
        app.answer_tx = Some(self.answer_tx.clone());
        app.cmd_tx = Some(self.cmd_tx.clone());
        app.shared_history = Some(Arc::clone(&self.history));
        app.shared_tool_outputs = Some(Arc::clone(&self.tool_outputs));
    }
}

fn spawn_agent(
    provider: &Arc<dyn Provider>,
    model: &Model,
    initial_history: Vec<Message>,
    skills: &Arc<[Skill]>,
) -> AgentHandles {
    let (agent_tx, agent_rx) = flume::unbounded::<Envelope>();
    let (cmd_tx, cmd_rx) = flume::unbounded::<AgentCommand>();
    let (answer_tx, answer_rx) = flume::unbounded::<String>();
    let (ecmd_tx, ecmd_rx) = flume::unbounded::<ExtractedCommand>();
    let shared_history: Arc<Mutex<Vec<Message>>> = Arc::new(Mutex::new(initial_history.clone()));
    let shared_history_inner = Arc::clone(&shared_history);
    let shared_tool_outputs: Arc<Mutex<HashMap<String, maki_agent::ToolOutput>>> =
        Arc::new(Mutex::new(HashMap::new()));
    let model = model.clone();
    let provider = Arc::clone(provider);
    let skills = Arc::clone(skills);

    smol::spawn(async move {
        let answer_mutex = Arc::new(async_lock::Mutex::new(answer_rx));
        let vars = template::env_vars();
        let cwd_owned = vars.apply("{cwd}").into_owned();
        let (instructions, loaded_instructions) =
            smol::unblock(move || agent::load_instruction_files(&cwd_owned)).await;
        let (tool_names, tools) = maki_agent::tools::ToolCall::definitions(
            &vars,
            &skills,
            model.family.supports_tool_examples(),
        );

        let cancel_trigger: Arc<Mutex<Option<CancelTrigger>>> = Arc::new(Mutex::new(None));
        let cancel_trigger_fwd = Arc::clone(&cancel_trigger);

        smol::spawn(async move {
            while let Ok(cmd) = cmd_rx.recv_async().await {
                let extracted = match cmd {
                    AgentCommand::Run(input, run_id) => ExtractedCommand::Interrupt(input, run_id),
                    AgentCommand::Cancel => {
                        if let Some(trigger) = cancel_trigger_fwd.lock().unwrap().take() {
                            trigger.cancel();
                        }
                        ExtractedCommand::Cancel
                    }
                    AgentCommand::Compact(run_id) => ExtractedCommand::Compact(run_id),
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

        while let Ok(cmd) = ecmd_rx.recv_async().await {
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
                    *shared_history_inner.lock().unwrap() = history.as_slice().to_vec();
                    r
                }
                ExtractedCommand::Cancel | ExtractedCommand::Ignore => unreachable!(),
                ExtractedCommand::Interrupt(input, _) => {
                    let system =
                        agent::build_system_prompt(&vars, &input.mode, &instructions, &tool_names);
                    let (trigger, cancel) = CancelToken::new();
                    *cancel_trigger.lock().unwrap() = Some(trigger);
                    let agent = Agent::new(
                        Arc::clone(&provider),
                        model.clone(),
                        std::mem::replace(&mut history, History::new(Vec::new())),
                        system,
                        event_tx,
                        tools.clone(),
                        Arc::clone(&skills),
                    )
                    .with_loaded_instructions(loaded_instructions.clone())
                    .with_user_response_rx(Arc::clone(&answer_mutex))
                    .with_cmd_rx(ecmd_rx)
                    .with_cancel(cancel);
                    let outcome = agent.run(input).await;
                    *cancel_trigger.lock().unwrap() = None;
                    history = outcome.history;
                    *shared_history_inner.lock().unwrap() = history.as_slice().to_vec();
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
                        message: e.to_string(),
                    });
                }
            }
        }
    })
    .detach();

    AgentHandles {
        cmd_tx,
        agent_rx,
        answer_tx,
        history: shared_history,
        tool_outputs: shared_tool_outputs,
    }
}

fn dispatch(
    actions: Vec<Action>,
    handles: &mut AgentHandles,
    provider: &Arc<dyn Provider>,
    model: &Model,
    skills: &Arc<[Skill]>,
    app: &mut App,
) {
    for action in actions {
        match action {
            Action::SendMessage(input) => {
                let cmd = AgentCommand::Run(input, app.run_id);
                if handles.cmd_tx.try_send(cmd).is_err() {
                    *handles = spawn_agent(provider, model, Vec::new(), skills);
                    handles.apply_to_app(app);
                }
            }
            Action::CancelAgent => {
                let _ = handles.cmd_tx.try_send(AgentCommand::Cancel);
            }
            Action::NewSession => {
                *handles = spawn_agent(provider, model, Vec::new(), skills);
                handles.apply_to_app(app);
            }
            Action::Compact => {
                let _ = handles.cmd_tx.try_send(AgentCommand::Compact(app.run_id));
            }
            Action::Quit => {}
        }
    }
}

fn scroll_delta(kind: MouseEventKind) -> i32 {
    if kind == MouseEventKind::ScrollUp {
        MOUSE_SCROLL_LINES
    } else {
        -MOUSE_SCROLL_LINES
    }
}

fn aggregate_scroll(column: u16, row: u16, mut delta: i32) -> (Msg, Option<Msg>) {
    while event::poll(Duration::ZERO).unwrap_or(false) {
        if let Ok(Event::Mouse(next)) = event::read() {
            match next.kind {
                MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => {
                    delta += scroll_delta(next.kind);
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

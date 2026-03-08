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

use std::io::stdout;
use std::sync::Arc;
use std::time::Duration;

use color_eyre::Result;
use color_eyre::eyre::Context;
use crossterm::ExecutableCommand;
use crossterm::event::{
    self, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture, Event, MouseButton,
    MouseEvent as CtMouseEvent, MouseEventKind,
};
use crossterm::terminal::{self, EnterAlternateScreen, LeaveAlternateScreen};
use maki_agent::agent;
use maki_agent::skill::Skill;
use maki_agent::template;
use maki_agent::{Agent, AgentEvent, AgentInput, Envelope, EventSender, ExtractedCommand, History};
use maki_providers::AgentError;
use maki_providers::Message;
use maki_providers::Model;
use maki_providers::TokenUsage;
use maki_providers::provider::Provider;
use tracing::error;

use app::{App, Msg};
use components::Action;

const MOUSE_SCROLL_LINES: i32 = 3;

const ANIMATION_INTERVAL_MS: u64 = 8;
const EVENT_POLL_INTERVAL_MS: u64 = 8;

pub fn run(model: Model, skills: Vec<Skill>, #[cfg(feature = "demo")] demo: bool) -> Result<()> {
    let mut terminal = ratatui::init();
    stdout().execute(EnterAlternateScreen)?;
    stdout().execute(EnableBracketedPaste)?;
    stdout().execute(EnableMouseCapture)?;
    terminal::enable_raw_mode()?;

    let result = run_event_loop(
        &mut terminal,
        model,
        skills,
        #[cfg(feature = "demo")]
        demo,
    );

    terminal::disable_raw_mode()?;
    stdout().execute(DisableMouseCapture)?;
    stdout().execute(event::DisableBracketedPaste)?;
    stdout().execute(LeaveAlternateScreen)?;
    ratatui::restore();

    result
}

fn run_event_loop(
    terminal: &mut ratatui::DefaultTerminal,
    model: Model,
    skills: Vec<Skill>,
    #[cfg(feature = "demo")] demo: bool,
) -> Result<()> {
    let mut app = App::new(model.spec(), model.pricing.clone(), model.context_window);
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
    let mut handles = spawn_agent(&provider, &model, Vec::new(), &skills);
    handles.apply_to_app(&mut app);

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
            break;
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

    Ok(())
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
}

impl AgentHandles {
    fn apply_to_app(&self, app: &mut App) {
        app.answer_tx = Some(self.answer_tx.clone());
        app.cmd_tx = Some(self.cmd_tx.clone());
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
    let model = model.clone();
    let provider = Arc::clone(provider);
    let skills = Arc::clone(skills);

    smol::spawn(async move {
        let answer_mutex = Arc::new(async_lock::Mutex::new(answer_rx));
        let vars = template::env_vars();
        let (instructions, loaded_instructions) =
            agent::load_instruction_files(&vars.apply("{cwd}"));
        let (tool_names, tools) = maki_agent::tools::ToolCall::definitions(
            &vars,
            &skills,
            model.family.supports_tool_examples(),
        );

        smol::spawn(async move {
            while let Ok(cmd) = cmd_rx.recv_async().await {
                let extracted = match cmd {
                    AgentCommand::Run(input, run_id) => ExtractedCommand::Interrupt(input, run_id),
                    AgentCommand::Cancel => ExtractedCommand::Cancel,
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

        while let Ok(cmd) = ecmd_rx.recv_async().await {
            let (event_tx, current_run_id) = match &cmd {
                ExtractedCommand::Interrupt(_, run_id) | ExtractedCommand::Compact(run_id) => {
                    (EventSender::new(agent_tx.clone(), *run_id), *run_id)
                }
                ExtractedCommand::Cancel | ExtractedCommand::Ignore => continue,
            };
            let result = match cmd {
                ExtractedCommand::Compact(_) => {
                    agent::compact(&*provider, &model, &mut history, &event_tx).await
                }
                ExtractedCommand::Cancel | ExtractedCommand::Ignore => unreachable!(),
                ExtractedCommand::Interrupt(input, _) => {
                    let system =
                        agent::build_system_prompt(&vars, &input.mode, &instructions, &tool_names);
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
                    .with_cmd_rx(ecmd_rx);
                    let outcome = agent.run(input).await;
                    history = outcome.history;
                    ecmd_rx = outcome.cmd_rx.expect("cmd_rx was set");
                    if matches!(outcome.result, Err(AgentError::Cancelled)) {
                        while ecmd_rx.try_recv().is_ok() {}
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

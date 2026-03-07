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
use std::sync::mpsc;
use std::thread;
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
use maki_agent::{Agent, AgentEvent, AgentInput, Envelope, ExtractedCommand, History};
use maki_providers::AgentError;
use maki_providers::Message;
use maki_providers::Model;
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
    Run(AgentInput),
    Compact,
    Cancel,
}

struct AgentHandles {
    cmd_tx: mpsc::Sender<AgentCommand>,
    agent_rx: mpsc::Receiver<Envelope>,
    answer_tx: mpsc::Sender<String>,
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
    let (agent_tx, agent_rx) = mpsc::channel::<Envelope>();
    let (cmd_tx, cmd_rx) = mpsc::channel::<AgentCommand>();
    let (answer_tx, answer_rx) = mpsc::channel::<String>();
    let (ecmd_tx, ecmd_rx) = mpsc::channel::<ExtractedCommand>();
    let model = model.clone();
    let provider = Arc::clone(provider);
    let skills = Arc::clone(skills);

    thread::spawn(move || {
        let answer_mutex = std::sync::Mutex::new(answer_rx);
        let mut history = History::new(initial_history);
        let vars = template::env_vars();
        let instructions = agent::load_instruction_files(&vars.apply("{cwd}"));
        let tools = maki_agent::tools::ToolCall::definitions(
            &vars,
            &skills,
            model.family().supports_tool_examples(),
        );

        thread::spawn(move || {
            for cmd in cmd_rx {
                let extracted = match cmd {
                    AgentCommand::Run(input) => ExtractedCommand::Interrupt(input),
                    AgentCommand::Cancel => ExtractedCommand::Cancel,
                    AgentCommand::Compact => ExtractedCommand::Compact,
                };
                if ecmd_tx.send(extracted).is_err() {
                    break;
                }
            }
        });

        while let Ok(cmd) = ecmd_rx.recv() {
            let result = match cmd {
                ExtractedCommand::Compact => {
                    agent::compact(&*provider, &model, &mut history, &agent_tx)
                }
                ExtractedCommand::Cancel | ExtractedCommand::Ignore => continue,
                ExtractedCommand::Interrupt(input) => {
                    let system =
                        agent::build_system_prompt(&vars, &input.mode, &model, &instructions);
                    let mut agent = Agent::new(
                        &*provider,
                        &model,
                        &mut history,
                        &system,
                        &agent_tx,
                        &tools,
                        &skills,
                    )
                    .with_user_response_rx(&answer_mutex)
                    .with_cmd_rx(&ecmd_rx);
                    let result = agent.run(input);
                    if matches!(result, Err(AgentError::Cancelled)) {
                        while ecmd_rx.try_recv().is_ok() {}
                    }
                    result
                }
            };
            match result {
                Ok(()) => {}
                Err(AgentError::Cancelled) => {
                    let _ = agent_tx.send(AgentEvent::Cancelled.into());
                }
                Err(e) => {
                    error!(error = %e, "agent error");
                    let _ = agent_tx.send(
                        AgentEvent::Error {
                            message: e.to_string(),
                        }
                        .into(),
                    );
                }
            }
        }
    });

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
                let cmd = AgentCommand::Run(input);
                if handles.cmd_tx.send(cmd).is_err() {
                    *handles = spawn_agent(provider, model, Vec::new(), skills);
                    handles.apply_to_app(app);
                }
            }
            Action::CancelAgent => {
                let _ = handles.cmd_tx.send(AgentCommand::Cancel);
            }
            Action::NewSession => {
                *handles = spawn_agent(provider, model, Vec::new(), skills);
                handles.apply_to_app(app);
            }
            Action::Compact => {
                let _ = handles.cmd_tx.send(AgentCommand::Compact);
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

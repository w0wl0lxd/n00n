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
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
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
use maki_agent::template;
use maki_agent::{AgentInput, History, SharedHistory};
use maki_providers::Model;
use maki_providers::provider::Provider;
use maki_providers::{AgentEvent, Envelope, Message};
use tracing::error;

use app::{App, Msg};
use components::Action;

const MOUSE_SCROLL_LINES: i32 = 3;

const ANIMATION_INTERVAL_MS: u64 = 8;
const EVENT_POLL_INTERVAL_MS: u64 = 8;

pub fn run(model: Model, #[cfg(feature = "demo")] demo: bool) -> Result<()> {
    let mut terminal = ratatui::init();
    stdout().execute(EnterAlternateScreen)?;
    stdout().execute(EnableBracketedPaste)?;
    stdout().execute(EnableMouseCapture)?;
    terminal::enable_raw_mode()?;

    let result = run_event_loop(
        &mut terminal,
        model,
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
    #[cfg(feature = "demo")] demo: bool,
) -> Result<()> {
    let mut app = App::new(model.spec(), model.pricing.clone(), model.context_window);
    #[cfg(feature = "demo")]
    if demo {
        app.load_messages(mock::mock_messages());
        app.load_subagent(
            mock::MOCK_TASK_TOOL_ID,
            "Explore config patterns",
            mock::mock_subagent_messages(),
        );
        let question_chat_idx = app.load_subagent(
            mock::MOCK_QUESTION_TOOL_ID,
            "Project setup",
            mock::mock_question_messages(),
        );
        app.set_demo_questions(question_chat_idx, mock::mock_questions());
    }
    let provider: Arc<dyn Provider> =
        Arc::from(maki_providers::provider::from_model(&model).context("create provider")?);
    let mut handles = spawn_agent(&provider, &model, Vec::new());
    handles.apply_to_app(&mut app);

    loop {
        terminal.draw(|f| app.view(f))?;

        let mut had_agent_msg = false;
        while let Ok(envelope) = handles.agent_rx.try_recv() {
            had_agent_msg = true;
            dispatch(
                app.update(Msg::Agent(Box::new(envelope))),
                &mut handles,
                &provider,
                &model,
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
            dispatch(app.update(msg), &mut handles, &provider, &model, &mut app);
        }
    }

    Ok(())
}

enum AgentCommand {
    Run(AgentInput),
    Compact,
}

struct AgentHandles {
    cmd_tx: mpsc::Sender<AgentCommand>,
    agent_rx: mpsc::Receiver<Envelope>,
    history: SharedHistory,
    answer_tx: mpsc::Sender<String>,
    interrupt_tx: mpsc::Sender<String>,
}

impl AgentHandles {
    fn apply_to_app(&self, app: &mut App) {
        app.answer_tx = Some(self.answer_tx.clone());
        app.interrupt_tx = Some(self.interrupt_tx.clone());
    }
}

fn spawn_agent(
    provider: &Arc<dyn Provider>,
    model: &Model,
    initial_history: Vec<Message>,
) -> AgentHandles {
    let (agent_tx, agent_rx) = mpsc::channel::<Envelope>();
    let (cmd_tx, cmd_rx) = mpsc::channel::<AgentCommand>();
    let (answer_tx, answer_rx) = mpsc::channel::<String>();
    let (interrupt_tx, interrupt_rx) = mpsc::channel::<String>();
    let model = model.clone();
    let shared: SharedHistory = Arc::new(Mutex::new(initial_history.clone()));
    let shared_ref = Arc::clone(&shared);
    let provider = Arc::clone(provider);

    thread::spawn(move || {
        let answer_mutex = std::sync::Mutex::new(answer_rx);
        let mut history = History::new(initial_history, Some(shared_ref));
        while let Ok(cmd) = cmd_rx.recv() {
            let result = match cmd {
                AgentCommand::Compact => {
                    agent::compact(&*provider, &model, &mut history, &agent_tx)
                }
                AgentCommand::Run(input) => {
                    let vars = template::env_vars();
                    let system = agent::build_system_prompt(&vars, &input.mode, &model);
                    let tools = maki_agent::tools::ToolCall::definitions(&vars);
                    agent::run(
                        &*provider,
                        &model,
                        input,
                        &mut history,
                        &system,
                        &agent_tx,
                        &tools,
                        Some(&answer_mutex),
                        Some(&interrupt_rx),
                    )
                }
            };
            if let Err(e) = result {
                error!(error = %e, "agent error");
                let _ = agent_tx.send(
                    AgentEvent::Error {
                        message: e.to_string(),
                    }
                    .into(),
                );
            }
        }
    });

    AgentHandles {
        cmd_tx,
        agent_rx,
        history: shared,
        answer_tx,
        interrupt_tx,
    }
}

fn dispatch(
    actions: Vec<Action>,
    handles: &mut AgentHandles,
    provider: &Arc<dyn Provider>,
    model: &Model,
    app: &mut App,
) {
    for action in actions {
        match action {
            Action::SendMessage(input) => {
                let cmd = AgentCommand::Run(input);
                let cmd = match handles.cmd_tx.send(cmd) {
                    Ok(()) => continue,
                    Err(e) => e.0,
                };
                let history = std::mem::take(&mut *handles.history.lock().unwrap());
                *handles = spawn_agent(provider, model, history);
                handles.apply_to_app(app);
                let _ = handles.cmd_tx.send(cmd);
            }
            Action::CancelAgent => {
                let history = std::mem::take(&mut *handles.history.lock().unwrap());
                *handles = spawn_agent(provider, model, history);
                handles.apply_to_app(app);
            }
            Action::NewSession => {
                *handles = spawn_agent(provider, model, Vec::new());
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

pub mod animation;
pub mod app;
pub mod chat;
mod components;
mod highlight;
mod markdown;
#[cfg(feature = "demo")]
mod mock;
mod text_buffer;
mod theme;

use std::io::stdout;
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use color_eyre::Result;
use crossterm::ExecutableCommand;
use crossterm::event::{self, EnableBracketedPaste, Event};
use crossterm::terminal::{self, EnterAlternateScreen, LeaveAlternateScreen};
use maki_agent::AgentInput;
use maki_agent::agent;
use maki_agent::template;
use maki_providers::Model;
use maki_providers::provider::Provider;
use maki_providers::{AgentEvent, Envelope, Message};
use tracing::error;

use app::{App, Msg};
use components::Action;

const ANIMATION_INTERVAL_MS: u64 = 8;
const EVENT_POLL_INTERVAL_MS: u64 = 8;

pub fn run(
    model: Model,
    #[cfg(feature = "demo")] demo: bool,
    excluded_tools: &'static [&'static str],
) -> Result<()> {
    let mut terminal = ratatui::init();
    stdout().execute(EnterAlternateScreen)?;
    stdout().execute(EnableBracketedPaste)?;
    terminal::enable_raw_mode()?;

    let result = run_event_loop(
        &mut terminal,
        model,
        #[cfg(feature = "demo")]
        demo,
        excluded_tools,
    );

    terminal::disable_raw_mode()?;
    stdout().execute(event::DisableBracketedPaste)?;
    stdout().execute(LeaveAlternateScreen)?;
    ratatui::restore();

    result
}

fn run_event_loop(
    terminal: &mut ratatui::DefaultTerminal,
    model: Model,
    #[cfg(feature = "demo")] demo: bool,
    excluded_tools: &'static [&'static str],
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
    let provider: Arc<dyn Provider> = Arc::from(maki_providers::provider::from_model(&model)?);
    let (mut cmd_tx, mut agent_rx, mut history, answer_tx) =
        spawn_agent(&provider, &model, Vec::new(), excluded_tools);
    app.answer_tx = Some(answer_tx);

    loop {
        terminal.draw(|f| app.view(f))?;

        let mut had_agent_msg = false;
        while let Ok(envelope) = agent_rx.try_recv() {
            had_agent_msg = true;
            dispatch(
                app.update(Msg::Agent(envelope)),
                &mut cmd_tx,
                &mut agent_rx,
                &mut history,
                &provider,
                &model,
                excluded_tools,
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
                _ => continue,
            };
            dispatch(
                app.update(msg),
                &mut cmd_tx,
                &mut agent_rx,
                &mut history,
                &provider,
                &model,
                excluded_tools,
                &mut app,
            );
        }
    }

    Ok(())
}

type SharedHistory = Arc<Mutex<Vec<Message>>>;

enum AgentCommand {
    Run(AgentInput),
    Compact,
}

fn spawn_agent(
    provider: &Arc<dyn Provider>,
    model: &Model,
    initial_history: Vec<Message>,
    excluded_tools: &'static [&'static str],
) -> (
    mpsc::Sender<AgentCommand>,
    mpsc::Receiver<Envelope>,
    SharedHistory,
    mpsc::Sender<String>,
) {
    let (agent_tx, agent_rx) = mpsc::channel::<Envelope>();
    let (cmd_tx, cmd_rx) = mpsc::channel::<AgentCommand>();
    let (answer_tx, answer_rx) = mpsc::channel::<String>();
    let model = model.clone();
    let shared_history: SharedHistory = Arc::new(Mutex::new(initial_history.clone()));
    let history_ref = Arc::clone(&shared_history);
    let provider = Arc::clone(provider);

    thread::spawn(move || {
        let answer_mutex = std::sync::Mutex::new(answer_rx);
        let mut history = initial_history;
        while let Ok(cmd) = cmd_rx.recv() {
            let result = match cmd {
                AgentCommand::Compact => {
                    agent::compact(&*provider, &model, &mut history, &agent_tx)
                }
                AgentCommand::Run(input) => {
                    let vars = template::env_vars();
                    let system = agent::build_system_prompt(&vars, &input.mode, &model);
                    let tools = maki_agent::tools::ToolCall::definitions(&vars, excluded_tools);
                    agent::run(
                        &*provider,
                        &model,
                        input,
                        &mut history,
                        &system,
                        &agent_tx,
                        &tools,
                        Some(&answer_mutex),
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
            *history_ref.lock().unwrap() = history.clone();
        }
    });

    (cmd_tx, agent_rx, shared_history, answer_tx)
}

#[allow(clippy::too_many_arguments)]
fn dispatch(
    actions: Vec<Action>,
    cmd_tx: &mut mpsc::Sender<AgentCommand>,
    agent_rx: &mut mpsc::Receiver<Envelope>,
    shared_history: &mut SharedHistory,
    provider: &Arc<dyn Provider>,
    model: &Model,
    excluded_tools: &'static [&'static str],
    app: &mut App,
) {
    for action in actions {
        match action {
            Action::SendMessage(input) => {
                let cmd = AgentCommand::Run(input);
                let cmd = match cmd_tx.send(cmd) {
                    Ok(()) => continue,
                    Err(e) => e.0,
                };
                let history = std::mem::take(&mut *shared_history.lock().unwrap());
                let answer_tx;
                (*cmd_tx, *agent_rx, *shared_history, answer_tx) =
                    spawn_agent(provider, model, history, excluded_tools);
                app.answer_tx = Some(answer_tx);
                let _ = cmd_tx.send(cmd);
            }
            Action::CancelAgent => {
                let history = std::mem::take(&mut *shared_history.lock().unwrap());
                let answer_tx;
                (*cmd_tx, *agent_rx, *shared_history, answer_tx) =
                    spawn_agent(provider, model, history, excluded_tools);
                app.answer_tx = Some(answer_tx);
            }
            Action::NewSession => {
                let answer_tx;
                (*cmd_tx, *agent_rx, *shared_history, answer_tx) =
                    spawn_agent(provider, model, Vec::new(), excluded_tools);
                app.answer_tx = Some(answer_tx);
            }
            Action::Compact => {
                let _ = cmd_tx.send(AgentCommand::Compact);
            }
            Action::Quit => {}
        }
    }
}

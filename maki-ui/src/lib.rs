pub mod animation;
pub mod app;
mod components;
mod highlight;
mod markdown;
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
use maki_providers::{AgentEvent, Envelope, Message};
use tracing::error;

use app::{App, Msg};
use components::Action;

const ANIMATION_INTERVAL_MS: u64 = 8;
const EVENT_POLL_INTERVAL_MS: u64 = 8;

pub fn run(model: Model) -> Result<()> {
    let mut terminal = ratatui::init();
    stdout().execute(EnterAlternateScreen)?;
    stdout().execute(EnableBracketedPaste)?;
    terminal::enable_raw_mode()?;

    let result = run_event_loop(&mut terminal, model);

    terminal::disable_raw_mode()?;
    stdout().execute(event::DisableBracketedPaste)?;
    stdout().execute(LeaveAlternateScreen)?;
    ratatui::restore();

    result
}

fn run_event_loop(terminal: &mut ratatui::DefaultTerminal, model: Model) -> Result<()> {
    let mut app = App::new(model.spec(), model.pricing.clone(), model.context_window);
    let (mut input_tx, mut agent_rx, mut history) = spawn_agent(&model, Vec::new());

    loop {
        terminal.draw(|f| app.view(f))?;

        let mut had_agent_msg = false;
        while let Ok(envelope) = agent_rx.try_recv() {
            had_agent_msg = true;
            dispatch(
                app.update(Msg::Agent(envelope.event)),
                &mut input_tx,
                &mut agent_rx,
                &mut history,
                &model,
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
                &mut input_tx,
                &mut agent_rx,
                &mut history,
                &model,
            );
        }
    }

    Ok(())
}

type SharedHistory = Arc<Mutex<Vec<Message>>>;

fn spawn_agent(
    model: &Model,
    initial_history: Vec<Message>,
) -> (
    mpsc::Sender<AgentInput>,
    mpsc::Receiver<Envelope>,
    SharedHistory,
) {
    let (agent_tx, agent_rx) = mpsc::channel::<Envelope>();
    let (input_tx, input_rx) = mpsc::channel::<AgentInput>();
    let model = model.clone();
    let shared_history: SharedHistory = Arc::new(Mutex::new(initial_history.clone()));
    let history_ref = Arc::clone(&shared_history);

    thread::spawn(move || {
        let provider = match maki_providers::provider::from_model(&model) {
            Ok(p) => p,
            Err(e) => {
                error!(error = %e, "provider error");
                let _ = agent_tx.send(
                    AgentEvent::Error {
                        message: e.to_string(),
                    }
                    .into(),
                );
                return;
            }
        };
        let mut history = initial_history;
        while let Ok(input) = input_rx.recv() {
            let vars = template::env_vars();
            let system = agent::build_system_prompt(&vars, &input.mode, &model);
            let tools = maki_agent::tools::ToolCall::definitions(&vars);
            if let Err(e) = agent::run(
                &*provider,
                &model,
                input,
                &mut history,
                &system,
                &agent_tx,
                &tools,
            ) {
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

    (input_tx, agent_rx, shared_history)
}

fn dispatch(
    actions: Vec<Action>,
    input_tx: &mut mpsc::Sender<AgentInput>,
    agent_rx: &mut mpsc::Receiver<Envelope>,
    shared_history: &mut SharedHistory,
    model: &Model,
) {
    for action in actions {
        match action {
            Action::SendMessage(input) => {
                let input = match input_tx.send(input) {
                    Ok(()) => continue,
                    Err(e) => e.0,
                };
                let history = std::mem::take(&mut *shared_history.lock().unwrap());
                (*input_tx, *agent_rx, *shared_history) = spawn_agent(model, history);
                let _ = input_tx.send(input);
            }
            Action::CancelAgent => {
                let history = std::mem::take(&mut *shared_history.lock().unwrap());
                (*input_tx, *agent_rx, *shared_history) = spawn_agent(model, history);
            }
            Action::Quit => {}
        }
    }
}

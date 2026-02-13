pub mod app;

use std::env;
use std::io::stdout;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use color_eyre::Result;

const EVENT_POLL_INTERVAL_MS: u64 = 16;
use crossterm::ExecutableCommand;
use crossterm::event::{self, Event};
use crossterm::terminal::{self, EnterAlternateScreen, LeaveAlternateScreen};
use maki_agent::AgentEvent;
use maki_agent::agent;
use tracing::error;

use app::{Action, App, Msg};

pub fn run() -> Result<()> {
    let mut terminal = ratatui::init();
    stdout().execute(EnterAlternateScreen)?;
    terminal::enable_raw_mode()?;

    let result = run_event_loop(&mut terminal);

    terminal::disable_raw_mode()?;
    stdout().execute(LeaveAlternateScreen)?;
    ratatui::restore();

    result
}

fn run_event_loop(terminal: &mut ratatui::DefaultTerminal) -> Result<()> {
    let mut app = App::new();
    let (agent_tx, agent_rx) = mpsc::channel::<AgentEvent>();
    let (input_tx, input_rx) = mpsc::channel::<String>();

    let cwd = env::current_dir()?.to_string_lossy().to_string();
    let system_prompt = agent::build_system_prompt(&cwd);

    spawn_agent_thread(input_rx, agent_tx, system_prompt);

    loop {
        terminal.draw(|f| app.view(f))?;

        while let Ok(event) = agent_rx.try_recv() {
            for action in app.update(Msg::Agent(event)) {
                handle_action(&action, &input_tx);
            }
        }

        if event::poll(Duration::from_millis(EVENT_POLL_INTERVAL_MS))?
            && let Event::Key(key) = event::read()?
        {
            for action in app.update(Msg::Key(key)) {
                handle_action(&action, &input_tx);
            }
        }

        if app.should_quit {
            break;
        }
    }

    Ok(())
}

fn spawn_agent_thread(
    input_rx: mpsc::Receiver<String>,
    event_tx: mpsc::Sender<AgentEvent>,
    system_prompt: String,
) {
    thread::spawn(move || {
        let mut history = Vec::new();
        while let Ok(user_msg) = input_rx.recv() {
            if let Err(e) = agent::run(user_msg, &mut history, &system_prompt, &event_tx) {
                error!(error = %e, "agent error");
                let _ = event_tx.send(AgentEvent::Error(e.to_string()));
            }
        }
    });
}

fn handle_action(action: &Action, input_tx: &mpsc::Sender<String>) {
    match action {
        Action::SendMessage(msg) => {
            let _ = input_tx.send(msg.clone());
        }
        Action::Quit => {}
    }
}

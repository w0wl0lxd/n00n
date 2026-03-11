mod print;

use clap::{Parser, Subcommand};
use color_eyre::Result;
use color_eyre::eyre::Context;
use maki_agent::skill::{self, Skill};
use maki_storage::DataDir;
use maki_ui::AppSession;
use tracing_subscriber::EnvFilter;

use maki_providers::model::Model;
use print::OutputFormat;

#[derive(Parser)]
#[command(name = "maki", version, about = "AI coding agent for the terminal")]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,

    /// Non-interactive mode. Runs the prompt and exits. Compatible with Claude Code's --print flag
    #[arg(short, long)]
    print: bool,

    /// Model as provider/model-id (e.g. anthropic/claude-sonnet-4, zai/claude-sonnet-4)
    #[arg(short, long, default_value = "anthropic/claude-opus-4-6")]
    model: String,

    /// Include full turn-by-turn messages in --print output
    #[arg(long)]
    verbose: bool,

    /// Resume the most recent session in this directory
    #[arg(short = 'c', long = "continue")]
    continue_session: bool,

    /// Resume a specific session by its ID
    #[arg(short = 's', long)]
    session: Option<String>,

    #[arg(long)]
    #[cfg(feature = "demo")]
    demo: bool,

    /// Output format for --print mode
    #[arg(long, value_enum, default_value_t = OutputFormat::Text)]
    output_format: OutputFormat,

    /// Skip loading skill files from .maki/skills, .claude/skills, etc.
    #[arg(long)]
    no_skills: bool,

    /// Initial prompt (reads stdin if omitted in --print mode)
    prompt: Option<String>,
}

fn discover(disable: bool) -> Vec<Skill> {
    if disable {
        return Vec::new();
    }
    let cwd = std::env::current_dir().unwrap_or_else(|_| ".".into());
    skill::discover_skills(&cwd)
}

#[derive(Subcommand)]
enum Command {
    /// Manage API authentication
    Auth {
        #[command(subcommand)]
        action: AuthAction,
    },
    /// List all available models
    Models,
}

#[derive(Subcommand)]
enum AuthAction {
    /// Save API keys for configured providers
    Login,
    /// Remove stored API keys
    Logout,
}

fn main() {
    color_eyre::install().ok();
    if let Err(e) = run() {
        print_error(&e);
        std::process::exit(1);
    }
}

fn print_error(e: &color_eyre::Report) {
    const RED: &str = "\x1b[31m";
    const BOLD_RED: &str = "\x1b[1;31m";
    const DIM: &str = "\x1b[2m";
    const RESET: &str = "\x1b[0m";

    eprintln!();
    eprintln!("  {BOLD_RED}✖ {e}{RESET}");
    for cause in e.chain().skip(1) {
        eprintln!("  {DIM}╰─{RESET} {RED}{cause}{RESET}");
    }
    eprintln!();
}

fn run() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Some(Command::Auth { action }) => {
            let storage = DataDir::resolve().context("resolve data directory")?;
            match action {
                AuthAction::Login => maki_providers::auth::login(&storage)?,
                AuthAction::Logout => maki_providers::auth::logout(&storage)?,
            }
        }
        Some(Command::Models) => {
            smol::block_on(maki_providers::provider::fetch_all_models(|batch| {
                for model in batch.models {
                    println!("{model}");
                }
                for warning in batch.warnings {
                    eprintln!("warning: {warning}");
                }
            }));
        }
        None => {
            let storage = DataDir::resolve().context("resolve data directory")?;
            let model = Model::from_spec(&cli.model).context("parse model spec")?;
            init_logging(&storage);
            let skills = discover(cli.no_skills);
            if cli.print {
                print::run(&model, cli.prompt, cli.output_format, cli.verbose, skills)
                    .context("run print mode")?;
            } else {
                let cwd = std::env::current_dir()
                    .unwrap_or_else(|_| ".".into())
                    .to_string_lossy()
                    .into_owned();
                let session = resolve_session(
                    cli.continue_session,
                    cli.session,
                    &model.spec(),
                    &cwd,
                    &storage,
                )?;
                let session_id = maki_ui::run(
                    model,
                    skills,
                    session,
                    storage,
                    #[cfg(feature = "demo")]
                    cli.demo,
                )
                .context("run UI")?;
                eprintln!("session: {session_id}");
            }
        }
    }
    Ok(())
}

fn resolve_session(
    continue_session: bool,
    session_id: Option<String>,
    model: &str,
    cwd: &str,
    storage: &DataDir,
) -> Result<AppSession> {
    if let Some(id) = session_id {
        return AppSession::load(&id, storage).map_err(|e| color_eyre::eyre::eyre!("{e}"));
    }
    if continue_session {
        match AppSession::latest(cwd, storage) {
            Ok(Some(session)) => return Ok(session),
            Ok(None) => {
                tracing::info!("no previous session found for this directory, starting new");
            }
            Err(e) => {
                tracing::warn!(error = %e, "failed to load latest session, starting new");
            }
        }
    }
    Ok(AppSession::new(model, cwd))
}

fn init_logging(storage: &DataDir) {
    let log_path = maki_storage::log::log_path(storage);
    let Some(log_dir) = log_path.parent() else {
        return;
    };
    let Some(log_file) = log_path.file_name() else {
        return;
    };
    let file_appender = tracing_appender::rolling::never(log_dir, log_file);
    let filter = EnvFilter::try_from_env("MAKI_LOG").unwrap_or_else(|_| EnvFilter::new("info"));
    tracing_subscriber::fmt()
        .json()
        .with_env_filter(filter)
        .with_writer(file_appender)
        .init();
}

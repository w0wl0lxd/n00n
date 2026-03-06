mod print;

use clap::{Parser, Subcommand};
use color_eyre::Result;
use color_eyre::eyre::Context;
use maki_agent::skill::{self, Skill};
use tracing_subscriber::EnvFilter;

use maki_providers::model::Model;
use print::OutputFormat;

const LOG_FILE_NAME: &str = "maki.log";

#[derive(Parser)]
#[command(name = "maki", version, about = "AI coding assistant")]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,

    #[arg(short, long)]
    print: bool,

    #[arg(short, long, default_value = "anthropic/claude-opus-4-6")]
    model: String,

    #[arg(long)]
    verbose: bool,

    #[arg(long)]
    #[cfg(feature = "demo")]
    demo: bool,

    #[arg(long, value_enum, default_value_t = OutputFormat::Text)]
    output_format: OutputFormat,

    #[arg(long)]
    disable_skills: bool,

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
    Auth {
        #[command(subcommand)]
        action: AuthAction,
    },
    Models,
}

#[derive(Subcommand)]
enum AuthAction {
    Login,
    Logout,
}

fn main() -> Result<()> {
    color_eyre::install()?;

    let cli = Cli::parse();
    match cli.command {
        Some(Command::Auth { action }) => match action {
            AuthAction::Login => maki_providers::auth::login()?,
            AuthAction::Logout => maki_providers::auth::logout()?,
        },
        Some(Command::Models) => {
            maki_providers::provider::fetch_all_models(|models| {
                for model in models {
                    println!("{model}");
                }
            });
        }
        None => {
            let model = Model::from_spec(&cli.model).context("parse model spec")?;
            init_logging();
            let skills = discover(cli.disable_skills);
            if cli.print {
                print::run(&model, cli.prompt, cli.output_format, cli.verbose, skills)
                    .context("run print mode")?;
            } else {
                maki_ui::run(
                    model,
                    skills,
                    #[cfg(feature = "demo")]
                    cli.demo,
                )
                .context("run UI")?;
            }
        }
    }
    Ok(())
}

fn init_logging() {
    let Ok(log_dir) = maki_providers::data_dir() else {
        return;
    };
    let file_appender = tracing_appender::rolling::never(&log_dir, LOG_FILE_NAME);
    let filter = EnvFilter::try_from_env("MAKI_LOG").unwrap_or_else(|_| EnvFilter::new("info"));
    tracing_subscriber::fmt()
        .json()
        .with_env_filter(filter)
        .with_writer(file_appender)
        .init();
}

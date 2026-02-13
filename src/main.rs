use std::env;
use std::fs;

use clap::{Parser, Subcommand};
use color_eyre::Result;
use tracing_subscriber::EnvFilter;

const LOG_DIR_NAME: &str = ".maki";
const LOG_FILE_NAME: &str = "maki.log";

#[derive(Parser)]
#[command(name = "maki", version, about = "AI coding assistant")]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    Auth {
        #[command(subcommand)]
        action: AuthAction,
    },
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
            AuthAction::Login => maki_agent::auth::login()?,
            AuthAction::Logout => maki_agent::auth::logout()?,
        },
        None => {
            init_logging();
            maki_ui::run()?;
        }
    }
    Ok(())
}

fn init_logging() {
    let log_dir = log_dir();
    let _ = fs::create_dir_all(&log_dir);
    let file_appender = tracing_appender::rolling::never(&log_dir, LOG_FILE_NAME);
    let filter = EnvFilter::try_from_env("MAKI_LOG").unwrap_or_else(|_| EnvFilter::new("info"));
    tracing_subscriber::fmt()
        .json()
        .with_env_filter(filter)
        .with_writer(file_appender)
        .init();
}

fn log_dir() -> String {
    let home = env::var("HOME").unwrap_or_else(|_| ".".to_string());
    format!("{home}/{LOG_DIR_NAME}")
}

mod print;

use clap::{Parser, Subcommand};
use color_eyre::Result;
use tracing_subscriber::EnvFilter;

use print::OutputFormat;

const LOG_FILE_NAME: &str = "maki.log";

#[derive(Parser)]
#[command(name = "maki", version, about = "AI coding assistant")]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,

    #[arg(short, long)]
    print: bool,

    #[arg(long)]
    verbose: bool,

    #[arg(long, value_enum, default_value_t = OutputFormat::Text)]
    output_format: OutputFormat,

    prompt: Option<String>,
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
        None if cli.print => {
            init_logging();
            print::run(cli.prompt, cli.output_format, cli.verbose)?;
        }
        None => {
            init_logging();
            maki_ui::run()?;
        }
    }
    Ok(())
}

fn init_logging() {
    let Ok(log_dir) = maki_agent::data_dir() else {
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

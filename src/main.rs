use std::env;
use std::fs;

use color_eyre::Result;
use tracing_subscriber::EnvFilter;

const LOG_DIR_NAME: &str = ".maki";
const LOG_FILE_NAME: &str = "maki.log";

fn main() -> Result<()> {
    color_eyre::install()?;
    init_logging();
    maki_ui::run()
}

fn init_logging() {
    let log_dir = log_dir();
    let _ = fs::create_dir_all(&log_dir);
    let file_appender = tracing_appender::rolling::never(&log_dir, LOG_FILE_NAME);
    let filter = EnvFilter::try_from_env("MAKI_LOG").unwrap_or_else(|_| EnvFilter::new("info"));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(file_appender)
        .with_ansi(false)
        .init();
}

fn log_dir() -> String {
    let home = env::var("HOME").unwrap_or_else(|_| ".".to_string());
    format!("{home}/{LOG_DIR_NAME}")
}

#![allow(semicolon_in_expressions_from_macros)]

mod cli;
mod cmd;
mod print;
mod sdk_mode;
mod setup;
mod update;

use clap::Parser;

use cli::Cli;

fn main() {
    color_eyre::install().ok();
    n00n_providers::ensure_rustls_crypto_provider();
    let cli = Cli::parse();
    let format = cli.output_format;
    if let Err(e) = cmd::dispatch(cli) {
        print_error(&e, format);
        std::process::exit(1);
    }
}

fn print_error(e: &color_eyre::Report, format: print::OutputFormat) {
    const RED: &str = "\x1b[31m";
    const BOLD_RED: &str = "\x1b[1;31m";
    const DIM: &str = "\x1b[2m";
    const RESET: &str = "\x1b[0m";

    if matches!(
        format,
        print::OutputFormat::Json | print::OutputFormat::StreamJson
    ) {
        eprintln!(
            "{}",
            serde_json::json!({"ok": false, "error": e.to_string()})
        );
        return;
    }

    eprintln!();
    eprintln!("{BOLD_RED}✖ {e}{RESET}");
    let causes: Vec<_> = e.chain().skip(1).collect();
    let last = causes.len().saturating_sub(1);
    for (i, cause) in causes.iter().enumerate() {
        let branch = if i == last { "└─" } else { "├─" };
        eprintln!("{DIM}{branch}{RESET} {RED}{cause}{RESET}");
    }
    eprintln!();
}

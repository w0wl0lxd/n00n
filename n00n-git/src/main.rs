use clap::{Parser, Subcommand};
use n00n_git::git;
use serde_json::json;
use std::path::PathBuf;
use tracing::Level;

#[derive(Parser)]
#[command(name = "n00n-git")]
#[command(about = "Native git operations using gix/gitoxide for n00n", long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Get the current git status of a repository
    Status {
        /// Path to the git repository
        repo: PathBuf,
    },
    /// Get commit history for a repository
    Log {
        /// Path to the git repository
        repo: PathBuf,
        /// Number of commits to return
        #[arg(short, long, default_value = "10")]
        count: usize,
    },
    /// Get diff between two references
    Diff {
        /// Path to the git repository
        repo: PathBuf,
        /// First reference (commit SHA, branch, tag)
        ref_a: String,
        /// Second reference (commit SHA, branch, tag)
        ref_b: String,
    },
    /// List branches in a repository
    Branches {
        /// Path to the git repository
        repo: PathBuf,
    },
    /// Get blame information for a file
    Blame {
        /// Path to the git repository
        repo: PathBuf,
        /// Relative path to the file within the repository
        file: String,
    },
    /// Stage files in a repository
    Add {
        /// Path to the git repository
        repo: PathBuf,
        /// File paths to stage, relative to the repository root
        files: Vec<String>,
    },
    /// Create a commit with the given message
    Commit {
        /// Path to the git repository
        repo: PathBuf,
        /// Commit message
        #[arg(short, long)]
        message: String,
    },
    /// Checkout a branch, tag, or ref
    Checkout {
        /// Path to the git repository
        repo: PathBuf,
        /// Branch, tag, or ref to checkout
        target: String,
    },
}

fn main() {
    color_eyre::install().ok();

    tracing_subscriber::fmt()
        .with_max_level(Level::ERROR)
        .init();

    let cli = Cli::parse();

    let result: Result<String, Box<dyn std::error::Error>> = match cli.command {
        Commands::Status { repo } => git::status(&repo)
            .map_err(Into::into)
            .and_then(|v| serde_json::to_string(&v).map_err(Into::into)),
        Commands::Log { repo, count } => git::log(&repo, count)
            .map_err(Into::into)
            .and_then(|v| serde_json::to_string(&v).map_err(Into::into)),
        Commands::Diff { repo, ref_a, ref_b } => git::diff(&repo, &ref_a, &ref_b)
            .map_err(Into::into)
            .and_then(|v| serde_json::to_string(&v).map_err(Into::into)),
        Commands::Branches { repo } => git::branches(&repo)
            .map_err(Into::into)
            .and_then(|v| serde_json::to_string(&v).map_err(Into::into)),
        Commands::Blame { repo, file } => git::blame(&repo, &file)
            .map_err(Into::into)
            .and_then(|v| serde_json::to_string(&v).map_err(Into::into)),
        Commands::Add { repo, files } => git::add(&repo, &files)
            .map_err(Into::into)
            .and_then(|()| serde_json::to_string(&json!({ "ok": true })).map_err(Into::into)),
        Commands::Commit { repo, message } => git::commit(&repo, &message)
            .map_err(Into::into)
            .and_then(|id| serde_json::to_string(&json!({ "commit_id": id })).map_err(Into::into)),
        Commands::Checkout { repo, target } => git::checkout(&repo, &target)
            .map_err(Into::into)
            .and_then(|()| serde_json::to_string(&json!({ "ok": true })).map_err(Into::into)),
    };

    match result {
        Ok(json) => {
            println!("{json}");
            std::process::exit(0);
        }
        Err(e) => {
            let error_json = json!({ "error": format!("{:#}", e) });
            println!("{error_json}");
            std::process::exit(1);
        }
    }
}

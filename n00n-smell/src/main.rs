use std::path::{Path, PathBuf};

use clap::{Parser, Subcommand};
use n00n_smell::{Query, SearchConfig, SmellIndex};

#[derive(Parser)]
#[command(name = "n00n-smell")]
#[command(about = "Persistent code-smell and comment index for n00n")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Build or rebuild the smell index for a repository
    Index {
        /// Path to the repository
        #[arg(allow_hyphen_values = true)]
        repo: PathBuf,
    },
    /// Search the smell index for a repository
    Search {
        /// Path to the repository
        #[arg(allow_hyphen_values = true)]
        repo: PathBuf,
        /// Query text
        #[arg(allow_hyphen_values = true)]
        query: String,
        /// Filter by kind
        #[arg(short, long)]
        kind: Option<String>,
        /// Maximum number of results
        #[arg(short, long, default_value = "5")]
        top_k: usize,
    },
}

fn resolve_repo(repo: &Path) -> Result<PathBuf, Box<dyn std::error::Error>> {
    if !repo.is_dir() {
        return Err(format!("{} is not a directory", repo.display()).into());
    }
    repo.canonicalize()
        .map_err(|err| format!("failed to resolve {}: {err}", repo.display()).into())
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    color_eyre::install().ok();

    let cli = Cli::parse();

    let output = match cli.command {
        Commands::Index { repo } => {
            let repo = resolve_repo(&repo)?;
            let index_dir = SmellIndex::index_dir(&repo);
            let mut index = SmellIndex::open_or_create(&index_dir, &SearchConfig::default())?;
            index.update(&repo, |progress| {
                eprintln!(
                    "[{}] {}/{}",
                    progress.phase, progress.processed, progress.total
                );
            })?;
            format!("indexed smells at {}", index_dir.display())
        }
        Commands::Search {
            repo,
            query,
            kind,
            top_k,
        } => {
            let repo = resolve_repo(&repo)?;
            if !SmellIndex::has_index(&repo) {
                return Err(format!(
                    "no smell index for {}; run `n00n-smell index`",
                    repo.display()
                )
                .into());
            }
            let index_dir = SmellIndex::index_dir(&repo);
            let index = SmellIndex::open_or_create(&index_dir, &SearchConfig::default())?;
            let results = index.search(&Query {
                text: query,
                kind,
                top_k,
            })?;
            n00n_smell::format_results(&results)
        }
    };

    println!("{output}");
    Ok(())
}

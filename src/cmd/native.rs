use std::path::{Path, PathBuf};

use color_eyre::Result;
use color_eyre::eyre::{Context, bail};
use n00n_git::conflicts::ConflictsOptions;
use n00n_git::git;
use n00n_smell::{Query, SearchConfig, SmellIndex};
use serde::Serialize;
use serde_json::json;

use crate::cli::{GitAction, SmellAction};

fn print_json(value: &impl Serialize) -> Result<()> {
    println!("{}", serde_json::to_string(value)?);
    Ok(())
}

pub fn git_command(action: GitAction) -> Result<()> {
    match action {
        GitAction::Status { repo } => print_json(&git::status(&repo)?),
        GitAction::Log { repo, count } => print_json(&git::log(&repo, count)?),
        GitAction::Diff { repo, ref_a, ref_b } => print_json(&git::diff(&repo, &ref_a, &ref_b)?),
        GitAction::Branches { repo } => print_json(&git::branches(&repo)?),
        GitAction::Blame { repo, file } => print_json(&git::blame(&repo, &file)?),
        GitAction::Add { repo, files } => {
            git::add(&repo, &files)?;
            print_json(&json!({ "ok": true }))
        }
        GitAction::Commit { repo, message } => {
            let commit_id = git::commit(&repo, &message)?;
            print_json(&json!({ "commit_id": commit_id }))
        }
        GitAction::Checkout { repo, target } => {
            git::checkout(&repo, &target)?;
            print_json(&json!({ "ok": true }))
        }
        GitAction::Conflicts {
            repo,
            output,
            max_hunk_lines,
            max_file_bytes,
            kinds,
            include_untracked,
            include_ignored,
        } => print_json(&n00n_git::conflicts::find(
            &repo,
            &ConflictsOptions {
                kinds,
                output,
                max_hunk_lines,
                max_file_bytes,
                include_untracked,
                include_ignored,
            },
        )?),
    }
}

fn resolve_repo(repo: &Path) -> Result<PathBuf> {
    if !repo.is_dir() {
        bail!("{} is not a directory", repo.display());
    }
    repo.canonicalize()
        .wrap_err_with(|| format!("failed to resolve {}", repo.display()))
}

pub fn smell_command(action: SmellAction) -> Result<()> {
    match action {
        SmellAction::Index { repo } => {
            let repo = resolve_repo(&repo)?;
            let index_dir = SmellIndex::index_dir(&repo);
            let mut index = SmellIndex::open_or_create(&index_dir, &SearchConfig::default())?;
            index.update(&repo, |progress| {
                eprintln!(
                    "[{}] {}/{}",
                    progress.phase, progress.processed, progress.total
                );
            })?;
            println!("indexed smells at {}", index_dir.display());
        }
        SmellAction::Search {
            repo,
            query,
            kind,
            top_k,
        } => {
            let repo = resolve_repo(&repo)?;
            if !SmellIndex::has_index(&repo) {
                bail!(
                    "no smell index for {}; run `n00n smell index`",
                    repo.display()
                );
            }
            let index_dir = SmellIndex::index_dir(&repo);
            let index = SmellIndex::open_or_create(&index_dir, &SearchConfig::default())?;
            let results = index.search(&Query {
                text: query,
                kind,
                top_k,
            })?;
            println!("{}", n00n_smell::format_results(&results));
        }
    }
    Ok(())
}

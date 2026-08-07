use std::path::{Path, PathBuf};

use gix::bstr::{BStr, ByteSlice};
use gix::object::Kind;
use gix::object::tree::diff::ChangeDetached as TreeChange;
use serde::{Deserialize, Serialize};
use similar::{ChangeTag, TextDiff};
use tracing::instrument;

use crate::error::GitError;

use gix::dir as gix_dir;
use gix::status::plumbing as gix_status;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GitStatus {
    pub branch: Option<String>,
    pub files: Vec<FileStatus>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileStatus {
    pub path: String,
    pub status: String,
    pub staged: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GitCommit {
    pub id: String,
    pub author: String,
    pub email: String,
    pub time: i64,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GitBranch {
    pub name: String,
    pub head: String,
    pub is_current: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GitDiff {
    pub files: Vec<FileDiff>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileDiff {
    pub path: String,
    pub additions: u32,
    pub deletions: u32,
    pub changes: Vec<DiffChange>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiffChange {
    pub kind: String,
    pub old_line: Option<u32>,
    pub new_line: Option<u32>,
    pub content: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GitBlame {
    pub lines: Vec<BlameLine>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BlameLine {
    pub line_number: u32,
    pub content: String,
    pub commit_id: String,
    pub author: String,
    pub time: i64,
}

/// Get the current git status of a repository.
///
/// # Errors
///
/// Returns `GitError` if the repository cannot be opened, is bare, or status operations fail.
#[instrument(skip(path))]
pub fn status(path: &Path) -> Result<GitStatus, GitError> {
    let repo = gix::open(path)
        .map_err(|e| GitError::GitOperation(format!("failed to open repository: {e}")))?;

    let branch = repo
        .head_name()
        .map_err(|e| GitError::GitOperation(format!("failed to get head name: {e}")))?
        .map(|name| name.shorten().to_string());

    repo.worktree().ok_or(GitError::BareRepo)?;

    let mut files = Vec::new();

    let platform = repo
        .status(gix::progress::Discard)
        .map_err(|e| GitError::GitOperation(format!("failed to create status platform: {e}")))?;

    let iter = platform
        .into_index_worktree_iter(Vec::new())
        .map_err(|e| GitError::GitOperation(format!("failed to create status iterator: {e}")))?;

    for item_result in iter {
        let item = item_result
            .map_err(|e| GitError::GitOperation(format!("failed to read status item: {e}")))?;

        let (entry_path, status, staged) = match item {
            gix::status::index_worktree::Item::Modification {
                rela_path, status, ..
            } => {
                let path = rela_path.to_string();
                let (status_str, is_staged) = match status {
                    gix_status::index_as_worktree::EntryStatus::Conflict { .. } => {
                        ("conflict", true)
                    }
                    gix_status::index_as_worktree::EntryStatus::Change(change) => match change {
                        gix_status::index_as_worktree::Change::Removed => ("deleted", true),
                        gix_status::index_as_worktree::Change::Type { .. }
                        | gix_status::index_as_worktree::Change::Modification { .. }
                        | gix_status::index_as_worktree::Change::SubmoduleModification(_) => {
                            ("modified", true)
                        }
                    },
                    gix_status::index_as_worktree::EntryStatus::NeedsUpdate(_) => continue,
                    gix_status::index_as_worktree::EntryStatus::IntentToAdd => ("added", true),
                };
                (path, status_str, is_staged)
            }
            gix::status::index_worktree::Item::DirectoryContents { entry, .. } => {
                let path = entry.rela_path.to_string();
                let (status_str, is_staged) = match entry.status {
                    gix_dir::entry::Status::Untracked => ("untracked", false),
                    gix_dir::entry::Status::Ignored(_) => ("ignored", false),
                    _ => continue,
                };
                (path, status_str, is_staged)
            }
            gix::status::index_worktree::Item::Rewrite {
                dirwalk_entry,
                copy,
                ..
            } => {
                let path = dirwalk_entry.rela_path.to_string();
                let status_str = if copy { "added" } else { "renamed" };
                (path, status_str, true)
            }
        };

        files.push(FileStatus {
            path: entry_path,
            status: status.to_string(),
            staged,
        });
    }

    Ok(GitStatus { branch, files })
}

/// Get commit history for a repository.
///
/// # Errors
///
/// Returns `GitError` if the repository cannot be opened or commit operations fail.
#[instrument(skip(path))]
pub fn log(path: &Path, count: usize) -> Result<Vec<GitCommit>, GitError> {
    let repo = gix::open(path)
        .map_err(|e| GitError::GitOperation(format!("failed to open repository: {e}")))?;

    let head_commit = repo
        .head_commit()
        .map_err(|e| GitError::GitOperation(format!("failed to get head commit: {e}")))?;

    let mut commits = Vec::new();
    let mut current = Some(head_commit);

    while let Some(commit) = current {
        let decoded = commit
            .decode()
            .map_err(|e| GitError::GitOperation(format!("failed to decode commit: {e}")))?;

        let author = decoded
            .author()
            .map_err(|e| GitError::GitOperation(format!("failed to decode author: {e}")))?;
        let time = author
            .time()
            .map_err(|e| GitError::GitOperation(format!("failed to decode author time: {e}")))?;
        commits.push(GitCommit {
            id: commit.id.to_hex().to_string(),
            author: author.name.to_string(),
            email: author.email.to_string(),
            time: time.seconds,
            message: decoded.message.to_string(),
        });

        if commits.len() >= count {
            break;
        }

        let parent_ids: Vec<_> = commit.parent_ids().collect();
        if let Some(parent_id) = parent_ids.first() {
            current = repo
                .find_object(*parent_id)
                .ok()
                .and_then(|obj| obj.try_into_commit().ok());
        } else {
            break;
        }
    }

    Ok(commits)
}

/// Get diff between two references.
///
/// # Errors
///
/// Returns `GitError` if the repository cannot be opened, references are invalid, or diff operations fail.
#[instrument(skip(path))]
pub fn diff(path: &Path, ref_a: &str, ref_b: &str) -> Result<GitDiff, GitError> {
    let repo = gix::open(path)
        .map_err(|e| GitError::GitOperation(format!("failed to open repository: {e}")))?;

    let spec_a = BStr::new(ref_a);
    let spec_b = BStr::new(ref_b);

    let id_a = repo
        .rev_parse_single(spec_a)
        .map_err(|e| GitError::InvalidReference(format!("failed to parse ref_a: {e}")))?
        .object()
        .map_err(|e| GitError::GitOperation(format!("failed to resolve ref_a to object: {e}")))?;

    let id_b = repo
        .rev_parse_single(spec_b)
        .map_err(|e| GitError::InvalidReference(format!("failed to parse ref_b: {e}")))?
        .object()
        .map_err(|e| GitError::GitOperation(format!("failed to resolve ref_b to object: {e}")))?;

    let tree_a = id_a
        .peel_to_tree()
        .map_err(|e| GitError::GitOperation(format!("failed to peel ref_a to tree: {e}")))?;

    let tree_b = id_b
        .peel_to_tree()
        .map_err(|e| GitError::GitOperation(format!("failed to peel ref_b to tree: {e}")))?;

    let changes = repo
        .diff_tree_to_tree(Some(&tree_a), Some(&tree_b), None)
        .map_err(|e| GitError::GitOperation(format!("failed to diff trees: {e}")))?;

    let mut files = Vec::new();

    for change in changes {
        let file_path = change.location().to_string();

        let file_diff = match change {
            TreeChange::Addition {
                entry_mode,
                id,
                relation: _,
                location: _,
            } if is_blob(entry_mode) => {
                let blob = repo
                    .find_object(id)
                    .map_err(|e| GitError::GitOperation(format!("failed to find blob: {e}")))?;
                let blob_data = blob
                    .peel_to_kind(Kind::Blob)
                    .map_err(|e| GitError::GitOperation(format!("failed to peel to blob: {e}")))?;
                let content = String::from_utf8_lossy(&blob_data.data).to_string();
                let lines: Vec<&str> = content.lines().collect();

                let additions = u32::try_from(lines.len()).map_err(|_| {
                    GitError::GitOperation("addition line count exceeds u32".to_string())
                })?;
                let changes: Vec<DiffChange> = lines
                    .iter()
                    .enumerate()
                    .map(|(idx, line)| {
                        let new_line = u32::try_from(idx + 1).map_err(|_| {
                            GitError::GitOperation("line number exceeds u32".to_string())
                        })?;
                        Ok(DiffChange {
                            kind: "added".to_string(),
                            old_line: None,
                            new_line: Some(new_line),
                            content: line.to_string(),
                        })
                    })
                    .collect::<Result<Vec<_>, GitError>>()?;

                FileDiff {
                    path: file_path,
                    additions,
                    deletions: 0,
                    changes,
                }
            }
            TreeChange::Deletion {
                entry_mode,
                id,
                relation: _,
                location: _,
            } if is_blob(entry_mode) => {
                let blob = repo
                    .find_object(id)
                    .map_err(|e| GitError::GitOperation(format!("failed to find blob: {e}")))?;
                let blob_data = blob
                    .peel_to_kind(Kind::Blob)
                    .map_err(|e| GitError::GitOperation(format!("failed to peel to blob: {e}")))?;
                let content = String::from_utf8_lossy(&blob_data.data).to_string();
                let lines: Vec<&str> = content.lines().collect();

                let deletions = u32::try_from(lines.len()).map_err(|_| {
                    GitError::GitOperation("deletion line count exceeds u32".to_string())
                })?;
                let changes: Vec<DiffChange> = lines
                    .iter()
                    .enumerate()
                    .map(|(idx, line)| {
                        let old_line = u32::try_from(idx + 1).map_err(|_| {
                            GitError::GitOperation("line number exceeds u32".to_string())
                        })?;
                        Ok(DiffChange {
                            kind: "deleted".to_string(),
                            old_line: Some(old_line),
                            new_line: None,
                            content: line.to_string(),
                        })
                    })
                    .collect::<Result<Vec<_>, GitError>>()?;

                FileDiff {
                    path: file_path,
                    additions: 0,
                    deletions,
                    changes,
                }
            }
            TreeChange::Modification {
                previous_entry_mode,
                previous_id,
                entry_mode,
                id,
                location: _,
            } if is_blob(previous_entry_mode) && is_blob(entry_mode) => {
                diff_blobs(&repo, file_path, previous_id, id)?
            }
            TreeChange::Rewrite {
                source_id,
                id,
                entry_mode,
                ..
            } if is_blob(entry_mode) => diff_blobs(&repo, file_path, source_id, id)?,
            _ => FileDiff {
                path: file_path,
                additions: 0,
                deletions: 0,
                changes: Vec::new(),
            },
        };

        files.push(file_diff);
    }

    files.retain(|f| !(f.additions == 0 && f.deletions == 0 && f.changes.is_empty()));

    Ok(GitDiff { files })
}

fn is_blob(mode: gix::object::tree::EntryMode) -> bool {
    mode.is_blob()
}

fn diff_blobs(
    repo: &gix::Repository,
    path: String,
    old_id: gix::ObjectId,
    new_id: gix::ObjectId,
) -> Result<FileDiff, GitError> {
    let new_blob = repo
        .find_object(new_id)
        .map_err(|e| GitError::GitOperation(format!("failed to find blob: {e}")))?;
    let new_blob_data = new_blob
        .peel_to_kind(Kind::Blob)
        .map_err(|e| GitError::GitOperation(format!("failed to peel to blob: {e}")))?;
    let new_content = String::from_utf8_lossy(&new_blob_data.data).to_string();

    let old_blob = repo
        .find_object(old_id)
        .map_err(|e| GitError::GitOperation(format!("failed to find old blob: {e}")))?;
    let old_blob_data = old_blob
        .peel_to_kind(Kind::Blob)
        .map_err(|e| GitError::GitOperation(format!("failed to peel old blob: {e}")))?;
    let old_content = String::from_utf8_lossy(&old_blob_data.data).to_string();

    let diff = TextDiff::from_lines(&old_content, &new_content);
    let mut additions = 0u32;
    let mut deletions = 0u32;
    let mut changes = Vec::new();
    let mut old_line_num = 1u32;
    let mut new_line_num = 1u32;

    for op in diff.ops() {
        for change in diff.iter_changes(op) {
            match change.tag() {
                ChangeTag::Equal => {
                    old_line_num += 1;
                    new_line_num += 1;
                }
                ChangeTag::Delete => {
                    deletions += 1;
                    let content = change.value().trim_end_matches(['\r', '\n']);
                    changes.push(DiffChange {
                        kind: "deleted".to_string(),
                        old_line: Some(old_line_num),
                        new_line: None,
                        content: content.to_string(),
                    });
                    old_line_num += 1;
                }
                ChangeTag::Insert => {
                    additions += 1;
                    let content = change.value().trim_end_matches(['\r', '\n']);
                    changes.push(DiffChange {
                        kind: "added".to_string(),
                        old_line: None,
                        new_line: Some(new_line_num),
                        content: content.to_string(),
                    });
                    new_line_num += 1;
                }
            }
        }
    }

    Ok(FileDiff {
        path,
        additions,
        deletions,
        changes,
    })
}

/// List branches in a repository.
///
/// # Errors
///
/// Returns `GitError` if the repository cannot be opened or branch operations fail.
#[instrument(skip(path))]
pub fn branches(path: &Path) -> Result<Vec<GitBranch>, GitError> {
    let repo = gix::open(path)
        .map_err(|e| GitError::GitOperation(format!("failed to open repository: {e}")))?;

    let current_head = repo
        .head_name()
        .map_err(|e| GitError::GitOperation(format!("failed to get head name: {e}")))?;

    let mut branches = Vec::new();

    let refs = repo
        .references()
        .map_err(|e| GitError::GitOperation(format!("failed to get references: {e}")))?;

    let local_branches = refs
        .local_branches()
        .map_err(|e| GitError::GitOperation(format!("failed to iterate local branches: {e}")))?;

    for branch_ref in local_branches {
        let branch_ref = branch_ref
            .map_err(|e| GitError::GitOperation(format!("failed to read branch: {e}")))?;

        let name = branch_ref.name().shorten().to_string();

        let head = branch_ref
            .try_id()
            .map_or_else(|| "unknown".to_string(), |id| id.to_hex().to_string());

        let is_current = current_head
            .as_ref()
            .is_some_and(|head_name| head_name.as_bstr() == branch_ref.name().as_bstr());

        branches.push(GitBranch {
            name,
            head,
            is_current,
        });
    }

    Ok(branches)
}

/// Get per-edit blame information for a file.
///
/// The result contains one line for each line of `file`, attributed to the commit
/// that introduced that hunk of consecutive lines (or that individual line if it
/// stands alone). This matches the behaviour of `git blame` and VS Code.
///
/// # Errors
///
/// Returns `GitError` if the repository cannot be opened, is bare, the file is not
/// in the current commit, or blame operations fail.
#[instrument(skip(path))]
pub fn blame(path: &Path, file: &str) -> Result<GitBlame, GitError> {
    if file.is_empty() {
        return Err(GitError::FileNotFound("file path is empty".to_string()));
    }

    let repo = gix::open(path)
        .map_err(|e| GitError::GitOperation(format!("failed to open repository: {e}")))?;
    let worktree = repo.worktree().ok_or(GitError::BareRepo)?;
    let repo_root = worktree.base().canonicalize().map_err(|e| {
        GitError::GitOperation(format!("failed to canonicalize repository root: {e}"))
    })?;

    let file_path = path
        .join(file)
        .canonicalize()
        .map_err(|e| GitError::FileNotFound(format!("failed to resolve file path: {e}")))?;

    if !file_path.starts_with(&repo_root) {
        return Err(GitError::FileNotFound(format!(
            "file is outside repository: {file}"
        )));
    }

    if file_path.is_dir() {
        return Err(GitError::FileNotFound(format!(
            "path is a directory: {file}"
        )));
    }

    let relative_path = file_path
        .strip_prefix(&repo_root)
        .map_err(|_| GitError::GitOperation(format!("file is outside repository root: {file}")))?;
    if relative_path.as_os_str().is_empty() {
        return Err(GitError::FileNotFound(format!(
            "file path is a directory: {file}"
        )));
    }

    let relative = relative_path
        .to_str()
        .ok_or_else(|| GitError::GitOperation(format!("file path is not valid UTF-8: {file}")))?;
    let file_bstr = gix::path::to_unix_separators(BStr::new(relative.as_bytes()));

    let head_commit = repo
        .head_commit()
        .map_err(|e| GitError::GitOperation(format!("failed to get head commit: {e}")))?;

    let outcome = repo
        .blame_file(
            &file_bstr,
            head_commit.id,
            gix::repository::blame_file::Options::default(),
        )
        .map_err(|e| GitError::GitOperation(format!("blame failed: {e}")))?;

    let mut commit_cache = std::collections::HashMap::new();
    let mut blame_lines = Vec::new();

    for (entry, lines) in outcome.entries_with_lines() {
        let (author, time) = blame_commit_info(&repo, entry.commit_id, &mut commit_cache)?;
        let base = entry.start_in_blamed_file;
        for (idx, line) in lines.iter().enumerate() {
            #[allow(clippy::cast_possible_truncation)]
            let line_number = base + idx as u32 + 1;
            blame_lines.push(BlameLine {
                line_number,
                content: String::from_utf8_lossy(line.as_bytes()).to_string(),
                commit_id: entry.commit_id.to_hex().to_string(),
                author: author.clone(),
                time,
            });
        }
    }

    Ok(GitBlame { lines: blame_lines })
}

fn blame_commit_info(
    repo: &gix::Repository,
    commit_id: gix::hash::ObjectId,
    cache: &mut std::collections::HashMap<gix::hash::ObjectId, (String, i64)>,
) -> Result<(String, i64), GitError> {
    if let Some(info) = cache.get(&commit_id) {
        return Ok(info.clone());
    }

    let commit = repo
        .find_commit(commit_id)
        .map_err(|e| GitError::GitOperation(format!("failed to find commit {commit_id}: {e}")))?;
    let decoded = commit
        .decode()
        .map_err(|e| GitError::GitOperation(format!("failed to decode commit {commit_id}: {e}")))?;
    let author = decoded.author().map_err(|e| {
        GitError::GitOperation(format!("failed to decode author for {commit_id}: {e}"))
    })?;
    let author_time = author
        .time()
        .map_err(|e| {
            GitError::GitOperation(format!("failed to decode author time for {commit_id}: {e}"))
        })?
        .seconds;
    let author_name = author.name.to_string();

    let info = (author_name, author_time);
    cache.insert(commit_id, info.clone());
    Ok(info)
}

fn worktree_root(path: &Path) -> Result<PathBuf, GitError> {
    let repo = gix::discover(path)
        .map_err(|e| GitError::GitOperation(format!("failed to discover repository: {e}")))?;
    let worktree = repo.worktree().ok_or(GitError::BareRepo)?;
    Ok(worktree.base().to_path_buf())
}

fn run_git(path: &Path, args: &[&str]) -> Result<std::process::Output, GitError> {
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(path)
        .args(args)
        .output()
        .map_err(|e| GitError::GitOperation(format!("failed to run git: {e}")))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
        return Err(GitError::GitOperation(stderr));
    }
    Ok(output)
}

/// Stage files in a repository.
///
/// # Errors
///
/// Returns `GitError` if the repository cannot be opened or git add fails.
#[instrument(skip(path, files))]
pub fn add(path: &Path, files: &[String]) -> Result<(), GitError> {
    let root = worktree_root(path)?;
    if files.is_empty() {
        return Err(GitError::InvalidReference("no files specified".to_string()));
    }
    let mut args = vec!["add", "--"];
    for file in files {
        args.push(file);
    }
    run_git(&root, &args)?;
    Ok(())
}

/// Create a commit with the given message and return the new commit id.
///
/// # Errors
///
/// Returns `GitError` if the repository cannot be opened or git commit fails.
#[instrument(skip(path, message))]
pub fn commit(path: &Path, message: &str) -> Result<String, GitError> {
    let root = worktree_root(path)?;
    run_git(&root, &["commit", "-m", message])?;
    let output = run_git(&root, &["rev-parse", "HEAD"])?;
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

/// Checkout a branch, tag, or ref in a repository.
///
/// # Errors
///
/// Returns `GitError` if the repository cannot be opened or git checkout fails.
#[instrument(skip(path, target))]
pub fn checkout(path: &Path, target: &str) -> Result<(), GitError> {
    let root = worktree_root(path)?;
    run_git(&root, &["checkout", target])?;
    Ok(())
}

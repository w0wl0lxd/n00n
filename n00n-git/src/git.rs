use std::io::BufWriter;
use std::path::{Component, Path, PathBuf};

use gix::bstr::{BStr, BString, ByteSlice};
use gix::object::Kind;
use gix::object::tree::diff::ChangeDetached as TreeChange;
use serde::{Deserialize, Serialize};
use similar::{ChangeTag, TextDiff};
use tracing::instrument;

use crate::error::GitError;

use gix::dir as gix_dir;
use gix::status::plumbing as gix_status;

const INDEX_WRITE_BUFFER_BYTES: usize = 64 * 1024;
const FILE_PATH_EMPTY_ERROR: &str = "file path is empty";

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

    // `into_iter` (unlike `into_index_worktree_iter`) also yields the tree-to-index
    // side of status, so staged changes (`TreeIndex`) are reported alongside
    // unstaged worktree changes (`IndexWorktree`) instead of both being folded
    // into a single "staged: true" bucket.
    let iter = platform
        .into_iter(Vec::new())
        .map_err(|e| GitError::GitOperation(format!("failed to create status iterator: {e}")))?;

    for item_result in iter {
        let item = item_result
            .map_err(|e| GitError::GitOperation(format!("failed to read status item: {e}")))?;

        let (entry_path, status, staged) = match item {
            gix::status::Item::TreeIndex(change) => {
                let status_str = match &change {
                    gix::diff::index::ChangeRef::Addition { .. } => "added",
                    gix::diff::index::ChangeRef::Deletion { .. } => "deleted",
                    gix::diff::index::ChangeRef::Modification { .. } => "modified",
                    gix::diff::index::ChangeRef::Rewrite { copy, .. } => {
                        if *copy {
                            "added"
                        } else {
                            "renamed"
                        }
                    }
                };
                (change.location().to_string(), status_str, true)
            }
            gix::status::Item::IndexWorktree(gix::status::index_worktree::Item::Modification {
                rela_path,
                status,
                ..
            }) => {
                let path = rela_path.to_string();
                let (status_str, is_staged) = match status {
                    gix_status::index_as_worktree::EntryStatus::Conflict { .. } => {
                        ("conflict", true)
                    }
                    gix_status::index_as_worktree::EntryStatus::Change(change) => match change {
                        gix_status::index_as_worktree::Change::Removed => ("deleted", false),
                        gix_status::index_as_worktree::Change::Type { .. }
                        | gix_status::index_as_worktree::Change::Modification { .. }
                        | gix_status::index_as_worktree::Change::SubmoduleModification(_) => {
                            ("modified", false)
                        }
                    },
                    gix_status::index_as_worktree::EntryStatus::NeedsUpdate(_) => continue,
                    gix_status::index_as_worktree::EntryStatus::IntentToAdd => ("added", false),
                };
                (path, status_str, is_staged)
            }
            gix::status::Item::IndexWorktree(
                gix::status::index_worktree::Item::DirectoryContents { entry, .. },
            ) => {
                let path = entry.rela_path.to_string();
                let (status_str, is_staged) = match entry.status {
                    gix_dir::entry::Status::Untracked => ("untracked", false),
                    gix_dir::entry::Status::Ignored(_) => ("ignored", false),
                    _ => continue,
                };
                (path, status_str, is_staged)
            }
            gix::status::Item::IndexWorktree(gix::status::index_worktree::Item::Rewrite {
                dirwalk_entry,
                copy,
                ..
            }) => {
                let path = dirwalk_entry.rela_path.to_string();
                let status_str = if copy { "added" } else { "renamed" };
                (path, status_str, false)
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
    if count == 0 {
        return Ok(Vec::new());
    }

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
            let parent_obj = repo.find_object(*parent_id).map_err(|e| {
                GitError::GitOperation(format!("failed to find parent object: {e}"))
            })?;
            current = Some(parent_obj.try_into_commit().map_err(|e| {
                GitError::GitOperation(format!("parent object is not a commit: {e}"))
            })?);
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
        return Err(GitError::FileNotFound(FILE_PATH_EMPTY_ERROR.to_string()));
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

fn repository_relative_path(path: &str) -> Result<String, GitError> {
    let mut parts = Vec::new();
    for component in Path::new(path).components() {
        match component {
            Component::Normal(part) => {
                let part = part.to_string_lossy().into_owned();
                gix::validate::path::component(
                    part.as_bytes().as_bstr(),
                    None,
                    gix::validate::path::component::Options::default(),
                )
                .map_err(|e| {
                    GitError::InvalidReference(format!("invalid path component '{part}': {e}"))
                })?;
                parts.push(part);
            }
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return Err(GitError::InvalidReference(format!(
                    "path must be repository-relative: {path}"
                )));
            }
        }
    }
    if parts.is_empty() {
        return Err(GitError::InvalidReference("empty file path".to_string()));
    }
    Ok(parts.join("/"))
}

fn repository_scoped_path(
    root: &Path,
    operation_path: &Path,
    file: &str,
) -> Result<String, GitError> {
    let root = root.canonicalize().map_err(|error| {
        GitError::GitOperation(format!("failed to resolve worktree root: {error}"))
    })?;
    let operation_path = operation_path.canonicalize().map_err(|error| {
        GitError::GitOperation(format!("failed to resolve repository path: {error}"))
    })?;
    let prefix = operation_path.strip_prefix(&root).map_err(|_| {
        GitError::InvalidReference(format!(
            "repository path '{}' is outside worktree '{}'",
            operation_path.display(),
            root.display()
        ))
    })?;
    let file = repository_relative_path(file)?;
    if prefix.as_os_str().is_empty() {
        return Ok(file);
    }
    let prefix = prefix
        .components()
        .map(|component| match component {
            Component::Normal(component) => component.to_str().ok_or_else(|| {
                GitError::InvalidReference("repository path is not valid UTF-8".to_string())
            }),
            _ => Err(GitError::InvalidReference(
                "repository path contains invalid components".to_string(),
            )),
        })
        .collect::<Result<Vec<_>, _>>()?
        .join("/");
    repository_relative_path(&format!("{prefix}/{file}"))
}

fn validate_worktree_path(root: &Path, relative: &BStr) -> Result<PathBuf, GitError> {
    let mut absolute = root.to_path_buf();
    let components = relative.split(|byte| *byte == b'/').collect::<Vec<_>>();
    for (index, component) in components.iter().enumerate() {
        absolute.push(gix::path::from_bstr(component.as_bstr()).as_ref());
        if let Ok(metadata) = std::fs::symlink_metadata(&absolute) {
            let is_leaf = index + 1 == components.len();
            if metadata.file_type().is_symlink() {
                if !is_leaf {
                    return Err(GitError::InvalidReference(format!(
                        "path traverses an intermediate symlink: {relative}"
                    )));
                }
                gix::validate::path::component(
                    component.as_bstr(),
                    Some(gix::validate::path::component::Mode::Symlink),
                    gix::validate::path::component::Options::default(),
                )
                .map_err(|e| {
                    GitError::InvalidReference(format!("invalid symlink '{relative}': {e}"))
                })?;
            }
        }
    }
    Ok(absolute)
}

fn path_collides(left: &BStr, right: &BStr) -> bool {
    fn is_parent(parent: &BStr, child: &BStr) -> bool {
        let parent: &[u8] = parent.as_ref();
        let child: &[u8] = child.as_ref();
        child.len() > parent.len()
            && child.starts_with(parent)
            && child.get(parent.len()) == Some(&b'/')
    }

    left == right || is_parent(left, right) || is_parent(right, left)
}

fn contains_path_collision(paths: impl IntoIterator<Item = BString>) -> bool {
    let mut paths = paths.into_iter().collect::<Vec<_>>();
    paths.sort_unstable();
    paths
        .windows(2)
        .any(|entries| path_collides(entries[0].as_bstr(), entries[1].as_bstr()))
}

fn map_index_lock_error(error: gix::lock::acquire::Error) -> GitError {
    match error {
        gix::lock::acquire::Error::PermanentlyLocked { .. } => GitError::RepositoryLocked,
        gix::lock::acquire::Error::Io(error) => {
            GitError::GitOperation(format!("failed to acquire index lock: {error}"))
        }
    }
}

fn acquire_index_lock(repo: &gix::Repository) -> Result<gix::lock::File, GitError> {
    gix::lock::File::acquire_to_update_resource(
        repo.index_path(),
        gix::lock::acquire::Fail::Immediately,
        None,
    )
    .map_err(map_index_lock_error)
}

fn write_locked_index(index: &gix::index::File, lock: gix::lock::File) -> Result<(), GitError> {
    let mut lock = BufWriter::with_capacity(INDEX_WRITE_BUFFER_BYTES, lock);
    index
        .write_to(&mut lock, gix::index::write::Options::default())
        .map_err(|e| GitError::GitOperation(format!("failed to write index: {e}")))?;
    let lock = lock
        .into_inner()
        .map_err(|e| GitError::GitOperation(format!("failed to flush index: {}", e.error())))?;
    lock.commit()
        .map_err(|e| GitError::GitOperation(format!("failed to commit index lock: {e}")))?;
    Ok(())
}

fn repository_uses_split_index(repo: &gix::Repository) -> bool {
    repo.config_snapshot().boolean("core.splitIndex") == Some(true)
        || std::fs::read_dir(repo.common_dir()).is_ok_and(|entries| {
            entries.filter_map(Result::ok).any(|entry| {
                entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with("sharedindex.")
            })
        })
}

fn reject_unsupported_index_for_add(index: &gix::index::State) -> Result<(), GitError> {
    if index.link().is_some()
        || index.entries().iter().any(|entry| {
            entry.mode.is_sparse()
                || entry
                    .flags
                    .contains(gix::index::entry::Flags::SKIP_WORKTREE)
        })
    {
        return Err(GitError::GitOperation(
            "sparse and split indexes are not supported by native mutations".to_string(),
        ));
    }
    if index.resolve_undo().is_some()
        || index
            .entries()
            .iter()
            .any(|entry| entry.stage() != gix::index::entry::Stage::Unconflicted)
    {
        return Err(GitError::GitOperation(
            "conflicted and resolve-undo indexes are not supported by native mutations".to_string(),
        ));
    }
    Ok(())
}

fn active_commit_hook(repo: &gix::Repository) -> Result<Option<PathBuf>, GitError> {
    let config = repo.config_snapshot();
    let configured = config
        .trusted_path("core.hooksPath")
        .map_err(|e| GitError::GitOperation(format!("invalid core.hooksPath: {e}")))?;
    let hooks = match configured {
        Some(path) if path.is_absolute() => path,
        Some(path) => repo.workdir().unwrap_or_else(|| repo.git_dir()).join(path),
        None => repo.git_dir().join("hooks"),
    };
    Ok([
        "pre-commit",
        "prepare-commit-msg",
        "commit-msg",
        "post-commit",
    ]
    .into_iter()
    .map(|name| hooks.join(name))
    .find(|path| hook_is_executable(path)))
}

#[cfg(unix)]
fn hook_is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path)
        .is_ok_and(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
}

#[cfg(not(unix))]
fn hook_is_executable(path: &Path) -> bool {
    path.is_file()
}

/// Stage files in a repository.
///
/// # Errors
///
/// Returns `GitError` if the repository cannot be opened or git add fails.
#[instrument(skip(path, files))]
pub fn add(path: &Path, files: &[String]) -> Result<(), GitError> {
    if files.is_empty() {
        return Err(GitError::InvalidReference("no files specified".to_string()));
    }

    let repo = gix::discover(path)
        .map_err(|e| GitError::GitOperation(format!("failed to discover repository: {e}")))?;
    let root = repo
        .worktree()
        .ok_or(GitError::BareRepo)?
        .base()
        .to_path_buf();
    if repository_uses_split_index(&repo) {
        return Err(GitError::GitOperation(
            "split indexes are not supported by native mutations".to_string(),
        ));
    }
    let paths = files
        .iter()
        .map(|file| {
            let relative = gix::bstr::BString::from(repository_scoped_path(&root, path, file)?);
            let absolute = validate_worktree_path(&root, relative.as_bstr())?;
            if std::fs::symlink_metadata(&absolute).is_ok_and(|metadata| metadata.is_dir()) {
                return Err(GitError::InvalidReference(format!(
                    "directories are not supported by native add: {relative}"
                )));
            }
            Ok((file, relative, absolute))
        })
        .collect::<Result<Vec<_>, GitError>>()?;

    let index_lock = acquire_index_lock(&repo)?;
    let mut index = repo
        .index_or_load_from_head_or_empty()
        .map_err(|e| GitError::GitOperation(format!("failed to load index: {e}")))?
        .into_owned();
    reject_unsupported_index_for_add(&index)?;
    let attributes = repo
        .attributes_only(
            &index,
            gix::worktree::stack::state::attributes::Source::WorktreeThenIdMapping,
        )
        .map_err(|e| GitError::GitOperation(format!("failed to load git attributes: {e}")))?;
    let mut pipeline = gix::filter::Pipeline::new(&repo, attributes.detach())
        .map_err(|e| GitError::GitOperation(format!("failed to initialize git filters: {e}")))?;
    let mut excludes = repo
        .excludes(
            &index,
            None,
            gix::worktree::stack::state::ignore::Source::WorktreeThenIdMappingIfNotSkipped,
        )
        .map_err(|e| GitError::GitOperation(format!("failed to load git excludes: {e}")))?;
    let fs = repo
        .filesystem_options()
        .map_err(|e| GitError::GitOperation(format!("failed to load filesystem options: {e}")))?;

    for (original, relative, absolute) in paths {
        let existing = index
            .entries()
            .iter()
            .find(|entry| {
                entry.stage() == gix::index::entry::Stage::Unconflicted
                    && entry.path(&index) == relative.as_bstr()
            })
            .map(|entry| entry.mode);
        let was_tracked = index
            .entries()
            .iter()
            .any(|entry| entry.path(&index) == relative.as_bstr());
        if !was_tracked
            && excludes
                .at_entry(relative.as_bstr(), None)
                .map_err(|e| {
                    GitError::GitOperation(format!(
                        "failed to check excludes for '{relative}': {e}"
                    ))
                })?
                .is_excluded()
        {
            return Err(GitError::GitOperation(format!(
                "path is ignored by git: {relative}"
            )));
        }
        let outcome = pipeline
            .worktree_file_to_object(relative.as_bstr(), &index)
            .map_err(|e| GitError::GitOperation(format!("failed to stage '{relative}': {e}")))?;
        index.remove_entries(|_, entry_path, _| path_collides(entry_path, relative.as_bstr()));

        match outcome {
            Some((id, kind, _)) => {
                let metadata =
                    gix::index::fs::Metadata::from_path_no_follow(&absolute).map_err(|e| {
                        GitError::GitOperation(format!("failed to stat '{relative}': {e}"))
                    })?;
                let stat = gix::index::entry::Stat::from_fs(&metadata).map_err(|e| {
                    GitError::GitOperation(format!("failed to stat '{relative}': {e}"))
                })?;
                let mut mode: gix::index::entry::Mode = kind.into();
                if !fs.executable_bit
                    && (mode == gix::index::entry::Mode::FILE
                        || mode == gix::index::entry::Mode::FILE_EXECUTABLE)
                {
                    mode = match existing {
                        Some(existing)
                            if existing == gix::index::entry::Mode::FILE
                                || existing == gix::index::entry::Mode::FILE_EXECUTABLE =>
                        {
                            existing
                        }
                        _ => gix::index::entry::Mode::FILE,
                    };
                }
                if !fs.symlink
                    && mode == gix::index::entry::Mode::FILE
                    && existing == Some(gix::index::entry::Mode::SYMLINK)
                {
                    mode = gix::index::entry::Mode::SYMLINK;
                }
                index.dangerously_push_entry(
                    stat,
                    id,
                    gix::index::entry::Flags::from_stage(gix::index::entry::Stage::Unconflicted),
                    mode,
                    relative.as_bstr(),
                );
            }
            None if !was_tracked => return Err(GitError::FileNotFound((*original).clone())),
            None => {}
        }
        index.sort_entries();
    }

    index.remove_tree();
    write_locked_index(&index, index_lock)
}

/// Create a commit with the given message and return the new commit id.
///
/// # Errors
///
/// Returns `GitError` if the repository cannot be opened or git commit fails.
#[instrument(skip(path, message))]
pub fn commit(path: &Path, message: &str) -> Result<String, GitError> {
    if message.trim().is_empty() {
        return Err(GitError::GitOperation("empty commit message".to_string()));
    }

    let repo = gix::discover(path)
        .map_err(|e| GitError::GitOperation(format!("failed to discover repository: {e}")))?;
    if repo.is_bare() {
        return Err(GitError::BareRepo);
    }
    if let Some(state) = repo.state() {
        return Err(GitError::GitOperation(format!(
            "repository operation is in progress: {state:?}"
        )));
    }
    if repo.config_snapshot().boolean("commit.gpgSign") == Some(true) {
        return Err(GitError::GitOperation(
            "signed commits are not supported by native commit".to_string(),
        ));
    }
    if let Some(hook) = active_commit_hook(&repo)? {
        return Err(GitError::GitOperation(format!(
            "commit hook is not supported by native commit: {}",
            hook.display()
        )));
    }

    let _index_lock = acquire_index_lock(&repo)?;
    let index = repo
        .index_or_load_from_head_or_empty()
        .map_err(|e| GitError::GitOperation(format!("failed to load index: {e}")))?;
    if index.entries().iter().any(|entry| {
        entry.mode.is_sparse()
            || entry.stage() != gix::index::entry::Stage::Unconflicted
            || entry
                .flags
                .contains(gix::index::entry::Flags::INTENT_TO_ADD)
    }) {
        return Err(GitError::MergeConflict);
    }
    if contains_path_collision(
        index
            .entries()
            .iter()
            .map(|entry| entry.path(&index).to_owned()),
    ) {
        return Err(GitError::GitOperation(
            "index contains colliding file and directory paths".to_string(),
        ));
    }

    let mut editor = repo
        .empty_tree()
        .edit()
        .map_err(|e| GitError::GitOperation(format!("failed to create tree editor: {e}")))?;
    for entry in index.entries() {
        let mode = entry.mode.to_tree_entry_mode().ok_or_else(|| {
            GitError::GitOperation(format!("invalid index mode for '{}'", entry.path(&index)))
        })?;
        editor
            .upsert(entry.path(&index), mode.kind(), entry.id)
            .map_err(|e| GitError::GitOperation(format!("failed to build commit tree: {e}")))?;
    }
    let tree_id = editor
        .write()
        .map_err(|e| GitError::GitOperation(format!("failed to write commit tree: {e}")))?
        .detach();

    let head = repo
        .head()
        .map_err(|e| GitError::GitOperation(format!("failed to read HEAD: {e}")))?;
    let parent = head.id().map(gix::Id::detach);
    if parent.is_none() && index.entries().is_empty() {
        return Err(GitError::GitOperation("nothing to commit".to_string()));
    }
    if let Some(parent_id) = parent {
        let parent_commit = repo
            .find_object(parent_id)
            .map_err(|e| GitError::GitOperation(format!("failed to read HEAD: {e}")))?
            .peel_to_commit()
            .map_err(|e| GitError::GitOperation(format!("failed to peel HEAD to commit: {e}")))?;
        let parent_tree = parent_commit
            .tree_id()
            .map_err(|e| GitError::GitOperation(format!("failed to read HEAD tree: {e}")))?
            .detach();
        if parent_tree == tree_id {
            return Err(GitError::GitOperation("nothing to commit".to_string()));
        }
    }

    let message = if message.ends_with('\n') {
        std::borrow::Cow::Borrowed(message)
    } else {
        std::borrow::Cow::Owned(format!("{message}\n"))
    };
    let commit_id = repo
        .commit("HEAD", message.as_ref(), tree_id, parent)
        .map_err(|e| GitError::GitOperation(format!("failed to create commit: {e}")))?;
    Ok(commit_id.detach().to_string())
}

/// Checkout a branch, tag, or ref in a repository.
///
/// # Errors
///
/// Returns `GitError` if the repository cannot be opened or git checkout fails.
#[instrument(skip(path, target))]
pub fn checkout(path: &Path, target: &str) -> Result<(), GitError> {
    if target.starts_with('-') {
        return Err(GitError::InvalidReference(format!(
            "refusing to treat '{target}' as a git option"
        )));
    }
    let root = worktree_root(path)?;
    run_git(&root, &["checkout", target])?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    fn init_repo(root: &Path) {
        std::fs::write(
            root.join(".gitconfig"),
            "[user]\nname = Test\nemail = test@example.com\n",
        )
        .unwrap();
        run(root, &["init", "--initial-branch=main"]);
        run(root, &["config", "user.name", "Test"]);
        run(root, &["config", "user.email", "test@example.com"]);
        run(root, &["config", "commit.gpgSign", "false"]);
        run(root, &["config", "core.hooksPath", ".git/hooks"]);
    }

    fn run(root: &Path, args: &[&str]) -> std::process::Output {
        let output = Command::new("git")
            .arg("-C")
            .arg(root)
            .env("HOME", root)
            .env("XDG_CONFIG_HOME", root)
            .args(args)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        output
    }

    #[test]
    fn checkout_rejects_a_target_that_looks_like_an_option() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        init_repo(root);
        std::fs::write(root.join("file.txt"), "content\n").unwrap();
        run(root, &["add", "file.txt"]);
        run(root, &["commit", "-m", "base"]);
        run(root, &["commit", "--allow-empty", "-m", "second"]);

        let err = checkout(root, "-f").unwrap_err();
        assert!(matches!(err, GitError::InvalidReference(_)));
        // The worktree must be untouched: git never ran with `-f` as an option.
        let head = String::from_utf8(run(root, &["rev-parse", "HEAD"]).stdout).unwrap();
        assert!(!head.trim().is_empty());
    }

    #[test]
    fn native_add_stages_new_modified_and_deleted_files() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        init_repo(root);
        std::fs::write(root.join("modified.txt"), "one\n").unwrap();
        std::fs::write(root.join("deleted.txt"), "one\n").unwrap();
        run(root, &["add", "modified.txt", "deleted.txt"]);
        run(root, &["commit", "-m", "base"]);

        std::fs::write(root.join("modified.txt"), "two\n").unwrap();
        std::fs::remove_file(root.join("deleted.txt")).unwrap();
        std::fs::write(root.join("new.txt"), "new\n").unwrap();
        add(
            root,
            &[
                "modified.txt".to_string(),
                "deleted.txt".to_string(),
                "new.txt".to_string(),
            ],
        )
        .unwrap();

        let staged =
            String::from_utf8(run(root, &["diff", "--cached", "--name-status"]).stdout).unwrap();
        assert!(staged.lines().any(|line| line == "M\tmodified.txt"));
        assert!(staged.lines().any(|line| line == "D\tdeleted.txt"));
        assert!(staged.lines().any(|line| line == "A\tnew.txt"));
    }

    #[test]
    fn native_add_recreates_missing_index_without_losing_head_entries() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        init_repo(root);
        std::fs::write(root.join("changed.txt"), "one\n").unwrap();
        std::fs::write(root.join("unchanged.txt"), "one\n").unwrap();
        run(root, &["add", "changed.txt", "unchanged.txt"]);
        run(root, &["commit", "-m", "base"]);
        std::fs::remove_file(root.join(".git/index")).unwrap();
        std::fs::write(root.join("changed.txt"), "two\n").unwrap();

        add(root, &["changed.txt".to_string()]).unwrap();

        let tracked = String::from_utf8(run(root, &["ls-files"]).stdout).unwrap();
        assert!(tracked.lines().any(|line| line == "changed.txt"));
        assert!(tracked.lines().any(|line| line == "unchanged.txt"));
    }

    #[test]
    fn native_add_rejects_ignored_files() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        init_repo(root);
        std::fs::write(root.join(".gitignore"), "ignored.txt\n").unwrap();
        std::fs::write(root.join("ignored.txt"), "ignored\n").unwrap();

        assert!(matches!(
            add(root, &["ignored.txt".to_string()]),
            Err(GitError::GitOperation(_))
        ));
        let tracked = String::from_utf8(run(root, &["ls-files"]).stdout).unwrap();
        assert!(tracked.trim().is_empty());
    }

    #[test]
    fn native_add_rejects_unsupported_paths() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        init_repo(root);
        std::fs::create_dir(root.join("directory")).unwrap();

        assert!(matches!(
            add(root, &["../outside".to_string()]),
            Err(GitError::InvalidReference(_))
        ));
        assert!(matches!(
            add(root, &["directory".to_string()]),
            Err(GitError::InvalidReference(_))
        ));
        assert!(matches!(
            add(root, &["missing.txt".to_string()]),
            Err(GitError::FileNotFound(_))
        ));
    }

    #[test]
    fn native_mutations_respect_an_existing_index_lock() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        init_repo(root);
        std::fs::write(root.join("file.txt"), "one\n").unwrap();
        run(root, &["add", "file.txt"]);
        std::fs::write(root.join(".git/index.lock"), "locked").unwrap();

        assert!(matches!(
            add(root, &["file.txt".to_string()]),
            Err(GitError::RepositoryLocked)
        ));
        assert!(matches!(
            commit(root, "locked"),
            Err(GitError::RepositoryLocked)
        ));
    }

    #[test]
    fn index_lock_io_errors_preserve_the_cause() {
        let error = map_index_lock_error(gix::lock::acquire::Error::Io(std::io::Error::other(
            "disk unavailable",
        )));

        assert!(matches!(
            error,
            GitError::GitOperation(message)
                if message == "failed to acquire index lock: disk unavailable"
        ));
    }

    #[test]
    fn native_add_rejects_git_metadata_paths_without_changing_index() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        init_repo(root);
        std::fs::write(root.join("tracked.txt"), "tracked\n").unwrap();
        run(root, &["add", "tracked.txt"]);
        let before = std::fs::read(root.join(".git/index")).unwrap();

        assert!(matches!(
            add(root, &[".git/config".to_string()]),
            Err(GitError::InvalidReference(_))
        ));
        assert_eq!(std::fs::read(root.join(".git/index")).unwrap(), before);
    }

    #[cfg(unix)]
    #[test]
    fn native_add_rejects_paths_through_intermediate_symlinks() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        init_repo(root);
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("secret.txt"), "secret\n").unwrap();
        symlink(outside.path(), root.join("link")).unwrap();

        assert!(matches!(
            add(root, &["link/secret.txt".to_string()]),
            Err(GitError::InvalidReference(_))
        ));
        assert!(
            String::from_utf8(run(root, &["ls-files"]).stdout)
                .unwrap()
                .trim()
                .is_empty()
        );
    }

    #[test]
    fn detects_nonadjacent_file_directory_collisions() {
        let paths = ["dir/file", "other", "dir"].into_iter().map(BString::from);

        assert!(contains_path_collision(paths));
    }

    #[test]
    fn native_add_resolves_files_relative_to_supplied_repository_path() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        init_repo(root);
        std::fs::create_dir(root.join("nested")).unwrap();
        std::fs::write(root.join("nested/file.txt"), "content\n").unwrap();

        add(&root.join("nested"), &["file.txt".to_string()]).unwrap();

        assert_eq!(
            String::from_utf8(run(root, &["ls-files"]).stdout)
                .unwrap()
                .trim(),
            "nested/file.txt"
        );
    }

    #[test]
    fn native_add_resolves_file_directory_collisions() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        init_repo(root);
        std::fs::write(root.join("entry"), "file\n").unwrap();
        run(root, &["add", "entry"]);
        run(root, &["commit", "-m", "file"]);

        std::fs::remove_file(root.join("entry")).unwrap();
        std::fs::create_dir(root.join("entry")).unwrap();
        std::fs::write(root.join("entry/child"), "child\n").unwrap();
        add(root, &["entry/child".to_string()]).unwrap();
        assert_eq!(
            String::from_utf8(run(root, &["ls-files"]).stdout)
                .unwrap()
                .trim(),
            "entry/child"
        );
        commit(root, "directory").unwrap();

        std::fs::remove_dir_all(root.join("entry")).unwrap();
        std::fs::write(root.join("entry"), "file again\n").unwrap();
        add(root, &["entry".to_string()]).unwrap();
        assert_eq!(
            String::from_utf8(run(root, &["ls-files"]).stdout)
                .unwrap()
                .trim(),
            "entry"
        );
    }

    #[cfg(unix)]
    #[test]
    fn native_add_respects_core_filemode() {
        use std::os::unix::fs::PermissionsExt;

        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        init_repo(root);
        std::fs::write(root.join("file.txt"), "one\n").unwrap();
        run(root, &["add", "file.txt"]);
        run(root, &["commit", "-m", "base"]);
        run(root, &["config", "core.fileMode", "false"]);
        let mut permissions = std::fs::metadata(root.join("file.txt"))
            .unwrap()
            .permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(root.join("file.txt"), permissions).unwrap();

        add(root, &["file.txt".to_string()]).unwrap();
        assert!(
            String::from_utf8(run(root, &["diff", "--cached", "--summary"]).stdout)
                .unwrap()
                .trim()
                .is_empty()
        );

        std::fs::write(root.join("new.txt"), "new\n").unwrap();
        let mut permissions = std::fs::metadata(root.join("new.txt"))
            .unwrap()
            .permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(root.join("new.txt"), permissions).unwrap();
        add(root, &["new.txt".to_string()]).unwrap();
        assert!(
            String::from_utf8(run(root, &["ls-files", "--stage", "new.txt"]).stdout)
                .unwrap()
                .starts_with("100644 ")
        );
    }

    #[test]
    fn native_add_rejects_split_indexes() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        init_repo(root);
        std::fs::write(root.join("file.txt"), "one\n").unwrap();
        run(root, &["add", "file.txt"]);
        run(root, &["update-index", "--split-index"]);

        assert!(matches!(
            add(root, &["file.txt".to_string()]),
            Err(GitError::GitOperation(_))
        ));
    }

    #[test]
    fn native_add_preserves_resolve_undo_index_data_by_rejecting_mutation() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        init_repo(root);
        std::fs::write(root.join("conflict.txt"), "base\n").unwrap();
        run(root, &["add", "conflict.txt"]);
        run(root, &["commit", "-m", "base"]);
        run(root, &["checkout", "-b", "side"]);
        std::fs::write(root.join("conflict.txt"), "side\n").unwrap();
        run(root, &["commit", "-am", "side"]);
        run(root, &["checkout", "main"]);
        std::fs::write(root.join("conflict.txt"), "main\n").unwrap();
        run(root, &["commit", "-am", "main"]);
        let merge = Command::new("git")
            .arg("-C")
            .arg(root)
            .env("HOME", root)
            .env("XDG_CONFIG_HOME", root)
            .args(["merge", "side"])
            .output()
            .unwrap();
        assert!(!merge.status.success());
        std::fs::write(root.join("conflict.txt"), "resolved\n").unwrap();
        let conflicted = std::fs::read(root.join(".git/index")).unwrap();
        assert!(matches!(
            add(root, &["conflict.txt".to_string()]),
            Err(GitError::GitOperation(_))
        ));
        assert_eq!(std::fs::read(root.join(".git/index")).unwrap(), conflicted);
        run(root, &["add", "conflict.txt"]);
        assert!(
            !String::from_utf8(run(root, &["ls-files", "--resolve-undo"]).stdout)
                .unwrap()
                .trim()
                .is_empty()
        );
        std::fs::write(root.join("other.txt"), "other\n").unwrap();
        let before = std::fs::read(root.join(".git/index")).unwrap();

        assert!(matches!(
            add(root, &["other.txt".to_string()]),
            Err(GitError::GitOperation(_))
        ));
        assert_eq!(std::fs::read(root.join(".git/index")).unwrap(), before);
    }

    #[test]
    fn native_commit_rejects_required_signing() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        init_repo(root);
        std::fs::write(root.join("file.txt"), "one\n").unwrap();
        run(root, &["add", "file.txt"]);
        run(root, &["config", "commit.gpgSign", "true"]);

        assert!(matches!(
            commit(root, "signed"),
            Err(GitError::GitOperation(_))
        ));
        let verify = Command::new("git")
            .arg("-C")
            .arg(root)
            .args(["rev-parse", "--verify", "HEAD"])
            .output()
            .unwrap();
        assert!(!verify.status.success());
    }

    #[cfg(unix)]
    #[test]
    fn native_commit_rejects_active_hooks() {
        use std::os::unix::fs::PermissionsExt;

        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        init_repo(root);
        std::fs::write(root.join("file.txt"), "one\n").unwrap();
        run(root, &["add", "file.txt"]);
        let hook = root.join(".git/hooks/pre-commit");
        std::fs::write(&hook, "#!/bin/sh\nexit 0\n").unwrap();
        let mut permissions = std::fs::metadata(&hook).unwrap().permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&hook, permissions).unwrap();

        assert!(matches!(
            commit(root, "hooked"),
            Err(GitError::GitOperation(_))
        ));
    }

    #[test]
    fn native_commit_advances_detached_head() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        init_repo(root);
        std::fs::write(root.join("file.txt"), "one\n").unwrap();
        run(root, &["add", "file.txt"]);
        run(root, &["commit", "-m", "base"]);
        run(root, &["checkout", "--detach", "HEAD"]);
        std::fs::write(root.join("file.txt"), "two\n").unwrap();
        add(root, &["file.txt".to_string()]).unwrap();

        let id = commit(root, "detached").unwrap();
        assert_eq!(
            String::from_utf8(run(root, &["rev-parse", "HEAD"]).stdout)
                .unwrap()
                .trim(),
            id
        );
        let symbolic = Command::new("git")
            .arg("-C")
            .arg(root)
            .args(["symbolic-ref", "-q", "HEAD"])
            .output()
            .unwrap();
        assert!(!symbolic.status.success());
    }

    #[test]
    fn native_commit_creates_commit_and_advances_head() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        init_repo(root);
        assert!(matches!(
            commit(root, "empty root"),
            Err(GitError::GitOperation(_))
        ));
        std::fs::write(root.join("file.txt"), "one\n").unwrap();
        add(root, &["file.txt".to_string()]).unwrap();

        let first = commit(root, "first").unwrap();
        let head = String::from_utf8(run(root, &["rev-parse", "HEAD"]).stdout).unwrap();
        assert_eq!(first, head.trim());
        let subject =
            String::from_utf8(run(root, &["show", "-s", "--format=%s", "HEAD"]).stdout).unwrap();
        assert_eq!(subject.trim(), "first");
        assert!(
            run(root, &["cat-file", "commit", "HEAD"])
                .stdout
                .ends_with(b"\n\nfirst\n")
        );

        std::fs::write(root.join("file.txt"), "two\n").unwrap();
        add(root, &["file.txt".to_string()]).unwrap();
        let second = commit(root, "second").unwrap();
        assert_ne!(first, second);
        let parents =
            String::from_utf8(run(root, &["show", "-s", "--format=%P", "HEAD"]).stdout).unwrap();
        assert_eq!(parents.trim(), first);
        assert!(matches!(
            commit(root, "empty"),
            Err(GitError::GitOperation(_))
        ));
    }

    #[test]
    fn native_commit_rejects_intent_to_add_and_repository_state() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        init_repo(root);
        std::fs::write(root.join("base.txt"), "base\n").unwrap();
        run(root, &["add", "base.txt"]);
        run(root, &["commit", "-m", "base"]);
        std::fs::write(root.join("intent.txt"), "intent\n").unwrap();
        run(root, &["add", "--intent-to-add", "intent.txt"]);
        assert!(matches!(
            commit(root, "intent"),
            Err(GitError::MergeConflict)
        ));

        run(root, &["reset"]);
        std::fs::write(
            root.join(".git/MERGE_HEAD"),
            run(root, &["rev-parse", "HEAD"]).stdout,
        )
        .unwrap();
        assert!(matches!(
            commit(root, "merge"),
            Err(GitError::GitOperation(_))
        ));
    }

    #[test]
    fn status_distinguishes_staged_from_unstaged_changes() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        init_repo(root);
        std::fs::write(root.join("tracked.txt"), "one\n").unwrap();
        std::fs::write(root.join("staged.txt"), "one\n").unwrap();
        run(root, &["add", "tracked.txt", "staged.txt"]);
        run(root, &["commit", "-m", "base"]);

        // `tracked.txt` gets an unstaged worktree edit; `staged.txt` gets an
        // edit that is added to the index (staged) but not committed.
        std::fs::write(root.join("tracked.txt"), "two\n").unwrap();
        std::fs::write(root.join("staged.txt"), "two\n").unwrap();
        run(root, &["add", "staged.txt"]);

        let result = status(root).unwrap();
        let tracked = result
            .files
            .iter()
            .find(|f| f.path == "tracked.txt")
            .unwrap();
        let staged = result
            .files
            .iter()
            .find(|f| f.path == "staged.txt")
            .unwrap();
        assert!(!tracked.staged, "worktree-only edit reported as staged");
        assert!(staged.staged, "index edit not reported as staged");
    }

    #[test]
    fn branches_returns_single_branch_and_marks_current() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        init_repo(root);
        std::fs::write(root.join("file.txt"), "hello\n").unwrap();
        run(root, &["add", "file.txt"]);
        run(root, &["commit", "-m", "initial commit"]);

        let branch_list = branches(root).unwrap();
        assert_eq!(branch_list.len(), 1);

        let main_branch = &branch_list[0];
        assert_eq!(main_branch.name, "main");
        assert!(main_branch.is_current);

        let head_sha = String::from_utf8(run(root, &["rev-parse", "HEAD"]).stdout).unwrap();
        assert_eq!(main_branch.head, head_sha.trim());
    }

    #[test]
    fn branches_lists_multiple_branches_and_identifies_current() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        init_repo(root);
        std::fs::write(root.join("file.txt"), "hello\n").unwrap();
        run(root, &["add", "file.txt"]);
        run(root, &["commit", "-m", "initial commit"]);
        let main_sha = String::from_utf8(run(root, &["rev-parse", "HEAD"]).stdout)
            .unwrap()
            .trim()
            .to_string();

        run(root, &["branch", "feature"]);

        let branch_list = branches(root).unwrap();
        assert_eq!(branch_list.len(), 2);

        let main_b = branch_list.iter().find(|b| b.name == "main").unwrap();
        let feature_b = branch_list.iter().find(|b| b.name == "feature").unwrap();

        assert!(main_b.is_current);
        assert!(!feature_b.is_current);
        assert_eq!(main_b.head, main_sha);
        assert_eq!(feature_b.head, main_sha);

        run(root, &["checkout", "feature"]);
        std::fs::write(root.join("file.txt"), "feature update\n").unwrap();
        run(root, &["add", "file.txt"]);
        run(root, &["commit", "-m", "feature commit"]);
        let feature_sha = String::from_utf8(run(root, &["rev-parse", "HEAD"]).stdout)
            .unwrap()
            .trim()
            .to_string();

        let updated_branch_list = branches(root).unwrap();
        let updated_main = updated_branch_list
            .iter()
            .find(|b| b.name == "main")
            .unwrap();
        let updated_feature = updated_branch_list
            .iter()
            .find(|b| b.name == "feature")
            .unwrap();

        assert!(!updated_main.is_current);
        assert!(updated_feature.is_current);
        assert_eq!(updated_main.head, main_sha);
        assert_eq!(updated_feature.head, feature_sha);
    }

    #[test]
    fn branches_handles_detached_head() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        init_repo(root);
        std::fs::write(root.join("file.txt"), "first\n").unwrap();
        run(root, &["add", "file.txt"]);
        run(root, &["commit", "-m", "first commit"]);
        let first_sha = String::from_utf8(run(root, &["rev-parse", "HEAD"]).stdout)
            .unwrap()
            .trim()
            .to_string();

        std::fs::write(root.join("file.txt"), "second\n").unwrap();
        run(root, &["add", "file.txt"]);
        run(root, &["commit", "-m", "second commit"]);

        run(root, &["checkout", "--detach", &first_sha]);

        let branch_list = branches(root).unwrap();
        assert!(!branch_list.is_empty());
        for branch in &branch_list {
            assert!(
                !branch.is_current,
                "branch {} should not be current in detached HEAD state",
                branch.name
            );
        }
    }

    #[test]
    fn branches_returns_error_on_invalid_repo_path() {
        let temp = tempfile::tempdir().unwrap();
        let non_repo = temp.path().join("not_a_repo");
        std::fs::create_dir_all(&non_repo).unwrap();

        let result = branches(&non_repo);
        assert!(matches!(result, Err(GitError::GitOperation(_))));
    }

    #[test]
    fn blame_multi_commit_attributes_lines_correctly() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        init_repo(root);

        std::fs::write(root.join("file.txt"), "line 1\nline 2\n").unwrap();
        run(root, &["add", "file.txt"]);
        run(root, &["commit", "-m", "first commit"]);
        let commit1 = String::from_utf8(run(root, &["rev-parse", "HEAD"]).stdout)
            .unwrap()
            .trim()
            .to_string();

        std::fs::write(root.join("file.txt"), "line 1 updated\nline 2\nline 3\n").unwrap();
        run(root, &["add", "file.txt"]);
        run(root, &["commit", "-m", "second commit"]);
        let commit2 = String::from_utf8(run(root, &["rev-parse", "HEAD"]).stdout)
            .unwrap()
            .trim()
            .to_string();

        let res = blame(root, "file.txt").unwrap();
        assert_eq!(res.lines.len(), 3);

        assert_eq!(res.lines[0].line_number, 1);
        assert_eq!(res.lines[0].content, "line 1 updated\n");
        assert_eq!(res.lines[0].author, "Test");
        assert_eq!(res.lines[0].commit_id, commit2);

        assert_eq!(res.lines[1].line_number, 2);
        assert_eq!(res.lines[1].content, "line 2\n");
        assert_eq!(res.lines[1].author, "Test");
        assert_eq!(res.lines[1].commit_id, commit1);

        assert_eq!(res.lines[2].line_number, 3);
        assert_eq!(res.lines[2].content, "line 3\n");
        assert_eq!(res.lines[2].author, "Test");
        assert_eq!(res.lines[2].commit_id, commit2);
    }

    #[test]
    fn blame_error_cases_handled_properly() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        init_repo(root);

        assert!(matches!(
            blame(root, ""),
            Err(GitError::FileNotFound(msg)) if msg == FILE_PATH_EMPTY_ERROR
        ));

        assert!(matches!(
            blame(root, "missing.txt"),
            Err(GitError::FileNotFound(_))
        ));

        std::fs::create_dir(root.join("subdir")).unwrap();
        assert!(matches!(
            blame(root, "subdir"),
            Err(GitError::FileNotFound(_))
        ));

        let outside_temp = tempfile::tempdir().unwrap();
        let outside_file = outside_temp.path().join("outside.txt");
        std::fs::write(&outside_file, "outside\n").unwrap();
        assert!(matches!(
            blame(root, outside_file.to_str().unwrap()),
            Err(GitError::FileNotFound(_))
        ));

        let bare_temp = tempfile::tempdir().unwrap();
        let bare_root = bare_temp.path();
        run(bare_root, &["init", "--bare"]);
        assert!(matches!(
            blame(bare_root, "file.txt"),
            Err(GitError::BareRepo)
        ));
    }
    #[test]
    fn log_returns_error_for_invalid_path() {
        let temp = tempfile::tempdir().unwrap();
        let invalid_path = temp.path().join("nonexistent");
        assert!(matches!(
            log(&invalid_path, 10),
            Err(GitError::GitOperation(_))
        ));
    }

    #[test]
    fn log_returns_error_for_repo_without_commits() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        init_repo(root);
        assert!(matches!(log(root, 10), Err(GitError::GitOperation(_))));
    }

    #[test]
    fn log_returns_commits_in_reverse_chronological_order_and_respects_count() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        init_repo(root);

        std::fs::write(root.join("file.txt"), "one\n").unwrap();
        add(root, &["file.txt".to_string()]).unwrap();
        let first_id = commit(root, "first commit\n\nbody text").unwrap();

        std::fs::write(root.join("file.txt"), "two\n").unwrap();
        add(root, &["file.txt".to_string()]).unwrap();
        let second_id = commit(root, "second commit").unwrap();

        std::fs::write(root.join("file.txt"), "three\n").unwrap();
        add(root, &["file.txt".to_string()]).unwrap();
        let third_id = commit(root, "third commit").unwrap();

        let history = log(root, 10).unwrap();
        assert_eq!(history.len(), 3);
        assert_eq!(history[0].id, third_id);
        assert_eq!(history[0].message, "third commit\n");
        assert_eq!(history[0].author, "Test");
        assert_eq!(history[0].email, "test@example.com");
        assert!(history[0].time > 0);
        assert_eq!(history[1].id, second_id);
        assert_eq!(history[1].message, "second commit\n");
        assert_eq!(history[2].id, first_id);
        assert_eq!(history[2].message, "first commit\n\nbody text\n");

        let limited = log(root, 2).unwrap();
        assert_eq!(limited.len(), 2);
        assert_eq!(limited[0].id, third_id);
        assert_eq!(limited[1].id, second_id);

        let single = log(root, 1).unwrap();
        assert_eq!(single.len(), 1);
        assert_eq!(single[0].id, third_id);
        assert!(log(root, 0).unwrap().is_empty());
    }
}

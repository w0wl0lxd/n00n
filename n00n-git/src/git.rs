use std::path::Path;
use std::process::Command;

use serde::{Deserialize, Serialize};
use tracing::instrument;

use crate::error::GitError;

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
    pub additions: u64,
    pub deletions: u64,
    pub changes: Vec<DiffLine>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiffLine {
    pub line_number: u32,
    pub content: String,
    pub change_type: String,
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
/// Uses git CLI for status operations.
#[instrument(skip(path))]
pub fn status(path: &Path) -> Result<GitStatus, GitError> {
    let output = Command::new("git")
        .arg("-C")
        .arg(path)
        .arg("status")
        .arg("--porcelain")
        .arg("-b")
        .output()
        .map_err(|e| GitError::GitOperation(format!("git status failed: {e}")))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(GitError::GitOperation(format!(
            "git status failed: {stderr}"
        )));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut branch = None;
    let mut files = Vec::new();

    for line in stdout.lines() {
        if line.starts_with("## ") {
            let rest = &line[3..];
            if let Some(branch_name) = rest.split_whitespace().next() {
                branch = Some(branch_name.to_string());
            }
        } else if !line.is_empty() {
            let chars: Vec<char> = line.chars().collect();
            let index_status = chars.get(0).unwrap_or(&' ');
            let worktree_status = chars.get(1).unwrap_or(&' ');
            let file_path = if chars.len() > 3 { &line[3..] } else { line };

            let staged = *index_status != ' ';
            let status = if *worktree_status != ' ' {
                worktree_status.to_string()
            } else {
                index_status.to_string()
            };

            files.push(FileStatus {
                path: file_path.to_string(),
                status,
                staged,
            });
        }
    }

    Ok(GitStatus { branch, files })
}

/// Get commit history for a repository.
///
/// Uses git CLI for log operations.
#[instrument(skip(path))]
pub fn log(path: &Path, count: usize) -> Result<Vec<GitCommit>, GitError> {
    let output = Command::new("git")
        .arg("-C")
        .arg(path)
        .arg("log")
        .arg(format!("-{}", count))
        .arg("--pretty=format:%H|%an|%ae|%at|%s")
        .output()
        .map_err(|e| GitError::GitOperation(format!("git log failed: {e}")))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(GitError::GitOperation(format!("git log failed: {stderr}")));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut commits = Vec::new();

    for line in stdout.lines() {
        let parts: Vec<&str> = line.split('|').collect();
        if parts.len() >= 5 {
            let time = parts[3].parse::<i64>().unwrap_or(0);
            commits.push(GitCommit {
                id: parts[0].to_string(),
                author: parts[1].to_string(),
                email: parts[2].to_string(),
                time,
                message: parts[4].to_string(),
            });
        }
    }

    Ok(commits)
}

/// Get diff between two references.
///
/// Uses git CLI for diff operations.
#[instrument(skip(path))]
pub fn diff(path: &Path, ref_a: &str, ref_b: &str) -> Result<GitDiff, GitError> {
    let output = Command::new("git")
        .arg("-C")
        .arg(path)
        .arg("diff")
        .arg("--numstat")
        .arg(format!("{}..{}", ref_a, ref_b))
        .output()
        .map_err(|e| GitError::GitOperation(format!("git diff failed: {e}")))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(GitError::GitOperation(format!("git diff failed: {stderr}")));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut files = Vec::new();

    for line in stdout.lines() {
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.len() >= 3 {
            let additions = parts[0].parse::<u64>().unwrap_or(0);
            let deletions = parts[1].parse::<u64>().unwrap_or(0);
            let path = parts[2].to_string();

            files.push(FileDiff {
                path,
                additions,
                deletions,
                changes: Vec::new(),
            });
        }
    }

    Ok(GitDiff { files })
}

/// List branches in a repository.
///
/// Uses git CLI for branch operations.
#[instrument(skip(path))]
pub fn branches(path: &Path) -> Result<Vec<GitBranch>, GitError> {
    let output = Command::new("git")
        .arg("-C")
        .arg(path)
        .arg("branch")
        .arg("--format=%(refname:short)|%(objectname)")
        .output()
        .map_err(|e| GitError::GitOperation(format!("git branch failed: {e}")))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(GitError::GitOperation(format!(
            "git branch failed: {stderr}"
        )));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut branches = Vec::new();

    let current_output = Command::new("git")
        .arg("-C")
        .arg(path)
        .arg("rev-parse")
        .arg("--abbrev-ref")
        .arg("HEAD")
        .output()
        .map_err(|e| GitError::GitOperation(format!("git rev-parse failed: {e}")))?;

    let current_branch = if current_output.status.success() {
        String::from_utf8_lossy(&current_output.stdout)
            .trim()
            .to_string()
    } else {
        String::new()
    };

    for line in stdout.lines() {
        let parts: Vec<&str> = line.split('|').collect();
        if parts.len() >= 2 {
            let name = parts[0].to_string();
            let head = parts[1].to_string();
            let is_current = name == current_branch;

            branches.push(GitBranch {
                name,
                head,
                is_current,
            });
        }
    }

    Ok(branches)
}

/// Get blame information for a file.
///
/// Uses git CLI for blame operations.
#[instrument(skip(path))]
pub fn blame(path: &Path, file: &str) -> Result<GitBlame, GitError> {
    let file_path = path.join(file);

    let _content = std::fs::read_to_string(&file_path)
        .map_err(|e| GitError::FileNotFound(format!("{}: {}", file, e)))?;

    let output = Command::new("git")
        .arg("-C")
        .arg(path)
        .arg("blame")
        .arg("--line-porcelain")
        .arg(file)
        .output()
        .map_err(|e| GitError::GitOperation(format!("git blame failed: {e}")))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(GitError::GitOperation(format!(
            "git blame failed: {stderr}"
        )));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut lines = Vec::new();
    let mut current_commit_id = String::new();
    let mut current_author = String::new();
    let mut current_time = 0i64;

    for line in stdout.lines() {
        if line.starts_with('\t') {
            let content = line[1..].to_string();
            let line_number = (lines.len() + 1) as u32;
            lines.push(BlameLine {
                line_number,
                content,
                commit_id: current_commit_id.clone(),
                author: current_author.clone(),
                time: current_time,
            });
        } else if let Some(rest) = line.strip_prefix("author ") {
            current_author = rest.to_string();
        } else if let Some(rest) = line.strip_prefix("author-time ") {
            current_time = rest.parse().unwrap_or(0);
        } else if !line.contains(' ') && !line.is_empty() {
            current_commit_id = line.split_whitespace().next().unwrap_or("").to_string();
        }
    }

    Ok(GitBlame { lines })
}

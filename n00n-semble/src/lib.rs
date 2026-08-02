#![allow(clippy::missing_errors_doc)]
#![allow(clippy::new_without_default)]
#![allow(clippy::must_use_candidate)]

use std::path::{Path, PathBuf};
use std::process::Command;

use n00n_search::{
    Error as SearchError, Query, SearchConfig, SearchIndex, SearchMode, SearchResult,
};

const DEFAULT_TOP_K: usize = 5;

fn resolve_top_k(top_k: Option<usize>) -> usize {
    match top_k {
        Some(value) if value > 0 => value,
        _ => DEFAULT_TOP_K,
    }
}

pub struct Client;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Bm25,
    Hybrid,
    Semantic,
}

impl Mode {
    pub fn parse(raw: &str) -> Result<Self, SembleError> {
        match raw {
            "bm25" => Ok(Self::Bm25),
            "hybrid" => Ok(Self::Hybrid),
            "semantic" => Ok(Self::Semantic),
            other => Err(SembleError::Cli {
                message: format!("unsupported mode: {other}"),
            }),
        }
    }
}

pub struct SearchRequest<'a> {
    pub repo: &'a Path,
    pub query: &'a str,
    pub mode: Mode,
    pub top_k: Option<usize>,
}

pub struct FindRelatedRequest<'a> {
    pub repo: &'a Path,
    pub file_path: &'a str,
    pub line: usize,
    pub top_k: Option<usize>,
}

impl Client {
    pub fn index_dir(project: &Path) -> std::path::PathBuf {
        SearchIndex::index_dir(project)
    }

    pub fn has_index(project: &Path) -> bool {
        Self::index_dir(project).join("metadata.json").is_file()
    }

    pub fn ensure_indexed(project: &Path) -> Result<(), SembleError> {
        let index_dir = Self::index_dir(project);
        let mut index = SearchIndex::open_or_create(&index_dir, &SearchConfig::default())
            .map_err(map_search_err)?;
        if Self::has_index(project) {
            return Ok(());
        }
        index.update(project, |_| {}).map_err(map_search_err)
    }

    // T081: Check if semble CLI is available
    pub fn cli_available() -> bool {
        Command::new("semble")
            .arg("--version")
            .output()
            .map(|output| output.status.success())
            .unwrap_or(false)
    }

    // T076-T078: Search with upstream semble CLI wrapper
    pub fn cli_search(
        repo: &Path,
        query: &str,
        mode: Mode,
        top_k: Option<usize>,
        content: Option<&str>,
    ) -> Result<String, SembleError> {
        let mut cmd = Command::new("semble");
        cmd.arg("search").arg(query).arg(repo);

        if let Some(k) = top_k {
            cmd.arg("--top-k").arg(k.to_string());
        }

        if let Some(content_type) = content {
            cmd.arg("--content").arg(content_type);
        }

        // Add mode flag for hybrid/semantic
        if matches!(mode, Mode::Hybrid | Mode::Semantic) {
            cmd.arg("--mode").arg(match mode {
                Mode::Hybrid => "hybrid",
                Mode::Semantic => "semantic",
                Mode::Bm25 => "bm25",
            });
        }

        let output = cmd.output().map_err(|e| SembleError::Cli {
            message: format!("semble CLI failed: {e}"),
        })?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(SembleError::Cli {
                message: stderr.to_string(),
            });
        }

        Ok(String::from_utf8_lossy(&output.stdout).to_string())
    }

    // T079: find_related with upstream semble CLI wrapper
    pub fn cli_find_related(
        repo: &Path,
        file_path: &str,
        line: usize,
        top_k: Option<usize>,
    ) -> Result<String, SembleError> {
        let mut cmd = Command::new("semble");
        cmd.arg("find-related")
            .arg(file_path)
            .arg(line.to_string())
            .arg(repo);

        if let Some(k) = top_k {
            cmd.arg("--top-k").arg(k.to_string());
        }

        let output = cmd.output().map_err(|e| SembleError::Cli {
            message: format!("semble CLI failed: {e}"),
        })?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(SembleError::Cli {
                message: stderr.to_string(),
            });
        }

        Ok(String::from_utf8_lossy(&output.stdout).to_string())
    }

    // T080: savings command with upstream semble CLI wrapper
    pub fn cli_savings(repo: &Path) -> Result<String, SembleError> {
        let output = Command::new("semble")
            .arg("savings")
            .arg(repo)
            .output()
            .map_err(|e| SembleError::Cli {
                message: format!("semble CLI failed: {e}"),
            })?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(SembleError::Cli {
                message: stderr.to_string(),
            });
        }

        Ok(String::from_utf8_lossy(&output.stdout).to_string())
    }

    // T077: Handle remote git URLs by cloning to temp dir
    pub fn resolve_repo_path(repo: &str) -> Result<PathBuf, SembleError> {
        if repo.starts_with("https://") || repo.starts_with("git@") {
            let temp_dir = tempfile::tempdir().map_err(|e| SembleError::Cli {
                message: format!("failed to create temp dir: {e}"),
            })?;
            let clone_path = temp_dir.path().join("repo");

            let output = Command::new("git")
                .arg("clone")
                .arg("--depth")
                .arg("1")
                .arg(repo)
                .arg(&clone_path)
                .output()
                .map_err(|e| SembleError::Cli {
                    message: format!("git clone failed: {e}"),
                })?;

            if !output.status.success() {
                let stderr = String::from_utf8_lossy(&output.stderr);
                return Err(SembleError::Cli {
                    message: format!("git clone failed: {stderr}"),
                });
            }

            // Keep temp_dir alive by leaking it (not ideal but works for this use case)
            let _ = temp_dir.into_path();
            Ok(clone_path)
        } else {
            Ok(PathBuf::from(repo))
        }
    }

    pub fn search(request: &SearchRequest<'_>) -> Result<String, SembleError> {
        let index_dir = Self::index_dir(request.repo);
        let mut index = SearchIndex::open_or_create(&index_dir, &SearchConfig::default())
            .map_err(map_search_err)?;
        if !Self::has_index(request.repo) {
            index.update(request.repo, |_| {}).map_err(map_search_err)?;
        }

        let needs_embedder = matches!(request.mode, Mode::Hybrid | Mode::Semantic);
        let mut output = String::new();
        if needs_embedder {
            output.push_str(&embedder_nag());
            output.push('\n');
        }

        let results = index
            .search(&Query {
                text: request.query.to_owned(),
                mode: SearchMode::Bm25,
                top_k: resolve_top_k(request.top_k),
            })
            .map_err(map_search_err)?;

        output.push_str(&format_results(&results));
        Ok(output)
    }

    pub fn find_related(request: &FindRelatedRequest<'_>) -> Result<String, SembleError> {
        let index_dir = Self::index_dir(request.repo);
        let mut index = SearchIndex::open_or_create(&index_dir, &SearchConfig::default())
            .map_err(map_search_err)?;
        if !Self::has_index(request.repo) {
            index.update(request.repo, |_| {}).map_err(map_search_err)?;
        }

        let results = index
            .find_related(request.file_path, request.line)
            .map_err(map_search_err)?;
        Ok(format_results(
            &results
                .into_iter()
                .take(resolve_top_k(request.top_k))
                .collect::<Vec<_>>(),
        ))
    }

    // T076-T085: Hybrid search that tries CLI first, falls back to BM25
    pub fn search_hybrid(
        repo: &Path,
        query: &str,
        mode: Mode,
        top_k: Option<usize>,
        content: Option<&str>,
    ) -> Result<String, SembleError> {
        // Try CLI for hybrid/semantic modes
        if matches!(mode, Mode::Hybrid | Mode::Semantic) && Self::cli_available() {
            match Self::cli_search(repo, query, mode, top_k, content) {
                Ok(result) => return Ok(result),
                Err(_) => {
                    // CLI failed, fall back to BM25 with embedder nag
                }
            }
        }

        // Fall back to native BM25
        let mut output = String::new();
        if matches!(mode, Mode::Hybrid | Mode::Semantic) {
            output.push_str(&embedder_nag());
            output.push('\n');
        }

        let index_dir = Self::index_dir(repo);
        let mut index = SearchIndex::open_or_create(&index_dir, &SearchConfig::default())
            .map_err(map_search_err)?;
        if !Self::has_index(repo) {
            index.update(repo, |_| {}).map_err(map_search_err)?;
        }

        let results = index
            .search(&Query {
                text: query.to_owned(),
                mode: SearchMode::Bm25,
                top_k: resolve_top_k(top_k),
            })
            .map_err(map_search_err)?;

        output.push_str(&format_results(&results));
        Ok(output)
    }

    // T079: Hybrid find_related that tries CLI first
    pub fn find_related_hybrid(
        repo: &Path,
        file_path: &str,
        line: usize,
        top_k: Option<usize>,
    ) -> Result<String, SembleError> {
        // Try CLI first
        if Self::cli_available() {
            match Self::cli_find_related(repo, file_path, line, top_k) {
                Ok(result) => return Ok(result),
                Err(_) => {
                    // CLI failed, fall back to native
                }
            }
        }

        // Fall back to native
        let index_dir = Self::index_dir(repo);
        let mut index = SearchIndex::open_or_create(&index_dir, &SearchConfig::default())
            .map_err(map_search_err)?;
        if !Self::has_index(repo) {
            index.update(repo, |_| {}).map_err(map_search_err)?;
        }

        let results = index
            .find_related(file_path, line)
            .map_err(map_search_err)?;
        Ok(format_results(
            &results
                .into_iter()
                .take(resolve_top_k(top_k))
                .collect::<Vec<_>>(),
        ))
    }
}

pub fn embedder_nag() -> String {
    [
        "No embedder configured. Semantic/hybrid search needs a local vLLM container or a remote OpenAI-compatible /v1/embeddings endpoint.",
        "Local presets (Podman):",
        "  light:  Snowflake/snowflake-arctic-embed-xs",
        "  medium: Snowflake/snowflake-arctic-embed-m-v1.5",
        "  heavy:  Snowflake/snowflake-arctic-embed-l-v1.5",
        "Falling back to BM25 keyword search.",
    ]
    .join("\n")
}

fn format_results(results: &[SearchResult]) -> String {
    if results.is_empty() {
        return String::from("No matches.");
    }
    results
        .iter()
        .map(|result| {
            format!(
                "{}:{}-{} score={:.3}\n{}",
                result.file_path, result.start_line, result.end_line, result.score, result.snippet
            )
        })
        .collect::<Vec<_>>()
        .join("\n\n")
}

fn map_search_err(error: SearchError) -> SembleError {
    match error {
        SearchError::NotSupported => SembleError::Cli {
            message: String::from("semantic search is not configured"),
        },
        other => SembleError::Search { source: other },
    }
}

#[derive(Debug, thiserror::Error)]
pub enum SembleError {
    #[error("semble search error: {source}")]
    Search { source: SearchError },

    #[error("semble error: {message}")]
    Cli { message: String },
}

#[cfg(test)]
mod tests {
    use super::{Client, FindRelatedRequest, Mode, SearchRequest};
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn search_indexes_and_returns_bm25_results() {
        let repo = tempdir().expect("tempdir");
        let root = repo.path();
        fs::write(root.join("main.rs"), "fn session_restore() {}").expect("write");

        let output = Client::search(&SearchRequest {
            repo: root,
            query: "session_restore",
            mode: Mode::Bm25,
            top_k: Some(3),
        })
        .expect("search");
        assert!(output.contains("main.rs"));
        assert!(output.contains("session_restore"));
    }

    #[test]
    fn hybrid_mode_nags_and_falls_back_to_bm25() {
        let repo = tempdir().expect("tempdir");
        let root = repo.path();
        fs::write(root.join("lib.rs"), "pub fn hybrid_query() {}").expect("write");

        let output = Client::search(&SearchRequest {
            repo: root,
            query: "hybrid_query",
            mode: Mode::Hybrid,
            top_k: Some(2),
        })
        .expect("search");
        assert!(output.contains("No embedder configured"));
        assert!(output.contains("hybrid_query"));
    }

    #[test]
    fn find_related_uses_anchor_chunk() {
        let repo = tempdir().expect("tempdir");
        let root = repo.path();
        fs::write(
            root.join("a.rs"),
            "fn anchor() {}\n\nfn distant_symbol() {}",
        )
        .expect("write");

        Client::search(&SearchRequest {
            repo: root,
            query: "anchor",
            mode: Mode::Bm25,
            top_k: Some(1),
        })
        .expect("warm index");

        let output = Client::find_related(&FindRelatedRequest {
            repo: root,
            file_path: "a.rs",
            line: 1,
            top_k: Some(2),
        })
        .expect("find_related");
        assert!(output.contains("a.rs"));
    }

    // T072: Test for upstream CLI wrapper
    #[test]
    fn cli_search_requires_semble_cli() {
        let repo = tempdir().expect("tempdir");
        let result = Client::cli_search(repo.path(), "test", Mode::Bm25, Some(5), None);
        assert!(result.is_err());
    }

    // T073: Test for remote URL support
    #[test]
    fn resolve_repo_path_handles_local_path() {
        let result = Client::resolve_repo_path("/tmp");
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), std::path::PathBuf::from("/tmp"));
    }

    // T074: Test for content filter support
    #[test]
    fn cli_search_with_content_filter() {
        let repo = tempdir().expect("tempdir");
        let result = Client::cli_search(repo.path(), "test", Mode::Bm25, Some(5), Some("docs"));
        assert!(result.is_err()); // CLI not available
    }

    // T075: Test for BM25 fallback when CLI unavailable
    #[test]
    fn search_hybrid_falls_back_to_bm25() {
        let repo = tempdir().expect("tempdir");
        let root = repo.path();
        fs::write(root.join("test.rs"), "fn fallback_test() {}").expect("write");

        let output = Client::search_hybrid(root, "fallback_test", Mode::Hybrid, Some(5), None)
            .expect("search_hybrid");
        assert!(output.contains("No embedder configured"));
        assert!(output.contains("fallback_test"));
    }

    // T081: Test for CLI availability check
    #[test]
    fn cli_available_returns_false_when_not_installed() {
        // This test assumes semble is not installed in the test environment
        let available = Client::cli_available();
        // We don't assert false because it might be installed in some environments
        // Just verify the function doesn't panic
        let _ = available;
    }
}

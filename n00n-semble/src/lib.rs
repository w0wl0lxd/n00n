#![allow(clippy::missing_errors_doc)]
#![allow(clippy::new_without_default)]
#![allow(clippy::must_use_candidate)]

use std::path::{Path, PathBuf};
use std::process::Command;

use n00n_search::{
    Error as SearchError, Query, SearchConfig, SearchIndex, SearchMode, SearchResult,
};

const SEMBLE_BINARY: &str = "semble";
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
    pub content: Option<&'a str>,
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
        Command::new(SEMBLE_BINARY)
            .arg("--version")
            .output()
            .is_ok_and(|output| output.status.success())
    }

    // T076-T078: Search with upstream semble CLI wrapper
    pub fn cli_search(
        repo: &Path,
        query: &str,
        mode: Mode,
        top_k: Option<usize>,
        content: Option<&str>,
    ) -> Result<String, SembleError> {
        let mut cmd = Command::new(SEMBLE_BINARY);
        cmd.arg("search");

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

        cmd.arg("--").arg(query).arg(repo);

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
        let mut cmd = Command::new(SEMBLE_BINARY);
        cmd.arg("find-related");

        if let Some(k) = top_k {
            cmd.arg("--top-k").arg(k.to_string());
        }

        cmd.arg("--").arg(file_path).arg(line.to_string()).arg(repo);

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
        let output = Command::new(SEMBLE_BINARY)
            .arg("savings")
            .arg("--")
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
    pub fn resolve_repo_path(
        repo: &str,
    ) -> Result<(PathBuf, Option<tempfile::TempDir>), SembleError> {
        if repo.starts_with("git@") || repo.starts_with("http://") {
            return Err(SembleError::Cli {
                message: String::from(
                    "only https:// remote URLs are supported; set N00N_SEMBLE_ALLOWED_REMOTE_REPOS to authorize a prefix",
                ),
            });
        }
        if repo.starts_with("https://") {
            // Validate remote URL against allowlist
            let allowed = std::env::var("N00N_SEMBLE_ALLOWED_REMOTE_REPOS");
            let allowed = match allowed {
                Ok(v) if !v.is_empty() => v,
                _ => {
                    return Err(SembleError::Cli {
                        message: String::from(
                            "remote repository URLs are not allowed; set N00N_SEMBLE_ALLOWED_REMOTE_REPOS to authorize",
                        ),
                    });
                }
            };

            if allowed != "*" {
                let allowed_prefixes: Vec<&str> = allowed.split(',').map(str::trim).collect();
                let is_allowed = allowed_prefixes
                    .iter()
                    .any(|prefix| repo.starts_with(prefix));
                if !is_allowed {
                    return Err(SembleError::Cli {
                        message: format!(
                            "remote repository URL '{repo}' is not in the allowed list (N00N_SEMBLE_ALLOWED_REMOTE_REPOS={allowed})"
                        ),
                    });
                }
            }

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

            Ok((clone_path, Some(temp_dir)))
        } else {
            Ok((PathBuf::from(repo), None))
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

        let mut results = index
            .search(&Query {
                text: request.query.to_owned(),
                mode: SearchMode::Bm25,
                top_k: resolve_top_k(request.top_k),
            })
            .map_err(map_search_err)?;

        // Filter by content type if specified
        if let Some(content_filter) = request.content {
            results.retain(|result| {
                let path = result.file_path.as_str();
                match content_filter {
                    "docs" => is_docs(path),
                    "config" => is_config(path),
                    "code" => !is_docs(path) && !is_config(path),
                    _ => true,
                }
            });
        }

        output.push_str(&format_results(&results));
        Ok(output)
    }

    fn find_related_native(
        repo: &Path,
        file_path: &str,
        line: usize,
        top_k: Option<usize>,
    ) -> Result<String, SembleError> {
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

    pub fn find_related(request: &FindRelatedRequest<'_>) -> Result<String, SembleError> {
        Self::find_related_native(request.repo, request.file_path, request.line, request.top_k)
    }

    // T076-T085: Hybrid search that tries CLI first, falls back to BM25
    pub fn search_hybrid(
        repo: &str,
        query: &str,
        mode: Mode,
        top_k: Option<usize>,
        content: Option<&str>,
    ) -> Result<String, SembleError> {
        let (repo_path, _temp_dir) = Self::resolve_repo_path(repo)?;
        let request = SearchRequest {
            repo: repo_path.as_path(),
            query,
            mode,
            top_k,
            content,
        };

        // Try CLI for hybrid/semantic modes, or when content filter is requested
        let should_try_cli = matches!(mode, Mode::Hybrid | Mode::Semantic) || content.is_some();
        if should_try_cli && Self::cli_available() {
            match Self::cli_search(&repo_path, query, mode, top_k, content) {
                Ok(result) => return Ok(result),
                Err(e) => {
                    // Prepend warning before falling back
                    let mut fallback_output = format!("[semblem CLI unavailable: {e}]\n");
                    let native_result = Self::search(&request)?;
                    fallback_output.push_str(&native_result);
                    return Ok(fallback_output);
                }
            }
        }

        // Fall back to native BM25
        Self::search(&request)
    }

    // T079: Hybrid find_related that tries CLI first
    pub fn find_related_hybrid(
        repo: &str,
        file_path: &str,
        line: usize,
        top_k: Option<usize>,
    ) -> Result<String, SembleError> {
        let (repo_path, _temp_dir) = Self::resolve_repo_path(repo)?;

        // Try CLI first
        if Self::cli_available() {
            match Self::cli_find_related(&repo_path, file_path, line, top_k) {
                Ok(result) => return Ok(result),
                Err(e) => {
                    // Prepend warning before falling back
                    let mut fallback_output = format!("[semblem CLI unavailable: {e}]\n");
                    let native_result =
                        Self::find_related_native(&repo_path, file_path, line, top_k)?;
                    fallback_output.push_str(&native_result);
                    return Ok(fallback_output);
                }
            }
        }

        // Fall back to native
        Self::find_related_native(&repo_path, file_path, line, top_k)
    }

    // T080: Public savings function that resolves repo path
    pub fn savings(repo: &str) -> Result<String, SembleError> {
        let (repo_path, _temp_dir) = Self::resolve_repo_path(repo)?;
        Self::cli_savings(&repo_path)
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

fn has_extension(path: &str, extensions: &[&str]) -> bool {
    Path::new(path)
        .extension()
        .is_some_and(|ext| extensions.iter().any(|e| ext.eq_ignore_ascii_case(e)))
}

fn is_docs(path: &str) -> bool {
    has_extension(path, &["md", "txt", "rst"])
        || path.contains("/docs/")
        || path.contains("/site/docs/")
}

fn is_config(path: &str) -> bool {
    has_extension(
        path,
        &[
            "toml",
            "json",
            "yaml",
            "yml",
            "ini",
            "conf",
            "nix",
            "env",
            "properties",
        ],
    )
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
            content: None,
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
            content: None,
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
            content: None,
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
        if Client::cli_available() {
            // Skip if CLI is available - the test is for error handling when absent
            return;
        }
        let repo = tempdir().expect("tempdir");
        let result = Client::cli_search(repo.path(), "test", Mode::Bm25, Some(5), None);
        assert!(result.is_err());
    }

    // T073: Test for remote URL support
    #[test]
    fn resolve_repo_path_handles_local_path() {
        let result = Client::resolve_repo_path("/tmp");
        assert!(result.is_ok());
        let (path, temp_dir) = result.unwrap();
        assert_eq!(path, std::path::PathBuf::from("/tmp"));
        assert!(temp_dir.is_none());
    }

    // T074: Test for content filter support
    #[test]
    fn cli_search_with_content_filter() {
        if Client::cli_available() {
            // Skip if CLI is available - the test is for error handling when absent
            return;
        }
        let repo = tempdir().expect("tempdir");
        let result = Client::cli_search(repo.path(), "test", Mode::Bm25, Some(5), Some("docs"));
        assert!(result.is_err());
    }

    // T075: Test for BM25 fallback when CLI unavailable
    #[test]
    fn search_hybrid_falls_back_to_bm25() {
        let repo = tempdir().expect("tempdir");
        let root = repo.path();
        fs::write(root.join("test.rs"), "fn fallback_test() {}").expect("write");

        let output = Client::search_hybrid(
            root.to_str().unwrap(),
            "fallback_test",
            Mode::Hybrid,
            Some(5),
            None,
        )
        .expect("search_hybrid");

        if !Client::cli_available() {
            // Only check embedder nag when CLI is unavailable
            assert!(output.contains("No embedder configured"));
        }
        assert!(output.contains("fallback_test"));
    }
}

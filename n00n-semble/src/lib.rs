#![allow(clippy::missing_errors_doc)]
#![allow(clippy::new_without_default)]
#![allow(clippy::must_use_candidate)]

use std::path::Path;

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
}

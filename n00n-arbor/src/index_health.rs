use std::path::{Path, PathBuf};
use std::time::SystemTime;

use crate::ArborError;

const STATUS_NOT_INDEXED_MARKERS: &[&str] = &["No index", "not indexed"];
const STATUS_STALE_MARKERS: &[&str] =
    &["stale", "out of date", "out-of-date", "re-index", "reindex"];

pub fn graph_json_path(project: &Path) -> PathBuf {
    project.join(".arbor").join("graph.json")
}

pub fn graph_index_available(project: &Path) -> bool {
    graph_json_path(project).is_file()
}

pub fn status_needs_index(stdout: &str) -> bool {
    STATUS_NOT_INDEXED_MARKERS
        .iter()
        .any(|marker| stdout.contains(marker))
}

pub fn status_is_stale(stdout: &str) -> bool {
    STATUS_STALE_MARKERS
        .iter()
        .any(|marker| stdout.contains(marker))
}

pub fn graph_modified_at(project: &Path) -> Result<Option<SystemTime>, ArborError> {
    let graph_path = graph_json_path(project);
    if !graph_path.is_file() {
        return Ok(None);
    }
    let metadata = std::fs::metadata(&graph_path).map_err(|source| ArborError::Exec { source })?;
    metadata
        .modified()
        .map(Some)
        .map_err(|source| ArborError::Exec { source })
}

pub fn ensure_fresh_index(project: &Path) -> Result<(), ArborError> {
    let graph_path = graph_json_path(project);
    if !graph_path.is_file() {
        return crate::Client::ensure_indexed(project);
    }

    match crate::Client::status(project) {
        Ok(status) => {
            if status_needs_index(&status) {
                return crate::Client::ensure_indexed(project);
            }
            if status_is_stale(&status) {
                return crate::Client::reindex(project);
            }
            Ok(())
        }
        Err(ArborError::Exec { .. }) => Ok(()),
        Err(err) => Err(err),
    }
}

#[cfg(test)]
mod tests {
    use super::{status_is_stale, status_needs_index};

    #[test]
    fn status_needs_index_detects_missing_index() {
        assert!(status_needs_index("No index found for project"));
        assert!(status_needs_index("project is not indexed"));
        assert!(!status_needs_index("indexed: 100 nodes"));
    }

    #[test]
    fn status_is_stale_detects_stale_markers() {
        assert!(status_is_stale("index is stale"));
        assert!(status_is_stale("re-index recommended"));
        assert!(!status_is_stale("index is fresh"));
    }
}

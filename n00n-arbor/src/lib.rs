#![allow(clippy::missing_errors_doc)]
#![allow(clippy::new_without_default)]
#![allow(clippy::must_use_candidate)]

use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde::{Deserialize, Serialize};

mod graph_json;
mod graph_query;
mod index_health;

pub use graph_json::{GraphIndex, GraphNode, SymbolQuery, SymbolRef};
pub use graph_query::{
    graph_callees, graph_callers, graph_entry_points, graph_index_available, graph_map,
    graph_trace_path,
};
pub use index_health::{
    ensure_fresh_index, graph_index_available as graph_file_available, graph_json_path,
    graph_modified_at, status_is_stale, status_needs_index,
};

#[derive(Debug, Serialize, Deserialize)]
pub struct Relation {
    pub name: String,
    #[serde(alias = "file")]
    pub path: String,
    pub kind: Option<String>,
    pub line: Option<u64>,
}

#[derive(Debug, Deserialize)]
struct CallersResponse {
    pub callers: Vec<Relation>,
}

#[derive(Debug, Deserialize)]
struct CalleesResponse {
    pub callees: Vec<Relation>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct MapSymbol {
    pub name: String,
    pub kind: String,
    pub line: u64,
    pub centrality: Option<f64>,
    pub callers: Option<u64>,
    pub is_entry_point: Option<bool>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct MapEntry {
    pub file: String,
    pub symbols: Vec<MapSymbol>,
}

#[derive(Debug, Deserialize)]
struct MapResponse {
    entries: Vec<MapEntry>,
    #[allow(dead_code)]
    files_total: u64,
    #[allow(dead_code)]
    symbols_total: u64,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct DiffImpact {
    pub direct_callers: u64,
    pub indirect_callers: u64,
    pub blast_radius_nodes: u64,
    pub api_entrypoints_affected: u64,
    pub files_likely_require_updates: u64,
}

#[derive(Debug, Deserialize)]
struct DiffResponse {
    #[allow(dead_code)]
    changed_files: Vec<String>,
    #[allow(dead_code)]
    changed_symbols: u64,
    impact: DiffImpact,
}

fn run_arbor_cmd<I, S>(subcommand: &str, args: I) -> Result<String, ArborError>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let output = Command::new("arbor")
        .arg(subcommand)
        .arg("--")
        .args(args)
        .output()
        .map_err(|source| ArborError::Exec { source })?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(ArborError::Cli {
            message: stderr.to_string(),
        });
    }

    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

pub struct Client;

impl Client {
    pub fn new() -> Self {
        Self
    }

    pub fn check_binary() -> Result<(), ArborError> {
        let output = Command::new("arbor")
            .arg("--version")
            .output()
            .map_err(|e| ArborError::Exec { source: e })?;

        if output.status.success() {
            Ok(())
        } else {
            Err(ArborError::Cli {
                message: String::from_utf8_lossy(&output.stderr).to_string(),
            })
        }
    }

    pub fn callers(symbol: &str, project: &Path) -> Result<Vec<Relation>, ArborError> {
        let output = Command::new("arbor")
            .arg("callers")
            .arg("--json")
            .arg("--")
            .arg(symbol)
            .arg(project.as_os_str())
            .output()
            .map_err(|e| ArborError::Exec { source: e })?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(ArborError::Cli {
                message: stderr.to_string(),
            });
        }

        let resp: CallersResponse =
            serde_json::from_slice(&output.stdout).map_err(|e| ArborError::Parse { source: e })?;
        Ok(resp.callers)
    }

    pub fn callees(symbol: &str, project: &Path) -> Result<Vec<Relation>, ArborError> {
        let output = Command::new("arbor")
            .arg("callees")
            .arg("--json")
            .arg("--")
            .arg(symbol)
            .arg(project.as_os_str())
            .output()
            .map_err(|e| ArborError::Exec { source: e })?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(ArborError::Cli {
                message: stderr.to_string(),
            });
        }

        let resp: CalleesResponse =
            serde_json::from_slice(&output.stdout).map_err(|e| ArborError::Parse { source: e })?;
        Ok(resp.callees)
    }

    pub fn map(project: &Path, token_budget: Option<u64>) -> Result<Vec<MapEntry>, ArborError> {
        let mut cmd = Command::new("arbor");
        cmd.arg("map").arg("--json");
        if let Some(budget) = token_budget {
            cmd.arg("--tokens").arg(budget.to_string());
        }
        cmd.arg("--").arg(project.as_os_str());

        let output = cmd.output().map_err(|e| ArborError::Exec { source: e })?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(ArborError::Cli {
                message: stderr.to_string(),
            });
        }

        let resp: MapResponse =
            serde_json::from_slice(&output.stdout).map_err(|e| ArborError::Parse { source: e })?;
        Ok(resp.entries)
    }

    pub fn query(query: &str, project: &Path) -> Result<String, ArborError> {
        let output = Command::new("arbor")
            .arg("query")
            .arg("--")
            .arg(query)
            .arg(project.as_os_str())
            .output()
            .map_err(|e| ArborError::Exec { source: e })?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(ArborError::Cli {
                message: stderr.to_string(),
            });
        }

        Ok(String::from_utf8_lossy(&output.stdout).to_string())
    }

    pub fn status(project: &Path) -> Result<String, ArborError> {
        let output = Command::new("arbor")
            .arg("status")
            .arg("--")
            .arg(project.as_os_str())
            .output()
            .map_err(|e| ArborError::Exec { source: e })?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(ArborError::Cli {
                message: stderr.to_string(),
            });
        }

        Ok(String::from_utf8_lossy(&output.stdout).to_string())
    }

    pub fn diff(project: &Path) -> Result<DiffImpact, ArborError> {
        let output = Command::new("arbor")
            .arg("diff")
            .arg("--json")
            .arg("--")
            .arg(project.as_os_str())
            .output()
            .map_err(|e| ArborError::Exec { source: e })?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(ArborError::Cli {
                message: stderr.to_string(),
            });
        }

        let resp: DiffResponse =
            serde_json::from_slice(&output.stdout).map_err(|e| ArborError::Parse { source: e })?;
        Ok(resp.impact)
    }

    pub fn ensure_indexed(project: &Path) -> Result<(), ArborError> {
        let output = Command::new("arbor")
            .arg("status")
            .arg("--")
            .arg(project.as_os_str())
            .output()
            .map_err(|e| ArborError::Exec { source: e })?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(ArborError::Cli {
                message: format!("status failed: {stderr}"),
            });
        }

        let status = String::from_utf8_lossy(&output.stdout);
        if index_health::status_needs_index(&status) {
            let output = Command::new("arbor")
                .arg("index")
                .arg("--")
                .arg(project.as_os_str())
                .output()
                .map_err(|e| ArborError::Exec { source: e })?;

            if !output.status.success() {
                let stderr = String::from_utf8_lossy(&output.stderr);
                return Err(ArborError::Cli {
                    message: format!("index failed: {stderr}"),
                });
            }
        }
        Ok(())
    }

    pub fn graph_json_path(project: &Path) -> PathBuf {
        index_health::graph_json_path(project)
    }

    pub fn reindex(project: &Path) -> Result<(), ArborError> {
        let output = Command::new("arbor")
            .arg("index")
            .arg("--")
            .arg(project.as_os_str())
            .output()
            .map_err(|source| ArborError::Exec { source })?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(ArborError::Cli {
                message: format!("index failed: {stderr}"),
            });
        }
        Ok(())
    }

    pub fn load_graph_index(project: &Path) -> Result<GraphIndex, ArborError> {
        index_health::ensure_fresh_index(project)?;
        let graph_path = Self::graph_json_path(project);
        if !graph_path.is_file() {
            return Err(ArborError::Cli {
                message: format!("missing Arbor graph file: {}", graph_path.display()),
            });
        }
        GraphIndex::from_graph_json_path(&graph_path)
    }

    // T059: entry-points command
    pub fn entry_points(project: &Path) -> Result<String, ArborError> {
        run_arbor_cmd("entry-points", [project.as_os_str()])
    }

    // T060: file-graph command
    pub fn file_graph(project: &Path, file: Option<&str>) -> Result<String, ArborError> {
        if let Some(f) = file {
            run_arbor_cmd("file-graph", [project.as_os_str(), f.as_ref()])
        } else {
            run_arbor_cmd("file-graph", [project.as_os_str()])
        }
    }

    // T061: inspect command
    pub fn inspect(symbol: &str, project: &Path) -> Result<String, ArborError> {
        run_arbor_cmd("inspect", [symbol.as_ref(), project.as_os_str()])
    }

    // T062: path command
    pub fn path(from: &str, to: &str, project: &Path) -> Result<String, ArborError> {
        run_arbor_cmd("path", [from.as_ref(), to.as_ref(), project.as_os_str()])
    }

    // T063: refactor command (mutates source files - use with caution)
    pub fn refactor(operation: &str, project: &Path) -> Result<String, ArborError> {
        run_arbor_cmd("refactor", [operation.as_ref(), project.as_os_str()])
    }

    // T064: check command
    pub fn check(project: &Path) -> Result<String, ArborError> {
        run_arbor_cmd("check", [project.as_os_str()])
    }

    // T065: summary command
    pub fn summary(project: &Path) -> Result<String, ArborError> {
        run_arbor_cmd("summary", [project.as_os_str()])
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ArborError {
    #[error("I/O error: {source}")]
    Io { source: std::io::Error },

    #[error("I/O error executing arbor: {source}")]
    Exec { source: std::io::Error },

    #[error("arbor CLI error: {message}")]
    Cli { message: String },

    #[error("JSON parse error: {source}")]
    Parse { source: serde_json::Error },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deserialize_callers_empty() {
        let json = r#"{"callers": [], "symbol": "register_tool"}"#;
        let resp: CallersResponse = serde_json::from_str(json).unwrap();
        assert!(resp.callers.is_empty());
    }

    #[test]
    fn deserialize_callees_with_entry() {
        let json = r#"{
            "callees": [{
                "file": "/home/w0w/dev/n00n/n00n-lua/src/api/tool.rs",
                "id": "7596527974691171",
                "kind": "function",
                "line": 1075,
                "name": "register_tool_from_lua"
            }],
            "symbol": "register_tool"
        }"#;
        let resp: CalleesResponse = serde_json::from_str(json).unwrap();
        assert_eq!(resp.callees.len(), 1);
        assert_eq!(resp.callees[0].name, "register_tool_from_lua");
        assert_eq!(
            resp.callees[0].path,
            "/home/w0w/dev/n00n/n00n-lua/src/api/tool.rs"
        );
        assert_eq!(resp.callees[0].kind.as_deref(), Some("function"));
        assert_eq!(resp.callees[0].line, Some(1075));
    }

    #[test]
    fn deserialize_map_response() {
        let json = r#"{
            "entries": [{
                "file": "src/main.rs",
                "file_short": "src/main.rs",
                "symbols": [{
                    "callers": 5,
                    "centrality": 0.8,
                    "is_entry_point": true,
                    "kind": "function",
                    "line": 42,
                    "name": "main",
                    "signature_short": "main()"
                }]
            }],
            "files_shown": 1,
            "files_total": 10,
            "schema": "map_v1",
            "symbols_shown": 1,
            "symbols_total": 50,
            "token_estimate": 500
        }"#;
        let resp: MapResponse = serde_json::from_str(json).unwrap();
        assert_eq!(resp.entries.len(), 1);
        assert_eq!(resp.files_total, 10);
        assert_eq!(resp.symbols_total, 50);
        assert_eq!(resp.entries[0].file, "src/main.rs");
        assert_eq!(resp.entries[0].symbols[0].name, "main");
        assert_eq!(resp.entries[0].symbols[0].centrality, Some(0.8));
        assert_eq!(resp.entries[0].symbols[0].is_entry_point, Some(true));
        assert_eq!(resp.entries[0].symbols[0].callers, Some(5));
    }

    #[test]
    fn deserialize_diff_response() {
        let json = r#"{
            "changed_files": ["src/lib.rs"],
            "changed_symbols": 10,
            "impact": {
                "api_entrypoints_affected": 2,
                "blast_radius_nodes": 15,
                "direct_callers": 8,
                "files_likely_require_updates": 3,
                "indirect_callers": 5
            }
        }"#;
        let resp: DiffResponse = serde_json::from_str(json).unwrap();
        assert_eq!(resp.changed_files, vec!["src/lib.rs"]);
        assert_eq!(resp.changed_symbols, 10);
        assert_eq!(resp.impact.direct_callers, 8);
        assert_eq!(resp.impact.indirect_callers, 5);
        assert_eq!(resp.impact.blast_radius_nodes, 15);
        assert_eq!(resp.impact.api_entrypoints_affected, 2);
        assert_eq!(resp.impact.files_likely_require_updates, 3);
    }

    #[test]
    fn deserialize_relation_with_file_alias() {
        let json = r#"{"name": "foo", "file": "src/lib.rs", "kind": "function", "line": 10}"#;
        let r: Relation = serde_json::from_str(json).unwrap();
        assert_eq!(r.name, "foo");
        assert_eq!(r.path, "src/lib.rs");
        assert_eq!(r.kind.as_deref(), Some("function"));
        assert_eq!(r.line, Some(10));
    }

    #[test]
    fn deserialize_relation_with_path_field() {
        let json = r#"{"name": "bar", "path": "src/main.rs", "kind": null, "line": null}"#;
        let r: Relation = serde_json::from_str(json).unwrap();
        assert_eq!(r.name, "bar");
        assert_eq!(r.path, "src/main.rs");
        assert!(r.kind.is_none());
        assert!(r.line.is_none());
    }

    #[test]
    fn error_display_exec() {
        let err = ArborError::Exec {
            source: std::io::Error::new(std::io::ErrorKind::NotFound, "arbor not found"),
        };
        let msg = format!("{err}");
        assert!(msg.contains("arbor not found"), "msg: {msg}");
    }

    #[test]
    fn error_display_cli() {
        let err = ArborError::Cli {
            message: "project not indexed".into(),
        };
        assert_eq!(format!("{err}"), "arbor CLI error: project not indexed");
    }

    #[test]
    fn error_display_parse() {
        let err = ArborError::Parse {
            source: serde_json::from_str::<()>("not json").unwrap_err(),
        };
        let msg = format!("{err}");
        assert!(msg.contains("JSON parse error"), "msg: {msg}");
    }

    #[test]
    fn map_entry_serialize_matches_arbor_format() {
        let json = r#"{
            "file": "src/lib.rs",
            "file_short": "src/lib.rs",
            "symbols": []
        }"#;
        let entry: MapEntry = serde_json::from_str(json).unwrap();
        assert_eq!(entry.file, "src/lib.rs");
        assert!(entry.symbols.is_empty());
    }

    #[test]
    fn map_symbol_minimal() {
        let json = r#"{"name": "helper", "kind": "function", "line": 1}"#;
        let s: MapSymbol = serde_json::from_str(json).unwrap();
        assert_eq!(s.name, "helper");
        assert!(s.centrality.is_none());
        assert!(s.callers.is_none());
        assert!(s.is_entry_point.is_none());
    }

    // T052-T058: Test helper for CLI commands that require Arbor binary
    fn assert_cli_requires_cli<F>(fn_name: &str, f: F)
    where
        F: FnOnce(&Path) -> Result<String, ArborError>,
    {
        let dir = tempfile::tempdir().expect("tempdir");
        let result = f(dir.path());
        assert!(result.is_err(), "{fn_name} should fail without Arbor CLI");
        match result.unwrap_err() {
            ArborError::Exec { .. } | ArborError::Cli { .. } => (),
            other => panic!("{fn_name}: expected Exec or Cli error, got: {other:?}"),
        }
    }

    #[test]
    fn entry_points_requires_cli() {
        assert_cli_requires_cli("entry_points", Client::entry_points);
    }

    #[test]
    fn file_graph_requires_cli() {
        assert_cli_requires_cli("file_graph", |p| Client::file_graph(p, None));
    }

    #[test]
    fn inspect_requires_cli() {
        assert_cli_requires_cli("inspect", |p| Client::inspect("main", p));
    }

    #[test]
    fn path_requires_cli() {
        assert_cli_requires_cli("path", |p| Client::path("main", "helper", p));
    }

    #[test]
    fn refactor_requires_cli() {
        assert_cli_requires_cli("refactor", |p| Client::refactor("rename", p));
    }

    #[test]
    fn check_requires_cli() {
        assert_cli_requires_cli("check", Client::check);
    }

    #[test]
    fn summary_requires_cli() {
        assert_cli_requires_cli("summary", Client::summary);
    }
}

#![allow(clippy::missing_errors_doc)]
#![allow(clippy::new_without_default)]
#![allow(clippy::must_use_candidate)]

use std::path::Path;
use std::process::Command;

use serde::{Deserialize, Serialize};

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
            .arg(symbol)
            .arg(project.as_os_str())
            .arg("--json")
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
            .arg(symbol)
            .arg(project.as_os_str())
            .arg("--json")
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
        cmd.arg("map").arg(project.as_os_str()).arg("--json");
        if let Some(budget) = token_budget {
            cmd.arg("--tokens").arg(budget.to_string());
        }

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
            .arg(project.as_os_str())
            .arg("--json")
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
            .arg(project.as_os_str())
            .output()
            .map_err(|e| ArborError::Exec { source: e })?;

        let status = String::from_utf8_lossy(&output.stdout);
        if status.contains("No index") || status.contains("not indexed") {
            let output = Command::new("arbor")
                .arg("index")
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
}

#[derive(Debug, thiserror::Error)]
pub enum ArborError {
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
}

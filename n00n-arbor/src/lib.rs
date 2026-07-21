use std::path::Path;
use std::process::Command;

use serde::Deserialize;
use serde::Serialize;

#[derive(Debug, Serialize, Deserialize)]
pub struct Relation {
    pub name: String,
    pub path: String,
    pub kind: Option<String>,
    pub line: Option<u64>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct MapEntry {
    pub path: String,
    pub rank: Option<f64>,
    pub symbols: Vec<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct DiffImpact {
    pub name: String,
    pub path: String,
    pub distance: u64,
    pub kind: Option<String>,
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

        serde_json::from_slice(&output.stdout).map_err(|e| ArborError::Parse { source: e })
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

        serde_json::from_slice(&output.stdout).map_err(|e| ArborError::Parse { source: e })
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

        serde_json::from_slice(&output.stdout).map_err(|e| ArborError::Parse { source: e })
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

    pub fn diff(project: &Path) -> Result<Vec<DiffImpact>, ArborError> {
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

        serde_json::from_slice(&output.stdout).map_err(|e| ArborError::Parse { source: e })
    }

    pub fn ensure_indexed(project: &Path) -> Result<(), ArborError> {
        let status_output = Command::new("arbor")
            .arg("status")
            .arg(project.as_os_str())
            .output()
            .map_err(|e| ArborError::Exec { source: e })?;

        let status = String::from_utf8_lossy(&status_output.stdout);
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

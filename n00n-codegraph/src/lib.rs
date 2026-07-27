#![allow(clippy::missing_errors_doc)]
#![allow(clippy::new_without_default)]
#![allow(clippy::must_use_candidate)]

use std::path::Path;
use std::process::{Command, Stdio};
use std::time::Duration;

use wait_timeout::ChildExt;

const DEFAULT_TIMEOUT_SECS: u64 = 30;

pub struct Client;

impl Client {
    pub fn check_binary() -> Result<(), CodegraphError> {
        let output = Command::new("codegraph")
            .arg("--version")
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .output()
            .map_err(|source| CodegraphError::Exec { source })?;

        if output.status.success() {
            Ok(())
        } else {
            let stderr = String::from_utf8_lossy(&output.stderr);
            Err(CodegraphError::Cli {
                message: stderr.to_string(),
            })
        }
    }

    pub fn available() -> bool {
        Self::check_binary().is_ok()
    }

    pub fn has_index(project: &Path) -> bool {
        project.join(".codegraph").is_dir()
    }

    pub fn explore(
        query: &str,
        project: &Path,
        timeout_secs: Option<u64>,
    ) -> Result<String, CodegraphError> {
        if query.trim().is_empty() {
            return Err(CodegraphError::Cli {
                message: String::from("query is required"),
            });
        }
        if !Self::has_index(project) {
            return Err(CodegraphError::Cli {
                message: format!("no .codegraph/ index found in {}", project.display()),
            });
        }

        let timeout_secs = match timeout_secs {
            Some(value) => value,
            None => DEFAULT_TIMEOUT_SECS,
        };
        let timeout = Duration::from_secs(timeout_secs);
        let mut child = Command::new("codegraph")
            .arg("explore")
            .arg("--")
            .arg(query)
            .arg(project)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|source| CodegraphError::Exec { source })?;

        let status = child
            .wait_timeout(timeout)
            .map_err(|source| CodegraphError::Exec { source })?;

        let Some(status) = status else {
            let _ = child.kill();
            let _ = child.wait();
            return Err(CodegraphError::Cli {
                message: format!("codegraph explore timed out after {}s", timeout.as_secs()),
            });
        };

        let output = child
            .wait_with_output()
            .map_err(|source| CodegraphError::Exec { source })?;

        if status.success() {
            Ok(String::from_utf8_lossy(&output.stdout)
                .trim_end()
                .to_string())
        } else {
            let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
            let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
            let message = if !stderr.is_empty() {
                stderr
            } else if !stdout.is_empty() {
                stdout
            } else {
                format!("exit code {status}")
            };
            Err(CodegraphError::Cli { message })
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum CodegraphError {
    #[error("I/O error executing codegraph: {source}")]
    Exec { source: std::io::Error },

    #[error("codegraph CLI error: {message}")]
    Cli { message: String },
}

#[cfg(test)]
mod tests {
    use super::{Client, CodegraphError};
    use std::path::Path;

    #[test]
    fn explore_requires_query() {
        let result = Client::explore("   ", Path::new("."), None);
        assert!(matches!(result, Err(CodegraphError::Cli { .. })));
    }

    #[test]
    fn explore_requires_index_directory() {
        let dir = tempfile::tempdir().expect("tempdir");
        let result = Client::explore("how does auth work", dir.path(), None);
        assert!(matches!(result, Err(CodegraphError::Cli { .. })));
    }
}

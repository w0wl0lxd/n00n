#![allow(clippy::missing_errors_doc)]
#![allow(clippy::new_without_default)]
#![allow(clippy::must_use_candidate)]

use std::io::{Error, Read};
use std::path::Path;
use std::process::{Command, Stdio};
use std::thread;
use std::time::Duration;

use wait_timeout::ChildExt;

const CODEGRAPH_BINARY: &str = "codegraph";
const DEFAULT_TIMEOUT_SECS: u64 = 30;

pub struct Client;

impl Client {
    pub fn check_binary() -> Result<(), CodegraphError> {
        let output = Command::new(CODEGRAPH_BINARY)
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
        let mut child = Command::new(CODEGRAPH_BINARY)
            .arg("explore")
            .arg("--")
            .arg(query)
            .arg(project)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|source| CodegraphError::Exec { source })?;

        let mut stdout = child.stdout.take().ok_or_else(|| CodegraphError::Cli {
            message: String::from("failed to capture codegraph stdout"),
        })?;
        let mut stderr = child.stderr.take().ok_or_else(|| CodegraphError::Cli {
            message: String::from("failed to capture codegraph stderr"),
        })?;

        let stdout_handle = thread::spawn(move || {
            let mut buf = Vec::new();
            stdout.read_to_end(&mut buf).map(|_| buf)
        });
        let stderr_handle = thread::spawn(move || {
            let mut buf = Vec::new();
            stderr.read_to_end(&mut buf).map(|_| buf)
        });

        let status = child
            .wait_timeout(timeout)
            .map_err(|source| CodegraphError::Exec { source })?;

        let Some(status) = status else {
            if let Err(source) = child.kill() {
                let _ = child.wait();
                let _ = stdout_handle.join();
                let _ = stderr_handle.join();
                return Err(CodegraphError::Exec { source });
            }
            if let Err(source) = child.wait() {
                let _ = stdout_handle.join();
                let _ = stderr_handle.join();
                return Err(CodegraphError::Exec { source });
            }
            let _ = stdout_handle.join();
            let _ = stderr_handle.join();
            return Err(CodegraphError::Cli {
                message: format!("codegraph explore timed out after {}s", timeout.as_secs()),
            });
        };

        let stdout_bytes = stdout_handle
            .join()
            .map_err(|_| CodegraphError::Cli {
                message: String::from("codegraph stdout reader panicked"),
            })?
            .map_err(|source| CodegraphError::Exec { source })?;
        let stderr_bytes = stderr_handle
            .join()
            .map_err(|_| CodegraphError::Cli {
                message: String::from("codegraph stderr reader panicked"),
            })?
            .map_err(|source| CodegraphError::Exec { source })?;

        let stdout = String::from_utf8_lossy(&stdout_bytes);
        let stderr = String::from_utf8_lossy(&stderr_bytes);

        if status.success() {
            Ok(stdout.trim_end().to_string())
        } else {
            let stderr = stderr.trim();
            let stdout = stdout.trim();
            let message = if !stderr.is_empty() {
                stderr.to_string()
            } else if !stdout.is_empty() {
                stdout.to_string()
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
    Exec { source: Error },

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

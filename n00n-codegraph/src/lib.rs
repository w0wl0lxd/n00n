#![allow(clippy::missing_errors_doc)]
#![allow(clippy::new_without_default)]
#![allow(clippy::must_use_candidate)]

mod db;

use std::ffi::OsStr;
use std::io::{Error, Read};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::Duration;

#[cfg(all(
    unix,
    not(any(target_os = "espidf", target_os = "horizon", target_os = "redox"))
))]
use std::io::ErrorKind;
#[cfg(all(
    unix,
    not(any(target_os = "espidf", target_os = "horizon", target_os = "redox"))
))]
use std::os::unix::process::CommandExt;
use wait_timeout::ChildExt;

const CODEGRAPH_BINARY: &str = "codegraph";
const DEFAULT_TIMEOUT_SECS: u64 = 30;
const PROJECT_PATH_FLAG: &str = "--path";
const PROJECT_POSITIONAL_SEPARATOR: &str = "--";
const EXPLORE_SUBCOMMAND: &str = "explore";
const CALLERS_SUBCOMMAND: &str = "callers";
const CALLEES_SUBCOMMAND: &str = "callees";
const IMPACT_SUBCOMMAND: &str = "impact";
const AFFECTED_SUBCOMMAND: &str = "affected";
const NODE_SUBCOMMAND: &str = "node";
const QUERY_SUBCOMMAND: &str = "query";
const SYNC_SUBCOMMAND: &str = "sync";
const FILES_SUBCOMMAND: &str = "files";

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

    pub fn has_database(project: &Path) -> bool {
        db::has_database(project)
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

        if Self::has_database(project) {
            return db::explore_database(query, project);
        }

        Self::explore_cli(query, project, timeout_secs)
    }

    pub fn callers(
        symbol: &str,
        project: &Path,
        timeout_secs: Option<u64>,
    ) -> Result<String, CodegraphError> {
        if symbol.trim().is_empty() {
            return Err(CodegraphError::Cli {
                message: String::from("symbol is required"),
            });
        }
        if !Self::has_index(project) {
            return Err(CodegraphError::Cli {
                message: format!("no .codegraph/ index found in {}", project.display()),
            });
        }

        if Self::has_database(project) {
            return db::callers_database(symbol, project);
        }

        Self::callers_cli(symbol, project, timeout_secs)
    }

    pub fn callees(
        symbol: &str,
        project: &Path,
        timeout_secs: Option<u64>,
    ) -> Result<String, CodegraphError> {
        if symbol.trim().is_empty() {
            return Err(CodegraphError::Cli {
                message: String::from("symbol is required"),
            });
        }
        if !Self::has_index(project) {
            return Err(CodegraphError::Cli {
                message: format!("no .codegraph/ index found in {}", project.display()),
            });
        }

        if Self::has_database(project) {
            return db::callees_database(symbol, project);
        }

        Self::callees_cli(symbol, project, timeout_secs)
    }

    pub fn impact(
        symbol: &str,
        project: &Path,
        timeout_secs: Option<u64>,
    ) -> Result<String, CodegraphError> {
        if symbol.trim().is_empty() {
            return Err(CodegraphError::Cli {
                message: String::from("symbol is required"),
            });
        }
        if !Self::has_index(project) {
            return Err(CodegraphError::Cli {
                message: format!("no .codegraph/ index found in {}", project.display()),
            });
        }

        if Self::has_database(project) {
            return db::impact_database(symbol, project);
        }

        Self::impact_cli(symbol, project, timeout_secs)
    }

    pub fn affected(
        files: &[&str],
        project: &Path,
        timeout_secs: Option<u64>,
    ) -> Result<String, CodegraphError> {
        if files.is_empty() {
            return Err(CodegraphError::Cli {
                message: String::from("at least one file is required"),
            });
        }
        if !Self::has_index(project) {
            return Err(CodegraphError::Cli {
                message: format!("no .codegraph/ index found in {}", project.display()),
            });
        }

        if Self::has_database(project) {
            return db::affected_database(files, project);
        }

        Self::affected_cli(files, project, timeout_secs)
    }

    pub fn node(
        name: &str,
        project: &Path,
        timeout_secs: Option<u64>,
    ) -> Result<String, CodegraphError> {
        if name.trim().is_empty() {
            return Err(CodegraphError::Cli {
                message: String::from("name is required"),
            });
        }
        if !Self::has_index(project) {
            return Err(CodegraphError::Cli {
                message: format!("no .codegraph/ index found in {}", project.display()),
            });
        }

        if Self::has_database(project) {
            return db::node_database(name, project);
        }

        Self::node_cli(name, project, timeout_secs)
    }

    pub fn query(
        search: &str,
        project: &Path,
        timeout_secs: Option<u64>,
    ) -> Result<String, CodegraphError> {
        if search.trim().is_empty() {
            return Err(CodegraphError::Cli {
                message: String::from("search is required"),
            });
        }
        if !Self::has_index(project) {
            return Err(CodegraphError::Cli {
                message: format!("no .codegraph/ index found in {}", project.display()),
            });
        }

        if Self::has_database(project) {
            return db::query_database(search, project);
        }

        Self::query_cli(search, project, timeout_secs)
    }

    pub fn sync(project: &Path, timeout_secs: Option<u64>) -> Result<String, CodegraphError> {
        if !Self::has_index(project) {
            return Err(CodegraphError::Cli {
                message: format!("no .codegraph/ index found in {}", project.display()),
            });
        }

        Self::sync_cli(project, timeout_secs)
    }

    pub fn files(project: &Path, timeout_secs: Option<u64>) -> Result<String, CodegraphError> {
        if !Self::has_index(project) {
            return Err(CodegraphError::Cli {
                message: format!("no .codegraph/ index found in {}", project.display()),
            });
        }

        if Self::has_database(project) {
            return db::files_database(project);
        }

        Self::files_cli(project, timeout_secs)
    }

    /// Builds the argv for a `codegraph` subcommand.
    ///
    /// Every subcommand except `sync` takes the project through `--path` and
    /// accepts only its own positionals; passing the project positionally makes
    /// the CLI reject the call with "too many arguments", and for the variadic
    /// `explore <query...>` it is silently appended to the query text instead.
    fn cli_args<'a>(
        subcommand: &'a str,
        project: &'a Path,
        positionals: &'a [&'a str],
    ) -> Vec<&'a OsStr> {
        let mut args: Vec<&OsStr> = Vec::with_capacity(positionals.len() + 4);
        args.push(subcommand.as_ref());
        if subcommand == SYNC_SUBCOMMAND {
            args.push(PROJECT_POSITIONAL_SEPARATOR.as_ref());
            args.push(project.as_os_str());
            return args;
        }
        args.push(PROJECT_PATH_FLAG.as_ref());
        args.push(project.as_os_str());
        args.push(PROJECT_POSITIONAL_SEPARATOR.as_ref());
        for positional in positionals {
            args.push(positional.as_ref());
        }
        args
    }

    fn run_cli(
        subcommand: &str,
        project: &Path,
        positionals: &[&str],
        timeout_secs: Option<u64>,
    ) -> Result<String, CodegraphError> {
        let timeout_secs = timeout_secs.unwrap_or_else(|| DEFAULT_TIMEOUT_SECS);
        let timeout = Duration::from_secs(timeout_secs);
        let mut command = Command::new(CODEGRAPH_BINARY);
        command
            .args(Self::cli_args(subcommand, project, positionals))
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        #[cfg(all(
            unix,
            not(any(target_os = "espidf", target_os = "horizon", target_os = "redox"))
        ))]
        command.process_group(0);
        let child = command
            .spawn()
            .map_err(|source| CodegraphError::Exec { source })?;

        Self::wait_for_output(child, timeout, subcommand)
    }

    fn explore_cli(
        query: &str,
        project: &Path,
        timeout_secs: Option<u64>,
    ) -> Result<String, CodegraphError> {
        Self::run_cli(EXPLORE_SUBCOMMAND, project, &[query], timeout_secs)
    }

    fn callers_cli(
        symbol: &str,
        project: &Path,
        timeout_secs: Option<u64>,
    ) -> Result<String, CodegraphError> {
        Self::run_cli(CALLERS_SUBCOMMAND, project, &[symbol], timeout_secs)
    }

    fn callees_cli(
        symbol: &str,
        project: &Path,
        timeout_secs: Option<u64>,
    ) -> Result<String, CodegraphError> {
        Self::run_cli(CALLEES_SUBCOMMAND, project, &[symbol], timeout_secs)
    }

    fn impact_cli(
        symbol: &str,
        project: &Path,
        timeout_secs: Option<u64>,
    ) -> Result<String, CodegraphError> {
        Self::run_cli(IMPACT_SUBCOMMAND, project, &[symbol], timeout_secs)
    }

    fn affected_cli(
        files: &[&str],
        project: &Path,
        timeout_secs: Option<u64>,
    ) -> Result<String, CodegraphError> {
        Self::run_cli(AFFECTED_SUBCOMMAND, project, files, timeout_secs)
    }

    fn node_cli(
        name: &str,
        project: &Path,
        timeout_secs: Option<u64>,
    ) -> Result<String, CodegraphError> {
        Self::run_cli(NODE_SUBCOMMAND, project, &[name], timeout_secs)
    }

    fn query_cli(
        search: &str,
        project: &Path,
        timeout_secs: Option<u64>,
    ) -> Result<String, CodegraphError> {
        Self::run_cli(QUERY_SUBCOMMAND, project, &[search], timeout_secs)
    }

    fn sync_cli(project: &Path, timeout_secs: Option<u64>) -> Result<String, CodegraphError> {
        Self::run_cli(SYNC_SUBCOMMAND, project, &[], timeout_secs)
    }

    fn files_cli(project: &Path, timeout_secs: Option<u64>) -> Result<String, CodegraphError> {
        Self::run_cli(FILES_SUBCOMMAND, project, &[], timeout_secs)
    }

    fn kill_child_tree(child: &mut Child) -> Result<(), Error> {
        #[cfg(all(
            unix,
            not(any(target_os = "espidf", target_os = "horizon", target_os = "redox"))
        ))]
        {
            use rustix::process::{kill_process_group, Pid, Signal};

            let raw_pid = i32::try_from(child.id()).map_err(|source| {
                Error::new(
                    ErrorKind::InvalidInput,
                    format!("invalid child pid: {source}"),
                )
            })?;
            let process_group = Pid::from_raw(raw_pid).ok_or_else(|| {
                Error::new(ErrorKind::InvalidInput, "child process id cannot be zero")
            })?;
            kill_process_group(process_group, Signal::KILL).map_err(Error::from)
        }
        #[cfg(not(all(
            unix,
            not(any(target_os = "espidf", target_os = "horizon", target_os = "redox"))
        )))]
        {
            child.kill()
        }
    }

    fn wait_for_output(
        mut child: std::process::Child,
        timeout: Duration,
        command_name: &str,
    ) -> Result<String, CodegraphError> {
        let mut stdout = child.stdout.take().ok_or_else(|| CodegraphError::Cli {
            message: format!("failed to capture codegraph {command_name} stdout"),
        })?;
        let mut stderr = child.stderr.take().ok_or_else(|| CodegraphError::Cli {
            message: format!("failed to capture codegraph {command_name} stderr"),
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
            if let Err(source) = Self::kill_child_tree(&mut child) {
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
                message: format!(
                    "codegraph {} timed out after {}s",
                    command_name,
                    timeout.as_secs()
                ),
            });
        };

        let stdout_bytes = stdout_handle
            .join()
            .map_err(|_| CodegraphError::Cli {
                message: format!("codegraph {command_name} stdout reader panicked"),
            })?
            .map_err(|source| CodegraphError::Exec { source })?;
        let stderr_bytes = stderr_handle
            .join()
            .map_err(|_| CodegraphError::Cli {
                message: format!("codegraph {command_name} stderr reader panicked"),
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

    #[error("codegraph database error: {source}")]
    Sqlite { source: rusqlite::Error },

    #[error("codegraph source path rejected: {reason}")]
    SourcePath { reason: &'static str },

    #[error(
        "codegraph source snippet reads are disabled on this target because race-free file opening is unsupported"
    )]
    SourceSnippetsUnsupported,

    #[error("codegraph source file is too large ({size} bytes, maximum {max} bytes)")]
    SourceTooLarge { size: u64, max: u64 },
}

#[cfg(test)]
mod tests {
    use super::{Client, CodegraphError};
    use std::path::Path;

    const NO_INDEX: &str = "no .codegraph/ index found";
    const QUERY_REQUIRED: &str = "query is required";
    const SYMBOL_REQUIRED: &str = "symbol is required";
    const NAME_REQUIRED: &str = "name is required";
    const SEARCH_REQUIRED: &str = "search is required";
    const FILES_REQUIRED: &str = "at least one file is required";

    fn assert_cli_error(result: Result<String, CodegraphError>, expected: &str) {
        match result {
            Err(CodegraphError::Cli { message }) => {
                assert!(message.contains(expected), "message: {message}");
            }
            other => panic!("expected Cli error, got: {other:?}"),
        }
    }

    fn args_of(subcommand: &str, project: &Path, positionals: &[&str]) -> Vec<String> {
        Client::cli_args(subcommand, project, positionals)
            .into_iter()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect()
    }

    /// `codegraph <sub>` accepts the project only through `--path`. Passing it
    /// positionally makes the CLI reject the call with "too many arguments",
    /// and for the variadic `explore <query...>` it is silently swallowed into
    /// the query text, which returns answers for the wrong question.
    #[test]
    fn project_is_passed_as_a_path_flag_not_a_positional() {
        let project = Path::new("/tmp/project");
        for (subcommand, positionals) in [
            (super::EXPLORE_SUBCOMMAND, &["how does auth work"][..]),
            (super::CALLERS_SUBCOMMAND, &["restore_item"][..]),
            (super::CALLEES_SUBCOMMAND, &["restore_item"][..]),
            (super::IMPACT_SUBCOMMAND, &["restore_item"][..]),
            (super::NODE_SUBCOMMAND, &["restore_item"][..]),
            (super::QUERY_SUBCOMMAND, &["restore"][..]),
            (super::FILES_SUBCOMMAND, &[][..]),
            (super::AFFECTED_SUBCOMMAND, &["src/a.rs", "src/b.rs"][..]),
        ] {
            let args = args_of(subcommand, project, positionals);
            let mut expected = vec![
                subcommand.to_owned(),
                super::PROJECT_PATH_FLAG.to_owned(),
                project.to_string_lossy().into_owned(),
                super::PROJECT_POSITIONAL_SEPARATOR.to_owned(),
            ];
            expected.extend(positionals.iter().map(|p| (*p).to_owned()));
            assert_eq!(args, expected, "argv for {subcommand}");
        }
    }

    /// `sync [path]` is the one subcommand that takes the project positionally.
    #[test]
    fn sync_keeps_the_project_positional() {
        let project = Path::new("/tmp/project");
        assert_eq!(
            args_of(super::SYNC_SUBCOMMAND, project, &[]),
            vec![
                super::SYNC_SUBCOMMAND.to_owned(),
                super::PROJECT_POSITIONAL_SEPARATOR.to_owned(),
                project.to_string_lossy().into_owned(),
            ]
        );
    }

    #[test]
    fn explore_requires_query() {
        assert_cli_error(Client::explore("   ", Path::new("."), None), QUERY_REQUIRED);
    }

    #[test]
    fn explore_requires_index_directory() {
        let dir = tempfile::tempdir().expect("tempdir");
        assert_cli_error(
            Client::explore("how does auth work", dir.path(), None),
            NO_INDEX,
        );
    }

    #[test]
    fn callers_requires_symbol() {
        assert_cli_error(
            Client::callers("   ", Path::new("."), None),
            SYMBOL_REQUIRED,
        );
    }

    #[test]
    fn callers_requires_index_directory() {
        let dir = tempfile::tempdir().expect("tempdir");
        assert_cli_error(Client::callers("restore_item", dir.path(), None), NO_INDEX);
    }

    #[test]
    fn callees_requires_symbol() {
        assert_cli_error(
            Client::callees("   ", Path::new("."), None),
            SYMBOL_REQUIRED,
        );
    }

    #[test]
    fn callees_requires_index_directory() {
        let dir = tempfile::tempdir().expect("tempdir");
        assert_cli_error(Client::callees("main", dir.path(), None), NO_INDEX);
    }

    #[test]
    fn impact_requires_symbol() {
        assert_cli_error(Client::impact("   ", Path::new("."), None), SYMBOL_REQUIRED);
    }

    #[test]
    fn impact_requires_index_directory() {
        let dir = tempfile::tempdir().expect("tempdir");
        assert_cli_error(Client::impact("Config", dir.path(), None), NO_INDEX);
    }

    #[test]
    fn affected_requires_files() {
        assert_cli_error(Client::affected(&[], Path::new("."), None), FILES_REQUIRED);
    }

    #[test]
    fn affected_requires_index_directory() {
        let dir = tempfile::tempdir().expect("tempdir");
        assert_cli_error(
            Client::affected(&["src/main.rs"], dir.path(), None),
            NO_INDEX,
        );
    }

    #[test]
    fn node_requires_name() {
        assert_cli_error(Client::node("   ", Path::new("."), None), NAME_REQUIRED);
    }

    #[test]
    fn node_requires_index_directory() {
        let dir = tempfile::tempdir().expect("tempdir");
        assert_cli_error(Client::node("AuthService", dir.path(), None), NO_INDEX);
    }

    #[test]
    fn query_requires_search() {
        assert_cli_error(Client::query("   ", Path::new("."), None), SEARCH_REQUIRED);
    }

    #[test]
    fn query_requires_index_directory() {
        let dir = tempfile::tempdir().expect("tempdir");
        assert_cli_error(Client::query("auth", dir.path(), None), NO_INDEX);
    }

    #[test]
    fn sync_requires_index_directory() {
        let dir = tempfile::tempdir().expect("tempdir");
        assert_cli_error(Client::sync(dir.path(), None), NO_INDEX);
    }

    #[test]
    fn files_requires_index_directory() {
        let dir = tempfile::tempdir().expect("tempdir");
        assert_cli_error(Client::files(dir.path(), None), NO_INDEX);
    }

    #[cfg(all(
        unix,
        not(any(target_os = "espidf", target_os = "horizon", target_os = "redox"))
    ))]
    #[test]
    fn timeout_kills_descendants_before_joining_pipe_readers() {
        use std::os::unix::process::CommandExt;
        use std::process::{Command, Stdio};
        use std::time::{Duration, Instant};

        let mut command = Command::new("sh");
        command
            .args(["-c", "sleep 2 & wait"])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .process_group(0);
        let child = command.spawn().expect("spawn timeout fixture");
        let started = Instant::now();

        assert_cli_error(
            Client::wait_for_output(child, Duration::from_millis(100), "fixture"),
            "timed out",
        );
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "pipe readers remained blocked for {:?}",
            started.elapsed()
        );
    }
}

//! Unix-only `agent` command surface: stub for non-Unix platforms.

use std::path::PathBuf;

use color_eyre::Result;
use color_eyre::eyre::eyre;

use crate::cli::AgentMode as CliAgentMode;
use crate::cli::SafetyFlags;

#[allow(dead_code)]
pub struct AgentRunOptions<'a> {
    pub prompt: &'a str,
    pub model: Option<&'a str>,
    pub mode: CliAgentMode,
    pub goal: Option<&'a str>,
    pub team_mode: Option<&'a str>,
    pub max_agents: Option<usize>,
    pub waves: bool,
    pub workflow_inputs: Option<&'a str>,
    pub task_description: Option<&'a str>,
    pub yolo: bool,
    pub no_jit: bool,
    pub fusion: bool,
    pub project_trusted: bool,
}

fn unsupported() -> Result<()> {
    Err(eyre!(
        "agent background commands are only supported on Unix"
    ))
}

pub fn server(_opts: &AgentRunOptions<'_>, _agent_id: Option<String>) -> Result<()> {
    unsupported()
}

pub fn run(_opts: &AgentRunOptions<'_>, _json: bool) -> Result<()> {
    unsupported()
}

pub fn list_client(
    _json: bool,
    _all: bool,
    _cwd: Option<PathBuf>,
    _state_dir_override: Option<PathBuf>,
) -> Result<()> {
    unsupported()
}

pub fn status_client(_id: &str, _json: bool, _state_dir_override: Option<PathBuf>) -> Result<()> {
    unsupported()
}

pub fn message_client(
    _id: &str,
    _text: &str,
    _json: bool,
    _state_dir_override: Option<PathBuf>,
) -> Result<()> {
    unsupported()
}

pub fn pause_client(_id: &str, _state_dir_override: Option<PathBuf>) -> Result<()> {
    unsupported()
}

pub fn resume_client(_id: &str, _state_dir_override: Option<PathBuf>) -> Result<()> {
    unsupported()
}

pub fn stop_client(
    _id: &str,
    _state_dir_override: Option<PathBuf>,
    _safety: SafetyFlags,
) -> Result<()> {
    unsupported()
}

pub fn daemon_serve(_state_dir: Option<PathBuf>) -> Result<()> {
    unsupported()
}

//! Async agent loop with tools.

pub mod agent;
pub mod cancel;
pub mod child_guard;
pub use child_guard::ChildGuard;
pub mod fusion;
pub mod headless;
pub mod mcp;
pub use mcp::config::{McpConfigError, McpConfigErrors, McpServerInfo, McpServerStatus};
pub use mcp::protocol::PromptRole;
pub use mcp::{
    McpCommand, McpHandle, McpPromptArg, McpPromptInfo, McpSession, McpSnapshot, McpSnapshotReader,
};
pub(crate) mod task_set;
pub use agent::{
    Agent, AgentParams, AgentRunParams, History, Instructions, LoadedInstructions, SharedMessages,
    SharedTranscript, find_subdirectory_instructions, is_instruction_file,
};
pub use cancel::{CancelMap, CancelToken, CancelTrigger, PreDispatchGate};
pub use fusion::{FusionLane, FusionRoute, FusionState, FusionUsageStats};
pub use n00n_config::{AgentConfig, FusionConfig, PermissionsConfig, ToolOutputLines};
pub mod command;
pub mod diff;
pub mod permissions;
pub mod prompt;
pub mod skill_policy;
pub mod template;
pub mod tokenize;
pub mod tools;
pub use tools::ToolFilter;
pub mod types;

use std::collections::HashMap;
use std::path::{Path, PathBuf};

pub use n00n_providers::AgentError;
use n00n_providers::Message;
pub use n00n_providers::{ImageMediaType, ImageSource, ThinkingConfig};
use n00n_storage::StateDir;
use n00n_storage::sessions::{SessionMeta, StoredMode};
pub use types::{
    AgentEvent, BufferSnapshot, Envelope, EventSender, GrepFileEntry, GrepLine, GrepMatchGroup,
    InstructionBlock, NO_FILES_FOUND, SharedBuf, SnapshotLine, SnapshotSpan, SpanStyle,
    SubagentInfo, SubagentPrompt, TextOutput, ToolDoneEvent, ToolInput, ToolOutput, ToolStartEvent,
    ToolTelemetry, ToolUsage, TurnCompleteEvent,
};

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub enum AgentMode {
    #[default]
    Build,
    Plan(PathBuf),
    Research,
}

impl AgentMode {
    #[must_use]
    pub fn plan_path(&self) -> Option<&Path> {
        match self {
            Self::Plan(p) => Some(p),
            Self::Build | Self::Research => None,
        }
    }

    #[must_use]
    pub fn is_readonly(&self) -> bool {
        matches!(self, Self::Plan(_) | Self::Research)
    }
}

/// Convert stored session metadata into a runtime mode and optional plan path,
/// with a logged fallback for generating a new plan path when needed.
#[must_use]
pub fn mode_and_plan_from_stored(
    state_dir: &StateDir,
    meta: &SessionMeta,
) -> (AgentMode, Option<PathBuf>) {
    let plan_path = meta.plan_path.as_ref().map(PathBuf::from);
    match meta.mode {
        Some(StoredMode::Build) | None => (AgentMode::Build, plan_path),
        Some(StoredMode::Plan) => {
            let path = plan_path.unwrap_or_else(|| {
                n00n_storage::plans::new_plan_path(state_dir).unwrap_or_else(|e| {
                    tracing::warn!(error = %e, "failed to generate new plan path; using fallback");
                    PathBuf::from("plan.md")
                })
            });
            (AgentMode::Plan(path.clone()), Some(path))
        }
        Some(StoredMode::Research) => (AgentMode::Research, plan_path),
    }
}

pub enum ExtractedCommand {
    Interrupt(AgentInput, u64),
    Compact(u64),
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum InterruptPoint {
    Safe,
    ToolComplete,
}

pub trait InterruptSource: Send + Sync {
    fn poll(&self, point: InterruptPoint) -> Option<ExtractedCommand>;
}

#[derive(Clone)]
pub struct McpPromptRef {
    pub qualified_name: String,
    pub arguments: HashMap<String, String>,
}

#[derive(Clone)]
pub struct AgentInput {
    pub message: String,
    pub mode: AgentMode,
    pub images: Vec<ImageSource>,
    pub preamble: Vec<Message>,
    pub thinking: ThinkingConfig,
    pub fast: bool,
    /// No `Default` on this struct so adding a field forces every call site to update.
    pub workflow: bool,
    pub control: bool,
    pub prompt: Option<Box<McpPromptRef>>,
    pub plan_path: Option<PathBuf>,
}

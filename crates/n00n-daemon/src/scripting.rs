//! Scripting-friendly agent state normalization (competitor parity).

use serde::Serialize;

use crate::protocol::AgentRecord;

/// Stable lifecycle state for scripting (`claude agents --json` parity).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentStateKind {
    Working,
    NeedsInput,
    Idle,
    Paused,
    Running,
    Stopped,
    Done,
    Failed,
    Unknown,
}

impl AgentStateKind {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Working => "working",
            Self::NeedsInput => "needs_input",
            Self::Idle => "idle",
            Self::Paused => "paused",
            Self::Running => "running",
            Self::Stopped => "stopped",
            Self::Done => "done",
            Self::Failed => "failed",
            Self::Unknown => "unknown",
        }
    }
}

/// Map a backend-specific status string to a stable scripting state.
#[must_use]
pub fn normalize_state(status: &str) -> AgentStateKind {
    match status {
        "working" => AgentStateKind::Working,
        "needs_input" | "blocked" => AgentStateKind::NeedsInput,
        "idle" => AgentStateKind::Idle,
        "paused" => AgentStateKind::Paused,
        "running" => AgentStateKind::Running,
        "stopped" | "cancelled" | "canceled" => AgentStateKind::Stopped,
        "done" | "completed" | "complete" => AgentStateKind::Done,
        "failed" | "error" => AgentStateKind::Failed,
        _ => AgentStateKind::Unknown,
    }
}

/// Returns whether a disk worker row represents a terminal (non-live) session.
#[must_use]
pub fn is_terminal_worker_status(status: &str) -> bool {
    matches!(
        normalize_state(status),
        AgentStateKind::Stopped | AgentStateKind::Done | AgentStateKind::Failed
    )
}

/// JSON row shape for `n00n agent list --json` / `status --json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AgentScriptView {
    pub id: String,
    pub backend: String,
    pub state: &'static str,
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
}

impl AgentScriptView {
    #[must_use]
    pub fn from_record(record: &AgentRecord) -> Self {
        let state = normalize_state(&record.status);
        Self {
            id: record.id.clone(),
            backend: record.backend.to_string(),
            state: state.as_str(),
            status: record.status.clone(),
            title: record.title.clone(),
            model: record.model.clone(),
            session_id: record.session_id.clone(),
            cwd: record.cwd.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::BackendKind;

    #[test]
    fn normalize_maps_tui_statuses() {
        assert_eq!(normalize_state("working"), AgentStateKind::Working);
        assert_eq!(normalize_state("needs_input"), AgentStateKind::NeedsInput);
        assert_eq!(normalize_state("blocked"), AgentStateKind::NeedsInput);
        assert_eq!(normalize_state("idle"), AgentStateKind::Idle);
    }

    #[test]
    fn normalize_maps_worker_statuses() {
        assert_eq!(normalize_state("running"), AgentStateKind::Running);
        assert_eq!(normalize_state("stopped"), AgentStateKind::Stopped);
        assert_eq!(normalize_state("done"), AgentStateKind::Done);
        assert_eq!(normalize_state("failed"), AgentStateKind::Failed);
    }

    #[test]
    fn script_view_includes_normalized_state() {
        let record = AgentRecord {
            id: "a1".into(),
            backend: BackendKind::Tui,
            session_id: Some("a1".into()),
            status: "needs_input".into(),
            title: Some("main".into()),
            model: None,
            output: None,
            cwd: Some("/tmp/proj".into()),
        };
        let view = AgentScriptView::from_record(&record);
        assert_eq!(view.state, "needs_input");
        assert_eq!(view.cwd.as_deref(), Some("/tmp/proj"));
    }

    #[test]
    fn terminal_worker_detection() {
        assert!(is_terminal_worker_status("stopped"));
        assert!(is_terminal_worker_status("done"));
        assert!(!is_terminal_worker_status("running"));
    }
}

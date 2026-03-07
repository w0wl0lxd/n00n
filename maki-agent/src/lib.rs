pub mod agent;
pub use agent::{Agent, History};
pub(crate) mod prompt;
pub mod skill;
pub mod template;
pub mod tools;
pub mod types;

use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

pub use maki_providers::AgentError;
pub use types::{
    AgentEvent, BatchToolEntry, BatchToolStatus, DiffHunk, DiffLine, DiffSpan, Envelope,
    GrepFileEntry, GrepMatch, NO_FILES_FOUND, QuestionAnswer, QuestionInfo, QuestionOption,
    TodoItem, TodoPriority, TodoStatus, ToolDoneEvent, ToolInput, ToolOutput, ToolStartEvent,
};

pub const PLANS_DIR: &str = "plans";

pub fn new_plan_path() -> String {
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    let plan_dir = maki_providers::data_dir()
        .map(|d| d.join(PLANS_DIR))
        .unwrap_or_else(|_| PLANS_DIR.into());
    format!("{}/{ts}.md", plan_dir.display())
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub enum AgentMode {
    #[default]
    Build,
    Plan(String),
}

pub enum ExtractedCommand {
    Interrupt(AgentInput),
    Cancel,
    Compact,
    Ignore,
}

pub struct AgentInput {
    pub message: String,
    pub mode: AgentMode,
    pub pending_plan: Option<String>,
}

impl AgentInput {
    pub fn effective_message(&self) -> String {
        match &self.pending_plan {
            Some(path) if self.mode == AgentMode::Build && Path::new(path).exists() => {
                format!(
                    "A plan was written to {path}. Follow the plan.\n\n{}",
                    self.message
                )
            }
            _ => self.message.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::TempDir;

    use super::*;

    #[test]
    fn effective_message_no_plan() {
        let input = AgentInput {
            message: "do stuff".into(),
            mode: AgentMode::Build,
            pending_plan: None,
        };
        assert_eq!(input.effective_message(), "do stuff");
    }

    #[test]
    fn effective_message_with_existing_plan() {
        let dir = TempDir::new().unwrap();
        let plan_path = dir.path().join("plan.md");
        fs::write(&plan_path, "the plan").unwrap();
        let path_str = plan_path.to_str().unwrap().to_string();

        let input = AgentInput {
            message: "go".into(),
            mode: AgentMode::Build,
            pending_plan: Some(path_str.clone()),
        };
        let msg = input.effective_message();
        assert!(msg.contains(&path_str));
        assert!(msg.contains("go"));
    }

    #[test]
    fn effective_message_skips_missing_plan() {
        let input = AgentInput {
            message: "go".into(),
            mode: AgentMode::Build,
            pending_plan: Some("/nonexistent/plan.md".into()),
        };
        assert_eq!(input.effective_message(), "go");
    }

    #[test]
    fn effective_message_plan_mode_ignores_pending() {
        let input = AgentInput {
            message: "plan this".into(),
            mode: AgentMode::Plan("/tmp/p.md".into()),
            pending_plan: Some("/tmp/p.md".into()),
        };
        assert_eq!(input.effective_message(), "plan this");
    }
}

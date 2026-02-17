use std::env;
use std::sync::mpsc;
use std::thread;

use maki_providers::ContentBlock;
use maki_tool_macro::Tool;

use super::ToolContext;
use crate::agent;
use crate::tools::ToolCall;
use crate::{AgentInput, AgentMode};

const RESEARCH_TOOLS: &[&str] = &["bash", "read", "glob", "grep", "webfetch"];

#[derive(Tool, Debug, Clone)]
pub struct Task {
    #[param(description = "Short (3-5 words) description of the task")]
    description: String,
    #[param(description = "Detailed task prompt for the research agent")]
    prompt: String,
}

impl Task {
    pub const NAME: &str = "task";
    pub const DESCRIPTION: &str = include_str!("task.md");

    pub fn execute(&self, ctx: &ToolContext) -> Result<String, String> {
        let cwd = env::current_dir()
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_else(|_| ".".into());

        let system = build_research_system_prompt(&cwd);
        let tools = ToolCall::definitions_filtered(Some(RESEARCH_TOOLS));

        let (sub_tx, sub_rx) = mpsc::channel();
        thread::spawn(move || while sub_rx.recv().is_ok() {});

        let input = AgentInput {
            message: self.prompt.clone(),
            mode: AgentMode::Build,
            pending_plan: None,
        };

        let mut history = Vec::new();
        agent::run(
            ctx.provider,
            ctx.model,
            input,
            &mut history,
            &system,
            &sub_tx,
            Some(tools),
        )
        .map_err(|e| format!("sub-agent error: {e}"))?;

        let text = history
            .iter()
            .rev()
            .filter(|m| matches!(m.role, maki_providers::Role::Assistant))
            .flat_map(|m| m.content.iter())
            .find_map(|b| match b {
                ContentBlock::Text { text } => Some(text.as_str()),
                _ => None,
            })
            .unwrap_or("(no response)");

        Ok(text.to_string())
    }

    pub fn start_summary(&self) -> String {
        self.description.clone()
    }

    pub fn mutable_path(&self) -> Option<&str> {
        None
    }
}

fn build_research_system_prompt(cwd: &str) -> String {
    let base = crate::prompt::RESEARCH_PROMPT;
    format!(
        "{base}\n\nEnvironment:\n- Working directory: {cwd}\n- Platform: {}",
        env::consts::OS
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn research_tools_all_registered() {
        let filtered = ToolCall::definitions_filtered(Some(RESEARCH_TOOLS));
        assert_eq!(filtered.as_array().unwrap().len(), RESEARCH_TOOLS.len());
    }
}

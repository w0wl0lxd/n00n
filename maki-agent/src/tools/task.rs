use std::sync::mpsc;
use std::thread;

use maki_providers::{AgentEvent, ContentBlock, ToolInput, ToolOutput};
use maki_tool_macro::Tool;

use super::ToolContext;
use crate::agent;
use crate::template;
use crate::tools::ToolCall;
use crate::{AgentInput, AgentMode};

const RESEARCH_TOOLS: &[&str] = &["bash", "read", "glob", "grep", "webfetch"];
const GENERAL_TOOLS: &[&str] = &[
    "bash",
    "read",
    "write",
    "edit",
    "multiedit",
    "glob",
    "grep",
    "webfetch",
    "batch",
];

#[derive(Tool, Debug, Clone)]
pub struct Task {
    #[param(description = "Short (3-5 words) description of the task")]
    description: String,
    #[param(description = "Detailed task prompt for the agent")]
    prompt: String,
    #[param(
        description = "Subagent type: \"research\" (read-only, default) or \"general\" (can modify files)"
    )]
    subagent_type: Option<String>,
}

impl Task {
    pub const NAME: &str = "task";
    pub const DESCRIPTION: &str = include_str!("task.md");

    pub fn execute(&self, ctx: &ToolContext) -> Result<ToolOutput, String> {
        let vars = template::env_vars();
        let agent_type = self.subagent_type.as_deref().unwrap_or("research");
        let (prompt, tool_names) = match agent_type {
            "research" => (crate::prompt::RESEARCH_PROMPT, RESEARCH_TOOLS),
            "general" => (crate::prompt::GENERAL_PROMPT, GENERAL_TOOLS),
            other => return Err(format!("unknown subagent type: {other}")),
        };
        let mut system = vars.apply(prompt).into_owned();
        agent::append_agents_md(&mut system, &vars.apply("{cwd}"));
        let tools = ToolCall::definitions_filtered(&vars, tool_names);

        let (sub_tx, sub_rx) = mpsc::channel::<crate::Envelope>();
        let parent_tx = ctx.event_tx.clone();
        let parent_id = ctx.tool_use_id.map(String::from);
        let parent_name = Some(self.description.clone());
        thread::spawn(move || {
            while let Ok(mut envelope) = sub_rx.recv() {
                if matches!(
                    envelope.event,
                    AgentEvent::Done { .. }
                        | AgentEvent::Error { .. }
                        | AgentEvent::ToolOutput { .. }
                ) {
                    continue;
                }
                envelope.parent_tool_use_id = parent_id.clone();
                envelope.parent_name = parent_name.clone();
                let _ = parent_tx.send(envelope);
            }
        });

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
            &tools,
            None,
            None,
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

        Ok(ToolOutput::Plain(text.to_string()))
    }

    pub fn start_summary(&self) -> String {
        self.description.clone()
    }

    pub fn start_input(&self) -> Option<ToolInput> {
        None
    }

    pub fn start_output(&self) -> Option<ToolOutput> {
        None
    }

    pub fn mutable_path(&self) -> Option<&str> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use test_case::test_case;

    #[test_case(RESEARCH_TOOLS ; "research_tools")]
    #[test_case(GENERAL_TOOLS  ; "general_tools")]
    fn subagent_tools_all_registered(tools: &[&str]) {
        let vars = template::Vars::new();
        let filtered = ToolCall::definitions_filtered(&vars, tools);
        assert_eq!(filtered.as_array().unwrap().len(), tools.len());
    }
}

use std::sync::Arc;

use crate::{AgentEvent, EventSender, SubagentInfo, ToolOutput};
use maki_providers::ContentBlock;
use maki_providers::model::ModelTier;
use maki_providers::provider;
use maki_tool_macro::Tool;

use super::{GENERAL_SUBAGENT_TOOLS, RESEARCH_SUBAGENT_TOOLS, ToolContext};
use crate::agent;
use crate::template;
use crate::tools::ToolCall;
use crate::{Agent, AgentInput, AgentMode};

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
    #[param(
        description = "Model tier (optional, omit to use current model, capped at current tier):\n- \"strong\" (e.g. Opus): Deep reasoning, complex architecture, subtle bugs. ~5x cost of medium.\n- \"medium\" (e.g. Sonnet): Balanced. Refactors, features, multi-file changes.\n- \"weak\" (e.g. Haiku): Fast/cheap. Search, summarize, boilerplate, simple edits."
    )]
    model_tier: Option<String>,
}

impl Task {
    pub const NAME: &str = "task";
    pub const DESCRIPTION: &str = include_str!("task.md");
    pub const EXAMPLES: Option<&str> = Some(
        r#"[
  {"description": "Find auth middleware", "prompt": "Search the codebase for authentication middleware. Return file paths and a summary of how auth is implemented.", "model_tier": "weak"},
  {"description": "Refactor error types", "prompt": "In src/errors.rs, replace all uses of String error types with thiserror derive macros.\n\nHere is the pattern to follow (from src/api/errors.rs):\n```rust\n#[derive(Debug, thiserror::Error)]\npub enum ApiError {\n    #[error(\"not found: {0}\")]\n    NotFound(String),\n    #[error(\"unauthorized\")]\n    Unauthorized,\n}\n```\n\nApply this same pattern to all error variants in src/errors.rs.", "subagent_type": "general"},
  {"description": "Debug race condition", "prompt": "Analyze the locking strategy in src/cache.rs. Identify potential deadlocks or race conditions.", "model_tier": "strong"}
]"#,
    );

    pub async fn execute(&self, ctx: &ToolContext) -> Result<ToolOutput, String> {
        let vars = template::env_vars();
        let agent_type = self.subagent_type.as_deref().unwrap_or("research");
        let (prompt, tool_names) = match agent_type {
            "research" => (crate::prompt::RESEARCH_PROMPT, RESEARCH_SUBAGENT_TOOLS),
            "general" => (crate::prompt::GENERAL_PROMPT, GENERAL_SUBAGENT_TOOLS),
            other => return Err(format!("unknown subagent type: {other}")),
        };

        let (model, provider): (
            maki_providers::Model,
            Arc<dyn maki_providers::provider::Provider>,
        ) = if let Some(ref tier_str) = self.model_tier {
            let requested: ModelTier = tier_str
                .parse()
                .map_err(|e: maki_providers::ModelError| e.to_string())?;
            let effective = requested.min(ctx.model.tier);
            let resolved_model = maki_providers::Model::from_tier(ctx.model.provider, effective)
                .map_err(|e| e.to_string())?;
            let resolved_provider =
                provider::from_model(&resolved_model).map_err(|e| e.to_string())?;
            (resolved_model, Arc::from(resolved_provider))
        } else {
            (ctx.model.clone(), Arc::clone(&ctx.provider))
        };

        let mut system = vars.apply(prompt).into_owned();
        let (instructions, _) = agent::load_instruction_files(&vars.apply("{cwd}"));
        system.push_str(&instructions);
        system.push_str(&agent::tool_efficiency_table(tool_names));
        let tools = ToolCall::definitions_filtered(
            &vars,
            tool_names,
            model.family.supports_tool_examples(),
        );

        let (sub_tx, sub_rx) = flume::unbounded::<crate::Envelope>();
        let sub_event_tx = EventSender::new(sub_tx, ctx.event_tx.run_id());
        let parent_tx = ctx.event_tx.clone();
        let subagent_info = ctx.tool_use_id.as_ref().map(|id| SubagentInfo {
            parent_tool_use_id: id.to_owned(),
            name: self.description.clone(),
            prompt: Some(self.prompt.clone()),
            model: Some(model.spec()),
        });
        smol::spawn(async move {
            while let Ok(mut envelope) = sub_rx.recv_async().await {
                if matches!(
                    envelope.event,
                    AgentEvent::Done { .. }
                        | AgentEvent::Error { .. }
                        | AgentEvent::ToolOutput { .. }
                ) {
                    continue;
                }
                envelope.subagent = subagent_info.clone();
                let _ = parent_tx.send_envelope(envelope);
            }
        })
        .detach();

        let input = AgentInput {
            message: self.prompt.clone(),
            mode: AgentMode::Build,
            pending_plan: None,
        };

        let agent = Agent::new(
            provider,
            model,
            crate::History::new(Vec::new()),
            system,
            sub_event_tx,
            tools,
            Arc::clone(&ctx.skills),
        );
        let outcome = agent.run(input).await;
        outcome
            .result
            .map_err(|e| format!("sub-agent error: {e}"))?;

        let text = outcome
            .history
            .as_slice()
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
}

impl super::ToolDefaults for Task {}

#[cfg(test)]
mod tests {
    use super::*;
    use test_case::test_case;

    #[test_case(RESEARCH_SUBAGENT_TOOLS ; "research_subagent_tools")]
    #[test_case(GENERAL_SUBAGENT_TOOLS  ; "general_subagent_tools")]
    fn subagent_tools_all_registered(tools: &[&str]) {
        let vars = template::Vars::new();
        let filtered = ToolCall::definitions_filtered(&vars, tools, true);
        assert_eq!(filtered.as_array().unwrap().len(), tools.len());
    }
}

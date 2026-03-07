use serde::{Deserialize, Serialize};

use crate::ToolOutput;
use crate::skill::{Skill, build_skill_list_description};
use maki_tool_macro::Tool;

use super::{Tool, ToolContext};

const NOT_FOUND: &str = "skill not found: ";

#[derive(Tool, Debug, Clone, Serialize, Deserialize)]
pub struct SkillTool {
    #[param(description = "Name of the skill to load")]
    name: String,
}

impl Tool for SkillTool {
    const NAME: &str = "skill";
    const DESCRIPTION: &str = "Load a skill by name to get detailed instructions.";

    fn execute(&self, ctx: &ToolContext) -> Result<ToolOutput, String> {
        Skill::find(&self.name, ctx.skills)
            .map(|s| ToolOutput::Plain(s.format_content()))
            .ok_or_else(|| format!("{NOT_FOUND}{}", self.name))
    }

    fn start_summary(&self) -> String {
        self.name.clone()
    }

    fn description_extra(skills: &[Skill]) -> Option<String> {
        let desc = build_skill_list_description(skills);
        if desc.is_empty() { None } else { Some(desc) }
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use serde_json::json;

    use super::*;
    use crate::AgentMode;
    use crate::tools::test_support::stub_ctx;

    fn test_skill() -> Skill {
        Skill {
            name: "test-skill".into(),
            description: "A test skill".into(),
            content: "Do the thing".into(),
            location: PathBuf::from("/home/.maki/skills/test-skill/SKILL.md"),
        }
    }

    #[test]
    fn execute_loads_skill_content() {
        let skill = test_skill();
        let skills = [skill];
        let mut ctx = stub_ctx(&AgentMode::Build);
        ctx.skills = &skills;

        let tool = SkillTool::parse_input(&json!({"name": "test-skill"})).unwrap();
        let output = tool.execute(&ctx).unwrap();
        assert!(output.as_text().contains("Do the thing"));
    }

    #[test]
    fn execute_returns_error_when_not_found() {
        let skills = [test_skill()];
        let mut ctx = stub_ctx(&AgentMode::Build);
        ctx.skills = &skills;

        let tool = SkillTool::parse_input(&json!({"name": "nonexistent"})).unwrap();
        assert!(tool.execute(&ctx).unwrap_err().starts_with(NOT_FOUND));
    }
}

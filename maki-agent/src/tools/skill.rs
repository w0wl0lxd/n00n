use serde::{Deserialize, Serialize};

use crate::ToolOutput;
use crate::skill::Skill;
use maki_tool_macro::Tool;

use super::ToolContext;

const NOT_FOUND: &str = "skill not found: ";

#[derive(Tool, Debug, Clone, Serialize, Deserialize)]
pub struct SkillTool {
    #[param(description = "Name of the skill to load")]
    name: String,
}

impl SkillTool {
    pub const NAME: &str = "skill";
    pub const DESCRIPTION: &str = "Load a skill by name to get detailed instructions.";
    pub const EXAMPLES: Option<&str> = None;

    pub fn execute(&self, ctx: &ToolContext) -> Result<ToolOutput, String> {
        Skill::find(&self.name, ctx.skills)
            .map(|s| ToolOutput::Plain(s.format_content()))
            .ok_or_else(|| format!("{NOT_FOUND}{}", self.name))
    }

    pub fn start_summary(&self) -> String {
        self.name.clone()
    }

    pub fn start_input(&self) -> Option<super::ToolInput> {
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
    use std::path::PathBuf;

    use serde_json::json;

    use super::*;
    use crate::AgentMode;
    use crate::skill::Skill;
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

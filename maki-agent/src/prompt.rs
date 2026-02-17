use crate::ModelFamily;

const CLAUDE_PROMPT: &str = include_str!("prompts/claude.md");
const GLM_PROMPT: &str = include_str!("prompts/glm.md");
pub const PLAN_PROMPT: &str = include_str!("prompts/plan.md");
pub const RESEARCH_PROMPT: &str = include_str!("prompts/research.md");

pub fn base_prompt(family: ModelFamily) -> &'static str {
    match family {
        ModelFamily::Claude => CLAUDE_PROMPT,
        ModelFamily::Glm => GLM_PROMPT,
    }
}

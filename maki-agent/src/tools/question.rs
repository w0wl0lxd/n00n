use maki_providers::{ToolInput, ToolOutput};
use maki_tool_macro::Tool;

const EMPTY_QUESTIONS: &str = "at least one question is required";

#[derive(Tool, Debug, Clone)]
pub struct Question {
    #[param(description = "List of questions to ask the user")]
    questions: Vec<String>,
}

impl Question {
    pub const NAME: &str = "question";
    pub const DESCRIPTION: &str = include_str!("question.md");

    pub fn execute(&self, _ctx: &super::ToolContext) -> Result<ToolOutput, String> {
        if self.questions.is_empty() {
            return Err(EMPTY_QUESTIONS.into());
        }
        let mut out = String::new();
        for (i, q) in self.questions.iter().enumerate() {
            if i > 0 {
                out.push('\n');
            }
            out.push_str(q);
        }
        Ok(ToolOutput::Plain(out))
    }

    pub fn start_summary(&self) -> String {
        let n = self.questions.len();
        format!("{n} question{}", if n == 1 { "" } else { "s" })
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
    use serde_json::json;

    use super::*;
    use crate::AgentMode;
    use crate::tools::test_support::stub_ctx;

    #[test]
    fn empty_questions_returns_error() {
        let q = Question::parse_input(&json!({"questions": []})).unwrap();
        let err = q.execute(&stub_ctx(&AgentMode::Build)).unwrap_err();
        assert_eq!(err, EMPTY_QUESTIONS);
    }

    #[test]
    fn formats_numbered_questions() {
        let q =
            Question::parse_input(&json!({"questions": ["What language?", "Which framework?"]}))
                .unwrap();
        let output = q.execute(&stub_ctx(&AgentMode::Build)).unwrap();
        assert_eq!(output.as_text(), "What language?\nWhich framework?");
    }
}

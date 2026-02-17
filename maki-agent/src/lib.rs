pub mod agent;
pub(crate) mod prompt;
pub mod tools;

use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

pub(crate) use maki_providers::model::ModelFamily;
pub(crate) use maki_providers::{
    AgentError, AgentEvent, ContentBlock, Message, Role, TokenUsage, ToolDoneEvent, ToolStartEvent,
};

pub const PLANS_DIR: &str = "plans";
const SCRUB_MAX_LINES: usize = 1000;
const SCRUB_TIERS: &[(usize, usize)] = &[(1000, 2), (500, 3), (100, 5)];

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

pub(crate) fn scrub_tool_use_inputs(msg: &mut Message, successful_ids: &[&str]) {
    for block in &mut msg.content {
        if let ContentBlock::ToolUse { id, name, input } = block
            && successful_ids.contains(&id.as_str())
        {
            tools::ToolCall::scrub_input(name, input);
        }
    }
}

fn scrub_target(msg: &Message, tool_use_id: &str, content: &str) -> Option<String> {
    msg.content.iter().find_map(|b| match b {
        ContentBlock::ToolUse { id, name, .. } if id == tool_use_id => {
            tools::ToolCall::scrub_result(name, content)
        }
        _ => None,
    })
}

fn truncate_to_lines(content: &str, max: usize) -> String {
    let total = content.lines().count();
    let mut out = String::new();
    for (i, line) in content.lines().enumerate() {
        if i >= max {
            out.push_str(&format!("\n[truncated, showing {max} of {total} lines]"));
            break;
        }
        if i > 0 {
            out.push('\n');
        }
        out.push_str(line);
    }
    out
}

fn assistant_turns_after(history: &[Message], from: usize) -> usize {
    history[from + 1..]
        .iter()
        .filter(|m| matches!(m.role, Role::Assistant))
        .count()
}

pub(crate) fn scrub_stale_tool_results(history: &mut [Message]) {
    for i in 1..history.len() {
        let turns_ago = assistant_turns_after(history, i);
        let (before, current) = history.split_at_mut(i);

        for block in &mut current[0].content {
            let ContentBlock::ToolResult {
                tool_use_id,
                content,
                is_error,
            } = block
            else {
                continue;
            };
            if *is_error || content.starts_with('[') {
                continue;
            }

            let line_count = content.lines().count();
            let should_scrub = SCRUB_TIERS
                .iter()
                .any(|&(min_lines, min_turns)| line_count >= min_lines && turns_ago >= min_turns);

            if should_scrub {
                if let Some(summary) = scrub_target(&before[i - 1], tool_use_id, content) {
                    *content = summary;
                }
            } else if line_count > SCRUB_MAX_LINES {
                *content = truncate_to_lines(content, SCRUB_MAX_LINES);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::TempDir;
    use test_case::test_case;

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

    fn tool_use_msg(id: &str, name: &str) -> Message {
        Message {
            role: Role::Assistant,
            content: vec![ContentBlock::ToolUse {
                id: id.into(),
                name: name.into(),
                input: serde_json::json!({}),
            }],
        }
    }

    fn tool_result_msg(tool_use_id: &str, content: &str, is_error: bool) -> Message {
        Message {
            role: Role::User,
            content: vec![ContentBlock::ToolResult {
                tool_use_id: tool_use_id.into(),
                content: content.into(),
                is_error,
            }],
        }
    }

    fn result_content(history: &[Message], idx: usize) -> &str {
        match &history[idx].content[0] {
            ContentBlock::ToolResult { content, .. } => content,
            _ => panic!("expected ToolResult"),
        }
    }

    fn make_lines(n: usize) -> String {
        (1..=n)
            .map(|i| format!("{i}: line"))
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn add_filler_turns(history: &mut Vec<Message>, n: usize) {
        for i in 0..n {
            let id = format!("filler_{i}");
            history.push(tool_use_msg(&id, "bash"));
            history.push(tool_result_msg(&id, "ok", false));
        }
    }

    #[test]
    fn scrub_ignores_small_results_and_non_targets() {
        let small = "1: fn main() {}";
        let mut history = vec![
            tool_use_msg("r1", "read"),
            tool_result_msg("r1", small, false),
            tool_use_msg("b1", "bash"),
            tool_result_msg("b1", &make_lines(200), false),
        ];
        add_filler_turns(&mut history, 10);
        scrub_stale_tool_results(&mut history);

        assert_eq!(result_content(&history, 1), small);
        assert!(!result_content(&history, 3).starts_with('['));
    }

    #[test_case(150, "read", 5 ; "100_line_tier_at_5_turns")]
    #[test_case(500, "read", 3 ; "500_line_tier_at_3_turns")]
    #[test_case(1000, "grep", 2 ; "1000_line_tier_at_2_turns")]
    fn scrub_triggers_at_tier_threshold(lines: usize, tool: &str, threshold: usize) {
        let content = make_lines(lines);
        let mut history = vec![
            tool_use_msg("t1", tool),
            tool_result_msg("t1", &content, false),
        ];

        add_filler_turns(&mut history, threshold - 1);
        scrub_stale_tool_results(&mut history);
        assert!(!result_content(&history, 1).starts_with('['));

        add_filler_turns(&mut history, 1);
        scrub_stale_tool_results(&mut history);
        assert!(result_content(&history, 1).starts_with('['));
    }

    #[test]
    fn truncate_to_max_lines_immediately() {
        let content = make_lines(1500);
        let mut history = vec![
            tool_use_msg("r1", "read"),
            tool_result_msg("r1", &content, false),
        ];
        scrub_stale_tool_results(&mut history);
        let result = result_content(&history, 1);
        assert!(!result.starts_with('['));
        assert!(result.contains("[truncated, showing 1000 of 1500 lines]"));
        assert_eq!(result.lines().count(), SCRUB_MAX_LINES + 1);
    }
}

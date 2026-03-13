pub(crate) mod code_view;
pub mod command;
pub(crate) mod help_modal;
pub mod input;
pub(crate) mod keybindings;
pub(crate) mod list_picker;
pub mod messages;
pub(crate) mod modal;
pub(crate) mod model_picker;
pub mod question_form;
pub mod queue_panel;
pub(crate) mod rewind_picker;
pub(crate) mod scrollbar;
pub(crate) mod session_picker;
pub mod status_bar;
pub(crate) mod theme_picker;
pub(crate) mod tool_display;

pub(crate) const TOOL_SEPARATOR: &str = "────────────";
pub(crate) const CHEVRON: &str = "❯ ";

use std::collections::HashMap;
use std::time::Instant;

use crossterm::event::{KeyEvent, KeyModifiers};
use maki_agent::AgentInput;
use maki_agent::{ToolInput, ToolOutput};
use maki_providers::Message;

pub(crate) fn visual_line_count(text_len: usize, width: usize) -> usize {
    if width == 0 {
        return 1;
    }
    text_len.div_ceil(width).max(1)
}

pub(crate) fn apply_scroll_delta(offset: u16, delta: i32) -> u16 {
    if delta > 0 {
        offset.saturating_sub(delta as u16)
    } else {
        offset.saturating_add(delta.unsigned_abs() as u16)
    }
}

pub fn is_ctrl(key: &KeyEvent) -> bool {
    key.modifiers.contains(KeyModifiers::CONTROL) && !key.modifiers.contains(KeyModifiers::ALT)
}

pub struct LoadedSession {
    pub messages: Vec<Message>,
    pub tool_outputs: HashMap<String, ToolOutput>,
}

pub enum Action {
    SendMessage(AgentInput),
    CancelAgent,
    NewSession,
    LoadSession(LoadedSession),
    ChangeModel(String),
    Compact,
    Quit,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Status {
    Idle,
    Streaming,
    Error(String),
}

pub struct RetryInfo {
    pub attempt: u32,
    pub message: String,
    pub deadline: Instant,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ToolStatus {
    InProgress,
    Success,
    Error,
}

#[derive(Debug, Clone)]
pub struct DisplayMessage {
    pub role: DisplayRole,
    pub text: String,
    pub tool_input: Option<ToolInput>,
    pub tool_output: Option<ToolOutput>,
    pub annotation: Option<String>,
    pub plan_path: Option<String>,
    pub timestamp: Option<String>,
}

impl DisplayMessage {
    pub fn new(role: DisplayRole, text: String) -> Self {
        Self {
            role,
            text,
            tool_input: None,
            tool_output: None,
            annotation: None,
            plan_path: None,
            timestamp: None,
        }
    }

    /// Source of truth for `Segment::copy_text`. Tool messages reconstruct
    /// from structured data (diffs, grep, etc.); non-tool messages return raw
    /// text (preserving markdown).
    pub fn copy_text(&self) -> String {
        match &self.role {
            DisplayRole::Tool { .. } => {
                let (header, body) = self.text.split_once('\n').unwrap_or((&self.text, ""));
                let mut out = header.to_owned();
                if let Some(ToolInput::Code { code, .. }) = &self.tool_input {
                    out.push('\n');
                    out.push_str(code.trim_end());
                }
                match &self.tool_output {
                    Some(
                        structured @ (ToolOutput::Diff { .. }
                        | ToolOutput::ReadCode { .. }
                        | ToolOutput::WriteCode { .. }
                        | ToolOutput::GrepResult { .. }
                        | ToolOutput::GlobResult { .. }
                        | ToolOutput::TodoList(_)),
                    ) => {
                        out.push('\n');
                        out.push_str(&structured.as_text());
                    }
                    Some(ToolOutput::Batch { entries, .. }) => {
                        for entry in entries {
                            out.push_str(&format!("\n  {} {}", entry.tool, entry.summary));
                            if let Some(output) = &entry.output {
                                let text = output.as_text();
                                if !text.is_empty() {
                                    out.push('\n');
                                    out.push_str(&text);
                                }
                            }
                        }
                    }
                    _ if !body.is_empty() => {
                        out.push('\n');
                        out.push_str(body);
                    }
                    _ => {}
                }
                out
            }
            _ => self.text.clone(),
        }
    }

    pub fn plan(text: String, plan_path: String) -> Self {
        Self {
            role: DisplayRole::Assistant,
            text,
            tool_input: None,
            tool_output: None,
            annotation: None,
            plan_path: Some(plan_path),
            timestamp: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum DisplayRole {
    User,
    Assistant,
    Thinking,
    Tool {
        id: String,
        status: ToolStatus,
        name: &'static str,
    },
    Error,
}

impl DisplayRole {
    pub fn tool_name(&self) -> Option<&'static str> {
        match self {
            DisplayRole::Tool { name, .. } => Some(*name),
            _ => None,
        }
    }
}

#[cfg(test)]
use maki_providers::ModelPricing;

#[cfg(test)]
pub(crate) const TEST_CONTEXT_WINDOW: u32 = 200_000;

#[cfg(test)]
pub(crate) fn test_pricing() -> ModelPricing {
    ModelPricing {
        input: 3.0,
        output: 15.0,
        cache_write: 3.75,
        cache_read: 0.30,
    }
}

#[cfg(test)]
pub(crate) fn key(code: crossterm::event::KeyCode) -> crossterm::event::KeyEvent {
    crossterm::event::KeyEvent {
        code,
        modifiers: crossterm::event::KeyModifiers::NONE,
        kind: crossterm::event::KeyEventKind::Press,
        state: crossterm::event::KeyEventState::NONE,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use test_case::test_case;

    #[test_case(0, 80, 1 ; "empty_text")]
    #[test_case(0, 0, 1 ; "zero_width")]
    #[test_case(5, 5, 1 ; "exact_fit")]
    #[test_case(6, 5, 2 ; "one_char_overflow")]
    fn visual_line_count_cases(text_len: usize, width: usize, expected: usize) {
        assert_eq!(visual_line_count(text_len, width), expected);
    }

    #[test_case(10, 3, 7   ; "scroll_up")]
    #[test_case(10, -3, 13 ; "scroll_down")]
    #[test_case(0, 5, 0    ; "clamp_underflow")]
    fn apply_scroll_delta_cases(offset: u16, delta: i32, expected: u16) {
        assert_eq!(apply_scroll_delta(offset, delta), expected);
    }

    fn tool_msg(text: &str) -> DisplayMessage {
        DisplayMessage::new(
            DisplayRole::Tool {
                id: "t1".into(),
                status: ToolStatus::Success,
                name: "read",
            },
            text.into(),
        )
    }

    #[test]
    fn copy_text_non_tool_returns_text() {
        let msg = DisplayMessage::new(DisplayRole::Assistant, "hello\nworld".into());
        assert_eq!(msg.copy_text(), "hello\nworld");
    }

    #[test]
    fn copy_text_tool_structured_output_uses_as_text() {
        let mut msg = tool_msg("read /src/main.rs\nignored body");
        msg.tool_output = Some(ToolOutput::ReadCode {
            path: "main.rs".into(),
            start_line: 1,
            lines: vec!["fn main() {}".into()],
        });
        assert_eq!(msg.copy_text(), "read /src/main.rs\n1: fn main() {}");
    }

    #[test]
    fn copy_text_tool_with_code_input() {
        let mut msg = tool_msg("bash\nold body");
        msg.tool_input = Some(ToolInput::Code {
            language: "bash".into(),
            code: "echo hi\n".into(),
        });
        msg.tool_output = Some(ToolOutput::Plain("done".into()));
        assert_eq!(msg.copy_text(), "bash\necho hi\nold body");
    }

    #[test]
    fn copy_text_tool_plain_falls_through_to_body() {
        let mut msg = tool_msg("header\nbody text");
        msg.tool_output = Some(ToolOutput::Plain("done".into()));
        assert_eq!(msg.copy_text(), "header\nbody text");
    }

    #[test]
    fn copy_text_tool_no_output_no_body() {
        let msg = tool_msg("header only");
        assert_eq!(msg.copy_text(), "header only");
    }

    #[test]
    fn copy_text_tool_batch_empty_entries() {
        let mut msg = tool_msg("batch\n3 tools ran");
        msg.tool_output = Some(ToolOutput::Batch {
            entries: vec![],
            text: "ignored".into(),
        });
        assert_eq!(msg.copy_text(), "batch");
    }
}

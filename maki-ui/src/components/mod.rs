pub mod chat_picker;
pub(crate) mod code_view;
pub mod command;
pub mod input;
pub mod messages;
pub mod question_form;
pub mod queue_panel;
pub(crate) mod scrollbar;
pub mod status_bar;
pub(crate) mod tool_display;

use crossterm::event::{KeyEvent, KeyModifiers};
use maki_agent::AgentInput;
use maki_providers::{ToolInput, ToolOutput};

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

pub enum Action {
    SendMessage(AgentInput),
    CancelAgent,
    NewSession,
    Compact,
    Quit,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Status {
    Idle,
    Streaming,
    Error(String),
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
    pub plan_path: Option<String>,
}

impl DisplayMessage {
    pub fn new(role: DisplayRole, text: String) -> Self {
        Self {
            role,
            text,
            tool_input: None,
            tool_output: None,
            plan_path: None,
        }
    }

    pub fn plan(text: String, plan_path: String) -> Self {
        Self {
            role: DisplayRole::Assistant,
            text,
            tool_input: None,
            tool_output: None,
            plan_path: Some(plan_path),
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
pub(crate) const TEST_CONTEXT_WINDOW: u32 = 200_000;

#[cfg(test)]
pub(crate) fn test_pricing() -> maki_providers::ModelPricing {
    maki_providers::ModelPricing {
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
pub(crate) fn ctrl(c: char) -> crossterm::event::KeyEvent {
    crossterm::event::KeyEvent {
        code: crossterm::event::KeyCode::Char(c),
        modifiers: crossterm::event::KeyModifiers::CONTROL,
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
}

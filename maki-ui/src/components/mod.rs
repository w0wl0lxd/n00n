mod code_view;
pub mod input;
pub mod messages;
pub mod status_bar;

use maki_agent::AgentInput;
use maki_providers::{ToolInput, ToolOutput};

pub enum Action {
    SendMessage(AgentInput),
    CancelAgent,
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

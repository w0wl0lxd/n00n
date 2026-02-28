use crate::components::messages::MessagesPanel;
use crate::components::{DisplayMessage, DisplayRole};

use maki_providers::{AgentEvent, TokenUsage};
use ratatui::Frame;
use ratatui::layout::Rect;

pub enum ChatEventResult {
    Continue,
    Done,
    Error(String),
}

pub struct Chat {
    pub name: String,
    pub token_usage: TokenUsage,
    pub context_size: u32,
    messages_panel: MessagesPanel,
}

impl Chat {
    pub fn new(name: String) -> Self {
        Self {
            name,
            token_usage: TokenUsage::default(),
            context_size: 0,
            messages_panel: MessagesPanel::new(),
        }
    }

    pub fn handle_event(&mut self, event: AgentEvent) -> ChatEventResult {
        match event {
            AgentEvent::ThinkingDelta { text } => self.messages_panel.thinking_delta(&text),
            AgentEvent::TextDelta { text } => self.messages_panel.text_delta(&text),
            AgentEvent::ToolStart(e) => self.messages_panel.tool_start(e),
            AgentEvent::ToolOutput { id, content } => {
                self.messages_panel.tool_output(&id, &content)
            }
            AgentEvent::ToolDone(e) => self.messages_panel.tool_done(e),
            AgentEvent::BatchProgress {
                batch_id,
                index,
                status,
            } => {
                self.messages_panel.batch_progress(&batch_id, index, status);
            }
            AgentEvent::TurnComplete { .. } => {}
            AgentEvent::ToolResultsSubmitted { .. } => {}
            AgentEvent::Done { .. } => {
                self.messages_panel.flush();
                return ChatEventResult::Done;
            }
            AgentEvent::Error { message } => {
                self.messages_panel.flush();
                return ChatEventResult::Error(message);
            }
        }
        ChatEventResult::Continue
    }

    pub fn scroll(&mut self, delta: i32) {
        self.messages_panel.scroll(delta);
    }

    pub fn half_page(&self) -> i32 {
        self.messages_panel.half_page()
    }

    pub fn auto_scroll(&self) -> bool {
        self.messages_panel.auto_scroll()
    }

    pub fn enable_auto_scroll(&mut self) {
        self.messages_panel.enable_auto_scroll();
    }

    pub fn is_animating(&self) -> bool {
        self.messages_panel.is_animating()
    }

    pub fn view(&mut self, frame: &mut Frame, area: Rect) {
        self.messages_panel.view(frame, area);
    }

    pub fn flush(&mut self) {
        self.messages_panel.flush();
    }

    pub fn fail_in_progress(&mut self) {
        self.messages_panel.fail_in_progress();
    }

    pub fn push(&mut self, msg: DisplayMessage) {
        self.messages_panel.push(msg);
    }

    pub fn update_tool_summary(&mut self, tool_id: &str, prefix: &str) {
        self.messages_panel.update_tool_summary(tool_id, prefix);
    }

    pub fn load_messages(&mut self, msgs: Vec<DisplayMessage>) {
        self.messages_panel.load_messages(msgs);
    }

    pub fn push_user_message(&mut self, text: &str) {
        self.messages_panel
            .push(DisplayMessage::new(DisplayRole::User, text.to_string()));
    }

    #[cfg(test)]
    pub fn in_progress_count(&self) -> usize {
        self.messages_panel.in_progress_count()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use maki_providers::{AgentEvent, ToolStartEvent};

    fn tool_start_event(id: &str) -> ToolStartEvent {
        ToolStartEvent {
            id: id.into(),
            tool: "bash",
            summary: "running".into(),
            input: None,
            output: None,
        }
    }

    #[test]
    fn handle_event_tool_lifecycle() {
        let mut chat = Chat::new("Main".into());
        chat.handle_event(AgentEvent::ToolStart(tool_start_event("t1")));
        assert_eq!(chat.in_progress_count(), 1);

        chat.handle_event(AgentEvent::ToolDone(maki_providers::ToolDoneEvent {
            id: "t1".into(),
            tool: "bash",
            output: maki_providers::ToolOutput::Plain("ok".into()),
            is_error: false,
        }));
        assert_eq!(chat.in_progress_count(), 0);
    }

    #[test]
    fn handle_event_done_returns_done() {
        let mut chat = Chat::new("Main".into());
        chat.handle_event(AgentEvent::TextDelta {
            text: "partial".into(),
        });
        let result = chat.handle_event(AgentEvent::Done {
            usage: TokenUsage::default(),
            num_turns: 1,
            stop_reason: None,
        });
        assert!(matches!(result, ChatEventResult::Done));
    }

    #[test]
    fn handle_event_error_returns_error() {
        let mut chat = Chat::new("Main".into());
        let result = chat.handle_event(AgentEvent::Error {
            message: "boom".into(),
        });
        assert!(matches!(result, ChatEventResult::Error(ref e) if e == "boom"));
    }
}

use std::path::Path;

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

    pub fn handle_event(&mut self, event: AgentEvent, plan_path: Option<&str>) -> ChatEventResult {
        match event {
            AgentEvent::ThinkingDelta { text } => self.messages_panel.thinking_delta(&text),
            AgentEvent::TextDelta { text } => self.messages_panel.text_delta(&text),
            AgentEvent::ToolStart(e) => self.messages_panel.tool_start(e),
            AgentEvent::ToolOutput { id, content } => {
                self.messages_panel.tool_output(&id, &content)
            }
            AgentEvent::ToolDone(e) => {
                let is_plan_write = plan_path.is_some_and(|pp| {
                    e.written_path()
                        .is_some_and(|wp| wp == pp || Path::new(pp).ends_with(wp))
                });
                self.messages_panel.tool_done(e);
                if is_plan_write {
                    let pp = plan_path.unwrap();
                    if let Ok(content) = std::fs::read_to_string(pp) {
                        self.messages_panel.push(DisplayMessage::new(
                            DisplayRole::Assistant,
                            format!("```markdown\n{content}\n```\n\n`{pp}`"),
                        ));
                    }
                }
            }
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

    pub fn update_tool_summary(&mut self, tool_id: &str, summary: &str) {
        self.messages_panel.update_tool_summary(tool_id, summary);
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

    #[cfg(test)]
    pub fn last_message_text(&self) -> &str {
        self.messages_panel.last_message_text()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use maki_providers::{AgentEvent, ToolDoneEvent, ToolOutput, ToolStartEvent};

    fn tool_start(id: &str, tool: &'static str) -> AgentEvent {
        AgentEvent::ToolStart(ToolStartEvent {
            id: id.into(),
            tool,
            summary: String::new(),
            input: None,
            output: None,
        })
    }

    fn write_done(id: &str, path: &str) -> AgentEvent {
        AgentEvent::ToolDone(ToolDoneEvent {
            id: id.into(),
            tool: "write",
            output: ToolOutput::WriteCode {
                path: path.into(),
                byte_count: 42,
                lines: vec![],
            },
            is_error: false,
        })
    }

    #[test]
    fn tool_lifecycle() {
        let mut chat = Chat::new("Main".into());
        chat.handle_event(tool_start("t1", "bash"), None);
        assert_eq!(chat.in_progress_count(), 1);

        chat.handle_event(
            AgentEvent::ToolDone(ToolDoneEvent {
                id: "t1".into(),
                tool: "bash",
                output: ToolOutput::Plain("ok".into()),
                is_error: false,
            }),
            None,
        );
        assert_eq!(chat.in_progress_count(), 0);
    }

    #[test]
    fn done_returns_done() {
        let mut chat = Chat::new("Main".into());
        let result = chat.handle_event(
            AgentEvent::Done {
                usage: TokenUsage::default(),
                num_turns: 1,
                stop_reason: None,
            },
            None,
        );
        assert!(matches!(result, ChatEventResult::Done));
    }

    #[test]
    fn error_returns_error() {
        let mut chat = Chat::new("Main".into());
        let result = chat.handle_event(
            AgentEvent::Error {
                message: "boom".into(),
            },
            None,
        );
        assert!(matches!(result, ChatEventResult::Error(ref e) if e == "boom"));
    }

    #[test]
    fn plan_write_renders_file_content() {
        let mut chat = Chat::new("Main".into());
        let dir = tempfile::tempdir().unwrap();
        let plan_path = dir.path().join("plan.md");
        std::fs::write(&plan_path, "# My Plan\n\n- Step 1").unwrap();
        let plan_str = plan_path.to_str().unwrap();

        chat.handle_event(tool_start("w1", "write"), Some(plan_str));
        chat.handle_event(write_done("w1", plan_str), Some(plan_str));

        let last = chat.last_message_text();
        assert!(last.contains("# My Plan"));
        assert!(last.contains("- Step 1"));
    }

    #[test]
    fn plan_write_ignores_different_path() {
        let mut chat = Chat::new("Main".into());
        chat.handle_event(tool_start("w1", "write"), Some("/plans/123.md"));
        chat.handle_event(write_done("w1", "src/main.rs"), Some("/plans/123.md"));
        assert!(!chat.last_message_text().contains("```markdown"));
    }
}

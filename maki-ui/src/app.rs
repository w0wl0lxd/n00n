use std::collections::VecDeque;

use crate::components::input::InputBox;
use crate::components::messages::MessagesPanel;
use crate::components::queue_panel;
use crate::components::status_bar::{CancelResult, StatusBar, StatusBarContext, UsageStats};
use crate::components::{Action, DisplayMessage, DisplayRole, Status};
use crate::theme;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use maki_agent::{AgentInput, AgentMode};
use maki_providers::{AgentEvent, ModelPricing, TokenUsage};
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout};
use ratatui::widgets::{Block, Widget};

const CANCEL_MSG: &str = "Cancelled. The agent will continue from the last successful result.";

pub enum Msg {
    Key(KeyEvent),
    Paste(String),
    Agent(AgentEvent),
}

pub struct App {
    messages_panel: MessagesPanel,
    pub(crate) input_box: InputBox,
    status_bar: StatusBar,
    pub status: Status,
    pub token_usage: TokenUsage,
    pub context_size: u32,
    pub mode: AgentMode,
    pending_plan: Option<String>,
    model_id: String,
    pricing: ModelPricing,
    context_window: u32,
    pub should_quit: bool,
    pub(crate) queue: VecDeque<AgentInput>,
}

impl App {
    pub fn new(model_id: String, pricing: ModelPricing, context_window: u32) -> Self {
        Self {
            messages_panel: MessagesPanel::new(),
            input_box: InputBox::new(),
            status_bar: StatusBar::new(),
            status: Status::Idle,
            token_usage: TokenUsage::default(),
            context_size: 0,
            mode: AgentMode::Build,
            pending_plan: None,
            model_id,
            pricing,
            context_window,
            should_quit: false,
            queue: VecDeque::new(),
        }
    }

    pub fn update(&mut self, msg: Msg) -> Vec<Action> {
        match msg {
            Msg::Key(key) => self.handle_key(key),
            Msg::Paste(text) => {
                self.input_box.buffer.insert_text(&text);
                vec![]
            }
            Msg::Agent(event) => self.handle_agent_event(event),
        }
    }

    fn handle_key(&mut self, key: KeyEvent) -> Vec<Action> {
        let is_ctrl = key.modifiers.contains(KeyModifiers::CONTROL)
            && !key.modifiers.contains(KeyModifiers::ALT);
        if is_ctrl {
            let half = self.messages_panel.half_page();
            return match key.code {
                KeyCode::Char('c') => {
                    if self.input_box.buffer.value().trim().is_empty() {
                        self.should_quit = true;
                        vec![Action::Quit]
                    } else {
                        self.input_box.buffer.clear();
                        vec![]
                    }
                }
                KeyCode::Char('u') => {
                    self.messages_panel.scroll(half);
                    vec![]
                }
                KeyCode::Char('d') => {
                    self.messages_panel.scroll(-half);
                    vec![]
                }
                KeyCode::Char('y') => {
                    self.messages_panel.scroll(1);
                    vec![]
                }
                KeyCode::Char('e') => {
                    self.messages_panel.scroll(-1);
                    vec![]
                }
                KeyCode::Char('w') => {
                    self.input_box.buffer.remove_word_before_cursor();
                    vec![]
                }
                KeyCode::Left => {
                    self.input_box.buffer.move_word_left();
                    vec![]
                }
                KeyCode::Right => {
                    self.input_box.buffer.move_word_right();
                    vec![]
                }
                _ => vec![],
            };
        }

        match key.code {
            KeyCode::Up if self.status == Status::Streaming => {
                self.messages_panel.scroll(1);
                return vec![];
            }
            KeyCode::Down if self.status == Status::Streaming => {
                self.messages_panel.scroll(-1);
                return vec![];
            }
            KeyCode::Up if self.input_box.is_at_first_line() => {
                self.input_box.history_up();
                return vec![];
            }
            KeyCode::Down if self.input_box.is_at_last_line() => {
                self.input_box.history_down();
                return vec![];
            }
            KeyCode::Up => {
                self.input_box.buffer.move_up();
                return vec![];
            }
            KeyCode::Down => {
                self.input_box.buffer.move_down();
                return vec![];
            }
            KeyCode::Tab => {
                return self.toggle_mode();
            }
            KeyCode::Esc if self.status == Status::Streaming => {
                return self.handle_cancel_press();
            }
            _ => {}
        }

        match key.code {
            KeyCode::Enter if self.input_box.char_before_cursor_is_backslash() => {
                self.input_box.continue_line();
                vec![]
            }
            KeyCode::Enter => {
                let Some(text) = self.input_box.submit() else {
                    return vec![];
                };
                let pending_plan = self.pending_plan.take();
                let input = AgentInput {
                    message: text.clone(),
                    mode: self.mode.clone(),
                    pending_plan,
                };
                if self.status == Status::Streaming {
                    self.queue.push_back(input);
                    self.messages_panel.enable_auto_scroll();
                    vec![]
                } else {
                    self.push_user_message(&text);
                    self.status = Status::Streaming;
                    self.messages_panel.enable_auto_scroll();
                    vec![Action::SendMessage(input)]
                }
            }
            KeyCode::Char(c) => {
                self.input_box.buffer.push_char(c);
                vec![]
            }
            KeyCode::Backspace => {
                self.input_box.buffer.remove_char();
                vec![]
            }
            KeyCode::Delete => {
                self.input_box.buffer.delete_char();
                vec![]
            }
            KeyCode::Left => {
                self.input_box.buffer.move_left();
                vec![]
            }
            KeyCode::Right => {
                self.input_box.buffer.move_right();
                vec![]
            }
            KeyCode::Home => {
                self.input_box.buffer.move_home();
                vec![]
            }
            KeyCode::End => {
                self.input_box.buffer.move_end();
                vec![]
            }
            _ => vec![],
        }
    }

    fn handle_cancel_press(&mut self) -> Vec<Action> {
        match self.status_bar.handle_cancel_press() {
            CancelResult::Confirmed => {
                self.messages_panel.flush();
                self.messages_panel.fail_in_progress();
                self.messages_panel.push(DisplayMessage {
                    role: DisplayRole::Error,
                    text: CANCEL_MSG.into(),
                    tool_input: None,
                    tool_output: None,
                });
                self.queue.clear();
                self.status = Status::Idle;
                vec![Action::CancelAgent]
            }
            CancelResult::FirstPress => vec![],
        }
    }

    fn handle_agent_event(&mut self, event: AgentEvent) -> Vec<Action> {
        match event {
            AgentEvent::ThinkingDelta { text } => {
                self.messages_panel.thinking_delta(&text);
            }
            AgentEvent::TextDelta { text } => {
                self.messages_panel.text_delta(&text);
            }
            AgentEvent::ToolStart(e) => {
                self.messages_panel.tool_start(e);
            }
            AgentEvent::ToolOutput { id, content } => {
                self.messages_panel.tool_output(&id, &content);
            }
            AgentEvent::ToolDone(e) => {
                self.messages_panel.tool_done(e);
            }
            AgentEvent::TurnComplete { usage, .. } => {
                self.context_size = usage.context_tokens();
                self.token_usage += usage;
            }
            AgentEvent::ToolResultsSubmitted { .. } => {}
            AgentEvent::Done { .. } => {
                self.messages_panel.flush();
                self.status_bar.clear_cancel_hint();
                if let Some(input) = self.queue.pop_front() {
                    self.push_user_message(&input.message);
                    self.messages_panel.enable_auto_scroll();
                    return vec![Action::SendMessage(input)];
                }
                self.status = Status::Idle;
            }
            AgentEvent::Error { message } => {
                self.messages_panel.flush();
                self.status = Status::Error(message);
                self.status_bar.clear_cancel_hint();
                self.status_bar.mark_error();
                self.queue.clear();
            }
        }
        vec![]
    }

    fn push_user_message(&mut self, text: &str) {
        self.messages_panel.push(DisplayMessage {
            role: DisplayRole::User,
            text: text.to_string(),
            tool_input: None,
            tool_output: None,
        });
    }

    fn toggle_mode(&mut self) -> Vec<Action> {
        if self.status == Status::Streaming {
            return vec![];
        }
        match &self.mode {
            AgentMode::Build => {
                self.mode = AgentMode::Plan(maki_agent::new_plan_path());
            }
            AgentMode::Plan(path) => {
                self.pending_plan = Some(path.clone());
                self.mode = AgentMode::Build;
            }
        }
        vec![]
    }

    pub fn view(&mut self, frame: &mut Frame) {
        self.status_bar.clear_expired_hint();
        if self.status_bar.is_error_expired() {
            self.status = Status::Idle;
        }

        let bg = Block::default().style(ratatui::style::Style::new().bg(theme::BACKGROUND));
        bg.render(frame.area(), frame.buffer_mut());

        let queue_height = queue_panel::height(self.queue.len());
        let input_height = self.input_box.height(frame.area().width);
        let [msg_area, queue_area, input_area, status_area] = Layout::vertical([
            Constraint::Min(1),
            Constraint::Length(queue_height),
            Constraint::Length(input_height),
            Constraint::Length(1),
        ])
        .areas(frame.area());
        self.messages_panel.view(frame, msg_area);
        let queue_texts: Vec<&str> = self.queue.iter().map(|i| i.message.as_str()).collect();
        queue_panel::view(frame, queue_area, &queue_texts);
        self.input_box.view(frame, input_area);
        let ctx = StatusBarContext {
            status: &self.status,
            mode: &self.mode,
            model_id: &self.model_id,
            stats: UsageStats {
                usage: &self.token_usage,
                context_size: self.context_size,
                pricing: &self.pricing,
                context_window: self.context_window,
            },
            auto_scroll: self.messages_panel.auto_scroll(),
        };
        self.status_bar.view(frame, status_area, &ctx);
    }

    pub fn is_animating(&self) -> bool {
        self.messages_panel.is_animating()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::components::{TEST_CONTEXT_WINDOW, ctrl, key, test_pricing};
    use crossterm::event::KeyCode;
    use maki_providers::ToolStartEvent;

    fn test_app() -> App {
        App::new("test-model".into(), test_pricing(), TEST_CONTEXT_WINDOW)
    }

    #[test]
    fn typing_and_submit() {
        let mut app = test_app();
        app.update(Msg::Key(key(KeyCode::Char('h'))));
        app.update(Msg::Key(key(KeyCode::Char('i'))));

        let actions = app.update(Msg::Key(key(KeyCode::Enter)));
        assert_eq!(actions.len(), 1);
        assert!(matches!(&actions[0], Action::SendMessage(s) if s.message == "hi"));
        assert_eq!(app.status, Status::Streaming);
    }

    #[test]
    fn ctrl_c_clears_nonempty_input() {
        let mut app = test_app();
        app.update(Msg::Key(key(KeyCode::Char('h'))));
        app.update(Msg::Key(key(KeyCode::Char('i'))));

        let actions = app.update(Msg::Key(ctrl('c')));
        assert!(actions.is_empty());
        assert!(!app.should_quit);
        assert_eq!(app.input_box.buffer.value(), "");
    }

    #[test]
    fn ctrl_c_quits_when_input_empty() {
        for status in [Status::Idle, Status::Streaming] {
            let mut app = test_app();
            app.status = status;
            let actions = app.update(Msg::Key(ctrl('c')));
            assert!(app.should_quit);
            assert!(matches!(&actions[0], Action::Quit));
        }
    }

    #[test]
    fn done_flushes_text_and_sets_idle() {
        let mut app = test_app();
        app.status = Status::Streaming;
        app.update(Msg::Agent(AgentEvent::TextDelta {
            text: "response text".into(),
        }));
        app.update(Msg::Agent(AgentEvent::Done {
            usage: TokenUsage::default(),
            num_turns: 1,
            stop_reason: None,
        }));

        assert_eq!(app.status, Status::Idle);
    }

    #[test]
    fn turn_complete_accumulates_usage_and_sets_context_size() {
        let mut app = test_app();
        app.status = Status::Streaming;
        let usage = TokenUsage {
            input: 1_000,
            output: 500,
            cache_creation: 200,
            cache_read: 3_000,
        };
        app.update(Msg::Agent(AgentEvent::TurnComplete {
            message: Default::default(),
            usage: usage.clone(),
            model: "test-model".into(),
        }));
        assert_eq!(app.context_size, usage.context_tokens());
        assert_eq!(app.token_usage.input, 1_000);
        assert_eq!(app.token_usage.output, 500);

        app.update(Msg::Agent(AgentEvent::TurnComplete {
            message: Default::default(),
            usage: TokenUsage {
                input: 20,
                output: 10,
                ..Default::default()
            },
            model: "test-model".into(),
        }));
        assert_eq!(app.token_usage.input, 1_020);
        assert_eq!(app.token_usage.output, 510);
    }

    #[test]
    fn error_event_sets_status() {
        let mut app = test_app();
        app.status = Status::Streaming;
        app.update(Msg::Agent(AgentEvent::Error {
            message: "boom".into(),
        }));
        assert!(matches!(app.status, Status::Error(ref e) if e == "boom"));
    }

    #[test]
    fn tab_toggles_mode_and_sets_pending_plan() {
        let mut app = test_app();
        assert_eq!(app.mode, AgentMode::Build);

        app.update(Msg::Key(key(KeyCode::Tab)));
        assert!(matches!(app.mode, AgentMode::Plan(ref p) if p.contains(maki_agent::PLANS_DIR)));

        app.update(Msg::Key(key(KeyCode::Tab)));
        assert_eq!(app.mode, AgentMode::Build);
        assert!(app.pending_plan.is_some());
    }

    #[test]
    fn submit_consumes_pending_plan() {
        let mut app = test_app();
        app.pending_plan = Some("plan.md".into());
        app.update(Msg::Key(key(KeyCode::Char('x'))));
        let actions = app.update(Msg::Key(key(KeyCode::Enter)));
        let Action::SendMessage(ref input) = actions[0] else {
            panic!("expected SendMessage");
        };
        assert_eq!(input.pending_plan.as_deref(), Some("plan.md"));
        assert!(app.pending_plan.is_none());
    }

    #[test]
    fn backslash_enter_creates_newline() {
        let mut app = test_app();
        for c in "hello\\".chars() {
            app.update(Msg::Key(key(KeyCode::Char(c))));
        }
        let actions = app.update(Msg::Key(key(KeyCode::Enter)));
        assert!(actions.is_empty(), "backslash-enter should not submit");
        assert_eq!(app.input_box.buffer.lines(), &["hello", ""]);

        for c in "world".chars() {
            app.update(Msg::Key(key(KeyCode::Char(c))));
        }
        let actions = app.update(Msg::Key(key(KeyCode::Enter)));
        assert_eq!(actions.len(), 1);
        assert!(matches!(&actions[0], Action::SendMessage(s) if s.message == "hello\nworld"));
    }

    #[test]
    fn altgr_chars_not_swallowed_by_ctrl_handler() {
        let mut app = test_app();
        let altgr_backslash = KeyEvent {
            code: KeyCode::Char('\\'),
            modifiers: KeyModifiers::CONTROL | KeyModifiers::ALT,
            kind: crossterm::event::KeyEventKind::Press,
            state: crossterm::event::KeyEventState::NONE,
        };
        app.update(Msg::Key(key(KeyCode::Char('h'))));
        app.update(Msg::Key(key(KeyCode::Char('i'))));
        app.update(Msg::Key(altgr_backslash));
        assert_eq!(app.input_box.buffer.value(), "hi\\");
    }

    #[test]
    fn double_esc_cancels_flushes_and_fails_tools() {
        let mut app = test_app();
        app.status = Status::Streaming;
        app.update(Msg::Agent(AgentEvent::TextDelta {
            text: "partial".into(),
        }));
        app.update(Msg::Agent(AgentEvent::ToolStart(ToolStartEvent {
            id: "t1".into(),
            tool: "bash",
            summary: "running".into(),
            input: None,
        })));

        let actions = app.update(Msg::Key(key(KeyCode::Esc)));
        assert!(actions.is_empty());

        let actions = app.update(Msg::Key(key(KeyCode::Esc)));
        assert!(matches!(&actions[0], Action::CancelAgent));
        assert_eq!(app.status, Status::Idle);
        assert_eq!(app.messages_panel.in_progress_count(), 0);
    }

    #[test]
    fn paste_works_regardless_of_status() {
        for status in [Status::Idle, Status::Streaming] {
            let mut app = test_app();
            app.status = status;
            let actions = app.update(Msg::Paste("pasted".into()));
            assert!(actions.is_empty());
            assert_eq!(app.input_box.buffer.value(), "pasted");
        }
    }

    #[test]
    fn submit_during_streaming_queues_message() {
        let mut app = test_app();
        app.update(Msg::Key(key(KeyCode::Char('a'))));
        let actions = app.update(Msg::Key(key(KeyCode::Enter)));
        assert_eq!(actions.len(), 1);
        assert!(matches!(&actions[0], Action::SendMessage(_)));
        assert_eq!(app.status, Status::Streaming);

        app.update(Msg::Key(key(KeyCode::Char('b'))));
        let actions = app.update(Msg::Key(key(KeyCode::Enter)));
        assert!(actions.is_empty());
        assert_eq!(app.queue.len(), 1);
        assert_eq!(app.queue[0].message, "b");
    }

    #[test]
    fn done_drains_queued_message() {
        let mut app = app_with_queued_message();
        let actions = app.update(Msg::Agent(AgentEvent::Done {
            usage: TokenUsage::default(),
            num_turns: 1,
            stop_reason: None,
        }));
        assert_eq!(actions.len(), 1);
        assert!(matches!(&actions[0], Action::SendMessage(i) if i.message == "queued"));
        assert!(app.queue.is_empty());
        assert_eq!(app.status, Status::Streaming);
    }

    #[test]
    fn error_clears_queue() {
        let mut app = app_with_queued_message();
        app.update(Msg::Agent(AgentEvent::Error {
            message: "boom".into(),
        }));
        assert!(app.queue.is_empty());
    }

    #[test]
    fn cancel_clears_queue() {
        let mut app = app_with_queued_message();
        app.update(Msg::Key(key(KeyCode::Esc)));
        let actions = app.update(Msg::Key(key(KeyCode::Esc)));
        assert!(matches!(&actions[0], Action::CancelAgent));
        assert!(app.queue.is_empty());
    }

    fn app_with_queued_message() -> App {
        let mut app = test_app();
        app.status = Status::Streaming;
        app.queue.push_back(AgentInput {
            message: "queued".into(),
            mode: AgentMode::Build,
            pending_plan: None,
        });
        app
    }
}

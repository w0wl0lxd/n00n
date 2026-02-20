use crate::components::input::InputBox;
use crate::components::messages::MessagesPanel;
use crate::components::status_bar::{CancelResult, StatusBar, UsageStats};
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
    Agent(AgentEvent),
}

pub struct App {
    messages_panel: MessagesPanel,
    input_box: InputBox,
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
        }
    }

    pub fn update(&mut self, msg: Msg) -> Vec<Action> {
        match msg {
            Msg::Key(key) => self.handle_key(key),
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
                    self.should_quit = true;
                    vec![Action::Quit]
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
                KeyCode::Char('w') if self.status != Status::Streaming => {
                    self.input_box.buffer.remove_word_before_cursor();
                    vec![]
                }
                KeyCode::Char('k') if self.status != Status::Streaming => {
                    self.input_box.buffer.remove_line();
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

        if self.status == Status::Streaming {
            return vec![];
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
                self.messages_panel.push(DisplayMessage {
                    role: DisplayRole::User,
                    text: text.clone(),
                    tool_output: None,
                });
                self.status = Status::Streaming;
                self.messages_panel.enable_auto_scroll();
                vec![Action::SendMessage(AgentInput {
                    message: text,
                    mode: self.mode.clone(),
                    pending_plan,
                })]
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
                self.messages_panel.push(DisplayMessage {
                    role: DisplayRole::Error,
                    text: CANCEL_MSG.into(),
                    tool_output: None,
                });
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
            AgentEvent::ToolDone(e) => {
                self.messages_panel.tool_done(e);
            }
            AgentEvent::TurnComplete { usage, .. } => {
                self.context_size = usage.context_tokens();
            }
            AgentEvent::ToolResultsSubmitted { .. } => {}
            AgentEvent::Done { usage, .. } => {
                self.messages_panel.flush();
                self.token_usage += usage;
                self.status = Status::Idle;
                self.status_bar.clear_cancel_hint();
            }
            AgentEvent::Error { message } => {
                self.messages_panel.flush();
                self.status = Status::Error(message);
                self.status_bar.clear_cancel_hint();
            }
        }
        vec![]
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

        let bg = Block::default().style(ratatui::style::Style::new().bg(theme::BACKGROUND));
        bg.render(frame.area(), frame.buffer_mut());

        let input_height = self.input_box.height();
        let [msg_area, input_area, status_area] = Layout::vertical([
            Constraint::Min(1),
            Constraint::Length(input_height),
            Constraint::Length(1),
        ])
        .areas(frame.area());

        let is_streaming = self.status == Status::Streaming;
        self.messages_panel.view(frame, msg_area);
        self.input_box.view(frame, input_area, is_streaming);
        let stats = UsageStats {
            usage: &self.token_usage,
            context_size: self.context_size,
            pricing: &self.pricing,
            context_window: self.context_window,
        };
        self.status_bar.view(
            frame,
            status_area,
            &self.status,
            &self.mode,
            &self.model_id,
            &stats,
        );
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

    #[test]
    fn typing_and_submit() {
        let mut app = App::new("test-model".into(), test_pricing(), TEST_CONTEXT_WINDOW);
        app.update(Msg::Key(key(KeyCode::Char('h'))));
        app.update(Msg::Key(key(KeyCode::Char('i'))));

        let actions = app.update(Msg::Key(key(KeyCode::Enter)));
        assert_eq!(actions.len(), 1);
        assert!(matches!(&actions[0], Action::SendMessage(s) if s.message == "hi"));
        assert_eq!(app.status, Status::Streaming);
    }

    #[test]
    fn ctrl_c_quits_regardless_of_state() {
        for status in [Status::Idle, Status::Streaming] {
            let mut app = App::new("test-model".into(), test_pricing(), TEST_CONTEXT_WINDOW);
            app.status = status;
            let actions = app.update(Msg::Key(ctrl('c')));
            assert!(app.should_quit);
            assert!(matches!(&actions[0], Action::Quit));
        }
    }

    #[test]
    fn done_flushes_text_and_accumulates_usage() {
        let mut app = App::new("test-model".into(), test_pricing(), TEST_CONTEXT_WINDOW);
        app.status = Status::Streaming;
        app.update(Msg::Agent(AgentEvent::TextDelta {
            text: "response text".into(),
        }));
        app.update(Msg::Agent(AgentEvent::Done {
            usage: TokenUsage {
                input: 100,
                output: 50,
                ..Default::default()
            },
            num_turns: 1,
            stop_reason: None,
        }));

        assert_eq!(app.status, Status::Idle);
        assert_eq!(app.token_usage.input, 100);
        assert_eq!(app.token_usage.output, 50);

        app.status = Status::Streaming;
        app.update(Msg::Agent(AgentEvent::Done {
            usage: TokenUsage {
                input: 20,
                output: 10,
                ..Default::default()
            },
            num_turns: 1,
            stop_reason: None,
        }));
        assert_eq!(app.token_usage.input, 120);
        assert_eq!(app.token_usage.output, 60);
    }

    #[test]
    fn turn_complete_sets_context_size() {
        let mut app = App::new("test-model".into(), test_pricing(), TEST_CONTEXT_WINDOW);
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
    }

    #[test]
    fn error_event_sets_status() {
        let mut app = App::new("test-model".into(), test_pricing(), TEST_CONTEXT_WINDOW);
        app.status = Status::Streaming;
        app.update(Msg::Agent(AgentEvent::Error {
            message: "boom".into(),
        }));
        assert!(matches!(app.status, Status::Error(ref e) if e == "boom"));
    }

    #[test]
    fn tab_toggles_mode_and_sets_pending_plan() {
        let mut app = App::new("test-model".into(), test_pricing(), TEST_CONTEXT_WINDOW);
        assert_eq!(app.mode, AgentMode::Build);

        app.update(Msg::Key(key(KeyCode::Tab)));
        assert!(matches!(app.mode, AgentMode::Plan(ref p) if p.contains(maki_agent::PLANS_DIR)));

        app.update(Msg::Key(key(KeyCode::Tab)));
        assert_eq!(app.mode, AgentMode::Build);
        assert!(app.pending_plan.is_some());
    }

    #[test]
    fn submit_consumes_pending_plan() {
        let mut app = App::new("test-model".into(), test_pricing(), TEST_CONTEXT_WINDOW);
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
        let mut app = App::new("test-model".into(), test_pricing(), TEST_CONTEXT_WINDOW);
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
        let mut app = App::new("test-model".into(), test_pricing(), TEST_CONTEXT_WINDOW);
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

        let actions = app.update(Msg::Key(key(KeyCode::Enter)));
        assert!(actions.is_empty(), "backslash-enter should not submit");
        assert_eq!(app.input_box.buffer.lines(), &["hi", ""]);
    }

    #[test]
    fn double_esc_cancels_and_flushes() {
        let mut app = App::new("test-model".into(), test_pricing(), TEST_CONTEXT_WINDOW);
        app.status = Status::Streaming;
        app.update(Msg::Agent(AgentEvent::TextDelta {
            text: "partial response".into(),
        }));

        let actions = app.update(Msg::Key(key(KeyCode::Esc)));
        assert!(actions.is_empty());

        let actions = app.update(Msg::Key(key(KeyCode::Esc)));
        assert!(matches!(&actions[0], Action::CancelAgent));
        assert_eq!(app.status, Status::Idle);
    }
}

use std::collections::{HashMap, VecDeque};

use crate::chat::{Chat, ChatEventResult};
use crate::components::chat_picker::{ChatPicker, ChatPickerAction};
use crate::components::command::{CommandAction, CommandPalette};
use crate::components::input::{InputAction, InputBox};
use crate::components::queue_panel;
use crate::components::status_bar::{CancelResult, StatusBar, StatusBarContext, UsageStats};
use crate::components::{Action, DisplayMessage, DisplayRole, Status, is_ctrl};
use crate::theme;

use crossterm::event::{KeyCode, KeyEvent};
use maki_agent::{AgentInput, AgentMode};
use maki_providers::{AgentEvent, Envelope, ModelPricing, TokenUsage};
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout};
use ratatui::widgets::{Block, Widget};

const CANCEL_MSG: &str = "Cancelled. The agent will continue from the last successful result.";

pub enum Msg {
    Key(KeyEvent),
    Paste(String),
    Agent(Envelope),
}

pub struct App {
    chats: Vec<Chat>,
    active_chat: usize,
    chat_index: HashMap<String, usize>,
    task_names: HashMap<String, String>,
    pub(crate) input_box: InputBox,
    command_palette: CommandPalette,
    chat_picker: ChatPicker,
    status_bar: StatusBar,
    pub status: Status,
    pub token_usage: TokenUsage,
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
            chats: vec![Chat::new("Main".into())],
            active_chat: 0,
            chat_index: HashMap::new(),
            task_names: HashMap::new(),
            input_box: InputBox::new(),
            command_palette: CommandPalette::new(),
            chat_picker: ChatPicker::new(),
            status_bar: StatusBar::new(),
            status: Status::Idle,
            token_usage: TokenUsage::default(),
            mode: AgentMode::Build,
            pending_plan: None,
            model_id,
            pricing,
            context_window,
            should_quit: false,
            queue: VecDeque::new(),
        }
    }

    fn main_chat(&mut self) -> &mut Chat {
        &mut self.chats[0]
    }

    fn active_chat(&mut self) -> &mut Chat {
        &mut self.chats[self.active_chat]
    }

    pub fn update(&mut self, msg: Msg) -> Vec<Action> {
        match msg {
            Msg::Key(key) => self.handle_key(key),
            Msg::Paste(text) => {
                if let InputAction::PaletteSync(val) = self.input_box.handle_paste(&text) {
                    self.command_palette.sync(&val);
                }
                vec![]
            }
            Msg::Agent(envelope) => self.handle_agent_event(envelope),
        }
    }

    fn handle_key(&mut self, key: KeyEvent) -> Vec<Action> {
        if self.chat_picker.is_open() {
            let names = self.chat_names();
            return match self.chat_picker.handle_key(key, &names) {
                ChatPickerAction::Consumed => vec![],
                ChatPickerAction::Select(idx) => {
                    self.active_chat = idx;
                    vec![]
                }
            };
        }

        if is_ctrl(&key) {
            let half = self.chats[self.active_chat].half_page();
            return match key.code {
                KeyCode::Char('c') => {
                    self.command_palette.close();
                    if self.input_box.buffer.value().trim().is_empty() {
                        self.should_quit = true;
                        vec![Action::Quit]
                    } else {
                        self.input_box.buffer.clear();
                        vec![]
                    }
                }
                KeyCode::Char('p') => {
                    self.active_chat = self.active_chat.saturating_sub(1);
                    vec![]
                }
                KeyCode::Char('n') => {
                    self.active_chat = (self.active_chat + 1).min(self.chats.len() - 1);
                    vec![]
                }
                KeyCode::Char('u') => {
                    self.active_chat().scroll(half);
                    vec![]
                }
                KeyCode::Char('d') => {
                    self.active_chat().scroll(-half);
                    vec![]
                }
                KeyCode::Char('y') => {
                    self.active_chat().scroll(1);
                    vec![]
                }
                KeyCode::Char('e') => {
                    self.active_chat().scroll(-1);
                    vec![]
                }
                _ => {
                    if let InputAction::PaletteSync(val) = self.input_box.handle_key(key) {
                        self.command_palette.sync(&val);
                    }
                    vec![]
                }
            };
        }

        match self.command_palette.handle_key(key) {
            CommandAction::Consumed => return vec![],
            CommandAction::Execute(name) => return self.execute_command(name),
            CommandAction::Close => {}
            CommandAction::Passthrough => {}
        }

        let streaming = self.status == Status::Streaming;
        match self.input_box.handle_key(key) {
            InputAction::Submit(text) => self.handle_submit(text),
            InputAction::PaletteSync(val) => {
                self.command_palette.sync(&val);
                vec![]
            }
            InputAction::Passthrough(key) => match key.code {
                KeyCode::Up if streaming => {
                    self.active_chat().scroll(1);
                    vec![]
                }
                KeyCode::Down if streaming => {
                    self.active_chat().scroll(-1);
                    vec![]
                }
                KeyCode::Tab => self.toggle_mode(),
                KeyCode::Esc if streaming => self.handle_cancel_press(),
                _ => vec![],
            },
            InputAction::ContinueLine | InputAction::None => vec![],
        }
    }

    fn handle_submit(&mut self, text: String) -> Vec<Action> {
        let pending_plan = self.pending_plan.take();
        let input = AgentInput {
            message: text.clone(),
            mode: self.mode.clone(),
            pending_plan,
        };
        if self.status == Status::Streaming {
            self.queue.push_back(input);
            self.main_chat().enable_auto_scroll();
            vec![]
        } else {
            self.main_chat().push_user_message(&text);
            self.status = Status::Streaming;
            self.main_chat().enable_auto_scroll();
            vec![Action::SendMessage(input)]
        }
    }

    fn handle_cancel_press(&mut self) -> Vec<Action> {
        match self.status_bar.handle_cancel_press() {
            CancelResult::Confirmed => {
                for chat in &mut self.chats {
                    chat.flush();
                    chat.fail_in_progress();
                }
                self.main_chat()
                    .push(DisplayMessage::new(DisplayRole::Error, CANCEL_MSG.into()));
                self.queue.clear();
                self.chat_index.clear();
                self.task_names.clear();
                self.status = Status::Idle;
                vec![Action::CancelAgent]
            }
            CancelResult::FirstPress => vec![],
        }
    }

    fn handle_agent_event(&mut self, envelope: Envelope) -> Vec<Action> {
        if envelope.parent_tool_use_id.is_none()
            && let AgentEvent::ToolStart(ref e) = envelope.event
            && e.tool == "task"
        {
            self.task_names.insert(e.id.clone(), e.summary.clone());
        }

        let chat_idx = self.resolve_or_create_chat(envelope.parent_tool_use_id.as_deref());
        let plan_path = match &self.mode {
            AgentMode::Plan(p) => Some(p.as_str()),
            AgentMode::Build => None,
        };

        if let AgentEvent::TurnComplete { usage, .. } = &envelope.event {
            self.token_usage += usage.clone();
            self.chats[chat_idx].token_usage += usage.clone();
            self.chats[chat_idx].context_size = usage.context_tokens();
        }

        let result = self.chats[chat_idx].handle_event(envelope.event, plan_path);

        if chat_idx == 0 {
            match result {
                ChatEventResult::Done => {
                    self.status_bar.clear_cancel_hint();
                    if let Some(input) = self.queue.pop_front() {
                        self.main_chat().push_user_message(&input.message);
                        self.main_chat().enable_auto_scroll();
                        return vec![Action::SendMessage(input)];
                    }
                    self.status = Status::Idle;
                }
                ChatEventResult::Error(message) => {
                    self.status = Status::Error(message);
                    self.status_bar.clear_cancel_hint();
                    self.status_bar.mark_error();
                    self.queue.clear();
                }
                ChatEventResult::Continue => {}
            }
        }
        vec![]
    }

    fn resolve_or_create_chat(&mut self, parent_id: Option<&str>) -> usize {
        let Some(id) = parent_id else { return 0 };
        if let Some(&idx) = self.chat_index.get(id) {
            return idx;
        }
        let name = self
            .task_names
            .remove(id)
            .unwrap_or_else(|| format!("Agent {}", self.chats.len()));
        let idx = self.chats.len();
        self.chats.push(Chat::new(name.clone()));
        self.chat_index.insert(id.to_owned(), idx);
        self.chats[0].update_tool_summary(id, &name);
        idx
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

    fn chat_names(&self) -> Vec<String> {
        self.chats.iter().map(|c| c.name.clone()).collect()
    }

    fn execute_command(&mut self, name: &str) -> Vec<Action> {
        self.input_box.buffer.clear();
        match name {
            "/chats" => {
                let names = self.chat_names();
                self.chat_picker.open(self.active_chat, &names);
                vec![]
            }
            "/new" => self.reset_session(),
            _ => vec![],
        }
    }

    fn reset_session(&mut self) -> Vec<Action> {
        self.chats.clear();
        self.chats.push(Chat::new("Main".into()));
        self.active_chat = 0;
        self.chat_index.clear();
        self.task_names.clear();
        self.status = Status::Idle;
        self.token_usage = TokenUsage::default();
        self.queue.clear();
        self.pending_plan = None;
        self.status_bar.clear_cancel_hint();
        self.chat_picker.close();
        vec![Action::NewSession]
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
        let picker_open = self.chat_picker.is_open();
        let names = if picker_open {
            Some(self.chat_names())
        } else {
            None
        };
        let render_chat = if let Some(ref names) = names {
            self.chat_picker
                .selected_chat(names)
                .unwrap_or(self.active_chat)
        } else {
            self.active_chat
        };
        self.chats[render_chat].view(frame, msg_area);
        let queue_texts: Vec<&str> = self.queue.iter().map(|i| i.message.as_str()).collect();
        queue_panel::view(frame, queue_area, &queue_texts);
        self.input_box
            .view(frame, input_area, self.status == Status::Streaming);
        self.command_palette.view(frame, input_area);
        if let Some(names) = names {
            let full_area = frame.area();
            self.chat_picker.view(frame, full_area, &names);
        }
        let chat = &self.chats[render_chat];
        let chat_name = (self.chats.len() > 1).then_some(chat.name.as_str());
        let ctx = StatusBarContext {
            status: &self.status,
            mode: &self.mode,
            model_id: &self.model_id,
            stats: UsageStats {
                usage: &chat.token_usage,
                global_usage: &self.token_usage,
                context_size: chat.context_size,
                pricing: &self.pricing,
                context_window: self.context_window,
                show_global: self.chats.len() > 1,
            },
            auto_scroll: chat.auto_scroll(),
            chat_name,
        };
        self.status_bar.view(frame, status_area, &ctx);
    }

    pub fn is_animating(&self) -> bool {
        self.chats.iter().any(|c| c.is_animating())
    }

    pub fn load_messages(&mut self, msgs: Vec<DisplayMessage>) {
        self.main_chat().load_messages(msgs);
    }

    pub fn load_subagent(&mut self, parent_tool_id: &str, name: &str, msgs: Vec<DisplayMessage>) {
        let idx = self.chats.len();
        let mut chat = Chat::new(name.to_owned());
        chat.load_messages(msgs);
        self.chats.push(chat);
        self.chat_index.insert(parent_tool_id.to_owned(), idx);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::components::{TEST_CONTEXT_WINDOW, ctrl, key, test_pricing};
    use crossterm::event::{KeyCode, KeyModifiers};
    use maki_providers::ToolStartEvent;

    fn test_app() -> App {
        App::new("test-model".into(), test_pricing(), TEST_CONTEXT_WINDOW)
    }

    fn agent_msg(event: AgentEvent) -> Msg {
        Msg::Agent(Envelope {
            event,
            parent_tool_use_id: None,
        })
    }

    fn subagent_msg(event: AgentEvent, parent_id: &str) -> Msg {
        Msg::Agent(Envelope {
            event,
            parent_tool_use_id: Some(parent_id.into()),
        })
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
        app.update(agent_msg(AgentEvent::TextDelta {
            text: "response text".into(),
        }));
        app.update(agent_msg(AgentEvent::Done {
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
        app.update(agent_msg(AgentEvent::TurnComplete {
            message: Default::default(),
            usage: usage.clone(),
            model: "test-model".into(),
        }));
        assert_eq!(app.chats[0].context_size, usage.context_tokens());
        assert_eq!(app.token_usage.input, 1_000);
        assert_eq!(app.token_usage.output, 500);

        app.update(agent_msg(AgentEvent::TurnComplete {
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
        app.update(agent_msg(AgentEvent::Error {
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
        app.update(agent_msg(AgentEvent::TextDelta {
            text: "partial".into(),
        }));
        app.update(agent_msg(AgentEvent::ToolStart(ToolStartEvent {
            id: "t1".into(),
            tool: "bash",
            summary: "running".into(),
            input: None,
            output: None,
        })));

        let actions = app.update(Msg::Key(key(KeyCode::Esc)));
        assert!(actions.is_empty());

        let actions = app.update(Msg::Key(key(KeyCode::Esc)));
        assert!(matches!(&actions[0], Action::CancelAgent));
        assert_eq!(app.status, Status::Idle);
        assert_eq!(app.chats[0].in_progress_count(), 0);
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
        let actions = app.update(agent_msg(AgentEvent::Done {
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
        app.update(agent_msg(AgentEvent::Error {
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

    fn type_slash(app: &mut App) {
        app.update(Msg::Key(key(KeyCode::Char('/'))));
    }

    #[test]
    fn typing_filters_palette() {
        let mut app = test_app();
        type_slash(&mut app);
        app.update(Msg::Key(key(KeyCode::Char('n'))));
        assert!(app.command_palette.is_active());

        app.update(Msg::Key(key(KeyCode::Char('z'))));
        assert!(!app.command_palette.is_active());
    }

    #[test]
    fn enter_executes_new_command() {
        let mut app = test_app();
        type_slash(&mut app);
        app.update(Msg::Key(key(KeyCode::Char('n'))));
        let actions = app.update(Msg::Key(key(KeyCode::Enter)));
        assert!(matches!(&actions[0], Action::NewSession));
        assert!(!app.command_palette.is_active());
    }

    #[test]
    fn ctrl_c_closes_palette() {
        let mut app = test_app();
        type_slash(&mut app);
        assert!(app.command_palette.is_active());

        app.update(Msg::Key(ctrl('c')));
        assert!(!app.command_palette.is_active());
    }

    #[test]
    fn reset_session_clears_state() {
        let mut app = test_app();
        app.token_usage.input = 500;
        app.chats[0].context_size = 1000;
        app.pending_plan = Some("plan.md".into());
        app.queue.push_back(AgentInput {
            message: "q".into(),
            mode: AgentMode::Build,
            pending_plan: None,
        });
        let actions = app.reset_session();
        assert!(matches!(&actions[0], Action::NewSession));
        assert_eq!(app.status, Status::Idle);
        assert_eq!(app.token_usage.input, 0);
        assert_eq!(app.chats[0].context_size, 0);
        assert!(app.pending_plan.is_none());
        assert!(app.queue.is_empty());
        assert_eq!(app.chats.len(), 1);
        assert_eq!(app.chats[0].name, "Main");
        assert_eq!(app.active_chat, 0);
        assert!(app.chat_index.is_empty());
        assert!(app.task_names.is_empty());
    }

    #[test]
    fn tab_in_palette_closes_and_toggles_mode() {
        let mut app = test_app();
        type_slash(&mut app);
        assert!(app.command_palette.is_active());

        app.update(Msg::Key(key(KeyCode::Tab)));
        assert!(!app.command_palette.is_active());
        assert!(matches!(app.mode, AgentMode::Plan(_)));
    }

    #[test]
    fn ctrl_p_n_navigation() {
        let mut app = test_app();
        app.status = Status::Streaming;
        app.update(agent_msg(AgentEvent::ToolStart(ToolStartEvent {
            id: "task1".into(),
            tool: "task",
            summary: "research".into(),
            input: None,
            output: None,
        })));
        app.update(subagent_msg(
            AgentEvent::TextDelta { text: "sub".into() },
            "task1",
        ));
        assert_eq!(app.chats.len(), 2);
        assert_eq!(app.active_chat, 0);

        app.update(Msg::Key(ctrl('n')));
        assert_eq!(app.active_chat, 1);

        app.update(Msg::Key(ctrl('n')));
        assert_eq!(app.active_chat, 1);

        app.update(Msg::Key(ctrl('p')));
        assert_eq!(app.active_chat, 0);

        app.update(Msg::Key(ctrl('p')));
        assert_eq!(app.active_chat, 0);
    }

    #[test]
    fn subagent_event_creates_chat() {
        let mut app = test_app();
        app.status = Status::Streaming;
        app.update(agent_msg(AgentEvent::ToolStart(ToolStartEvent {
            id: "task1".into(),
            tool: "task",
            summary: "research".into(),
            input: None,
            output: None,
        })));
        app.update(subagent_msg(
            AgentEvent::TextDelta { text: "hi".into() },
            "task1",
        ));
        assert_eq!(app.chats.len(), 2);
        assert_eq!(app.chats[1].name, "research");
    }

    #[test]
    fn multiple_subagents_get_descriptive_names() {
        let mut app = test_app();
        app.status = Status::Streaming;
        app.update(agent_msg(AgentEvent::ToolStart(ToolStartEvent {
            id: "task1".into(),
            tool: "task",
            summary: "first".into(),
            input: None,
            output: None,
        })));
        app.update(agent_msg(AgentEvent::ToolStart(ToolStartEvent {
            id: "task2".into(),
            tool: "task",
            summary: "second".into(),
            input: None,
            output: None,
        })));
        app.update(subagent_msg(
            AgentEvent::TextDelta { text: "a".into() },
            "task1",
        ));
        app.update(subagent_msg(
            AgentEvent::TextDelta { text: "b".into() },
            "task2",
        ));
        assert_eq!(app.chats.len(), 3);
        assert_eq!(app.chats[1].name, "first");
        assert_eq!(app.chats[2].name, "second");
    }

    #[test]
    fn turn_complete_accumulates_usage_globally() {
        let mut app = test_app();
        app.status = Status::Streaming;
        app.update(agent_msg(AgentEvent::ToolStart(ToolStartEvent {
            id: "task1".into(),
            tool: "task",
            summary: "research".into(),
            input: None,
            output: None,
        })));
        app.update(subagent_msg(
            AgentEvent::TextDelta { text: "x".into() },
            "task1",
        ));

        let main_usage = TokenUsage {
            input: 100,
            output: 50,
            ..Default::default()
        };
        app.update(agent_msg(AgentEvent::TurnComplete {
            message: Default::default(),
            usage: main_usage,
            model: "test".into(),
        }));

        let sub_usage = TokenUsage {
            input: 200,
            output: 75,
            ..Default::default()
        };
        app.update(subagent_msg(
            AgentEvent::TurnComplete {
                message: Default::default(),
                usage: sub_usage,
                model: "test".into(),
            },
            "task1",
        ));

        assert_eq!(app.token_usage.input, 300);
        assert_eq!(app.token_usage.output, 125);
        assert_eq!(app.chats[0].token_usage.input, 100);
        assert_eq!(app.chats[0].token_usage.output, 50);
        assert_eq!(app.chats[1].token_usage.input, 200);
        assert_eq!(app.chats[1].token_usage.output, 75);
    }

    #[test]
    fn context_size_per_chat() {
        let mut app = test_app();
        app.status = Status::Streaming;
        app.update(agent_msg(AgentEvent::ToolStart(ToolStartEvent {
            id: "task1".into(),
            tool: "task",
            summary: "research".into(),
            input: None,
            output: None,
        })));
        app.update(subagent_msg(
            AgentEvent::TextDelta { text: "x".into() },
            "task1",
        ));

        let main_usage = TokenUsage {
            input: 100,
            output: 50,
            ..Default::default()
        };
        app.update(agent_msg(AgentEvent::TurnComplete {
            message: Default::default(),
            usage: main_usage.clone(),
            model: "test".into(),
        }));
        assert_eq!(app.chats[0].context_size, main_usage.context_tokens());
        assert_eq!(app.chats[1].context_size, 0);

        let sub_usage = TokenUsage {
            input: 9999,
            output: 9999,
            ..Default::default()
        };
        app.update(subagent_msg(
            AgentEvent::TurnComplete {
                message: Default::default(),
                usage: sub_usage.clone(),
                model: "test".into(),
            },
            "task1",
        ));
        assert_eq!(app.chats[0].context_size, main_usage.context_tokens());
        assert_eq!(app.chats[1].context_size, sub_usage.context_tokens());
    }

    #[test]
    fn cancel_fails_all_chats() {
        let mut app = test_app();
        app.status = Status::Streaming;
        app.update(agent_msg(AgentEvent::ToolStart(ToolStartEvent {
            id: "task1".into(),
            tool: "task",
            summary: "research".into(),
            input: None,
            output: None,
        })));
        app.update(subagent_msg(
            AgentEvent::ToolStart(ToolStartEvent {
                id: "sub_t1".into(),
                tool: "bash",
                summary: "running".into(),
                input: None,
                output: None,
            }),
            "task1",
        ));

        app.update(Msg::Key(key(KeyCode::Esc)));
        let actions = app.update(Msg::Key(key(KeyCode::Esc)));
        assert!(matches!(&actions[0], Action::CancelAgent));
        assert_eq!(app.chats[0].in_progress_count(), 0);
        assert_eq!(app.chats[1].in_progress_count(), 0);
    }

    #[test]
    fn cancel_clears_chat_index() {
        let mut app = test_app();
        app.status = Status::Streaming;
        app.update(agent_msg(AgentEvent::ToolStart(ToolStartEvent {
            id: "task1".into(),
            tool: "task",
            summary: "research".into(),
            input: None,
            output: None,
        })));
        app.update(subagent_msg(
            AgentEvent::TextDelta { text: "x".into() },
            "task1",
        ));
        assert!(!app.chat_index.is_empty());

        app.update(Msg::Key(key(KeyCode::Esc)));
        app.update(Msg::Key(key(KeyCode::Esc)));
        assert!(app.chat_index.is_empty());
    }

    fn open_chats_picker(app: &mut App) {
        for c in "/chats".chars() {
            app.update(Msg::Key(key(KeyCode::Char(c))));
        }
        app.update(Msg::Key(key(KeyCode::Enter)));
    }

    fn app_with_subagent() -> App {
        let mut app = test_app();
        app.status = Status::Streaming;
        app.update(agent_msg(AgentEvent::ToolStart(ToolStartEvent {
            id: "task1".into(),
            tool: "task",
            summary: "research".into(),
            input: None,
            output: None,
        })));
        app.update(subagent_msg(
            AgentEvent::TextDelta { text: "x".into() },
            "task1",
        ));
        app
    }

    #[test]
    fn chats_command_opens_picker() {
        let mut app = test_app();
        open_chats_picker(&mut app);
        assert!(app.chat_picker.is_open());
    }

    #[test]
    fn picker_escape_restores_chat() {
        let mut app = app_with_subagent();
        assert_eq!(app.active_chat, 0);

        open_chats_picker(&mut app);
        app.update(Msg::Key(key(KeyCode::Down)));
        app.update(Msg::Key(key(KeyCode::Esc)));

        assert!(!app.chat_picker.is_open());
        assert_eq!(app.active_chat, 0);
    }

    #[test]
    fn picker_enter_stays_at_navigated() {
        let mut app = app_with_subagent();

        open_chats_picker(&mut app);
        app.update(Msg::Key(key(KeyCode::Down)));
        app.update(Msg::Key(key(KeyCode::Enter)));

        assert!(!app.chat_picker.is_open());
        assert_eq!(app.active_chat, 1);
    }

    #[test]
    fn picker_swallows_ctrl_keys() {
        let mut app = app_with_subagent();

        open_chats_picker(&mut app);
        app.update(Msg::Key(ctrl('n')));
        app.update(Msg::Key(ctrl('p')));
        app.update(Msg::Key(ctrl('u')));
        app.update(Msg::Key(ctrl('d')));

        assert!(app.chat_picker.is_open());
        assert_eq!(app.active_chat, 0);
    }
}

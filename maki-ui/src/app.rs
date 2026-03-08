use std::collections::{HashMap, VecDeque};
use std::path::Path;
use std::time::{Duration, Instant};

use crate::chat::{Chat, ChatEventResult};
use crate::components::chat_picker::{ChatPicker, ChatPickerAction};
use crate::components::command::{CommandAction, CommandPalette};
use crate::components::help_modal::HelpModal;
use crate::components::input::{InputAction, InputBox};
use crate::components::keybindings::KeybindContext;
use crate::components::question_form::{QuestionForm, QuestionFormAction};
use crate::components::queue_panel::{self, QueueEntry};
use crate::components::status_bar::{CancelResult, StatusBar, StatusBarContext, UsageStats};
use crate::components::{Action, DisplayMessage, DisplayRole, RetryInfo, Status, is_ctrl};
use crate::selection::{
    self, ContentRegion, EdgeScroll, SelectableZone, Selection, SelectionState, SelectionZone,
    ZoneRegistry,
};
use crate::theme;
use arboard::Clipboard;

use crossterm::event::{KeyCode, KeyEvent, MouseButton, MouseEvent, MouseEventKind};
#[cfg(feature = "demo")]
use maki_agent::QuestionInfo;
use maki_agent::{AgentEvent, Envelope, SubagentInfo};
use maki_agent::{AgentInput, AgentMode};
use maki_providers::{ModelPricing, TokenUsage};
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Position, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::widgets::{Block, Widget};

const CANCEL_MSG: &str = "Cancelled.";
const COMPACT_LABEL: &str = "/compact";

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Mode {
    Build,
    Plan { path: String, written: bool },
    BuildPlan,
}

impl Mode {
    fn color(&self) -> Color {
        match self {
            Self::Build => theme::CYAN,
            Self::Plan { .. } => theme::PINK,
            Self::BuildPlan => theme::PURPLE,
        }
    }
}

const EDGE_SCROLL_LINES: i32 = 1;
const EDGE_SCROLL_INTERVAL: Duration = Duration::from_millis(25);

pub(crate) enum QueuedItem {
    Message(AgentInput),
    Compact,
}

impl QueuedItem {
    fn as_queue_entry(&self) -> QueueEntry<'_> {
        match self {
            Self::Message(input) => QueueEntry {
                text: &input.message,
                color: theme::FOREGROUND,
            },
            Self::Compact => QueueEntry {
                text: COMPACT_LABEL,
                color: theme::PURPLE,
            },
        }
    }
}

pub enum Msg {
    Key(KeyEvent),
    Paste(String),
    Mouse(MouseEvent),
    Scroll { column: u16, row: u16, delta: i32 },
    Agent(Box<Envelope>),
}

pub struct App {
    chats: Vec<Chat>,
    active_chat: usize,
    chat_index: HashMap<String, usize>,
    pub(crate) input_box: InputBox,
    command_palette: CommandPalette,
    chat_picker: ChatPicker,
    help_modal: HelpModal,
    question_form: QuestionForm,
    status_bar: StatusBar,
    pub status: Status,
    pub token_usage: TokenUsage,
    pub(crate) mode: Mode,
    pub(crate) ready_plan: Option<String>,
    model_id: String,
    pricing: ModelPricing,
    context_window: u32,
    pub should_quit: bool,
    pub(crate) queue: VecDeque<QueuedItem>,
    pub answer_tx: Option<flume::Sender<String>>,
    pub(crate) cmd_tx: Option<flume::Sender<super::AgentCommand>>,
    pending_question: bool,
    /// Current agent run ID. Incremented on each new prompt submission.
    /// Events with mismatched run_id are ignored (stale events from cancelled runs).
    pub(crate) run_id: u64,
    retry_info: Option<RetryInfo>,
    #[cfg(feature = "demo")]
    demo_questions: Option<(usize, Vec<QuestionInfo>)>,
    zones: ZoneRegistry,
    selection_state: Option<SelectionState>,
    clipboard: Option<Clipboard>,
    queue_focus: Option<usize>,
}

impl App {
    pub fn new(model_id: String, pricing: ModelPricing, context_window: u32) -> Self {
        Self {
            chats: vec![Chat::new("Main".into())],
            active_chat: 0,
            chat_index: HashMap::new(),
            input_box: InputBox::new(),
            command_palette: CommandPalette::new(),
            chat_picker: ChatPicker::new(),
            help_modal: HelpModal::new(),
            question_form: QuestionForm::new(),
            status_bar: StatusBar::new(),
            status: Status::Idle,
            token_usage: TokenUsage::default(),
            mode: Mode::Build,
            ready_plan: None,
            model_id,
            pricing,
            context_window,
            should_quit: false,
            queue: VecDeque::new(),
            answer_tx: None,
            cmd_tx: None,
            pending_question: false,
            run_id: 0,
            retry_info: None,
            #[cfg(feature = "demo")]
            demo_questions: None,
            zones: [None; 3],
            selection_state: None,
            clipboard: Clipboard::new().ok(),
            queue_focus: None,
        }
    }

    pub(crate) fn main_chat(&mut self) -> &mut Chat {
        &mut self.chats[0]
    }

    fn active_chat(&mut self) -> &mut Chat {
        &mut self.chats[self.active_chat]
    }

    fn visible_queue_len(&self) -> usize {
        self.queue.len()
    }

    fn visible_queue_entries(&self) -> Vec<QueueEntry<'_>> {
        self.queue
            .iter()
            .map(|item| item.as_queue_entry())
            .collect()
    }

    fn clear_queue(&mut self) {
        self.queue.clear();
        self.queue_focus = None;
    }

    fn remove_queue_item(&mut self, index: usize) {
        if index < self.queue.len() {
            self.queue.remove(index);
            self.queue_focus = if self.queue.is_empty() {
                None
            } else {
                Some(self.queue_focus.unwrap_or(0).min(self.queue.len() - 1))
            };
        }
    }

    fn pop_queue_front(&mut self) {
        self.queue.pop_front();
        match self.queue_focus {
            Some(sel) if sel >= self.queue.len() && !self.queue.is_empty() => {
                self.queue_focus = Some(self.queue.len() - 1);
            }
            Some(_) if self.queue.is_empty() => self.queue_focus = None,
            _ => {}
        }
    }

    fn focus_queue(&mut self) {
        if !self.queue.is_empty() {
            self.queue_focus = Some(0);
        }
    }

    fn zone_at(&self, row: u16, col: u16) -> Option<SelectableZone> {
        selection::zone_at(&self.zones, row, col)
    }

    /// Keep in sync with the priority chain in `handle_key`.
    fn active_keybind_contexts(&self) -> Vec<KeybindContext> {
        let mut contexts = vec![KeybindContext::General];
        if self.question_form.is_visible() {
            contexts.push(KeybindContext::QuestionForm);
        } else if self.queue_focus.is_some() {
            contexts.push(KeybindContext::QueueFocus);
        } else if self.chat_picker.is_open() {
            contexts.push(KeybindContext::ChatPicker);
        } else if self.command_palette.is_active() {
            contexts.push(KeybindContext::CommandPalette);
        } else {
            if self.status == Status::Streaming {
                contexts.push(KeybindContext::Streaming);
            }
            contexts.push(KeybindContext::Editing);
        }
        contexts
    }

    fn scroll_offset(&self, zone: SelectionZone) -> u32 {
        match zone {
            SelectionZone::Messages => self.chats[self.active_chat].scroll_top() as u32,
            SelectionZone::Input => self.input_box.scroll_y() as u32,
            SelectionZone::StatusBar => 0,
        }
    }

    fn scroll_zone(&mut self, zone: SelectionZone, delta: i32) {
        match zone {
            SelectionZone::Messages => self.chats[self.active_chat].scroll(delta),
            SelectionZone::Input => self.input_box.scroll(delta),
            SelectionZone::StatusBar => {}
        }
    }

    fn msg_area(&self) -> Rect {
        self.zones[SelectionZone::Messages.idx()]
            .map(|z| z.highlight_area)
            .unwrap_or_default()
    }

    pub fn update(&mut self, msg: Msg) -> Vec<Action> {
        match msg {
            Msg::Key(key) => self.handle_key(key),
            Msg::Paste(text) => {
                if !self.question_form.handle_paste(&text)
                    && let InputAction::PaletteSync(val) = self.input_box.handle_paste(&text)
                {
                    self.command_palette.sync(&val);
                }
                vec![]
            }
            Msg::Mouse(event) => {
                self.handle_mouse(event);
                vec![]
            }
            Msg::Scroll { column, row, delta } => {
                self.selection_state = None;
                let pos = Position::new(column, row);
                if self.chat_picker.is_open() {
                    if self.chat_picker.contains(pos) {
                        let names = self.chat_names();
                        self.chat_picker.scroll(delta, &names);
                    }
                } else if let Some(zone) = self.zone_at(row, column) {
                    self.scroll_zone(zone.zone, delta);
                }
                vec![]
            }
            Msg::Agent(envelope) => self.handle_agent_event(*envelope),
        }
    }

    fn send_answer(&self, answer: String) {
        if let Some(tx) = &self.answer_tx {
            let _ = tx.try_send(answer);
        }
    }

    fn handle_mouse(&mut self, event: MouseEvent) {
        match event.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                if let Some(zone) = self.zone_at(event.row, event.column) {
                    let scroll = self.scroll_offset(zone.zone);
                    self.selection_state = Some(SelectionState {
                        sel: Selection::start(
                            event.row,
                            event.column,
                            zone.area,
                            zone.zone,
                            scroll,
                        ),
                        copy_on_release: false,
                        edge_scroll: None,
                        last_drag_col: event.column,
                    });
                }
            }
            MouseEventKind::Drag(MouseButton::Left) => {
                let Some(ref mut state) = self.selection_state else {
                    return;
                };
                let zone = state.sel.zone;
                let area = state.sel.area;

                let edge_dir = if event.row <= area.y {
                    Some(EDGE_SCROLL_LINES)
                } else if event.row + 1 >= area.bottom() {
                    Some(-EDGE_SCROLL_LINES)
                } else {
                    None
                };

                if let Some(dir) = edge_dir {
                    match state.edge_scroll.as_mut() {
                        None => {
                            self.scroll_zone(zone, dir);
                            let scroll = self.scroll_offset(zone);
                            let state = self.selection_state.as_mut().unwrap();
                            state.edge_scroll = Some(EdgeScroll {
                                dir,
                                last_tick: Instant::now(),
                            });
                            state.last_drag_col = event.column;
                            let edge_row = if dir > 0 {
                                state.sel.area.y
                            } else {
                                state.sel.area.bottom().saturating_sub(1)
                            };
                            state.sel.update(edge_row, event.column, scroll);
                        }
                        Some(es) => {
                            es.dir = dir;
                            let state = self.selection_state.as_mut().unwrap();
                            state.last_drag_col = event.column;
                        }
                    }
                } else {
                    state.edge_scroll = None;
                    let state = self.selection_state.as_mut().unwrap();
                    state.last_drag_col = event.column;
                    let scroll = self.scroll_offset(zone);
                    let state = self.selection_state.as_mut().unwrap();
                    state.sel.update(event.row, event.column, scroll);
                }
            }
            MouseEventKind::Up(MouseButton::Left) => {
                if let Some(ref mut state) = self.selection_state {
                    state.edge_scroll = None;
                    if !state.sel.is_empty() {
                        state.copy_on_release = true;
                    } else {
                        self.selection_state = None;
                    }
                }
            }
            _ => {}
        }
    }

    pub fn tick_edge_scroll(&mut self) {
        let Some(ref mut state) = self.selection_state else {
            return;
        };
        let Some(ref mut es) = state.edge_scroll else {
            return;
        };
        if es.last_tick.elapsed() < EDGE_SCROLL_INTERVAL {
            return;
        }
        let dir = es.dir;
        let zone = state.sel.zone;
        let last_drag_col = state.last_drag_col;
        es.last_tick = Instant::now();

        self.scroll_zone(zone, dir);

        let state = self.selection_state.as_mut().unwrap();
        let edge_row = if dir > 0 {
            state.sel.area.y
        } else {
            state.sel.area.bottom().saturating_sub(1)
        };
        let scroll = self.scroll_offset(zone);
        let state = self.selection_state.as_mut().unwrap();
        state.sel.update(edge_row, last_drag_col, scroll);
    }

    fn handle_key(&mut self, key: KeyEvent) -> Vec<Action> {
        if self.question_form.is_visible() {
            let action = self.question_form.handle_key(key);
            let answer = match action {
                QuestionFormAction::Submit(a) => {
                    let display = self.question_form.format_answers_display();
                    self.main_chat().push_user_message(&display);
                    a
                }
                QuestionFormAction::Dismiss => String::new(),
                QuestionFormAction::Consumed => return vec![],
            };
            self.question_form.close();
            self.send_answer(answer);
            return vec![];
        }
        self.selection_state = None;

        if self.help_modal.is_open() {
            self.help_modal.handle_key(key);
            return vec![];
        }

        if let Some(selected) = self.queue_focus {
            match key.code {
                KeyCode::Up => {
                    if selected > 0 {
                        self.queue_focus = Some(selected - 1);
                    }
                    return vec![];
                }
                KeyCode::Down => {
                    if selected < self.queue.len() - 1 {
                        self.queue_focus = Some(selected + 1);
                    }
                    return vec![];
                }
                KeyCode::Enter => {
                    self.remove_queue_item(selected);
                    return vec![];
                }
                KeyCode::Esc => {
                    self.queue_focus = None;
                    return vec![];
                }
                _ => {}
            }
        }

        if self.chat_picker.is_open() {
            let names = self.chat_names();
            return match self.chat_picker.handle_key(key, &names) {
                ChatPickerAction::Consumed => vec![],
                ChatPickerAction::Select(idx) => {
                    self.active_chat = idx;
                    #[cfg(feature = "demo")]
                    self.check_demo_questions();
                    vec![]
                }
            };
        }

        // Keep in sync with: `KEYBINDS` in keybindings.rs
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
                    #[cfg(feature = "demo")]
                    self.check_demo_questions();
                    vec![]
                }
                KeyCode::Char('n') => {
                    self.active_chat = (self.active_chat + 1).min(self.chats.len() - 1);
                    #[cfg(feature = "demo")]
                    self.check_demo_questions();
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
                KeyCode::Char('g') => {
                    self.active_chat().scroll_to_top();
                    vec![]
                }
                KeyCode::Char('b') => {
                    self.active_chat().enable_auto_scroll();
                    vec![]
                }
                KeyCode::Char('q') => {
                    if !self.queue.is_empty() {
                        self.pop_queue_front();
                    }
                    vec![]
                }
                KeyCode::Char('h') if is_ctrl(&key) => {
                    self.help_modal.toggle();
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
                KeyCode::Esc => {
                    self.status_bar.clear_cancel_hint();
                    vec![]
                }
                _ => vec![],
            },
            InputAction::ContinueLine | InputAction::None => vec![],
        }
    }

    fn handle_submit(&mut self, text: String) -> Vec<Action> {
        if self.pending_question {
            self.pending_question = false;
            self.main_chat().push_user_message(&text);
            self.send_answer(text);
            return vec![];
        }
        let input = AgentInput {
            message: text.clone(),
            mode: self.agent_mode(),
            pending_plan: self.pending_plan().map(String::from),
        };
        if self.status == Status::Streaming {
            self.queue.push_back(QueuedItem::Message(input));
            if self.queue.len() == 1
                && let Some(tx) = &self.cmd_tx
            {
                let cmd = super::AgentCommand::Run(
                    AgentInput {
                        message: text,
                        mode: self.agent_mode(),
                        pending_plan: self.pending_plan().map(String::from),
                    },
                    self.run_id,
                );
                let _ = tx.try_send(cmd);
            }
            vec![]
        } else {
            self.run_id += 1;
            self.main_chat().push_user_message(&text);
            self.status = Status::Streaming;
            self.main_chat().enable_auto_scroll();
            vec![Action::SendMessage(input)]
        }
    }

    fn handle_cancel_press(&mut self) -> Vec<Action> {
        match self.status_bar.handle_cancel_press() {
            CancelResult::Confirmed => {
                self.run_id += 1;
                self.retry_info = None;
                self.question_form.close();
                self.pending_question = false;
                for chat in &mut self.chats {
                    chat.flush();
                    chat.fail_in_progress();
                }
                self.main_chat()
                    .push(DisplayMessage::new(DisplayRole::Error, CANCEL_MSG.into()));
                self.clear_queue();
                self.chat_index.clear();
                self.status = Status::Idle;
                vec![Action::CancelAgent]
            }
            CancelResult::FirstPress => vec![],
        }
    }

    fn handle_agent_event(&mut self, envelope: Envelope) -> Vec<Action> {
        if envelope.run_id != self.run_id {
            return vec![];
        }

        let chat_idx = match envelope.subagent {
            Some(ref subagent) => self.resolve_or_create_chat(subagent),
            None => 0,
        };

        if let AgentEvent::ToolDone(ref e) = envelope.event
            && let Mode::Plan {
                ref path,
                ref mut written,
            } = self.mode
            && e.written_path()
                .is_some_and(|wp| wp == path || Path::new(path).ends_with(wp))
        {
            *written = true;
        }

        if let AgentEvent::Retry {
            attempt,
            message,
            delay_ms,
        } = envelope.event
        {
            if chat_idx == 0 {
                self.retry_info = Some(RetryInfo {
                    attempt,
                    message,
                    deadline: Instant::now() + Duration::from_millis(delay_ms),
                });
            }
            return vec![];
        }

        self.retry_info = None;

        let plan_path = match &self.mode {
            Mode::Plan { path, .. } => Some(path.as_str()),
            _ => None,
        };

        if let AgentEvent::TurnComplete {
            usage,
            context_size,
            ..
        } = envelope.event
        {
            self.token_usage += usage;
            self.chats[chat_idx].token_usage += usage;
            self.chats[chat_idx].context_size =
                context_size.unwrap_or_else(|| usage.context_tokens());
        }

        let result = self.chats[chat_idx].handle_event(envelope.event, plan_path);

        if matches!(result, ChatEventResult::InterruptConsumed) && chat_idx == 0 {
            let pos = self
                .queue
                .iter()
                .position(|item| matches!(item, QueuedItem::Message(_)));
            if let Some(pos) = pos {
                self.queue.remove(pos);
            }
            return vec![];
        }

        if chat_idx == 0 {
            match result {
                ChatEventResult::Done => {
                    self.status_bar.clear_cancel_hint();
                    if let Some(item) = self.queue.pop_front() {
                        return match item {
                            QueuedItem::Message(input) => {
                                self.main_chat().push_user_message(&input.message);
                                self.main_chat().enable_auto_scroll();
                                vec![Action::SendMessage(input)]
                            }
                            QueuedItem::Compact => vec![Action::Compact],
                        };
                    }
                    self.status = Status::Idle;
                }
                ChatEventResult::Error(message) => {
                    self.status = Status::Error(message);
                    self.status_bar.clear_cancel_hint();
                    self.status_bar.mark_error();
                    self.clear_queue();
                    for chat in &mut self.chats {
                        chat.fail_in_progress();
                    }
                }
                ChatEventResult::QuestionPrompt { questions } => {
                    if QuestionForm::is_form_suitable(&questions) {
                        self.question_form.open(questions);
                    } else {
                        let text = QuestionForm::format_questions_as_text(&questions);
                        self.main_chat()
                            .push(DisplayMessage::new(DisplayRole::Assistant, text));
                        self.pending_question = true;
                    }
                }
                ChatEventResult::Continue | ChatEventResult::InterruptConsumed => {}
            }
        }
        vec![]
    }

    fn resolve_or_create_chat(&mut self, subagent: &SubagentInfo) -> usize {
        let id = &subagent.parent_tool_use_id;
        if let Some(&idx) = self.chat_index.get(id.as_str()) {
            return idx;
        }
        let idx = self.chats.len();
        self.chat_index.insert(id.clone(), idx);
        self.chats[0].update_tool_summary(id, &subagent.name);
        if let Some(ref model) = subagent.model {
            self.chats[0].update_tool_model(id, model);
        }
        let mut chat = Chat::new(subagent.name.clone());
        chat.model_id = subagent.model.clone();
        if let Some(ref prompt) = subagent.prompt {
            chat.push_user_message(prompt);
        }
        self.chats.push(chat);
        idx
    }

    fn toggle_mode(&mut self) -> Vec<Action> {
        self.mode = match std::mem::replace(&mut self.mode, Mode::Build) {
            Mode::BuildPlan => Mode::Build,
            Mode::Build => Mode::Plan {
                path: maki_agent::new_plan_path(),
                written: false,
            },
            Mode::Plan { path, written } => {
                if written {
                    self.ready_plan = Some(path);
                }
                if self.ready_plan.is_some() {
                    Mode::BuildPlan
                } else {
                    Mode::Build
                }
            }
        };
        vec![]
    }

    fn agent_mode(&self) -> AgentMode {
        match &self.mode {
            Mode::Plan { path, .. } => AgentMode::Plan(path.clone()),
            Mode::Build | Mode::BuildPlan => AgentMode::Build,
        }
    }

    fn pending_plan(&self) -> Option<&str> {
        match &self.mode {
            Mode::BuildPlan => self.ready_plan.as_deref(),
            _ => None,
        }
    }

    fn mode_label(&self) -> (&'static str, Style) {
        let label = match &self.mode {
            Mode::Build => "[BUILD]",
            Mode::Plan { .. } => "[PLAN]",
            Mode::BuildPlan => "[BUILD PLAN]",
        };
        let style = Style::new()
            .fg(self.mode.color())
            .add_modifier(Modifier::BOLD);
        (label, style)
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
            "/compact" => {
                if self.status == Status::Streaming {
                    self.queue.push_back(QueuedItem::Compact);
                    return vec![];
                }
                self.status = Status::Streaming;
                vec![Action::Compact]
            }
            "/new" => self.reset_session(),
            "/queue" => {
                self.focus_queue();
                vec![]
            }
            _ => vec![],
        }
    }

    fn reset_session(&mut self) -> Vec<Action> {
        self.chats.clear();
        self.chats.push(Chat::new("Main".into()));
        self.active_chat = 0;
        self.chat_index.clear();
        self.status = Status::Idle;
        self.token_usage = TokenUsage::default();
        self.clear_queue();
        #[cfg(feature = "demo")]
        {
            self.demo_questions = None;
        }
        self.question_form.close();
        self.help_modal.close();
        self.pending_question = false;
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

        let form_visible = self.question_form.is_visible();
        let max_form_height = frame.area().height.saturating_sub(3);
        let bottom_height = if form_visible {
            self.question_form
                .height(frame.area().width)
                .min(max_form_height)
        } else {
            queue_panel::height(self.visible_queue_len())
                + self.input_box.height(frame.area().width)
        };
        let [msg_area, bottom_area, status_area] = Layout::vertical([
            Constraint::Min(1),
            Constraint::Length(bottom_height),
            Constraint::Length(1),
        ])
        .areas(frame.area());
        self.zones[SelectionZone::Messages.idx()] = Some(SelectableZone {
            area: msg_area,
            highlight_area: Rect::new(
                msg_area.x,
                msg_area.y,
                msg_area.width.saturating_sub(1),
                msg_area.height,
            ),
            zone: SelectionZone::Messages,
        });
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
        self.chats[render_chat].view(frame, msg_area, self.selection_state.is_some());

        let queue_height = queue_panel::height(self.visible_queue_len());
        let input_height = bottom_area.height.saturating_sub(queue_height);
        let [queue_area, input_area] = Layout::vertical([
            Constraint::Length(queue_height),
            Constraint::Length(input_height),
        ])
        .areas(bottom_area);
        let input_inner = selection::inset_border(input_area);
        self.zones[SelectionZone::Input.idx()] = Some(SelectableZone {
            area: input_inner,
            highlight_area: input_inner,
            zone: SelectionZone::Input,
        });

        if form_visible {
            self.question_form.view(frame, bottom_area);
        } else {
            let queue_entries = self.visible_queue_entries();
            queue_panel::view(frame, queue_area, &queue_entries, self.queue_focus);
            self.input_box.view(
                frame,
                input_area,
                self.status == Status::Streaming,
                self.mode.color(),
            );
            self.command_palette.view(frame, input_area);
        }

        if let Some(names) = names {
            let full_area = frame.area();
            self.chat_picker.view(frame, full_area, &names);
        }

        let chat = &self.chats[render_chat];
        let chat_name = (self.chats.len() > 1).then_some(chat.name.as_str());
        let (mode_label, mode_style) = self.mode_label();
        let ctx = StatusBarContext {
            status: &self.status,
            mode_label,
            mode_style,
            model_id: chat.model_id.as_deref().unwrap_or(&self.model_id),
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
            retry_info: self.retry_info.as_ref(),
        };
        self.status_bar.view(frame, status_area, &ctx);

        self.zones[SelectionZone::StatusBar.idx()] = Some(SelectableZone {
            area: status_area,
            highlight_area: status_area,
            zone: SelectionZone::StatusBar,
        });

        let help_contexts = self.active_keybind_contexts();
        self.help_modal.view(frame, frame.area(), &help_contexts);

        if let Some(ref state) = self.selection_state {
            let zone = state.sel.zone;
            let scroll = self.scroll_offset(zone);
            if let Some(screen_sel) = state.sel.to_screen(scroll) {
                let highlight_area = self.zones[zone.idx()]
                    .map(|z| z.highlight_area)
                    .unwrap_or_default();
                selection::apply_highlight(frame.buffer_mut(), highlight_area, &screen_sel);
            }
            if state.copy_on_release {
                let sel = state.sel;
                self.copy_selection(frame.buffer_mut(), &sel, render_chat);
            }
        }
    }

    /// Called from `view()` when `copy_on_release` is set. Must happen during
    /// rendering because the terminal buffer is only valid then.
    fn copy_selection(
        &mut self,
        buf: &mut ratatui::buffer::Buffer,
        sel: &Selection,
        render_chat: usize,
    ) {
        let text = match sel.zone {
            SelectionZone::Messages => {
                let msg_area = self.msg_area();
                let chat = &self.chats[render_chat];
                let scroll_top = chat.scroll_top();
                let heights = chat.segment_heights();
                let copy_texts = chat.segment_copy_texts();
                selection::extract_doc_range(buf, sel, msg_area, scroll_top, heights, &copy_texts)
            }
            SelectionZone::Input => {
                let scroll = self.scroll_offset(sel.zone);
                let Some(screen_sel) = sel.to_screen(scroll) else {
                    self.selection_state = None;
                    return;
                };
                let copy_text = self.input_box.copy_text();
                let input_area = sel.area;
                let regions = [ContentRegion {
                    area: input_area,
                    raw_text: &copy_text,
                }];
                selection::extract_selected_text(buf, &screen_sel, &regions)
            }
            SelectionZone::StatusBar => {
                let scroll = self.scroll_offset(sel.zone);
                let Some(screen_sel) = sel.to_screen(scroll) else {
                    self.selection_state = None;
                    return;
                };
                let regions = [ContentRegion {
                    area: sel.area,
                    raw_text: "",
                }];
                selection::extract_selected_text(buf, &screen_sel, &regions)
            }
        };

        if !text.is_empty() {
            match &mut self.clipboard {
                Some(cb) => match cb.set_text(&text) {
                    Ok(()) => self.status_bar.flash("Copied selection".into()),
                    Err(e) => self.status_bar.flash(format!("Copy failed: {e}")),
                },
                None => self.status_bar.flash("Copy failed: no clipboard".into()),
            }
        }
        self.selection_state = None;
    }

    pub fn is_animating(&self) -> bool {
        self.selection_state
            .as_ref()
            .is_some_and(|s| s.edge_scroll.is_some())
            || self.chats.iter().any(|c| c.is_animating())
    }

    #[cfg(feature = "demo")]
    pub fn flush_all_chats(&mut self) {
        for chat in &mut self.chats {
            chat.flush();
        }
    }

    #[cfg(feature = "demo")]
    pub fn chat_index_for(&self, tool_id: &str) -> Option<usize> {
        self.chat_index.get(tool_id).copied()
    }

    #[cfg(feature = "demo")]
    pub fn set_demo_questions(&mut self, chat_idx: usize, questions: Vec<QuestionInfo>) {
        self.demo_questions = Some((chat_idx, questions));
    }

    #[cfg(feature = "demo")]
    fn check_demo_questions(&mut self) {
        if let Some((idx, _)) = &self.demo_questions
            && self.active_chat == *idx
        {
            let (_, questions) = self.demo_questions.take().unwrap();
            self.question_form.open(questions);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::components::{TEST_CONTEXT_WINDOW, ctrl, key, test_pricing};
    use crossterm::event::{KeyCode, KeyModifiers, MouseButton, MouseEventKind};
    use maki_agent::{QuestionInfo, QuestionOption, ToolDoneEvent, ToolOutput, ToolStartEvent};
    use test_case::test_case;

    fn set_zone(app: &mut App, zone: SelectionZone, area: Rect) {
        app.zones[zone.idx()] = Some(SelectableZone {
            area,
            highlight_area: area,
            zone,
        });
    }

    fn test_app() -> App {
        App::new("test-model".into(), test_pricing(), TEST_CONTEXT_WINDOW)
    }

    fn mouse_event(kind: MouseEventKind, column: u16, row: u16) -> Msg {
        Msg::Mouse(MouseEvent {
            kind,
            column,
            row,
            modifiers: KeyModifiers::NONE,
        })
    }

    fn agent_msg(event: AgentEvent) -> Msg {
        agent_msg_with_run_id(event, 1)
    }

    fn agent_msg_with_run_id(event: AgentEvent, run_id: u64) -> Msg {
        Msg::Agent(Box::new(Envelope {
            event,
            subagent: None,
            run_id,
        }))
    }

    fn subagent_info(parent_id: &str, name: &str) -> SubagentInfo {
        SubagentInfo {
            parent_tool_use_id: parent_id.into(),
            name: name.into(),
            prompt: None,
            model: None,
        }
    }

    fn subagent_msg(event: AgentEvent, parent_id: &str, name: Option<&str>) -> Msg {
        subagent_msg_with_run_id(event, parent_id, name, 1)
    }

    fn subagent_msg_with_run_id(
        event: AgentEvent,
        parent_id: &str,
        name: Option<&str>,
        run_id: u64,
    ) -> Msg {
        Msg::Agent(Box::new(Envelope {
            event,
            subagent: Some(subagent_info(parent_id, name.unwrap_or("Agent"))),
            run_id,
        }))
    }

    fn subagent_msg_with_prompt(
        event: AgentEvent,
        parent_id: &str,
        name: Option<&str>,
        prompt: Option<&str>,
    ) -> Msg {
        let mut info = subagent_info(parent_id, name.unwrap_or("Agent"));
        info.prompt = prompt.map(String::from);
        Msg::Agent(Box::new(Envelope {
            event,
            subagent: Some(info),
            run_id: 1,
        }))
    }

    fn subagent_msg_with_model(event: AgentEvent, parent_id: &str, name: &str, model: &str) -> Msg {
        let mut info = subagent_info(parent_id, name);
        info.model = Some(model.into());
        Msg::Agent(Box::new(Envelope {
            event,
            subagent: Some(info),
            run_id: 1,
        }))
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
    fn error_event_sets_status() {
        let mut app = test_app();
        app.status = Status::Streaming;
        app.run_id = 1;
        app.update(agent_msg(AgentEvent::Error {
            message: "boom".into(),
        }));
        assert!(matches!(app.status, Status::Error(ref e) if e == "boom"));
    }

    #[test]
    fn toggle_mode_state_machine() {
        let tab = |app: &mut App| app.update(Msg::Key(key(KeyCode::Tab)));
        let is_plan = |app: &App| matches!(&app.mode, Mode::Plan { .. });

        let mut app = test_app();
        assert_eq!(app.mode, Mode::Build);

        // Build -> Plan (generates path under PLANS_DIR)
        tab(&mut app);
        assert!(is_plan(&app));
        assert!(
            matches!(&app.mode, Mode::Plan { path, .. } if path.contains(maki_agent::PLANS_DIR))
        );

        // Plan(unwritten) -> Build (draft discarded, no ready_plan)
        tab(&mut app);
        assert_eq!(app.mode, Mode::Build);
        assert!(app.ready_plan.is_none());

        // Plan(written) -> BuildPlan (draft promoted to ready_plan)
        tab(&mut app);
        if let Mode::Plan {
            ref mut written, ..
        } = app.mode
        {
            *written = true;
        }
        tab(&mut app);
        assert_eq!(app.mode, Mode::BuildPlan);
        assert!(app.ready_plan.is_some());
        let plan = app.ready_plan.clone().unwrap();

        // BuildPlan -> Build -> Plan -> BuildPlan (3-way cycle preserves ready_plan)
        tab(&mut app);
        assert_eq!(app.mode, Mode::Build);
        assert_eq!(app.ready_plan.as_deref(), Some(plan.as_str()));

        tab(&mut app);
        assert!(is_plan(&app));

        tab(&mut app);
        assert_eq!(app.mode, Mode::BuildPlan);
        assert_eq!(app.ready_plan.as_deref(), Some(plan.as_str()));

        // Tab toggles during streaming
        app.mode = Mode::Build;
        app.status = Status::Streaming;
        app.run_id = 1;
        tab(&mut app);
        assert!(is_plan(&app));
    }

    #[test_case(Mode::BuildPlan, Some("plan.md"), Some("plan.md") ; "build_plan_sends_pending")]
    #[test_case(Mode::Build,     Some("plan.md"), None            ; "build_ignores_ready_plan")]
    fn submit_pending_plan(mode: Mode, ready_plan: Option<&str>, expected: Option<&str>) {
        let mut app = test_app();
        app.mode = mode;
        app.ready_plan = ready_plan.map(String::from);
        let actions = type_and_submit(&mut app, "x");
        let Action::SendMessage(ref input) = actions[0] else {
            panic!("expected SendMessage");
        };
        assert_eq!(input.pending_plan.as_deref(), expected);
    }

    #[test_case("plans/test.md", true  ; "matching_path_sets_written")]
    #[test_case("other.rs",      false ; "non_matching_path_stays_unwritten")]
    fn write_event_sets_written_flag(written_path: &str, expect_written: bool) {
        let mut app = test_app();
        app.mode = Mode::Plan {
            path: "plans/test.md".into(),
            written: false,
        };
        app.status = Status::Streaming;
        app.run_id = 1;

        app.update(agent_msg(AgentEvent::ToolDone(ToolDoneEvent {
            id: "t1".into(),
            tool: "write",
            output: ToolOutput::WriteCode {
                path: written_path.into(),
                byte_count: 100,
                lines: vec![],
            },
            is_error: false,
        })));

        assert!(matches!(&app.mode, Mode::Plan { written, .. } if *written == expect_written));
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
    fn paste_routed_to_question_form_in_custom_mode() {
        let mut app = test_app();
        app.question_form.open(vec![QuestionInfo {
            question: "Pick one".into(),
            header: String::new(),
            options: vec![QuestionOption {
                label: "A".into(),
                description: String::new(),
            }],
            multiple: false,
        }]);
        app.update(Msg::Key(key(KeyCode::Down)));
        app.update(Msg::Key(key(KeyCode::Enter)));

        app.update(Msg::Paste("pasted".into()));
        assert_eq!(app.input_box.buffer.value(), "");
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
        assert!(matches!(app.queue[0], QueuedItem::Message(ref i) if i.message == "b"));
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

    #[test_case(error_app as fn(&mut App) ; "error")]
    #[test_case(cancel_app as fn(&mut App) ; "cancel")]
    fn clears_queue(terminate: fn(&mut App)) {
        let mut app = app_with_queued_message();
        terminate(&mut app);
        assert!(app.queue.is_empty());
    }

    fn queued_msg(text: &str) -> QueuedItem {
        QueuedItem::Message(AgentInput {
            message: text.into(),
            mode: AgentMode::Build,
            pending_plan: None,
        })
    }

    fn app_with_queued_message() -> App {
        let mut app = test_app();
        app.status = Status::Streaming;
        app.run_id = 1;
        app.queue.push_back(queued_msg("queued"));
        app
    }

    fn type_and_submit(app: &mut App, text: &str) -> Vec<Action> {
        for c in text.chars() {
            app.update(Msg::Key(key(KeyCode::Char(c))));
        }
        app.update(Msg::Key(key(KeyCode::Enter)))
    }

    fn cancel_app(app: &mut App) {
        app.update(Msg::Key(key(KeyCode::Esc)));
        app.update(Msg::Key(key(KeyCode::Esc)));
    }

    fn error_app(app: &mut App) {
        app.update(agent_msg(AgentEvent::Error {
            message: "boom".into(),
        }));
    }

    #[test]
    fn multiple_interrupts_drained_in_order() {
        let mut app = app_with_queued_message();
        app.queue.push_back(queued_msg("second"));

        app.update(agent_msg(AgentEvent::InterruptConsumed {
            message: "queued".into(),
        }));
        assert_eq!(app.queue.len(), 1);

        app.update(agent_msg(AgentEvent::InterruptConsumed {
            message: "second".into(),
        }));
        assert!(app.queue.is_empty());
    }

    #[test]
    fn submit_during_streaming_queues_and_sends_on_cmd_tx() {
        let mut app = test_app();
        let (tx, rx) = flume::unbounded::<crate::AgentCommand>();
        app.cmd_tx = Some(tx);
        app.status = Status::Streaming;
        app.run_id = 1;

        let actions = type_and_submit(&mut app, "urgent");
        assert!(actions.is_empty());
        assert_eq!(app.queue.len(), 1);
        assert!(rx.try_recv().is_ok());
    }

    #[test]
    fn second_submit_during_streaming_does_not_send_on_cmd_tx() {
        let mut app = test_app();
        let (tx, rx) = flume::unbounded::<crate::AgentCommand>();
        app.cmd_tx = Some(tx);
        app.status = Status::Streaming;
        app.run_id = 1;

        type_and_submit(&mut app, "first");
        assert!(rx.try_recv().is_ok());

        type_and_submit(&mut app, "second");
        assert_eq!(app.queue.len(), 2);
        assert!(
            rx.try_recv().is_err(),
            "second message should not be sent on cmd_tx"
        );
    }

    #[test]
    fn interrupt_displayed_only_on_consumed_event() {
        let mut app = app_with_queued_message();
        let before = app.chats[0].message_count();

        app.update(agent_msg(AgentEvent::InterruptConsumed {
            message: "queued".into(),
        }));
        assert!(app.queue.is_empty());
        assert_eq!(app.chats[0].message_count(), before + 1);
        assert_eq!(app.chats[0].last_message_text(), "queued");
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
    fn reset_session_preserves_plan() {
        let mut app = test_app();
        app.token_usage.input = 500;
        app.chats[0].context_size = 1000;
        app.mode = Mode::BuildPlan;
        app.ready_plan = Some("plan.md".into());
        app.queue.push_back(queued_msg("q"));
        app.queue_focus = Some(0);
        let actions = app.reset_session();
        assert!(matches!(&actions[0], Action::NewSession));
        assert_eq!(app.status, Status::Idle);
        assert_eq!(app.token_usage.input, 0);
        assert_eq!(app.chats[0].context_size, 0);
        assert_eq!(app.mode, Mode::BuildPlan);
        assert_eq!(app.ready_plan.as_deref(), Some("plan.md"));
        assert!(app.queue.is_empty());
        assert_eq!(app.chats.len(), 1);
        assert_eq!(app.chats[0].name, "Main");
        assert_eq!(app.active_chat, 0);
        assert!(app.chat_index.is_empty());
        assert!(app.queue_focus.is_none());
    }

    #[test]
    fn tab_in_palette_closes_and_toggles_mode() {
        let mut app = test_app();
        type_slash(&mut app);
        assert!(app.command_palette.is_active());

        app.update(Msg::Key(key(KeyCode::Tab)));
        assert!(!app.command_palette.is_active());
        assert!(matches!(&app.mode, Mode::Plan { .. }));
    }

    #[test]
    fn ctrl_p_n_navigation() {
        let mut app = test_app();
        app.status = Status::Streaming;
        app.run_id = 1;
        app.update(subagent_msg(
            AgentEvent::TextDelta { text: "sub".into() },
            "task1",
            Some("research"),
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
    fn subagents_get_descriptive_names() {
        let mut app = test_app();
        app.status = Status::Streaming;
        app.run_id = 1;
        app.update(subagent_msg(
            AgentEvent::TextDelta { text: "a".into() },
            "task1",
            Some("first"),
        ));
        app.update(subagent_msg(
            AgentEvent::TextDelta { text: "b".into() },
            "task2",
            Some("second"),
        ));
        assert_eq!(app.chats.len(), 3);
        assert_eq!(app.chats[1].name, "first");
        assert_eq!(app.chats[2].name, "second");
    }

    #[test]
    fn subagent_prompt_shown_as_first_message() {
        let mut app = test_app();
        app.status = Status::Streaming;
        app.run_id = 1;
        app.update(subagent_msg_with_prompt(
            AgentEvent::TextDelta { text: "ok".into() },
            "task1",
            Some("research"),
            Some("Find all TODO comments"),
        ));
        assert_eq!(app.chats[1].message_count(), 1);
        assert_eq!(app.chats[1].last_message_text(), "Find all TODO comments");
    }

    #[test]
    fn subagent_prompt_not_duplicated_on_subsequent_events() {
        let mut app = test_app();
        app.status = Status::Streaming;
        app.run_id = 1;
        app.update(subagent_msg_with_prompt(
            AgentEvent::TextDelta { text: "a".into() },
            "task1",
            Some("research"),
            Some("Find all TODO comments"),
        ));
        app.update(subagent_msg(
            AgentEvent::TextDelta { text: "b".into() },
            "task1",
            Some("research"),
        ));
        app.chats[1].flush();
        assert_eq!(app.chats[1].message_count(), 2);
        assert_eq!(app.chats[1].last_message_text(), "ab");
    }

    #[test]
    fn turn_complete_tracks_usage_and_context_per_chat() {
        let mut app = app_with_subagent();

        let main_usage = TokenUsage {
            input: 100,
            output: 50,
            ..Default::default()
        };
        app.update(agent_msg(AgentEvent::TurnComplete {
            message: Default::default(),
            usage: main_usage,
            model: "test".into(),
            context_size: None,
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
                context_size: None,
            },
            "task1",
            None,
        ));

        assert_eq!(app.token_usage.input, 300);
        assert_eq!(app.token_usage.output, 125);
        assert_eq!(app.chats[0].token_usage.input, 100);
        assert_eq!(app.chats[1].token_usage.input, 200);
        assert_eq!(app.chats[0].context_size, main_usage.context_tokens());
        assert_eq!(app.chats[1].context_size, sub_usage.context_tokens());
    }

    #[test]
    fn cancel_resets_all_chats_and_indices() {
        let mut app = app_with_subagent();
        app.update(subagent_msg(
            AgentEvent::ToolStart(ToolStartEvent {
                id: "sub_t1".into(),
                tool: "bash",
                summary: "running".into(),
                annotation: None,
                input: None,
                output: None,
            }),
            "task1",
            None,
        ));

        app.update(Msg::Key(key(KeyCode::Esc)));
        let actions = app.update(Msg::Key(key(KeyCode::Esc)));
        assert!(matches!(&actions[0], Action::CancelAgent));
        assert_eq!(app.chats[0].in_progress_count(), 0);
        assert_eq!(app.chats[1].in_progress_count(), 0);
        assert!(app.chat_index.is_empty());
    }

    fn open_chats_picker(app: &mut App) {
        for c in "/chats".chars() {
            app.update(Msg::Key(key(KeyCode::Char(c))));
        }
        app.update(Msg::Key(key(KeyCode::Enter)));
    }

    #[test]
    fn chats_command_opens_picker() {
        let mut app = test_app();
        open_chats_picker(&mut app);
        assert!(app.chat_picker.is_open());
    }

    fn app_with_subagent() -> App {
        let mut app = test_app();
        app.status = Status::Streaming;
        app.run_id = 1;
        app.update(subagent_msg(
            AgentEvent::TextDelta { text: "x".into() },
            "task1",
            Some("research"),
        ));
        app
    }

    #[test]
    fn done_flushes_text_and_sets_idle() {
        let mut app = test_app();
        app.status = Status::Streaming;
        app.run_id = 1;
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

    #[test]
    fn compact_command_sets_streaming() {
        let mut app = test_app();
        let actions = app.execute_command("/compact");
        assert!(matches!(&actions[0], Action::Compact));
        assert_eq!(app.status, Status::Streaming);
    }

    #[test]
    fn compact_during_streaming_queued() {
        let mut app = test_app();
        app.status = Status::Streaming;
        app.run_id = 1;
        let actions = app.execute_command("/compact");
        assert!(actions.is_empty());
        assert_eq!(app.queue.len(), 1);
        assert!(matches!(app.queue[0], QueuedItem::Compact));
    }

    fn long_question_no_options() -> AgentEvent {
        let long = (0..20)
            .map(|i| format!("line {i}"))
            .collect::<Vec<_>>()
            .join("\n");
        AgentEvent::QuestionPrompt {
            id: "q1".into(),
            questions: vec![QuestionInfo {
                question: long,
                header: String::new(),
                options: vec![],
                multiple: false,
            }],
        }
    }

    fn short_question_with_options() -> AgentEvent {
        AgentEvent::QuestionPrompt {
            id: "q2".into(),
            questions: vec![QuestionInfo {
                question: "Pick a DB".into(),
                header: "DB".into(),
                options: vec![QuestionOption {
                    label: "PostgreSQL".into(),
                    description: "Relational".into(),
                }],
                multiple: false,
            }],
        }
    }

    #[test]
    fn question_routing_by_suitability() {
        let cases = [
            (long_question_no_options(), false, true),
            (short_question_with_options(), true, false),
        ];
        for (event, expect_form, expect_pending) in cases {
            let mut app = test_app();
            app.status = Status::Streaming;
            app.run_id = 1;
            app.update(agent_msg(event));
            assert_eq!(app.question_form.is_visible(), expect_form);
            assert_eq!(app.pending_question, expect_pending);
        }
    }

    #[test]
    fn pending_question_submit_routes_through_answer_tx() {
        let mut app = test_app();
        app.status = Status::Streaming;
        app.run_id = 1;
        let (tx, rx) = flume::unbounded();
        app.answer_tx = Some(tx);

        app.update(agent_msg(long_question_no_options()));
        assert!(app.pending_question);

        let actions = type_and_submit(&mut app, "my answer");
        assert!(actions.is_empty());
        assert!(!app.pending_question);
        assert_eq!(rx.try_recv().unwrap(), "my answer");
    }

    #[test]
    fn cancel_clears_pending_question() {
        let mut app = test_app();
        app.status = Status::Streaming;
        app.run_id = 1;
        app.update(agent_msg(long_question_no_options()));
        assert!(app.pending_question);

        app.update(Msg::Key(key(KeyCode::Esc)));
        app.update(Msg::Key(key(KeyCode::Esc)));
        assert!(!app.pending_question);
    }

    #[test_case(3  ; "scroll_up")]
    #[test_case(-3 ; "scroll_down")]
    fn scroll_disables_auto_scroll(delta: i32) {
        let mut app = test_app();
        set_zone(&mut app, SelectionZone::Messages, Rect::new(0, 0, 80, 20));
        app.active_chat().enable_auto_scroll();

        let actions = app.update(Msg::Scroll {
            column: 10,
            row: 10,
            delta,
        });
        assert!(actions.is_empty());
        assert!(!app.chats[0].auto_scroll());
    }

    #[test]
    fn scroll_outside_msg_area_ignored() {
        let mut app = test_app();
        set_zone(&mut app, SelectionZone::Messages, Rect::new(0, 0, 80, 20));
        app.active_chat().enable_auto_scroll();

        app.update(Msg::Scroll {
            column: 10,
            row: 25,
            delta: 3,
        });
        assert!(app.chats[0].auto_scroll());
    }

    #[test_case('g', false ; "ctrl_g_disables_auto_scroll")]
    #[test_case('b', true  ; "ctrl_b_enables_auto_scroll")]
    fn ctrl_g_scroll_shortcuts(ch: char, expected_auto_scroll: bool) {
        let mut app = test_app();
        app.active_chat().enable_auto_scroll();
        app.update(Msg::Key(ctrl(ch)));
        assert_eq!(app.chats[0].auto_scroll(), expected_auto_scroll);
    }

    #[test]
    fn tick_edge_scroll_noop_without_state() {
        let mut app = test_app();
        app.tick_edge_scroll();
        assert!(app.chats[0].auto_scroll());
    }

    #[test]
    fn mouse_drag_updates_selection() {
        let mut app = test_app();
        set_zone(&mut app, SelectionZone::Messages, Rect::new(0, 0, 80, 20));
        app.active_chat().scroll_to_top();

        app.update(mouse_event(MouseEventKind::Down(MouseButton::Left), 5, 5));
        app.update(mouse_event(MouseEventKind::Drag(MouseButton::Left), 20, 10));

        let state = app.selection_state.as_ref().unwrap();
        let (_, end) = state.sel.normalized();
        assert_eq!(end.row, 10);
        assert_eq!(end.col, 20);
    }

    #[test]
    fn mouse_drag_clamps_to_area() {
        let mut app = test_app();
        set_zone(&mut app, SelectionZone::Messages, Rect::new(0, 0, 80, 20));
        app.active_chat().scroll_to_top();

        app.update(mouse_event(MouseEventKind::Down(MouseButton::Left), 5, 5));
        app.update(mouse_event(
            MouseEventKind::Drag(MouseButton::Left),
            100,
            50,
        ));

        let state = app.selection_state.as_ref().unwrap();
        let (_, end) = state.sel.normalized();
        assert_eq!(end.col, 79);
        assert_eq!(end.row, 20, "clamped to area bottom + edge scroll offset");
        assert!(
            state.edge_scroll.is_some(),
            "outside area triggers edge scroll"
        );
    }

    #[test_case(Rect::new(0, 2, 80, 20), (10, 12), (10, 1),  Some(EDGE_SCROLL_LINES)  ; "top_edge")]
    #[test_case(Rect::new(0, 2, 80, 20), (10, 10), (10, 22), Some(-EDGE_SCROLL_LINES) ; "bottom_edge")]
    #[test_case(Rect::new(0, 2, 80, 20), (10, 10), (20, 15), None                     ; "middle_no_scroll")]
    #[test_case(Rect::new(0, 1, 80, 20), (10, 10), (10, 0),  Some(EDGE_SCROLL_LINES)  ; "above_area")]
    #[test_case(Rect::new(0, 0, 80, 20), (10, 10), (10, 0),  Some(EDGE_SCROLL_LINES)  ; "first_row")]
    #[test_case(Rect::new(0, 0, 80, 20), (10, 10), (10, 20), Some(-EDGE_SCROLL_LINES) ; "below_area")]
    #[test_case(Rect::new(0, 0, 80, 20), (10, 10), (10, 19), Some(-EDGE_SCROLL_LINES) ; "last_row")]
    #[test_case(Rect::new(0, 0, 80, 20), (10, 10), (10, 1),  None                     ; "interior")]
    fn edge_scroll_direction(
        zone: Rect,
        down: (u16, u16),
        drag: (u16, u16),
        expected: Option<i32>,
    ) {
        let mut app = test_app();
        set_zone(&mut app, SelectionZone::Messages, zone);
        app.active_chat().scroll_to_top();

        app.update(mouse_event(
            MouseEventKind::Down(MouseButton::Left),
            down.0,
            down.1,
        ));
        app.update(mouse_event(
            MouseEventKind::Drag(MouseButton::Left),
            drag.0,
            drag.1,
        ));

        let state = app.selection_state.as_ref().unwrap();
        assert_eq!(state.edge_scroll.as_ref().map(|es| es.dir), expected);
    }

    #[test]
    fn mouse_up_clears_edge_scroll() {
        let mut app = test_app();
        set_zone(&mut app, SelectionZone::Messages, Rect::new(0, 2, 80, 20));
        app.active_chat().scroll_to_top();

        app.update(mouse_event(MouseEventKind::Down(MouseButton::Left), 10, 10));
        app.update(mouse_event(MouseEventKind::Drag(MouseButton::Left), 10, 1));
        assert!(app.selection_state.as_ref().unwrap().edge_scroll.is_some());

        app.update(mouse_event(MouseEventKind::Up(MouseButton::Left), 10, 1));
        let state = app.selection_state.as_ref().unwrap();
        assert!(state.edge_scroll.is_none());
    }

    #[test]
    fn tick_edge_scroll_scrolls_continuously() {
        let mut app = test_app();
        set_zone(&mut app, SelectionZone::Messages, Rect::new(0, 2, 80, 20));
        app.active_chat().scroll_to_top();

        app.update(mouse_event(MouseEventKind::Down(MouseButton::Left), 10, 10));
        app.update(mouse_event(MouseEventKind::Drag(MouseButton::Left), 10, 1));

        let state = app.selection_state.as_mut().unwrap();
        state.edge_scroll.as_mut().unwrap().last_tick = Instant::now() - EDGE_SCROLL_INTERVAL;
        app.tick_edge_scroll();
        assert!(!app.chats[0].auto_scroll());
    }

    #[test]
    fn double_esc_cancels_flushes_and_fails_tools() {
        let mut app = test_app();
        app.status = Status::Streaming;
        app.run_id = 1;
        app.update(agent_msg(AgentEvent::TextDelta {
            text: "partial".into(),
        }));
        app.update(agent_msg(AgentEvent::ToolStart(ToolStartEvent {
            id: "t1".into(),
            tool: "bash",
            summary: "running".into(),
            annotation: None,
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
    fn edge_scroll_makes_app_animating() {
        let mut app = test_app();
        assert!(!app.is_animating());
        set_zone(&mut app, SelectionZone::Messages, Rect::new(0, 0, 80, 20));
        app.update(mouse_event(MouseEventKind::Down(MouseButton::Left), 5, 5));
        let state = app.selection_state.as_mut().unwrap();
        state.edge_scroll = Some(EdgeScroll {
            dir: 1,
            last_tick: Instant::now(),
        });
        assert!(app.is_animating());
    }

    #[test]
    fn mouse_up_behavior() {
        let mut app = test_app();
        set_zone(&mut app, SelectionZone::Messages, Rect::new(0, 0, 80, 20));

        app.update(mouse_event(MouseEventKind::Down(MouseButton::Left), 5, 5));
        app.update(mouse_event(MouseEventKind::Drag(MouseButton::Left), 10, 10));
        app.update(mouse_event(MouseEventKind::Up(MouseButton::Left), 10, 10));
        assert!(
            app.selection_state.as_ref().unwrap().copy_on_release,
            "non-empty selection sets copy flag"
        );

        app.selection_state.as_mut().unwrap().copy_on_release = false;
        app.selection_state = None;
        app.update(mouse_event(MouseEventKind::Down(MouseButton::Left), 5, 5));
        app.update(mouse_event(MouseEventKind::Up(MouseButton::Left), 5, 5));
        assert!(app.selection_state.is_none(), "empty selection is cleared");
    }

    #[test]
    fn key_and_scroll_clear_selection() {
        let mut app = test_app();
        set_zone(&mut app, SelectionZone::Messages, Rect::new(0, 0, 80, 20));

        app.update(mouse_event(MouseEventKind::Down(MouseButton::Left), 5, 5));
        app.update(Msg::Key(key(KeyCode::Char('a'))));
        assert!(app.selection_state.is_none(), "key press clears selection");

        app.update(mouse_event(MouseEventKind::Down(MouseButton::Left), 5, 5));
        app.update(Msg::Scroll {
            column: 10,
            row: 10,
            delta: 3,
        });
        assert!(app.selection_state.is_none(), "scroll clears selection");
    }

    #[test]
    fn form_submit_pushes_answer_to_chat() {
        let mut app = test_app();
        app.status = Status::Streaming;
        app.run_id = 1;
        let (tx, rx) = flume::unbounded();
        app.answer_tx = Some(tx);

        app.update(agent_msg(short_question_with_options()));
        assert!(app.question_form.is_visible());

        app.update(Msg::Key(key(KeyCode::Enter)));
        assert!(!app.question_form.is_visible());
        assert_eq!(app.chats[0].last_message_text(), "Pick a DB: PostgreSQL");
        assert!(rx.try_recv().is_ok());
    }

    #[test]
    fn form_dismiss_does_not_push_to_chat() {
        let mut app = test_app();
        app.status = Status::Streaming;
        app.run_id = 1;
        let (tx, rx) = flume::unbounded();
        app.answer_tx = Some(tx);

        app.update(agent_msg(short_question_with_options()));
        app.update(Msg::Key(key(KeyCode::Esc)));
        assert!(!app.question_form.is_visible());
        assert_eq!(app.chats[0].last_message_text(), "");
        assert_eq!(rx.try_recv().unwrap(), "");
    }

    #[test_case(true  ; "non_empty")]
    #[test_case(false ; "empty")]
    fn queue_command_sets_focus(has_queue: bool) {
        let mut app = if has_queue {
            app_with_queued_message()
        } else {
            test_app()
        };
        app.execute_command("/queue");
        assert_eq!(app.queue_focus.is_some(), has_queue);
    }

    #[test]
    fn queue_navigation_clamps() {
        let mut app = app_with_queued_message();
        app.queue.push_back(queued_msg("second"));
        app.queue_focus = Some(0);

        app.update(Msg::Key(key(KeyCode::Up)));
        assert_eq!(app.queue_focus, Some(0));

        app.queue_focus = Some(1);
        app.update(Msg::Key(key(KeyCode::Down)));
        assert_eq!(app.queue_focus, Some(1));
    }

    #[test]
    fn queue_enter_removes_selected() {
        let mut app = app_with_queued_message();
        app.queue.push_back(queued_msg("second"));
        app.queue_focus = Some(0);

        app.update(Msg::Key(key(KeyCode::Enter)));
        assert_eq!(app.queue.len(), 1);
        match &app.queue[0] {
            QueuedItem::Message(input) => assert_eq!(input.message, "second"),
            _ => panic!("expected Message variant"),
        }
        assert_eq!(app.queue_focus, Some(0));
    }

    #[test]
    fn queue_enter_deletes_last_unfocuses() {
        let mut app = app_with_queued_message();
        app.queue_focus = Some(0);

        app.update(Msg::Key(key(KeyCode::Enter)));
        assert!(app.queue.is_empty());
        assert!(app.queue_focus.is_none());
    }

    #[test]
    fn queue_esc_unfocuses_without_removing() {
        let mut app = app_with_queued_message();
        app.queue_focus = Some(0);

        app.update(Msg::Key(key(KeyCode::Esc)));
        assert!(app.queue_focus.is_none());
        assert_eq!(app.queue.len(), 1);
    }

    #[test_case(None    ; "unfocused")]
    #[test_case(Some(1) ; "focused_on_second")]
    fn ctrl_q_pops_front(initial_focus: Option<usize>) {
        let mut app = app_with_queued_message();
        app.queue.push_back(queued_msg("second"));
        app.queue_focus = initial_focus;

        app.update(Msg::Key(ctrl('q')));
        assert_eq!(app.queue.len(), 1);
        match &app.queue[0] {
            QueuedItem::Message(input) => assert_eq!(input.message, "second"),
            _ => panic!("expected Message variant"),
        }
        assert_eq!(app.queue_focus, initial_focus.map(|_| 0));
    }

    #[test_case(cancel_app as fn(&mut App) ; "cancel")]
    #[test_case(error_app as fn(&mut App)  ; "error")]
    fn clears_queue_focus_on_terminate(terminate: fn(&mut App)) {
        let mut app = app_with_queued_message();
        app.queue_focus = Some(0);
        terminate(&mut app);
        assert!(app.queue_focus.is_none());
    }

    #[test]
    fn compact_fifo_with_messages() {
        let mut app = test_app();
        app.status = Status::Streaming;
        app.run_id = 1;
        app.queue.push_back(queued_msg("first"));
        app.queue.push_back(QueuedItem::Compact);

        let actions = app.update(agent_msg(AgentEvent::Done {
            usage: TokenUsage::default(),
            num_turns: 1,
            stop_reason: None,
        }));
        assert!(matches!(&actions[0], Action::SendMessage(i) if i.message == "first"));

        let actions = app.update(agent_msg(AgentEvent::Done {
            usage: TokenUsage::default(),
            num_turns: 1,
            stop_reason: None,
        }));
        assert!(matches!(&actions[0], Action::Compact));
        assert!(app.queue.is_empty());
    }

    #[test]
    fn stale_events_ignored_after_run_id_increment() {
        let mut app = test_app();
        app.status = Status::Streaming;
        app.run_id = 1;

        cancel_app(&mut app);
        assert_eq!(app.run_id, 2);

        let actions = type_and_submit(&mut app, "new prompt");
        assert!(matches!(&actions[0], Action::SendMessage(i) if i.message == "new prompt"));
        assert_eq!(app.run_id, 3);

        // Stale event with run_id 1 should be ignored
        app.update(agent_msg_with_run_id(
            AgentEvent::TextDelta {
                text: "stale text".into(),
            },
            1,
        ));
        assert_eq!(app.chats[0].last_message_text(), "new prompt");

        // New event with run_id 3 should be processed
        app.update(agent_msg_with_run_id(
            AgentEvent::TextDelta {
                text: "new text".into(),
            },
            3,
        ));
        app.chats[0].flush();
        assert_eq!(app.chats[0].last_message_text(), "new text");
    }

    #[test]
    fn stale_done_does_not_drain_queue() {
        let mut app = test_app();
        app.status = Status::Streaming;
        app.run_id = 1;

        cancel_app(&mut app);
        app.queue.push_back(queued_msg("next"));

        // Stale done with old run_id (1) should be ignored, not drain queue
        app.update(agent_msg_with_run_id(
            AgentEvent::Done {
                usage: TokenUsage::default(),
                num_turns: 1,
                stop_reason: None,
            },
            1,
        ));
        assert_eq!(app.queue.len(), 1);
        assert_eq!(app.status, Status::Idle);
    }

    #[test]
    fn zone_at_returns_correct_zone() {
        let mut app = test_app();
        let msg = Rect::new(0, 0, 80, 15);
        let input = Rect::new(0, 15, 80, 5);
        let status = Rect::new(0, 20, 80, 1);
        set_zone(&mut app, SelectionZone::Messages, msg);
        set_zone(&mut app, SelectionZone::Input, input);
        set_zone(&mut app, SelectionZone::StatusBar, status);

        assert_eq!(app.zone_at(5, 10).unwrap().zone, SelectionZone::Messages);
        assert_eq!(app.zone_at(16, 10).unwrap().zone, SelectionZone::Input);
        assert_eq!(app.zone_at(20, 10).unwrap().zone, SelectionZone::StatusBar);
        assert!(app.zone_at(22, 10).is_none());
    }

    #[test]
    fn mouse_down_in_input_creates_input_zone_selection() {
        let mut app = test_app();
        let input = Rect::new(0, 15, 80, 5);
        set_zone(&mut app, SelectionZone::Messages, Rect::new(0, 0, 80, 15));
        set_zone(&mut app, SelectionZone::Input, input);

        app.update(mouse_event(MouseEventKind::Down(MouseButton::Left), 10, 16));
        let state = app.selection_state.as_ref().unwrap();
        assert_eq!(state.sel.zone, SelectionZone::Input);
        assert_eq!(state.sel.area, input);
    }

    #[test]
    fn resolve_or_create_chat_sets_model_id_and_annotation() {
        let mut app = test_app();
        app.status = Status::Streaming;
        app.run_id = 1;
        app.update(agent_msg(AgentEvent::ToolStart(ToolStartEvent {
            id: "task1".into(),
            tool: "task",
            summary: "research".into(),
            annotation: None,
            input: None,
            output: None,
        })));

        app.update(subagent_msg_with_model(
            AgentEvent::TextDelta { text: "hi".into() },
            "task1",
            "research",
            "anthropic/claude-sonnet-4-20250514",
        ));

        assert_eq!(app.chats.len(), 2);
        assert_eq!(
            app.chats[1].model_id.as_deref(),
            Some("anthropic/claude-sonnet-4-20250514")
        );
    }

    #[test]
    fn ctrl_h_toggles_help_modal() {
        let mut app = test_app();
        assert!(!app.help_modal.is_open());

        app.update(Msg::Key(ctrl('h')));
        assert!(app.help_modal.is_open());

        app.update(Msg::Key(ctrl('h')));
        assert!(!app.help_modal.is_open());
    }

    #[test]
    fn help_modal_esc_closes() {
        let mut app = test_app();
        app.update(Msg::Key(ctrl('h')));
        assert!(app.help_modal.is_open());

        app.update(Msg::Key(key(KeyCode::Esc)));
        assert!(!app.help_modal.is_open());
    }

    #[test]
    fn help_modal_consumes_keys() {
        let mut app = test_app();
        app.update(Msg::Key(ctrl('h')));

        app.update(Msg::Key(key(KeyCode::Char('h'))));
        app.update(Msg::Key(key(KeyCode::Char('i'))));
        assert_eq!(app.input_box.buffer.value(), "");
    }

    #[test]
    fn help_modal_closed_on_reset() {
        let mut app = test_app();
        app.update(Msg::Key(ctrl('h')));
        assert!(app.help_modal.is_open());

        app.reset_session();
        assert!(!app.help_modal.is_open());
    }

    #[test]
    fn active_contexts_idle() {
        let app = test_app();
        let contexts = app.active_keybind_contexts();
        assert!(contexts.contains(&KeybindContext::General));
        assert!(contexts.contains(&KeybindContext::Editing));
        assert!(!contexts.contains(&KeybindContext::Streaming));
    }

    #[test]
    fn active_contexts_streaming() {
        let mut app = test_app();
        app.status = Status::Streaming;
        let contexts = app.active_keybind_contexts();
        assert!(contexts.contains(&KeybindContext::General));
        assert!(contexts.contains(&KeybindContext::Streaming));
        assert!(contexts.contains(&KeybindContext::Editing));
    }

    #[test]
    fn active_contexts_queue_focus() {
        let mut app = app_with_queued_message();
        app.queue_focus = Some(0);
        let contexts = app.active_keybind_contexts();
        assert!(contexts.contains(&KeybindContext::QueueFocus));
        assert!(!contexts.contains(&KeybindContext::Editing));
    }

    #[test]
    fn active_contexts_chat_picker() {
        let mut app = test_app();
        open_chats_picker(&mut app);
        let contexts = app.active_keybind_contexts();
        assert!(contexts.contains(&KeybindContext::ChatPicker));
        assert!(!contexts.contains(&KeybindContext::Editing));
    }
}

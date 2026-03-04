use std::collections::{HashMap, VecDeque};
use std::path::Path;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use crate::chat::{Chat, ChatEventResult};
use crate::components::chat_picker::{ChatPicker, ChatPickerAction};
use crate::components::command::{CommandAction, CommandPalette};
use crate::components::input::{InputAction, InputBox};
use crate::components::question_form::{QuestionForm, QuestionFormAction};
use crate::components::queue_panel;
use crate::components::status_bar::{CancelResult, StatusBar, StatusBarContext, UsageStats};
use crate::components::{Action, DisplayMessage, DisplayRole, RetryInfo, Status, is_ctrl};
use crate::selection::{self, ContentRegion, Selection, inset_border};
use crate::theme;
use arboard::Clipboard;

use crossterm::event::{KeyCode, KeyEvent, MouseButton, MouseEvent, MouseEventKind};
use maki_agent::{AgentInput, AgentMode};
#[cfg(feature = "demo")]
use maki_providers::QuestionInfo;
use maki_providers::{AgentEvent, Envelope, ModelPricing, TokenUsage};
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Position, Rect};
use ratatui::style::Style;
use ratatui::widgets::{Block, Widget};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Mode {
    Build,
    Plan { path: String, written: bool },
    BuildPlan,
}

const CANCEL_MSG: &str = "Cancelled.";

struct ViewAreas {
    render_chat: usize,
    form_visible: bool,
    bottom_area: Rect,
    queue_area: Rect,
    input_area: Rect,
    status_area: Rect,
    cmd_popup_area: Option<Rect>,
    picker_inner_area: Option<Rect>,
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
    pub(crate) queue: VecDeque<AgentInput>,
    pending_interrupts: Vec<String>,
    pub answer_tx: Option<mpsc::Sender<String>>,
    pub interrupt_tx: Option<mpsc::Sender<String>>,
    pending_question: bool,
    retry_info: Option<RetryInfo>,
    #[cfg(feature = "demo")]
    demo_questions: Option<(usize, Vec<QuestionInfo>)>,
    msg_area: Rect,
    input_area: Rect,
    frame_area: Rect,
    selection: Option<Selection>,
    copy_on_next_render: bool,
    clipboard: Option<Clipboard>,
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
            pending_interrupts: Vec::new(),
            answer_tx: None,
            interrupt_tx: None,
            pending_question: false,
            retry_info: None,
            #[cfg(feature = "demo")]
            demo_questions: None,
            msg_area: Rect::default(),
            input_area: Rect::default(),
            frame_area: Rect::default(),
            selection: None,
            copy_on_next_render: false,
            clipboard: Clipboard::new().ok(),
        }
    }

    fn main_chat(&mut self) -> &mut Chat {
        &mut self.chats[0]
    }

    fn active_chat(&mut self) -> &mut Chat {
        &mut self.chats[self.active_chat]
    }

    fn visible_queue_len(&self) -> usize {
        self.pending_interrupts.len() + self.queue.len()
    }

    fn visible_queue_texts(&self) -> Vec<&str> {
        self.pending_interrupts
            .iter()
            .map(|s| s.as_str())
            .chain(self.queue.iter().map(|i| i.message.as_str()))
            .collect()
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
            Msg::Mouse(event) => {
                self.handle_mouse(event);
                vec![]
            }
            Msg::Scroll { column, row, delta } => {
                self.selection = None;
                let pos = Position::new(column, row);
                if self.msg_area.contains(pos) {
                    self.active_chat().scroll(delta);
                } else if self.input_area.contains(pos) {
                    self.input_box.scroll(delta);
                }
                vec![]
            }
            Msg::Agent(envelope) => self.handle_agent_event(*envelope),
        }
    }

    fn send_answer(&self, answer: String) {
        if let Some(tx) = &self.answer_tx {
            let _ = tx.send(answer);
        }
    }

    fn handle_mouse(&mut self, event: MouseEvent) {
        match event.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                self.selection = Some(Selection::start(event.row, event.column));
                self.copy_on_next_render = false;
            }
            MouseEventKind::Drag(MouseButton::Left) => {
                if let Some(ref mut sel) = self.selection {
                    let row = event.row.clamp(
                        self.frame_area.y,
                        self.frame_area.bottom().saturating_sub(1),
                    );
                    let col = event
                        .column
                        .clamp(self.frame_area.x, self.frame_area.right().saturating_sub(1));
                    sel.update(row, col);
                }
            }
            MouseEventKind::Up(MouseButton::Left) => {
                if self.selection.as_ref().is_some_and(|s| !s.is_empty()) {
                    self.copy_on_next_render = true;
                } else {
                    self.selection = None;
                }
            }
            _ => {}
        }
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
        self.selection = None;

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
            if let Some(tx) = &self.interrupt_tx
                && tx.send(text.clone()).is_ok()
            {
                self.pending_interrupts.push(text);
                return vec![];
            }
            self.queue.push_back(input);
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
                self.retry_info = None;
                self.question_form.close();
                self.pending_question = false;
                for chat in &mut self.chats {
                    chat.flush();
                    chat.fail_in_progress();
                }
                self.main_chat()
                    .push(DisplayMessage::new(DisplayRole::Error, CANCEL_MSG.into()));
                self.pending_interrupts.clear();
                self.queue.clear();
                self.chat_index.clear();
                self.status = Status::Idle;
                vec![Action::CancelAgent]
            }
            CancelResult::FirstPress => vec![],
        }
    }

    fn handle_agent_event(&mut self, envelope: Envelope) -> Vec<Action> {
        let chat_idx = self.resolve_or_create_chat(
            envelope.parent_tool_use_id.as_deref(),
            envelope.parent_name.as_deref(),
            envelope.parent_prompt.as_deref(),
        );

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
        } = &envelope.event
        {
            self.token_usage += usage.clone();
            self.chats[chat_idx].token_usage += usage.clone();
            self.chats[chat_idx].context_size =
                context_size.unwrap_or_else(|| usage.context_tokens());
        }

        let result = self.chats[chat_idx].handle_event(envelope.event, plan_path);

        if matches!(result, ChatEventResult::InterruptConsumed) && chat_idx == 0 {
            if !self.pending_interrupts.is_empty() {
                self.pending_interrupts.remove(0);
            }
            return vec![];
        }

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
                    self.pending_interrupts.clear();
                    self.queue.clear();
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

    fn resolve_or_create_chat(
        &mut self,
        parent_id: Option<&str>,
        parent_name: Option<&str>,
        parent_prompt: Option<&str>,
    ) -> usize {
        let Some(id) = parent_id else { return 0 };
        if let Some(&idx) = self.chat_index.get(id) {
            return idx;
        }
        let name = parent_name
            .map(String::from)
            .unwrap_or_else(|| format!("Agent {}", self.chats.len()));
        let idx = self.chats.len();
        self.chat_index.insert(id.to_owned(), idx);
        self.chats[0].update_tool_summary(id, &name);
        let mut chat = Chat::new(name);
        if let Some(prompt) = parent_prompt {
            chat.push_user_message(prompt);
        }
        self.chats.push(chat);
        idx
    }

    fn toggle_mode(&mut self) -> Vec<Action> {
        if self.status == Status::Streaming {
            return vec![];
        }
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
        match &self.mode {
            Mode::Build => ("[BUILD]", theme::MODE_BUILD),
            Mode::Plan { .. } => ("[PLAN]", theme::MODE_PLAN),
            Mode::BuildPlan => ("[BUILD PLAN]", theme::MODE_BUILD_PLAN),
        }
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
                    return vec![];
                }
                self.status = Status::Streaming;
                vec![Action::Compact]
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
        self.status = Status::Idle;
        self.token_usage = TokenUsage::default();
        self.pending_interrupts.clear();
        self.queue.clear();
        #[cfg(feature = "demo")]
        {
            self.demo_questions = None;
        }
        self.question_form.close();
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
        self.msg_area = msg_area;
        self.frame_area = frame.area();
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
        self.chats[render_chat].view(frame, msg_area, self.selection.is_some());

        let queue_height = queue_panel::height(self.visible_queue_len());
        let input_height = bottom_area.height.saturating_sub(queue_height);
        let [queue_area, input_area] = Layout::vertical([
            Constraint::Length(queue_height),
            Constraint::Length(input_height),
        ])
        .areas(bottom_area);
        self.input_area = input_area;

        let cmd_popup_area = if form_visible {
            self.question_form.view(frame, bottom_area);
            None
        } else {
            let queue_texts = self.visible_queue_texts();
            queue_panel::view(frame, queue_area, &queue_texts);
            self.input_box
                .view(frame, input_area, self.status == Status::Streaming);
            self.command_palette.view(frame, input_area)
        };

        let picker_inner_area = if let Some(names) = names {
            let full_area = frame.area();
            self.chat_picker.view(frame, full_area, &names)
        } else {
            None
        };

        let chat = &self.chats[render_chat];
        let chat_name = (self.chats.len() > 1).then_some(chat.name.as_str());
        let (mode_label, mode_style) = self.mode_label();
        let ctx = StatusBarContext {
            status: &self.status,
            mode_label,
            mode_style,
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
            retry_info: self.retry_info.as_ref(),
        };
        self.status_bar.view(frame, status_area, &ctx);

        if let Some(sel) = self.selection {
            selection::apply_highlight(frame.buffer_mut(), self.frame_area, &sel);
            if self.copy_on_next_render {
                self.copy_selection(
                    frame.buffer_mut(),
                    &sel,
                    &ViewAreas {
                        render_chat,
                        form_visible,
                        bottom_area,
                        queue_area,
                        input_area,
                        status_area,
                        cmd_popup_area,
                        picker_inner_area,
                    },
                );
            }
        }
    }

    fn copy_selection(
        &mut self,
        buf: &mut ratatui::buffer::Buffer,
        sel: &Selection,
        va: &ViewAreas,
    ) {
        let mut regions = Vec::new();
        self.chats[va.render_chat].push_content_regions(&mut regions);

        let queue_text;
        let input_value;
        if va.form_visible {
            regions.push(ContentRegion {
                area: inset_border(va.bottom_area),
                raw_text: "",
            });
        } else {
            if va.queue_area.height > 0 {
                let raw = self.visible_queue_texts();
                queue_text = raw.join("\n");
                regions.push(ContentRegion {
                    area: inset_border(va.queue_area),
                    raw_text: &queue_text,
                });
            }
            regions.push(ContentRegion {
                area: Rect::new(va.input_area.x, va.input_area.y, va.input_area.width, 1),
                raw_text: "",
            });
            input_value = self.input_box.buffer.value();
            regions.push(ContentRegion {
                area: Rect::new(
                    va.input_area.x,
                    va.input_area.y + 1,
                    va.input_area.width,
                    va.input_area.height.saturating_sub(2),
                ),
                raw_text: &input_value,
            });
            let input_bottom = va.input_area.y + va.input_area.height.saturating_sub(1);
            regions.push(ContentRegion {
                area: Rect::new(va.input_area.x, input_bottom, va.input_area.width, 1),
                raw_text: "",
            });
        }
        regions.push(ContentRegion {
            area: va.status_area,
            raw_text: "",
        });
        if let Some(area) = va.cmd_popup_area {
            regions.push(ContentRegion { area, raw_text: "" });
        }
        if let Some(area) = va.picker_inner_area {
            regions.push(ContentRegion { area, raw_text: "" });
        }

        let text = selection::extract_selected_text(buf, sel, &regions);
        if !text.is_empty() {
            match &mut self.clipboard {
                Some(cb) => match cb.set_text(&text) {
                    Ok(()) => self.status_bar.flash("Copied selection".into()),
                    Err(e) => self.status_bar.flash(format!("Copy failed: {e}")),
                },
                None => self.status_bar.flash("Copy failed: no clipboard".into()),
            }
        }
        self.copy_on_next_render = false;
        self.selection = None;
    }

    pub fn is_animating(&self) -> bool {
        self.chats.iter().any(|c| c.is_animating())
    }

    #[cfg(feature = "demo")]
    pub fn load_messages(&mut self, msgs: Vec<DisplayMessage>) {
        self.main_chat().load_messages(msgs);
    }

    #[cfg(feature = "demo")]
    pub fn load_subagent(
        &mut self,
        parent_tool_id: &str,
        name: &str,
        msgs: Vec<DisplayMessage>,
    ) -> usize {
        let idx = self.chats.len();
        let mut chat = Chat::new(name.to_owned());
        chat.load_messages(msgs);
        self.chats.push(chat);
        self.chat_index.insert(parent_tool_id.to_owned(), idx);
        idx
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
    use test_case::test_case;

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
        Msg::Agent(Box::new(Envelope {
            event,
            parent_tool_use_id: None,
            parent_name: None,
            parent_prompt: None,
        }))
    }

    fn subagent_msg(event: AgentEvent, parent_id: &str, name: Option<&str>) -> Msg {
        subagent_msg_with_prompt(event, parent_id, name, None)
    }

    fn subagent_msg_with_prompt(
        event: AgentEvent,
        parent_id: &str,
        name: Option<&str>,
        prompt: Option<&str>,
    ) -> Msg {
        Msg::Agent(Box::new(Envelope {
            event,
            parent_tool_use_id: Some(parent_id.into()),
            parent_name: name.map(String::from),
            parent_prompt: prompt.map(String::from),
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
    fn error_event_sets_status() {
        let mut app = test_app();
        app.status = Status::Streaming;
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

        // Tab blocked during streaming
        app.mode = Mode::Build;
        app.status = Status::Streaming;
        tab(&mut app);
        assert_eq!(app.mode, Mode::Build);
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

        app.update(agent_msg(AgentEvent::ToolDone(
            maki_providers::ToolDoneEvent {
                id: "t1".into(),
                tool: "write",
                output: maki_providers::ToolOutput::WriteCode {
                    path: written_path.into(),
                    byte_count: 100,
                    lines: vec![],
                },
                is_error: false,
            },
        )));

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
    fn double_esc_cancels_flushes_and_fails_tools() {
        let mut app = test_app();
        app.status = Status::Streaming;
        app.update(agent_msg(AgentEvent::TextDelta {
            text: "partial".into(),
        }));
        app.update(agent_msg(AgentEvent::ToolStart(
            maki_providers::ToolStartEvent {
                id: "t1".into(),
                tool: "bash",
                summary: "running".into(),
                input: None,
                output: None,
            },
        )));

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

    fn app_with_pending_interrupt() -> App {
        let mut app = test_app();
        let (tx, _rx) = mpsc::channel();
        app.interrupt_tx = Some(tx);
        app.status = Status::Streaming;
        app.pending_interrupts.push("pending".into());
        app
    }

    fn type_and_submit(app: &mut App, text: &str) -> Vec<Action> {
        for c in text.chars() {
            app.update(Msg::Key(key(KeyCode::Char(c))));
        }
        app.update(Msg::Key(key(KeyCode::Enter)))
    }

    #[test]
    fn cancel_clears_pending_interrupts() {
        let mut app = app_with_pending_interrupt();
        app.update(Msg::Key(key(KeyCode::Esc)));
        app.update(Msg::Key(key(KeyCode::Esc)));
        assert!(app.pending_interrupts.is_empty());
    }

    #[test]
    fn error_clears_pending_interrupts() {
        let mut app = app_with_pending_interrupt();
        app.update(agent_msg(AgentEvent::Error {
            message: "boom".into(),
        }));
        assert!(app.pending_interrupts.is_empty());
    }

    #[test]
    fn multiple_interrupts_drained_in_order() {
        let mut app = app_with_pending_interrupt();
        app.pending_interrupts.push("second".into());

        app.update(agent_msg(AgentEvent::InterruptConsumed {
            message: "pending".into(),
        }));
        assert_eq!(app.pending_interrupts, vec!["second"]);

        app.update(agent_msg(AgentEvent::InterruptConsumed {
            message: "second".into(),
        }));
        assert!(app.pending_interrupts.is_empty());
    }

    #[test]
    fn submit_during_streaming_sends_via_interrupt_channel() {
        let mut app = test_app();
        let (tx, rx) = mpsc::channel();
        app.interrupt_tx = Some(tx);
        app.status = Status::Streaming;

        let actions = type_and_submit(&mut app, "urgent");
        assert!(actions.is_empty());
        assert!(app.queue.is_empty());
        assert_eq!(rx.try_recv().unwrap(), "urgent");
        assert_ne!(app.chats[0].last_message_text(), "urgent");
        assert_eq!(app.pending_interrupts, vec!["urgent"]);
    }

    #[test]
    fn submit_during_streaming_falls_back_to_queue_when_channel_closed() {
        let mut app = test_app();
        let (tx, rx) = mpsc::channel();
        app.interrupt_tx = Some(tx);
        app.status = Status::Streaming;
        drop(rx);

        let actions = type_and_submit(&mut app, "late");
        assert!(actions.is_empty());
        assert_eq!(app.queue.len(), 1);
        assert_eq!(app.queue[0].message, "late");
    }

    #[test]
    fn interrupt_displayed_only_on_consumed_event() {
        let mut app = app_with_pending_interrupt();
        let before = app.chats[0].message_count();

        app.update(agent_msg(AgentEvent::InterruptConsumed {
            message: "pending".into(),
        }));
        assert!(app.pending_interrupts.is_empty());
        assert_eq!(app.chats[0].message_count(), before + 1);
        assert_eq!(app.chats[0].last_message_text(), "pending");
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
        app.queue.push_back(AgentInput {
            message: "q".into(),
            mode: AgentMode::Build,
            pending_plan: None,
        });
        app.pending_interrupts.push("p".into());
        let actions = app.reset_session();
        assert!(matches!(&actions[0], Action::NewSession));
        assert_eq!(app.status, Status::Idle);
        assert_eq!(app.token_usage.input, 0);
        assert_eq!(app.chats[0].context_size, 0);
        assert_eq!(app.mode, Mode::BuildPlan);
        assert_eq!(app.ready_plan.as_deref(), Some("plan.md"));
        assert!(app.queue.is_empty());
        assert!(app.pending_interrupts.is_empty());
        assert_eq!(app.chats.len(), 1);
        assert_eq!(app.chats[0].name, "Main");
        assert_eq!(app.active_chat, 0);
        assert!(app.chat_index.is_empty());
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
            usage: main_usage.clone(),
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
                usage: sub_usage.clone(),
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
            AgentEvent::ToolStart(maki_providers::ToolStartEvent {
                id: "sub_t1".into(),
                tool: "bash",
                summary: "running".into(),
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

    fn app_with_subagent() -> App {
        let mut app = test_app();
        app.status = Status::Streaming;
        app.update(subagent_msg(
            AgentEvent::TextDelta { text: "x".into() },
            "task1",
            Some("research"),
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

    #[test]
    fn compact_command_sets_streaming() {
        let mut app = test_app();
        let actions = app.execute_command("/compact");
        assert!(matches!(&actions[0], Action::Compact));
        assert_eq!(app.status, Status::Streaming);
    }

    #[test]
    fn compact_during_streaming_ignored() {
        let mut app = test_app();
        app.status = Status::Streaming;
        let actions = app.execute_command("/compact");
        assert!(actions.is_empty());
    }

    fn long_question_no_options() -> AgentEvent {
        let long = (0..20)
            .map(|i| format!("line {i}"))
            .collect::<Vec<_>>()
            .join("\n");
        AgentEvent::QuestionPrompt {
            id: "q1".into(),
            questions: vec![maki_providers::QuestionInfo {
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
            questions: vec![maki_providers::QuestionInfo {
                question: "Pick a DB".into(),
                header: "DB".into(),
                options: vec![maki_providers::QuestionOption {
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
            app.update(agent_msg(event));
            assert_eq!(app.question_form.is_visible(), expect_form);
            assert_eq!(app.pending_question, expect_pending);
        }
    }

    #[test]
    fn pending_question_submit_routes_through_answer_tx() {
        let mut app = test_app();
        app.status = Status::Streaming;
        let (tx, rx) = mpsc::channel();
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
        app.msg_area = Rect::new(0, 0, 80, 20);
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
        app.msg_area = Rect::new(0, 0, 80, 20);
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

    #[test_case(20, 10, 80, 30, 20, 10 ; "within_bounds")]
    #[test_case(100, 50, 80, 30, 79, 29 ; "clamps_to_frame")]
    fn mouse_drag_updates_selection(
        drag_col: u16,
        drag_row: u16,
        frame_w: u16,
        frame_h: u16,
        expect_col: u16,
        expect_row: u16,
    ) {
        let mut app = test_app();
        app.msg_area = Rect::new(0, 0, 80, 20);
        app.frame_area = Rect::new(0, 0, frame_w, frame_h);

        app.update(mouse_event(MouseEventKind::Down(MouseButton::Left), 5, 5));
        app.update(mouse_event(
            MouseEventKind::Drag(MouseButton::Left),
            drag_col,
            drag_row,
        ));

        let sel = app.selection.unwrap();
        assert_eq!(sel.cursor_col, expect_col);
        assert_eq!(sel.cursor_row, expect_row);
    }

    #[test]
    fn mouse_up_behavior() {
        let mut app = test_app();

        app.update(mouse_event(MouseEventKind::Down(MouseButton::Left), 5, 5));
        app.update(mouse_event(MouseEventKind::Drag(MouseButton::Left), 10, 10));
        app.update(mouse_event(MouseEventKind::Up(MouseButton::Left), 10, 10));
        assert!(
            app.copy_on_next_render,
            "non-empty selection sets copy flag"
        );

        app.copy_on_next_render = false;
        app.update(mouse_event(MouseEventKind::Down(MouseButton::Left), 5, 5));
        app.update(mouse_event(MouseEventKind::Up(MouseButton::Left), 5, 5));
        assert!(
            !app.copy_on_next_render,
            "empty selection does not set copy flag"
        );
        assert!(app.selection.is_none(), "empty selection is cleared");
    }

    #[test]
    fn key_and_scroll_clear_selection() {
        let mut app = test_app();
        app.msg_area = Rect::new(0, 0, 80, 20);

        app.selection = Some(Selection::start(5, 5));
        app.update(Msg::Key(key(KeyCode::Char('a'))));
        assert!(app.selection.is_none(), "key press clears selection");

        app.selection = Some(Selection::start(5, 5));
        app.update(Msg::Scroll {
            column: 10,
            row: 10,
            delta: 3,
        });
        assert!(app.selection.is_none(), "scroll clears selection");
    }

    #[test]
    fn form_submit_pushes_answer_to_chat() {
        let mut app = test_app();
        app.status = Status::Streaming;
        let (tx, rx) = mpsc::channel();
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
        let (tx, rx) = mpsc::channel();
        app.answer_tx = Some(tx);

        app.update(agent_msg(short_question_with_options()));
        app.update(Msg::Key(key(KeyCode::Esc)));
        assert!(!app.question_form.is_visible());
        assert_eq!(app.chats[0].last_message_text(), "");
        assert_eq!(rx.try_recv().unwrap(), "");
    }
}

use std::borrow::Cow;
use std::time::{Duration, Instant};

use crate::animation::{Typewriter, spinner_frame};
use crate::highlight;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use maki_agent::tools::WEBFETCH_TOOL_NAME;
use maki_agent::{AgentInput, AgentMode};
use maki_providers::{AgentEvent, ModelPricing, TokenUsage};
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};

const TOOL_OUTPUT_MAX_DISPLAY_LINES: usize = 5;
const CANCEL_WINDOW: Duration = Duration::from_secs(3);
const CANCEL_MSG: &str = "Cancelled. The agent will continue from the last successful result.";

const USER_STYLE: Style = Style::new().fg(Color::Cyan);
const ASSISTANT_STYLE: Style = Style::new().fg(Color::White);
const THINKING_STYLE: Style = Style::new()
    .fg(Color::DarkGray)
    .add_modifier(Modifier::ITALIC);
const TOOL_STYLE: Style = Style::new().fg(Color::Yellow).add_modifier(Modifier::DIM);
const TOOL_INDICATOR: &str = "● ";
const TOOL_IN_PROGRESS_STYLE: Style = Style::new().fg(Color::White);
const TOOL_SUCCESS_STYLE: Style = Style::new().fg(Color::Green);
const TOOL_ERROR_STYLE: Style = Style::new().fg(Color::Red);
const CURSOR_STYLE: Style = Style::new()
    .fg(Color::White)
    .add_modifier(Modifier::SLOW_BLINK);
const STATUS_IDLE_STYLE: Style = Style::new().fg(Color::DarkGray);
const STATUS_STREAMING_STYLE: Style = Style::new().fg(Color::Yellow);
const STATUS_ERROR_STYLE: Style = Style::new().fg(Color::Red);
const BOLD_STYLE: Style = Style::new().fg(Color::Cyan).add_modifier(Modifier::BOLD);
const CODE_STYLE: Style = Style::new().fg(Color::Magenta);
const MODE_BUILD_STYLE: Style = Style::new().fg(Color::Green).add_modifier(Modifier::BOLD);
const MODE_PLAN_STYLE: Style = Style::new().fg(Color::Blue).add_modifier(Modifier::BOLD);
const CANCEL_HINT_STYLE: Style = Style::new().fg(Color::Yellow);

struct Delimiter {
    open: &'static str,
    style: Style,
}

const DELIMITERS: [Delimiter; 2] = [
    Delimiter {
        open: "**",
        style: BOLD_STYLE,
    },
    Delimiter {
        open: "`",
        style: CODE_STYLE,
    },
];

fn parse_inline_markdown<'a>(text: &'a str, base_style: Style) -> Vec<Span<'a>> {
    let mut spans = Vec::new();
    let mut remaining = text;

    while !remaining.is_empty() {
        let next = DELIMITERS
            .iter()
            .filter_map(|d| remaining.find(d.open).map(|pos| (pos, d)))
            .min_by_key(|(pos, _)| *pos);

        let Some((pos, delim)) = next else {
            spans.push(Span::styled(remaining, base_style));
            break;
        };

        if pos > 0 {
            spans.push(Span::styled(&remaining[..pos], base_style));
        }

        let after_open = &remaining[pos + delim.open.len()..];
        if let Some(close) = after_open.find(delim.open) {
            spans.push(Span::styled(&after_open[..close], delim.style));
            remaining = &after_open[close + delim.open.len()..];
        } else {
            spans.push(Span::styled(&remaining[pos..], base_style));
            break;
        }
    }

    spans
}

enum TextBlock<'a> {
    Normal(&'a str),
    Code { lang: &'a str, code: &'a str },
}

fn parse_blocks(text: &str) -> Vec<TextBlock<'_>> {
    let mut blocks = Vec::new();
    let mut rest = text;

    while let Some(fence_start) = rest.find("```") {
        let before = &rest[..fence_start];
        if !before.is_empty() {
            blocks.push(TextBlock::Normal(
                before.strip_suffix('\n').unwrap_or(before),
            ));
        }

        let after_fence = &rest[fence_start + 3..];
        let lang_end = after_fence.find('\n').unwrap_or(after_fence.len());
        let lang = after_fence[..lang_end].trim();

        let code_start_offset = lang_end + 1;
        if code_start_offset > after_fence.len() {
            rest = "";
            break;
        }
        let code_region = &after_fence[code_start_offset..];

        if let Some(close) = code_region.find("```") {
            let code = code_region[..close]
                .strip_suffix('\n')
                .unwrap_or(&code_region[..close]);
            blocks.push(TextBlock::Code { lang, code });
            let after_close = &code_region[close + 3..];
            rest = after_close.strip_prefix('\n').unwrap_or(after_close);
        } else {
            let code = code_region;
            blocks.push(TextBlock::Code { lang, code });
            rest = "";
            break;
        }
    }

    if !rest.is_empty() {
        blocks.push(TextBlock::Normal(rest));
    }

    blocks
}

fn text_to_lines(text: &str, prefix: &str, base_style: Style) -> Vec<Line<'static>> {
    let prefix_style = base_style.add_modifier(Modifier::BOLD);
    let blocks = parse_blocks(text);
    let mut lines: Vec<Line<'static>> = Vec::new();
    let mut first_line = true;

    for block in blocks {
        match block {
            TextBlock::Normal(content) => {
                for line in content.split('\n') {
                    let mut spans: Vec<Span<'static>> = Vec::new();
                    if first_line {
                        spans.push(Span::styled(prefix.to_owned(), prefix_style));
                        first_line = false;
                    }
                    spans.extend(
                        parse_inline_markdown(line, base_style)
                            .into_iter()
                            .map(|s| Span::styled(s.content.into_owned(), s.style)),
                    );
                    lines.push(Line::from(spans));
                }
            }
            TextBlock::Code { lang, code } => {
                if first_line {
                    lines.push(Line::from(Span::styled(prefix.to_owned(), prefix_style)));
                    first_line = false;
                }
                lines.extend(highlight::highlight_code(lang, code));
            }
        }
    }

    if lines.is_empty() {
        lines.push(Line::from(Span::styled(prefix.to_owned(), prefix_style)));
    }

    lines
}

fn truncate_lines(s: &str, max_lines: usize) -> Cow<'_, str> {
    match s.match_indices('\n').nth(max_lines.saturating_sub(1)) {
        Some((i, _)) => Cow::Owned(format!("{}\n...", &s[..i])),
        None => Cow::Borrowed(s),
    }
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
}

#[derive(Debug, Clone, PartialEq)]
pub enum DisplayRole {
    User,
    Assistant,
    Thinking,
    Tool { id: String, status: ToolStatus },
    Error,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Status {
    Idle,
    Streaming,
    Error(String),
}

pub enum Msg {
    Key(KeyEvent),
    Agent(AgentEvent),
}

pub enum Action {
    SendMessage(AgentInput),
    CancelAgent,
    Quit,
}

pub struct App {
    pub messages: Vec<DisplayMessage>,
    pub input: String,
    pub cursor_pos: usize,
    streaming_thinking: Typewriter,
    streaming_text: Typewriter,
    pub status: Status,
    scroll_top: u16,
    auto_scroll: bool,
    viewport_height: u16,
    pub token_usage: TokenUsage,
    pub should_quit: bool,
    cancel_hint_since: Option<Instant>,
    pub mode: AgentMode,
    pending_plan: Option<String>,
    pricing: ModelPricing,
    started_at: Instant,
    cached_lines: Vec<Line<'static>>,
    cached_msg_count: usize,
}

impl App {
    pub fn new(pricing: ModelPricing) -> Self {
        Self {
            messages: Vec::new(),
            input: String::new(),
            cursor_pos: 0,
            streaming_thinking: Typewriter::new(),
            streaming_text: Typewriter::new(),
            status: Status::Idle,
            scroll_top: u16::MAX,
            auto_scroll: true,
            viewport_height: 24,
            token_usage: TokenUsage::default(),
            should_quit: false,
            cancel_hint_since: None,
            mode: AgentMode::Build,
            pending_plan: None,
            pricing,
            started_at: Instant::now(),
            cached_lines: Vec::new(),
            cached_msg_count: 0,
        }
    }

    pub fn update(&mut self, msg: Msg) -> Vec<Action> {
        match msg {
            Msg::Key(key) => self.handle_key(key),
            Msg::Agent(event) => self.handle_agent_event(event),
        }
    }

    fn scroll(&mut self, delta: i32) {
        if delta > 0 {
            self.scroll_top = self.scroll_top.saturating_sub(delta as u16);
        } else {
            self.scroll_top = self.scroll_top.saturating_add(delta.unsigned_abs() as u16);
        }
        self.auto_scroll = false;
    }

    fn handle_key(&mut self, key: KeyEvent) -> Vec<Action> {
        if key.modifiers.contains(KeyModifiers::CONTROL) {
            let half = self.viewport_height as i32 / 2;
            return match key.code {
                KeyCode::Char('c') => {
                    self.should_quit = true;
                    vec![Action::Quit]
                }
                KeyCode::Char('u') => {
                    self.scroll(half);
                    vec![]
                }
                KeyCode::Char('d') => {
                    self.scroll(-half);
                    vec![]
                }
                _ => vec![],
            };
        }

        match key.code {
            KeyCode::Up => {
                self.scroll(1);
                return vec![];
            }
            KeyCode::Down => {
                self.scroll(-1);
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
            KeyCode::Enter => {
                let text = self.input.trim().to_string();
                if text.is_empty() {
                    return vec![];
                }

                let pending_plan = self.pending_plan.take();

                self.messages.push(DisplayMessage {
                    role: DisplayRole::User,
                    text: text.clone(),
                });
                self.input.clear();
                self.cursor_pos = 0;
                drop(self.streaming_thinking.take_all());
                drop(self.streaming_text.take_all());
                self.status = Status::Streaming;
                self.auto_scroll = true;
                vec![Action::SendMessage(AgentInput {
                    message: text,
                    mode: self.mode.clone(),
                    pending_plan,
                })]
            }
            KeyCode::Char(c) => {
                self.input.insert(self.cursor_pos, c);
                self.cursor_pos += 1;
                vec![]
            }
            KeyCode::Backspace => {
                if self.cursor_pos > 0 {
                    self.cursor_pos -= 1;
                    self.input.remove(self.cursor_pos);
                }
                vec![]
            }
            KeyCode::Left => {
                self.cursor_pos = self.cursor_pos.saturating_sub(1);
                vec![]
            }
            KeyCode::Right => {
                self.cursor_pos = (self.cursor_pos + 1).min(self.input.len());
                vec![]
            }
            _ => vec![],
        }
    }

    fn handle_cancel_press(&mut self) -> Vec<Action> {
        if let Some(t) = self.cancel_hint_since
            && t.elapsed() < CANCEL_WINDOW
        {
            self.flush_streaming_text();
            self.messages.push(DisplayMessage {
                role: DisplayRole::Error,
                text: CANCEL_MSG.into(),
            });
            self.status = Status::Idle;
            self.cancel_hint_since = None;
            return vec![Action::CancelAgent];
        }
        self.cancel_hint_since = Some(Instant::now());
        vec![]
    }

    fn handle_agent_event(&mut self, event: AgentEvent) -> Vec<Action> {
        match event {
            AgentEvent::ThinkingDelta { text } => {
                self.streaming_thinking.push(&text);
            }
            AgentEvent::TextDelta { text } => {
                self.flush_streaming_thinking();
                self.streaming_text.push(&text);
            }
            AgentEvent::ToolStart(start) => {
                self.flush_streaming_text();
                let text = format!("[{}] {}", start.tool, start.summary);
                self.messages.push(DisplayMessage {
                    role: DisplayRole::Tool {
                        id: start.id,
                        status: ToolStatus::InProgress,
                    },
                    text,
                });
            }
            AgentEvent::ToolDone(done) => {
                let status = if done.is_error {
                    ToolStatus::Error
                } else {
                    ToolStatus::Success
                };
                if let Some(msg) = self.messages.iter_mut().rfind(|m| {
                    matches!(m.role, DisplayRole::Tool { ref id, .. } if *id == done.id)
                }) {
                    msg.role = DisplayRole::Tool {
                        id: done.id.clone(),
                        status,
                    };
                    self.invalidate_line_cache();
                }
                let text = if done.tool == WEBFETCH_TOOL_NAME {
                    let n = done.content.lines().count();
                    format!("[{} done] ({n} lines)", done.tool)
                } else {
                    let truncated = truncate_lines(&done.content, TOOL_OUTPUT_MAX_DISPLAY_LINES);
                    format!("[{} done] {truncated}", done.tool)
                };
                self.messages.push(DisplayMessage {
                    role: DisplayRole::Tool {
                        id: done.id,
                        status,
                    },
                    text,
                });
            }
            AgentEvent::TurnComplete { .. } | AgentEvent::ToolResultsSubmitted { .. } => {}
            AgentEvent::Done { usage, .. } => {
                self.flush_streaming_text();
                self.token_usage += usage;
                self.status = Status::Idle;
                self.cancel_hint_since = None;
            }
            AgentEvent::Error { message } => {
                self.flush_streaming_text();
                self.status = Status::Error(message);
                self.cancel_hint_since = None;
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

    fn flush_streaming_thinking(&mut self) {
        if !self.streaming_thinking.is_empty() {
            self.messages.push(DisplayMessage {
                role: DisplayRole::Thinking,
                text: self.streaming_thinking.take_all(),
            });
        }
    }

    fn flush_streaming_text(&mut self) {
        self.flush_streaming_thinking();
        if !self.streaming_text.is_empty() {
            self.messages.push(DisplayMessage {
                role: DisplayRole::Assistant,
                text: self.streaming_text.take_all(),
            });
        }
    }

    fn invalidate_line_cache(&mut self) {
        self.cached_msg_count = 0;
        self.cached_lines.clear();
    }

    pub fn view(&mut self, frame: &mut Frame) {
        if self
            .cancel_hint_since
            .is_some_and(|t| t.elapsed() >= CANCEL_WINDOW)
        {
            self.cancel_hint_since = None;
        }

        let [messages_area, input_area, status_area] = Layout::vertical([
            Constraint::Min(1),
            Constraint::Length(3),
            Constraint::Length(1),
        ])
        .areas(frame.area());

        self.render_messages(frame, messages_area);
        self.render_input(frame, input_area);
        self.render_status(frame, status_area);
    }

    fn rebuild_line_cache(&mut self) {
        if self.cached_msg_count == self.messages.len() {
            return;
        }
        for msg in &self.messages[self.cached_msg_count..] {
            let (prefix, base_style) = match &msg.role {
                DisplayRole::User => ("you> ", USER_STYLE),
                DisplayRole::Assistant => ("maki> ", ASSISTANT_STYLE),
                DisplayRole::Thinking => ("thinking> ", THINKING_STYLE),
                DisplayRole::Tool { .. } => ("tool> ", TOOL_STYLE),
                DisplayRole::Error => ("", STATUS_ERROR_STYLE),
            };
            let mut lines = text_to_lines(&msg.text, prefix, base_style);
            if let DisplayRole::Tool { status, .. } = &msg.role
                && let Some(first) = lines.first_mut()
            {
                let indicator_style = match status {
                    ToolStatus::Success => TOOL_SUCCESS_STYLE,
                    ToolStatus::Error => TOOL_ERROR_STYLE,
                    ToolStatus::InProgress => TOOL_IN_PROGRESS_STYLE,
                };
                first
                    .spans
                    .insert(0, Span::styled(TOOL_INDICATOR, indicator_style));
            }
            self.cached_lines.extend(lines);
        }
        self.cached_msg_count = self.messages.len();
    }

    fn render_messages(&mut self, frame: &mut Frame, area: Rect) {
        self.viewport_height = area.height;
        self.rebuild_line_cache();

        self.streaming_thinking.tick();
        self.streaming_text.tick();

        let mut lines = self.cached_lines.clone();
        for (tw, prefix, style) in [
            (&self.streaming_thinking, "thinking> ", THINKING_STYLE),
            (&self.streaming_text, "maki> ", ASSISTANT_STYLE),
        ] {
            if !tw.is_empty() {
                let mut parsed = text_to_lines(tw.visible(), prefix, style);
                if let Some(last) = parsed.last_mut() {
                    last.spans.push(Span::styled("_", CURSOR_STYLE));
                }
                lines.extend(parsed);
            }
        }

        let paragraph = Paragraph::new(lines).wrap(Wrap { trim: false });
        let total_lines = paragraph.line_count(area.width) as u16;
        let max_scroll = total_lines.saturating_sub(area.height);
        self.scroll_top = self.scroll_top.min(max_scroll);
        if self.scroll_top >= max_scroll {
            self.auto_scroll = true;
        }
        if self.auto_scroll {
            self.scroll_top = max_scroll;
        }

        let paragraph = paragraph.scroll((self.scroll_top, 0));
        frame.render_widget(paragraph, area);
    }

    fn render_input(&self, frame: &mut Frame, area: Rect) {
        let indicator = if self.status == Status::Streaming {
            "..."
        } else {
            "> "
        };
        let input_text = format!("{indicator}{}", self.input);
        let paragraph = Paragraph::new(input_text).block(Block::default().borders(Borders::ALL));

        frame.render_widget(paragraph, area);

        if self.status != Status::Streaming {
            let cursor_x = area.x + 1 + indicator.len() as u16 + self.cursor_pos as u16;
            let cursor_y = area.y + 1;
            frame.set_cursor_position((cursor_x, cursor_y));
        }
    }

    fn render_status(&self, frame: &mut Frame, area: Rect) {
        let (mode_label, mode_style) = match &self.mode {
            AgentMode::Build => ("[BUILD]", MODE_BUILD_STYLE),
            AgentMode::Plan(_) => ("[PLAN]", MODE_PLAN_STYLE),
        };

        let stats = format!(
            " tokens: {}in / {}out (${:.3})",
            self.token_usage.input,
            self.token_usage.output,
            self.token_usage.cost(&self.pricing)
        );

        let mut spans = Vec::new();

        if self.status == Status::Streaming {
            let ch = spinner_frame(self.started_at.elapsed().as_millis());
            spans.push(Span::styled(format!(" {ch}"), STATUS_STREAMING_STYLE));
        }

        spans.push(Span::styled(format!(" {mode_label}"), mode_style));

        match &self.status {
            Status::Error(e) => {
                spans.push(Span::styled(format!(" error: {e}"), STATUS_ERROR_STYLE));
            }
            _ => {
                spans.push(Span::styled(stats, STATUS_IDLE_STYLE));
            }
        }

        if self.cancel_hint_since.is_some() {
            spans.push(Span::styled(" press Esc again to stop", CANCEL_HINT_STYLE));
        }

        frame.render_widget(Paragraph::new(Line::from(spans)), area);
    }

    pub fn is_animating(&self) -> bool {
        self.streaming_thinking.is_animating() || self.streaming_text.is_animating()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers};
    use maki_providers::{ToolDoneEvent, ToolStartEvent};
    use ratatui::backend::TestBackend;
    use test_case::test_case;

    fn test_pricing() -> ModelPricing {
        ModelPricing {
            input: 3.0,
            output: 15.0,
            cache_write: 3.75,
            cache_read: 0.30,
        }
    }

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent {
            code,
            modifiers: KeyModifiers::NONE,
            kind: KeyEventKind::Press,
            state: KeyEventState::NONE,
        }
    }

    fn ctrl(c: char) -> KeyEvent {
        KeyEvent {
            code: KeyCode::Char(c),
            modifiers: KeyModifiers::CONTROL,
            kind: KeyEventKind::Press,
            state: KeyEventState::NONE,
        }
    }

    #[test]
    fn typing_and_submit() {
        let mut app = App::new(test_pricing());
        app.update(Msg::Key(key(KeyCode::Char('h'))));
        app.update(Msg::Key(key(KeyCode::Char('i'))));
        assert_eq!(app.input, "hi");
        assert_eq!(app.cursor_pos, 2);

        let actions = app.update(Msg::Key(key(KeyCode::Enter)));
        assert_eq!(actions.len(), 1);
        assert!(matches!(&actions[0], Action::SendMessage(s) if s.message == "hi"));
        assert!(app.input.is_empty());
        assert_eq!(app.status, Status::Streaming);
        assert_eq!(app.messages.len(), 1);
        assert_eq!(app.messages[0].role, DisplayRole::User);
    }

    #[test]
    fn ctrl_c_quits_regardless_of_state() {
        for status in [Status::Idle, Status::Streaming] {
            let mut app = App::new(test_pricing());
            app.status = status;
            let actions = app.update(Msg::Key(ctrl('c')));
            assert!(app.should_quit);
            assert!(matches!(&actions[0], Action::Quit));
        }
    }

    #[test]
    fn agent_text_delta_accumulates() {
        let mut app = App::new(test_pricing());
        app.status = Status::Streaming;
        app.update(Msg::Agent(AgentEvent::TextDelta {
            text: "hello".into(),
        }));
        app.update(Msg::Agent(AgentEvent::TextDelta {
            text: " world".into(),
        }));
        assert_eq!(app.streaming_text, "hello world");
    }

    #[test]
    fn done_flushes_text_and_accumulates_usage() {
        let mut app = App::new(test_pricing());
        app.status = Status::Streaming;
        app.cancel_hint_since = Some(Instant::now());
        app.streaming_text.set_buffer("response text");
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
        assert!(app.streaming_text.is_empty());
        assert!(app.cancel_hint_since.is_none());
        assert_eq!(app.messages.last().unwrap().role, DisplayRole::Assistant);

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

    #[test_case(false, ToolStatus::Success ; "success_updates_start_to_success")]
    #[test_case(true,  ToolStatus::Error   ; "error_updates_start_to_error")]
    fn tool_done_updates_start_status(is_error: bool, expected: ToolStatus) {
        let mut app = App::new(test_pricing());
        app.status = Status::Streaming;
        app.update(Msg::Agent(AgentEvent::ToolStart(ToolStartEvent {
            id: "t1".into(),
            tool: "bash",
            summary: "cmd".into(),
        })));
        assert!(matches!(
            app.messages[0].role,
            DisplayRole::Tool { status: ToolStatus::InProgress, .. }
        ));

        app.update(Msg::Agent(AgentEvent::ToolDone(ToolDoneEvent {
            id: "t1".into(),
            tool: "bash",
            content: "output".into(),
            is_error,
        })));

        assert_eq!(app.messages.len(), 2);
        assert!(app.messages.iter().all(|m| matches!(m.role, DisplayRole::Tool { .. })));
        for msg in &app.messages {
            assert!(matches!(msg.role, DisplayRole::Tool { status, .. } if status == expected));
        }
    }

    #[test]
    fn webfetch_done_shows_line_count_only() {
        let mut app = App::new(test_pricing());
        app.status = Status::Streaming;
        app.update(Msg::Agent(AgentEvent::ToolDone(ToolDoneEvent {
            id: "t1".into(),
            tool: WEBFETCH_TOOL_NAME,
            content: "line1\nline2\nline3".into(),
            is_error: false,
        })));
        assert_eq!(
            app.messages[0].text,
            format!("[{WEBFETCH_TOOL_NAME} done] (3 lines)")
        );
    }

    #[test]
    fn backspace_and_cursor_movement() {
        let mut app = App::new(test_pricing());
        app.update(Msg::Key(key(KeyCode::Char('a'))));
        app.update(Msg::Key(key(KeyCode::Char('b'))));
        app.update(Msg::Key(key(KeyCode::Char('c'))));
        assert_eq!(app.input, "abc");

        app.update(Msg::Key(key(KeyCode::Left)));
        assert_eq!(app.cursor_pos, 2);

        app.update(Msg::Key(key(KeyCode::Backspace)));
        assert_eq!(app.input, "ac");
        assert_eq!(app.cursor_pos, 1);
    }

    #[test]
    fn tool_start_flushes_streaming_text() {
        let mut app = App::new(test_pricing());
        app.status = Status::Streaming;
        app.streaming_text.set_buffer("partial response");

        app.update(Msg::Agent(AgentEvent::ToolStart(ToolStartEvent {
            id: "t1".into(),
            tool: "read",
            summary: "/tmp/file".into(),
        })));

        assert!(app.streaming_text.is_empty());
        assert_eq!(app.messages[0].role, DisplayRole::Assistant);
        assert!(matches!(app.messages[1].role, DisplayRole::Tool { .. }));
    }

    #[test]
    fn thinking_delta_separate_from_text() {
        let mut app = App::new(test_pricing());
        app.status = Status::Streaming;
        app.update(Msg::Agent(AgentEvent::ThinkingDelta {
            text: "reasoning".into(),
        }));
        assert_eq!(app.streaming_thinking, "reasoning");
        assert!(app.streaming_text.is_empty());

        app.update(Msg::Agent(AgentEvent::TextDelta {
            text: "output".into(),
        }));
        assert!(app.streaming_thinking.is_empty());
        assert_eq!(app.streaming_text, "output");
        assert_eq!(app.messages[0].role, DisplayRole::Thinking);
        assert_eq!(app.messages[0].text, "reasoning");
    }

    #[test]
    fn error_event_sets_status() {
        let mut app = App::new(test_pricing());
        app.status = Status::Streaming;
        app.update(Msg::Agent(AgentEvent::Error {
            message: "boom".into(),
        }));
        assert!(matches!(app.status, Status::Error(ref e) if e == "boom"));
    }

    #[test_case(10, 'u', 0  ; "ctrl_u_saturates_at_zero")]
    #[test_case(20, 'u', 10 ; "ctrl_u_scrolls_up")]
    #[test_case(5,  'd', 15 ; "ctrl_d_scrolls_down")]
    #[test_case(0,  'd', 10 ; "ctrl_d_from_top")]
    fn half_page_scroll(initial: u16, key_char: char, expected: u16) {
        let mut app = App::new(test_pricing());
        app.viewport_height = 20;
        app.scroll_top = initial;
        app.update(Msg::Key(ctrl(key_char)));
        assert_eq!(app.scroll_top, expected);
    }

    #[test]
    fn scroll_top_clamped_to_content() {
        let mut app = App::new(test_pricing());
        app.messages.push(DisplayMessage {
            role: DisplayRole::User,
            text: "short".into(),
        });

        app.scroll_top = 1000;
        app.auto_scroll = false;
        let backend = TestBackend::new(80, 24);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal.draw(|f| app.view(f)).unwrap();

        assert_eq!(app.scroll_top, 0);
    }

    #[test]
    fn scroll_up_pins_viewport_during_streaming() {
        let mut app = App::new(test_pricing());
        app.status = Status::Streaming;
        app.streaming_text.set_buffer(&"a\n".repeat(30));

        let backend = TestBackend::new(80, 10);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal.draw(|f| app.view(f)).unwrap();

        app.update(Msg::Key(key(KeyCode::Up)));
        app.update(Msg::Key(key(KeyCode::Up)));
        terminal.draw(|f| app.view(f)).unwrap();
        let pinned = app.scroll_top;

        app.update(Msg::Agent(AgentEvent::TextDelta {
            text: "b\nb\nb\n".into(),
        }));
        terminal.draw(|f| app.view(f)).unwrap();

        assert!(!app.auto_scroll);
        assert_eq!(app.scroll_top, pinned);
    }

    #[test]
    fn ctrl_d_to_bottom_re_enables_auto_scroll() {
        let mut app = App::new(test_pricing());
        app.status = Status::Streaming;
        app.streaming_text.set_buffer(&"a\n".repeat(30));

        let backend = TestBackend::new(80, 10);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal.draw(|f| app.view(f)).unwrap();
        assert!(app.auto_scroll);

        app.update(Msg::Key(ctrl('u')));
        terminal.draw(|f| app.view(f)).unwrap();
        assert!(!app.auto_scroll);

        app.update(Msg::Key(ctrl('d')));
        terminal.draw(|f| app.view(f)).unwrap();
        assert!(app.auto_scroll);
    }

    #[test_case("a **bold** b", &[("a ", None), ("bold", Some(BOLD_STYLE)), (" b", None)] ; "bold")]
    #[test_case("use `foo` here", &[("use ", None), ("foo", Some(CODE_STYLE)), (" here", None)] ; "inline_code")]
    #[test_case("a `code` then **bold**", &[("a ", None), ("code", Some(CODE_STYLE)), (" then ", None), ("bold", Some(BOLD_STYLE))] ; "code_before_bold")]
    #[test_case("a **unclosed", &[("a ", None), ("**unclosed", None)] ; "unclosed_delimiter")]
    fn parse_inline_markdown_cases(input: &str, expected: &[(&str, Option<Style>)]) {
        let base = Style::default();
        let spans = parse_inline_markdown(input, base);
        assert_eq!(spans.len(), expected.len());
        for (span, (text, style)) in spans.iter().zip(expected) {
            assert_eq!(span.content, *text);
            assert_eq!(span.style, style.unwrap_or(base));
        }
    }

    #[test]
    fn text_to_lines_splits_newlines() {
        let style = Style::default();
        let lines = text_to_lines("line1\nline2\nline3", "p> ", style);
        assert_eq!(lines.len(), 3);
        assert_eq!(lines[0].spans[0].content, "p> ");
        assert_eq!(lines[1].spans.len(), 1);
    }

    #[test_case("a\nb\nc", 5, "a\nb\nc" ; "under_limit")]
    #[test_case("a\nb\nc\nd", 2, "a\nb\n..." ; "over_limit")]
    #[test_case("single", 1, "single" ; "single_line")]
    fn truncate_lines_cases(input: &str, max: usize, expected: &str) {
        assert_eq!(truncate_lines(input, max), expected);
    }

    #[test]
    fn tab_toggles_mode_and_sets_pending_plan() {
        let mut app = App::new(test_pricing());
        assert_eq!(app.mode, AgentMode::Build);

        app.update(Msg::Key(key(KeyCode::Tab)));
        assert!(matches!(app.mode, AgentMode::Plan(ref p) if p.contains(maki_agent::PLANS_DIR)));

        app.update(Msg::Key(key(KeyCode::Tab)));
        assert_eq!(app.mode, AgentMode::Build);
        assert!(app.pending_plan.is_some());
    }

    #[test]
    fn submit_consumes_pending_plan() {
        let mut app = App::new(test_pricing());
        app.pending_plan = Some("plan.md".into());
        app.update(Msg::Key(key(KeyCode::Char('x'))));
        let actions = app.update(Msg::Key(key(KeyCode::Enter)));
        let Action::SendMessage(ref input) = actions[0] else {
            panic!("expected SendMessage");
        };
        assert_eq!(input.pending_plan.as_deref(), Some("plan.md"));
        assert!(app.pending_plan.is_none());
    }

    fn block_summary<'a>(blocks: &'a [TextBlock<'a>]) -> Vec<(&'a str, Option<&'a str>)> {
        blocks
            .iter()
            .map(|b| match b {
                TextBlock::Normal(t) => (*t, None),
                TextBlock::Code { lang, code } => (*code, Some(*lang)),
            })
            .collect()
    }

    #[test_case(
        "hello world\nsecond line",
        &[("hello world\nsecond line", None)]
        ; "no_fences"
    )]
    #[test_case(
        "before\n```rust\nfn main() {}\n```\nafter",
        &[("before", None), ("fn main() {}", Some("rust")), ("after", None)]
        ; "single_code_block"
    )]
    #[test_case(
        "a\n```py\nx=1\n```\nb\n```js\ny=2\n```\nc",
        &[("a", None), ("x=1", Some("py")), ("b", None), ("y=2", Some("js")), ("c", None)]
        ; "multiple_code_blocks"
    )]
    #[test_case(
        "before\n```rust\nfn main() {}",
        &[("before", None), ("fn main() {}", Some("rust"))]
        ; "unclosed_fence"
    )]
    #[test_case(
        "a\n```rs\n```\nb",
        &[("a", None), ("", Some("rs")), ("b", None)]
        ; "empty_code_block"
    )]
    #[test_case(
        "```\ncode\n```",
        &[("code", Some(""))]
        ; "no_language_tag"
    )]
    fn parse_blocks_cases(input: &str, expected: &[(&str, Option<&str>)]) {
        let blocks = parse_blocks(input);
        assert_eq!(block_summary(&blocks), expected);
    }

    #[test]
    fn double_esc_cancels_and_flushes() {
        let mut app = App::new(test_pricing());
        app.status = Status::Streaming;
        app.streaming_text.set_buffer("partial response");

        let actions = app.update(Msg::Key(key(KeyCode::Esc)));
        assert!(actions.is_empty());
        assert!(app.cancel_hint_since.is_some());

        let actions = app.update(Msg::Key(key(KeyCode::Esc)));
        assert!(matches!(&actions[0], Action::CancelAgent));
        assert_eq!(app.status, Status::Idle);
        assert!(app.cancel_hint_since.is_none());

        let flushed = app
            .messages
            .iter()
            .find(|m| m.role == DisplayRole::Assistant);
        assert_eq!(flushed.unwrap().text, "partial response");
        let cancel = app.messages.iter().find(|m| m.role == DisplayRole::Error);
        assert_eq!(cancel.unwrap().text, CANCEL_MSG);
    }

    #[test]
    fn esc_after_expired_window_resets_hint() {
        let mut app = App::new(test_pricing());
        app.status = Status::Streaming;
        app.cancel_hint_since = Some(Instant::now() - CANCEL_WINDOW - Duration::from_millis(1));

        let actions = app.update(Msg::Key(key(KeyCode::Esc)));
        assert!(actions.is_empty());
        assert!(app.cancel_hint_since.is_some());
    }
}

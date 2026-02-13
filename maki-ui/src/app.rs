use std::mem;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use maki_agent::AgentEvent;
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};

const TOOL_OUTPUT_MAX_DISPLAY_LEN: usize = 200;
const ASSISTANT_COLOR: Color = Color::Green;
const BOLD_STYLE: Style = Style::new().fg(Color::White).add_modifier(Modifier::BOLD);
const CODE_STYLE: Style = Style::new().fg(Color::Yellow);

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

fn text_to_lines<'a>(
    text: &'a str,
    prefix: &'a str,
    prefix_style: Style,
    base_style: Style,
) -> Vec<Line<'a>> {
    text.split('\n')
        .enumerate()
        .map(|(i, line)| {
            let mut spans = Vec::new();
            if i == 0 {
                spans.push(Span::styled(prefix, prefix_style));
            }
            spans.extend(parse_inline_markdown(line, base_style));
            Line::from(spans)
        })
        .collect()
}

fn truncate_utf8(s: &str, max_bytes: usize) -> &str {
    &s[..s.floor_char_boundary(max_bytes)]
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
    Tool,
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
    SendMessage(String),
    Quit,
}

pub fn tool_start_msg(name: &str, input: &str) -> String {
    format!("[{name}] {input}")
}

pub fn tool_done_msg(name: &str, output: &str) -> String {
    format!("[{name} done] {output}")
}

pub struct App {
    pub messages: Vec<DisplayMessage>,
    pub input: String,
    pub cursor_pos: usize,
    streaming_text: String,
    pub status: Status,
    scroll_offset: u16,
    viewport_height: u16,
    pub token_usage: (u32, u32),
    pub should_quit: bool,
}

impl Default for App {
    fn default() -> Self {
        Self::new()
    }
}

impl App {
    pub fn new() -> Self {
        Self {
            messages: Vec::new(),
            input: String::new(),
            cursor_pos: 0,
            streaming_text: String::new(),
            status: Status::Idle,
            scroll_offset: 0,
            viewport_height: 24,
            token_usage: (0, 0),
            should_quit: false,
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
            self.scroll_offset = self.scroll_offset.saturating_add(delta as u16);
        } else {
            self.scroll_offset = self
                .scroll_offset
                .saturating_sub(delta.unsigned_abs() as u16);
        }
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
                self.messages.push(DisplayMessage {
                    role: DisplayRole::User,
                    text: text.clone(),
                });
                self.input.clear();
                self.cursor_pos = 0;
                self.streaming_text.clear();
                self.status = Status::Streaming;
                self.scroll_offset = 0;
                vec![Action::SendMessage(text)]
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

    fn handle_agent_event(&mut self, event: AgentEvent) -> Vec<Action> {
        match event {
            AgentEvent::TextDelta(text) => {
                self.streaming_text.push_str(&text);
                self.scroll_offset = 0;
            }
            AgentEvent::ToolStart { name, input } => {
                self.flush_streaming_text();
                self.messages.push(DisplayMessage {
                    role: DisplayRole::Tool,
                    text: tool_start_msg(&name, &input),
                });
            }
            AgentEvent::ToolDone { name, output } => {
                let truncated = if output.len() > TOOL_OUTPUT_MAX_DISPLAY_LEN {
                    format!("{}...", truncate_utf8(&output, TOOL_OUTPUT_MAX_DISPLAY_LEN))
                } else {
                    output
                };
                self.messages.push(DisplayMessage {
                    role: DisplayRole::Tool,
                    text: tool_done_msg(&name, &truncated),
                });
            }
            AgentEvent::Done {
                input_tokens,
                output_tokens,
            } => {
                self.flush_streaming_text();
                self.token_usage.0 += input_tokens;
                self.token_usage.1 += output_tokens;
                self.status = Status::Idle;
            }
            AgentEvent::Error(err) => {
                self.flush_streaming_text();
                self.status = Status::Error(err);
            }
        }
        vec![]
    }

    fn flush_streaming_text(&mut self) {
        if !self.streaming_text.is_empty() {
            self.messages.push(DisplayMessage {
                role: DisplayRole::Assistant,
                text: mem::take(&mut self.streaming_text),
            });
        }
    }

    pub fn view(&mut self, frame: &mut Frame) {
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

    fn render_messages(&mut self, frame: &mut Frame, area: Rect) {
        self.viewport_height = area.height;
        let assistant_style = Style::default().fg(ASSISTANT_COLOR);
        let mut lines: Vec<Line> = Vec::new();

        for msg in &self.messages {
            let (prefix, base_style) = match msg.role {
                DisplayRole::User => ("you> ", Style::default().fg(Color::Cyan)),
                DisplayRole::Assistant => ("maki> ", assistant_style),
                DisplayRole::Tool => (
                    "tool> ",
                    Style::default()
                        .fg(Color::Yellow)
                        .add_modifier(Modifier::DIM),
                ),
            };
            let prefix_style = base_style.add_modifier(Modifier::BOLD);
            lines.extend(text_to_lines(&msg.text, prefix, prefix_style, base_style));
        }

        if !self.streaming_text.is_empty() {
            let prefix_style = assistant_style.add_modifier(Modifier::BOLD);
            let mut parsed = text_to_lines(
                &self.streaming_text,
                "maki> ",
                prefix_style,
                assistant_style,
            );
            if let Some(last) = parsed.last_mut() {
                last.spans.push(Span::styled(
                    "_",
                    Style::default()
                        .fg(ASSISTANT_COLOR)
                        .add_modifier(Modifier::SLOW_BLINK),
                ));
            }
            lines.extend(parsed);
        }

        let total_lines = lines.len() as u16;
        let visible = area.height;
        let max_scroll_offset = total_lines.saturating_sub(visible);
        self.scroll_offset = self.scroll_offset.min(max_scroll_offset);
        let scroll = max_scroll_offset.saturating_sub(self.scroll_offset);

        let paragraph = Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .scroll((scroll, 0));

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
        let (text, style) = match &self.status {
            Status::Idle => (
                format!(
                    " tokens: {}in / {}out",
                    self.token_usage.0, self.token_usage.1
                ),
                Style::default().fg(Color::DarkGray),
            ),
            Status::Streaming => (
                " streaming...".to_string(),
                Style::default().fg(Color::Yellow),
            ),
            Status::Error(e) => (format!(" error: {e}"), Style::default().fg(Color::Red)),
        };

        frame.render_widget(Paragraph::new(text).style(style), area);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers};
    use ratatui::backend::TestBackend;
    use test_case::test_case;

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
        let mut app = App::new();
        app.update(Msg::Key(key(KeyCode::Char('h'))));
        app.update(Msg::Key(key(KeyCode::Char('i'))));
        assert_eq!(app.input, "hi");
        assert_eq!(app.cursor_pos, 2);

        let actions = app.update(Msg::Key(key(KeyCode::Enter)));
        assert_eq!(actions.len(), 1);
        assert!(matches!(&actions[0], Action::SendMessage(s) if s == "hi"));
        assert!(app.input.is_empty());
        assert_eq!(app.status, Status::Streaming);
        assert_eq!(app.messages.len(), 1);
        assert_eq!(app.messages[0].role, DisplayRole::User);
    }

    #[test]
    fn empty_submit_ignored() {
        let mut app = App::new();
        let actions = app.update(Msg::Key(key(KeyCode::Enter)));
        assert!(actions.is_empty());
    }

    #[test]
    fn keys_ignored_while_streaming() {
        let mut app = App::new();
        app.status = Status::Streaming;
        app.update(Msg::Key(key(KeyCode::Char('x'))));
        assert!(app.input.is_empty());
    }

    #[test]
    fn ctrl_c_quits_regardless_of_state() {
        for status in [Status::Idle, Status::Streaming] {
            let mut app = App::new();
            app.status = status;
            let actions = app.update(Msg::Key(ctrl('c')));
            assert!(app.should_quit);
            assert!(matches!(&actions[0], Action::Quit));
        }
    }

    #[test]
    fn agent_text_delta_accumulates() {
        let mut app = App::new();
        app.status = Status::Streaming;
        app.update(Msg::Agent(AgentEvent::TextDelta("hello".into())));
        app.update(Msg::Agent(AgentEvent::TextDelta(" world".into())));
        assert_eq!(app.streaming_text, "hello world");
    }

    #[test]
    fn agent_done_flushes_and_tracks_tokens() {
        let mut app = App::new();
        app.status = Status::Streaming;
        app.streaming_text = "response text".into();
        app.update(Msg::Agent(AgentEvent::Done {
            input_tokens: 100,
            output_tokens: 50,
        }));

        assert_eq!(app.status, Status::Idle);
        assert_eq!(app.token_usage, (100, 50));
        assert!(app.streaming_text.is_empty());
        assert_eq!(app.messages.last().unwrap().text, "response text");
        assert_eq!(app.messages.last().unwrap().role, DisplayRole::Assistant);
    }

    #[test]
    fn tool_events_create_messages() {
        let mut app = App::new();
        app.status = Status::Streaming;
        app.update(Msg::Agent(AgentEvent::ToolStart {
            name: "bash".into(),
            input: "ls".into(),
        }));
        app.update(Msg::Agent(AgentEvent::ToolDone {
            name: "bash".into(),
            output: "file.txt".into(),
        }));

        assert_eq!(app.messages.len(), 2);
        assert_eq!(app.messages[0].role, DisplayRole::Tool);
        assert_eq!(app.messages[0].text, tool_start_msg("bash", "ls"));
        assert_eq!(app.messages[1].text, tool_done_msg("bash", "file.txt"));
    }

    #[test]
    fn backspace_and_cursor_movement() {
        let mut app = App::new();
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
    fn error_event_sets_status() {
        let mut app = App::new();
        app.status = Status::Streaming;
        app.update(Msg::Agent(AgentEvent::Error("boom".into())));
        assert!(matches!(app.status, Status::Error(ref e) if e == "boom"));
    }

    #[test_case(0,  'u', 10 ; "ctrl_u_from_zero")]
    #[test_case(10, 'u', 20 ; "ctrl_u_accumulates")]
    #[test_case(25, 'd', 15 ; "ctrl_d_scrolls_down")]
    #[test_case(3,  'd', 0  ; "ctrl_d_saturates_at_zero")]
    fn half_page_scroll(initial: u16, key_char: char, expected: u16) {
        let mut app = App::new();
        app.viewport_height = 20;
        app.scroll_offset = initial;
        app.update(Msg::Key(ctrl(key_char)));
        assert_eq!(app.scroll_offset, expected);
    }

    #[test]
    fn scroll_offset_clamped_to_content() {
        let mut app = App::new();
        app.messages.push(DisplayMessage {
            role: DisplayRole::User,
            text: "short".into(),
        });

        app.scroll_offset = 1000;
        let backend = TestBackend::new(80, 24);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal.draw(|f| app.view(f)).unwrap();

        assert_eq!(app.scroll_offset, 0);
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
        let prefix_style = style.add_modifier(Modifier::BOLD);
        let lines = text_to_lines("line1\nline2\nline3", "p> ", prefix_style, style);
        assert_eq!(lines.len(), 3);
        assert_eq!(lines[0].spans[0].content, "p> ");
        assert_eq!(lines[1].spans.len(), 1);
    }

    #[test_case("hello world", 5, "hello" ; "ascii")]
    #[test_case("héllo", 3, "hé" ; "multibyte_boundary")]
    #[test_case("héllo", 2, "h" ; "mid_char_boundary")]
    fn truncate_utf8_cases(input: &str, max: usize, expected: &str) {
        assert_eq!(truncate_utf8(input, max), expected);
    }
}

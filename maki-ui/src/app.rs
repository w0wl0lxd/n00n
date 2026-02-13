use std::mem;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use maki_agent::AgentEvent;
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};

const TOOL_OUTPUT_MAX_DISPLAY_LEN: usize = 200;

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

    fn handle_key(&mut self, key: KeyEvent) -> Vec<Action> {
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            self.should_quit = true;
            return vec![Action::Quit];
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
            KeyCode::Up => {
                self.scroll_offset = self.scroll_offset.saturating_add(1);
                vec![]
            }
            KeyCode::Down => {
                self.scroll_offset = self.scroll_offset.saturating_sub(1);
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
                    format!("{}...", &output[..TOOL_OUTPUT_MAX_DISPLAY_LEN])
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

    pub fn view(&self, frame: &mut Frame) {
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

    fn render_messages(&self, frame: &mut Frame, area: Rect) {
        let mut lines: Vec<Line> = Vec::new();

        for msg in &self.messages {
            let (prefix, style) = match msg.role {
                DisplayRole::User => ("you> ", Style::default().fg(Color::Cyan)),
                DisplayRole::Assistant => ("maki> ", Style::default().fg(Color::Green)),
                DisplayRole::Tool => (
                    "tool> ",
                    Style::default()
                        .fg(Color::Yellow)
                        .add_modifier(Modifier::DIM),
                ),
            };
            lines.push(Line::from(vec![
                Span::styled(prefix, style.add_modifier(Modifier::BOLD)),
                Span::styled(&msg.text, style),
            ]));
        }

        if !self.streaming_text.is_empty() {
            lines.push(Line::from(vec![
                Span::styled(
                    "maki> ",
                    Style::default()
                        .fg(Color::Green)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(&self.streaming_text, Style::default().fg(Color::Green)),
                Span::styled(
                    "_",
                    Style::default()
                        .fg(Color::Green)
                        .add_modifier(Modifier::SLOW_BLINK),
                ),
            ]));
        }

        let total_lines = lines.len() as u16;
        let visible = area.height.saturating_sub(2);
        let scroll = if self.scroll_offset == 0 {
            total_lines.saturating_sub(visible)
        } else {
            total_lines
                .saturating_sub(visible)
                .saturating_sub(self.scroll_offset)
        };

        let paragraph = Paragraph::new(lines)
            .block(Block::default().borders(Borders::ALL).title(" maki "))
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

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent {
            code,
            modifiers: KeyModifiers::NONE,
            kind: KeyEventKind::Press,
            state: KeyEventState::NONE,
        }
    }

    fn ctrl_c() -> KeyEvent {
        KeyEvent {
            code: KeyCode::Char('c'),
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
            let actions = app.update(Msg::Key(ctrl_c()));
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
}

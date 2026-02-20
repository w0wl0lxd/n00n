use super::{DisplayMessage, DisplayRole, ToolStatus};

use crate::animation::Typewriter;
use crate::markdown::{text_to_lines, truncate_lines};

use maki_agent::tools::WEBFETCH_TOOL_NAME;
use maki_providers::{ToolDoneEvent, ToolStartEvent};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Wrap};

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
const STATUS_ERROR_STYLE: Style = Style::new().fg(Color::Red);
const TOOL_OUTPUT_MAX_DISPLAY_LINES: usize = 5;

pub struct MessagesPanel {
    messages: Vec<DisplayMessage>,
    streaming_thinking: Typewriter,
    streaming_text: Typewriter,
    scroll_top: u16,
    auto_scroll: bool,
    viewport_height: u16,
    cached_lines: Vec<Line<'static>>,
    cached_msg_count: usize,
}

impl MessagesPanel {
    pub fn new() -> Self {
        Self {
            messages: Vec::new(),
            streaming_thinking: Typewriter::new(),
            streaming_text: Typewriter::new(),
            scroll_top: u16::MAX,
            auto_scroll: true,
            viewport_height: 24,
            cached_lines: Vec::new(),
            cached_msg_count: 0,
        }
    }

    pub fn push(&mut self, msg: DisplayMessage) {
        self.messages.push(msg);
    }

    pub fn thinking_delta(&mut self, text: &str) {
        self.streaming_thinking.push(text);
    }

    pub fn text_delta(&mut self, text: &str) {
        self.flush_thinking();
        self.streaming_text.push(text);
    }

    pub fn tool_start(&mut self, event: ToolStartEvent) {
        self.flush();
        let text = format!("[{}] {}", event.tool, event.summary);
        self.messages.push(DisplayMessage {
            role: DisplayRole::Tool {
                id: event.id,
                status: ToolStatus::InProgress,
            },
            text,
        });
    }

    pub fn tool_done(&mut self, event: ToolDoneEvent) {
        let status = if event.is_error {
            ToolStatus::Error
        } else {
            ToolStatus::Success
        };
        if let Some(msg) = self
            .messages
            .iter_mut()
            .rfind(|m| matches!(m.role, DisplayRole::Tool { ref id, .. } if *id == event.id))
        {
            msg.role = DisplayRole::Tool {
                id: event.id.clone(),
                status,
            };
            self.invalidate_line_cache();
        }
        let text = if event.tool == WEBFETCH_TOOL_NAME {
            let n = event.content.lines().count();
            format!("[{} done] ({n} lines)", event.tool)
        } else {
            let truncated = truncate_lines(&event.content, TOOL_OUTPUT_MAX_DISPLAY_LINES);
            format!("[{} done] {truncated}", event.tool)
        };
        self.messages.push(DisplayMessage {
            role: DisplayRole::Tool {
                id: event.id,
                status,
            },
            text,
        });
    }

    pub fn flush(&mut self) {
        self.flush_thinking();
        if !self.streaming_text.is_empty() {
            self.messages.push(DisplayMessage {
                role: DisplayRole::Assistant,
                text: self.streaming_text.take_all(),
            });
        }
    }

    pub fn scroll(&mut self, delta: i32) {
        if delta > 0 {
            self.scroll_top = self.scroll_top.saturating_sub(delta as u16);
        } else {
            self.scroll_top = self.scroll_top.saturating_add(delta.unsigned_abs() as u16);
        }
        self.auto_scroll = false;
    }

    pub fn enable_auto_scroll(&mut self) {
        self.auto_scroll = true;
    }

    pub fn half_page(&self) -> i32 {
        self.viewport_height as i32 / 2
    }

    pub fn is_animating(&self) -> bool {
        self.streaming_thinking.is_animating() || self.streaming_text.is_animating()
    }

    pub fn view(&mut self, frame: &mut Frame, area: Rect) {
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

    fn flush_thinking(&mut self) {
        if !self.streaming_thinking.is_empty() {
            self.messages.push(DisplayMessage {
                role: DisplayRole::Thinking,
                text: self.streaming_thinking.take_all(),
            });
        }
    }

    fn invalidate_line_cache(&mut self) {
        self.cached_msg_count = 0;
        self.cached_lines.clear();
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::backend::TestBackend;
    use test_case::test_case;

    #[test]
    fn agent_text_delta_accumulates() {
        let mut panel = MessagesPanel::new();
        panel.text_delta("hello");
        panel.text_delta(" world");
        assert_eq!(panel.streaming_text, "hello world");
    }

    #[test_case(false, ToolStatus::Success ; "success_updates_start_to_success")]
    #[test_case(true,  ToolStatus::Error   ; "error_updates_start_to_error")]
    fn tool_done_updates_start_status(is_error: bool, expected: ToolStatus) {
        let mut panel = MessagesPanel::new();
        panel.tool_start(ToolStartEvent {
            id: "t1".into(),
            tool: "bash",
            summary: "cmd".into(),
        });
        assert!(matches!(
            panel.messages[0].role,
            DisplayRole::Tool {
                status: ToolStatus::InProgress,
                ..
            }
        ));

        panel.tool_done(ToolDoneEvent {
            id: "t1".into(),
            tool: "bash",
            content: "output".into(),
            is_error,
        });

        assert_eq!(panel.messages.len(), 2);
        for msg in &panel.messages {
            assert!(matches!(msg.role, DisplayRole::Tool { status, .. } if status == expected));
        }
    }

    #[test]
    fn webfetch_done_shows_line_count_only() {
        let mut panel = MessagesPanel::new();
        panel.tool_done(ToolDoneEvent {
            id: "t1".into(),
            tool: WEBFETCH_TOOL_NAME,
            content: "line1\nline2\nline3".into(),
            is_error: false,
        });
        assert_eq!(
            panel.messages[0].text,
            format!("[{WEBFETCH_TOOL_NAME} done] (3 lines)")
        );
    }

    #[test]
    fn tool_start_flushes_streaming_text() {
        let mut panel = MessagesPanel::new();
        panel.streaming_text.set_buffer("partial response");

        panel.tool_start(ToolStartEvent {
            id: "t1".into(),
            tool: "read",
            summary: "/tmp/file".into(),
        });

        assert!(panel.streaming_text.is_empty());
        assert_eq!(panel.messages[0].role, DisplayRole::Assistant);
        assert!(matches!(panel.messages[1].role, DisplayRole::Tool { .. }));
    }

    #[test]
    fn thinking_delta_separate_from_text() {
        let mut panel = MessagesPanel::new();
        panel.thinking_delta("reasoning");
        assert_eq!(panel.streaming_thinking, "reasoning");
        assert!(panel.streaming_text.is_empty());

        panel.text_delta("output");
        assert!(panel.streaming_thinking.is_empty());
        assert_eq!(panel.streaming_text, "output");
        assert_eq!(panel.messages[0].role, DisplayRole::Thinking);
        assert_eq!(panel.messages[0].text, "reasoning");
    }

    #[test_case(10, 'u', 0  ; "ctrl_u_saturates_at_zero")]
    #[test_case(20, 'u', 10 ; "ctrl_u_scrolls_up")]
    #[test_case(5,  'd', 15 ; "ctrl_d_scrolls_down")]
    #[test_case(0,  'd', 10 ; "ctrl_d_from_top")]
    fn half_page_scroll(initial: u16, key_char: char, expected: u16) {
        let mut panel = MessagesPanel::new();
        panel.viewport_height = 20;
        panel.scroll_top = initial;
        let half = panel.half_page();
        let delta = if key_char == 'u' { half } else { -half };
        panel.scroll(delta);
        assert_eq!(panel.scroll_top, expected);
    }

    #[test]
    fn scroll_top_clamped_to_content() {
        let mut panel = MessagesPanel::new();
        panel.push(DisplayMessage {
            role: DisplayRole::User,
            text: "short".into(),
        });

        panel.scroll_top = 1000;
        panel.auto_scroll = false;
        let backend = TestBackend::new(80, 24);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal.draw(|f| panel.view(f, f.area())).unwrap();

        assert_eq!(panel.scroll_top, 0);
    }

    #[test]
    fn scroll_up_pins_viewport_during_streaming() {
        let mut panel = MessagesPanel::new();
        panel.streaming_text.set_buffer(&"a\n".repeat(30));

        let backend = TestBackend::new(80, 10);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal.draw(|f| panel.view(f, f.area())).unwrap();

        panel.scroll(1);
        panel.scroll(1);
        terminal.draw(|f| panel.view(f, f.area())).unwrap();
        let pinned = panel.scroll_top;

        panel.text_delta("b\nb\nb\n");
        terminal.draw(|f| panel.view(f, f.area())).unwrap();

        assert!(!panel.auto_scroll);
        assert_eq!(panel.scroll_top, pinned);
    }

    #[test]
    fn ctrl_d_to_bottom_re_enables_auto_scroll() {
        let mut panel = MessagesPanel::new();
        panel.streaming_text.set_buffer(&"a\n".repeat(30));

        let backend = TestBackend::new(80, 10);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal.draw(|f| panel.view(f, f.area())).unwrap();
        assert!(panel.auto_scroll);

        let half = panel.half_page();
        panel.scroll(half);
        terminal.draw(|f| panel.view(f, f.area())).unwrap();
        assert!(!panel.auto_scroll);

        panel.scroll(-half);
        terminal.draw(|f| panel.view(f, f.area())).unwrap();
        assert!(panel.auto_scroll);
    }
}

use std::time::{SystemTime, UNIX_EPOCH};

use crate::text_buffer::TextBuffer;
use crate::theme;

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{
    Block, BorderType, Borders, Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState, Wrap,
};

const MAX_INPUT_LINES: u16 = 10;
const SCROLLBAR_THUMB: &str = "\u{2590}";

const PLACEHOLDER_SUGGESTIONS: &[&str] = &[
    "research how something works",
    "fix a bug",
    "add a feature",
    "add a database migration",
    "create a helm chart",
    "simplify some function",
    "remove trivial comments",
    "analyze data",
    "profile and improve performance",
    "add tests",
    "add benchmarks",
    "refactor a module",
    "remove dead code",
];

pub struct InputBox {
    pub(crate) buffer: TextBuffer,
    history: Vec<String>,
    history_index: Option<usize>,
    draft: String,
    scroll_y: u16,
    placeholder_hint: &'static str,
}

impl InputBox {
    pub fn new() -> Self {
        Self {
            buffer: TextBuffer::new(String::new()),
            history: Vec::new(),
            history_index: None,
            draft: String::new(),
            scroll_y: 0,
            placeholder_hint: random_placeholder_hint(),
        }
    }

    pub fn height(&self, width: u16) -> u16 {
        let content_width = width.saturating_sub(2) as usize;
        let visual_lines = total_visual_lines(&self.buffer, content_width, true);
        (visual_lines as u16).min(MAX_INPUT_LINES) + 2
    }

    pub fn is_at_first_line(&self) -> bool {
        self.buffer.y() == 0
    }

    pub fn is_at_last_line(&self) -> bool {
        self.buffer.y() == self.buffer.line_count() - 1
    }

    pub fn char_before_cursor_is_backslash(&self) -> bool {
        let line = &self.buffer.lines()[self.buffer.y()];
        let x = self.buffer.x();
        x > 0 && line.as_bytes()[x - 1] == b'\\'
    }

    pub fn continue_line(&mut self) {
        self.buffer.remove_char();
        self.buffer.add_line();
    }

    pub fn submit(&mut self) -> Option<String> {
        let text = self.buffer.value().trim().to_string();
        if text.is_empty() {
            return None;
        }
        self.history.push(text.clone());
        self.history_index = None;
        self.draft.clear();
        self.buffer.clear();
        self.scroll_y = 0;
        Some(text)
    }

    fn set_input(&mut self, s: String) {
        self.buffer = TextBuffer::new(s);
        self.buffer.move_to_end();
    }

    pub fn history_up(&mut self) {
        if self.history.is_empty() {
            return;
        }
        let new_index = match self.history_index {
            None => {
                self.draft = self.buffer.value();
                self.history.len() - 1
            }
            Some(0) => return,
            Some(i) => i - 1,
        };
        self.history_index = Some(new_index);
        let entry = self.history[new_index].clone();
        self.set_input(entry);
    }

    pub fn history_down(&mut self) {
        let Some(i) = self.history_index else {
            return;
        };
        if i + 1 < self.history.len() {
            self.history_index = Some(i + 1);
            let entry = self.history[i + 1].clone();
            self.set_input(entry);
        } else {
            self.history_index = None;
            let draft = self.draft.clone();
            self.set_input(draft);
        }
    }

    fn visual_cursor_y(&self, content_width: usize) -> u16 {
        let lines_above: u16 = self
            .buffer
            .lines()
            .iter()
            .take(self.buffer.y())
            .map(|line| visual_line_count(line.len(), content_width) as u16)
            .sum();

        let wrap_row = if content_width == 0 {
            0
        } else {
            (self.buffer.x() / content_width) as u16
        };

        lines_above + wrap_row
    }

    pub fn view(&mut self, frame: &mut Frame, area: Rect) {
        let content_height = area.height.saturating_sub(2);
        let content_width = area.width.saturating_sub(2) as usize;

        let visual_cursor_y = self.visual_cursor_y(content_width);
        if visual_cursor_y < self.scroll_y {
            self.scroll_y = visual_cursor_y;
        } else if visual_cursor_y >= self.scroll_y + content_height {
            self.scroll_y = visual_cursor_y - content_height + 1;
        }

        let total_vl = total_visual_lines(&self.buffer, content_width, true) as u16;
        let max_scroll = total_vl.saturating_sub(content_height);
        self.scroll_y = self.scroll_y.min(max_scroll);

        let is_empty = self.buffer.value().is_empty();
        let styled_lines: Vec<Line> = if is_empty {
            let placeholder_base = Style::new().fg(theme::COMMENT);
            vec![Line::from(vec![
                Span::styled("A", placeholder_base.reversed()),
                Span::styled("sk maki to ", placeholder_base),
                Span::styled(
                    self.placeholder_hint,
                    placeholder_base.add_modifier(ratatui::style::Modifier::ITALIC),
                ),
                Span::styled("...", placeholder_base),
            ])]
        } else {
            self.buffer
                .lines()
                .iter()
                .enumerate()
                .map(|(i, line)| {
                    let mut spans = Vec::new();

                    if i == self.buffer.y() {
                        let x = self.buffer.x();
                        let (before, after) = line.split_at(x.min(line.len()));
                        if after.is_empty() {
                            spans.push(Span::raw(before.to_string()));
                            spans.push(Span::styled(" ", Style::new().reversed()));
                        } else {
                            let mut chars = after.chars();
                            let cursor_char = chars.next().unwrap();
                            spans.push(Span::raw(before.to_string()));
                            spans.push(Span::styled(
                                cursor_char.to_string(),
                                Style::new().reversed(),
                            ));
                            let rest: String = chars.collect();
                            spans.push(Span::raw(rest));
                        }
                    } else {
                        spans.push(Span::raw(line.clone()));
                    }
                    Line::from(spans)
                })
                .collect()
        };

        let text = Text::from(styled_lines);
        let border_style = Style::new().fg(theme::INPUT_BORDER);
        let paragraph = Paragraph::new(text)
            .style(Style::new().fg(theme::FOREGROUND))
            .wrap(Wrap { trim: false })
            .scroll((self.scroll_y, 0))
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_type(BorderType::Rounded)
                    .border_style(border_style),
            );
        frame.render_widget(paragraph, area);

        if max_scroll > 0 {
            let inner = area.inner(ratatui::layout::Margin::new(1, 1));
            let mut state = ScrollbarState::default()
                .content_length(max_scroll as usize + 1)
                .position(self.scroll_y as usize);
            let scrollbar = Scrollbar::new(ScrollbarOrientation::VerticalRight)
                .thumb_symbol(SCROLLBAR_THUMB)
                .track_symbol(None)
                .begin_symbol(None)
                .end_symbol(None);
            frame.render_stateful_widget(scrollbar, inner, &mut state);
        }
    }
}

fn random_placeholder_hint() -> &'static str {
    let idx = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as usize % PLACEHOLDER_SUGGESTIONS.len())
        .unwrap_or(0);
    PLACEHOLDER_SUGGESTIONS[idx]
}

fn total_visual_lines(buffer: &TextBuffer, content_width: usize, cursor_visible: bool) -> usize {
    let cursor_y = buffer.y();
    buffer
        .lines()
        .iter()
        .enumerate()
        .map(|(i, line)| {
            let mut text_len = line.len();
            if cursor_visible && i == cursor_y {
                text_len += 1;
            }
            visual_line_count(text_len, content_width)
        })
        .sum()
}

fn visual_line_count(text_len: usize, width: usize) -> usize {
    if width == 0 {
        return 1;
    }
    text_len.div_ceil(width).max(1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use test_case::test_case;

    fn type_text(input: &mut InputBox, text: &str) {
        for c in text.chars() {
            input.buffer.push_char(c);
        }
    }

    fn submit_text(input: &mut InputBox, text: &str) {
        type_text(input, text);
        input.submit();
    }

    #[test]
    fn submit() {
        let mut input = InputBox::new();
        assert!(input.submit().is_none());

        type_text(&mut input, " ");
        assert!(input.submit().is_none());

        type_text(&mut input, " x ");
        assert_eq!(input.submit().as_deref(), Some("x"));
        assert_eq!(input.buffer.value(), "");

        type_text(&mut input, "line1");
        input.buffer.add_line();
        type_text(&mut input, "line2");
        assert_eq!(input.submit().as_deref(), Some("line1\nline2"));
    }

    #[test]
    fn backslash_continuation() {
        let mut input = InputBox::new();
        type_text(&mut input, "hello\\");
        assert!(input.char_before_cursor_is_backslash());
        input.continue_line();
        assert_eq!(input.buffer.lines(), &["hello", ""]);

        let mut input = InputBox::new();
        type_text(&mut input, "asd\\asd");
        for _ in 0..3 {
            input.buffer.move_left();
        }
        assert!(input.char_before_cursor_is_backslash());
        input.continue_line();
        assert_eq!(input.buffer.lines(), &["asd", "asd"]);
    }

    const TEST_WIDTH: u16 = 80;

    #[test]
    fn height_capped_at_max() {
        let mut input = InputBox::new();
        let base = input.height(TEST_WIDTH);
        for _ in 0..20 {
            input.buffer.add_line();
        }
        assert!(input.height(TEST_WIDTH) > base);
        assert!(input.height(TEST_WIDTH) <= MAX_INPUT_LINES + 2);
    }

    #[test]
    fn first_last_line() {
        let mut input = InputBox::new();
        assert!(input.is_at_first_line());
        assert!(input.is_at_last_line());

        input.buffer.add_line();
        assert!(!input.is_at_first_line());
        assert!(input.is_at_last_line());

        input.buffer.move_up();
        assert!(input.is_at_first_line());
        assert!(!input.is_at_last_line());
    }

    #[test]
    fn history() {
        let mut input = InputBox::new();

        input.history_up();
        input.history_down();
        assert_eq!(input.buffer.value(), "");

        submit_text(&mut input, "a");
        submit_text(&mut input, "b");
        type_text(&mut input, "draft");

        input.history_up();
        assert_eq!(input.buffer.value(), "b");
        input.history_up();
        assert_eq!(input.buffer.value(), "a");
        input.history_up();
        assert_eq!(input.buffer.value(), "a");

        input.history_down();
        assert_eq!(input.buffer.value(), "b");
        input.history_down();
        assert_eq!(input.buffer.value(), "draft");

        input.buffer.clear();
        type_text(&mut input, "line1");
        input.buffer.add_line();
        type_text(&mut input, "line2");
        input.submit();
        input.history_up();
        assert_eq!(input.buffer.value(), "line1\nline2");
        assert!(input.is_at_last_line());
    }

    #[test]
    fn cursor_adds_extra_wrap_row_at_boundary() {
        let content_width: u16 = 12;
        let width = content_width + 2;

        let mut at_boundary = InputBox::new();
        type_text(&mut at_boundary, &"x".repeat(content_width as usize));

        let mut before_boundary = InputBox::new();
        type_text(
            &mut before_boundary,
            &"x".repeat(content_width as usize - 1),
        );

        assert_eq!(
            at_boundary.height(width),
            before_boundary.height(width) + 1,
            "cursor at boundary should cause one extra visual line"
        );
    }

    fn render_input(
        input: &mut InputBox,
        width: u16,
        height: u16,
    ) -> ratatui::Terminal<ratatui::backend::TestBackend> {
        let backend = ratatui::backend::TestBackend::new(width, height);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| {
                let area = Rect::new(0, 0, width, height);
                input.view(frame, area);
            })
            .unwrap();
        terminal
    }

    fn has_scrollbar_thumb(terminal: &ratatui::Terminal<ratatui::backend::TestBackend>) -> bool {
        let buf = terminal.backend().buffer();
        (0..buf.area.height).any(|y| {
            buf.cell((buf.area.width - 2, y))
                .is_some_and(|c| c.symbol() == SCROLLBAR_THUMB)
        })
    }

    #[test_case(20, true  ; "visible_when_content_overflows")]
    #[test_case(0,  false ; "hidden_when_content_fits")]
    fn scrollbar_visibility(extra_lines: usize, expect_visible: bool) {
        let mut input = InputBox::new();
        for _ in 0..extra_lines {
            input.buffer.add_line();
        }
        let terminal = render_input(&mut input, 40, MAX_INPUT_LINES + 2);
        assert_eq!(has_scrollbar_thumb(&terminal), expect_visible);
    }

    #[test]
    fn scroll_clamped_on_content_shrink() {
        let mut input = InputBox::new();
        for _ in 0..20 {
            input.buffer.add_line();
        }
        let area_height = 5_u16;
        let _ = render_input(&mut input, 40, area_height);
        let scroll_before = input.scroll_y;
        assert!(scroll_before > 0);

        input.buffer = TextBuffer::new("short".into());
        let _ = render_input(&mut input, 40, area_height);
        assert_eq!(input.scroll_y, 0);
    }
}

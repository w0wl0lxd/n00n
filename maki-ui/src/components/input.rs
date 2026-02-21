use std::time::{SystemTime, UNIX_EPOCH};

use crate::text_buffer::TextBuffer;
use crate::theme;

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, BorderType, Borders, Paragraph, Wrap};

const MAX_INPUT_LINES: u16 = 10;
const CONTINUATION_PREFIX: &str = "  ";
const PROMPT_INDICATOR: &str = "> ";
const STREAMING_INDICATOR: &str = "...";

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

    pub fn height(&self, width: u16, is_streaming: bool) -> u16 {
        let content_width = width.saturating_sub(2) as usize;
        let indicator_len = indicator(is_streaming).len();
        let visual_lines: usize = self
            .buffer
            .lines()
            .iter()
            .enumerate()
            .map(|(i, line)| {
                visual_line_count(line.len() + prefix_len(i, indicator_len), content_width)
            })
            .sum();
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

    fn visual_cursor_y(&self, indicator_len: usize, content_width: usize) -> u16 {
        let lines_above: u16 = self
            .buffer
            .lines()
            .iter()
            .enumerate()
            .take(self.buffer.y())
            .map(|(i, line)| {
                visual_line_count(line.len() + prefix_len(i, indicator_len), content_width) as u16
            })
            .sum();

        let cursor_col = self.buffer.x() + prefix_len(self.buffer.y(), indicator_len);
        let wrap_row = if content_width == 0 {
            0
        } else {
            (cursor_col / content_width) as u16
        };

        lines_above + wrap_row
    }

    pub fn view(&mut self, frame: &mut Frame, area: Rect, is_streaming: bool) {
        let ind = indicator(is_streaming);
        let content_height = area.height.saturating_sub(2);
        let content_width = area.width.saturating_sub(2) as usize;

        let visual_cursor_y = self.visual_cursor_y(ind.len(), content_width);
        if visual_cursor_y < self.scroll_y {
            self.scroll_y = visual_cursor_y;
        } else if visual_cursor_y >= self.scroll_y + content_height {
            self.scroll_y = visual_cursor_y - content_height + 1;
        }

        let is_empty = self.buffer.value().is_empty();
        let styled_lines: Vec<Line> = if is_empty && !is_streaming {
            vec![Line::from(vec![
                Span::raw(ind.to_string()),
                Span::styled("Ask maki to ", Style::new().fg(theme::COMMENT)),
                Span::styled(
                    self.placeholder_hint,
                    Style::new()
                        .fg(theme::COMMENT)
                        .add_modifier(ratatui::style::Modifier::ITALIC),
                ),
                Span::styled("...", Style::new().fg(theme::COMMENT)),
            ])]
        } else {
            self.buffer
                .lines()
                .iter()
                .enumerate()
                .map(|(i, line)| {
                    let prefix = if i == 0 { ind } else { CONTINUATION_PREFIX };
                    let mut spans = vec![Span::raw(prefix.to_string())];

                    if !is_streaming && i == self.buffer.y() {
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
    }
}

fn indicator(is_streaming: bool) -> &'static str {
    if is_streaming {
        STREAMING_INDICATOR
    } else {
        PROMPT_INDICATOR
    }
}

fn random_placeholder_hint() -> &'static str {
    let idx = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as usize % PLACEHOLDER_SUGGESTIONS.len())
        .unwrap_or(0);
    PLACEHOLDER_SUGGESTIONS[idx]
}

fn prefix_len(line_index: usize, indicator_len: usize) -> usize {
    if line_index == 0 {
        indicator_len
    } else {
        CONTINUATION_PREFIX.len()
    }
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
        let base = input.height(TEST_WIDTH, false);
        for _ in 0..20 {
            input.buffer.add_line();
        }
        assert!(input.height(TEST_WIDTH, false) > base);
        assert!(input.height(TEST_WIDTH, false) <= MAX_INPUT_LINES + 2);
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
}

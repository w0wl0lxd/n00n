use crate::text_buffer::TextBuffer;
use crate::theme;

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, BorderType, Borders, Paragraph};

const MAX_INPUT_LINES: u16 = 10;

pub struct InputBox {
    pub(crate) buffer: TextBuffer,
    history: Vec<String>,
    history_index: Option<usize>,
    draft: String,
    scroll_y: u16,
}

impl InputBox {
    pub fn new() -> Self {
        Self {
            buffer: TextBuffer::new(String::new()),
            history: Vec::new(),
            history_index: None,
            draft: String::new(),
            scroll_y: 0,
        }
    }

    pub fn height(&self) -> u16 {
        (self.buffer.line_count() as u16).min(MAX_INPUT_LINES) + 2
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

    pub fn view(&mut self, frame: &mut Frame, area: Rect, is_streaming: bool) {
        let indicator = if is_streaming { "..." } else { "> " };
        let content_height = area.height.saturating_sub(2);
        let cursor_y = self.buffer.y() as u16;

        if cursor_y < self.scroll_y {
            self.scroll_y = cursor_y;
        } else if cursor_y >= self.scroll_y + content_height {
            self.scroll_y = cursor_y - content_height + 1;
        }

        let styled_lines: Vec<Line> = self
            .buffer
            .lines()
            .iter()
            .enumerate()
            .map(|(i, line)| {
                let prefix = if i == 0 { indicator } else { "  " };
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
            .collect();

        let text = Text::from(styled_lines);
        let border_style = Style::new().fg(theme::INPUT_BORDER);
        let paragraph = Paragraph::new(text)
            .style(Style::new().fg(theme::FOREGROUND))
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

        // multiline
        type_text(&mut input, "line1");
        input.buffer.add_line();
        type_text(&mut input, "line2");
        assert_eq!(input.submit().as_deref(), Some("line1\nline2"));
    }

    #[test]
    fn backslash_continuation() {
        // at end of line: cursor is after backslash
        let mut input = InputBox::new();
        type_text(&mut input, "hello\\");
        assert!(input.char_before_cursor_is_backslash());
        input.continue_line();
        assert_eq!(input.buffer.lines(), &["hello", ""]);

        // mid-line: cursor right after backslash
        let mut input = InputBox::new();
        type_text(&mut input, "asd\\asd");
        for _ in 0..3 {
            input.buffer.move_left();
        }
        assert!(input.char_before_cursor_is_backslash());
        input.continue_line();
        assert_eq!(input.buffer.lines(), &["asd", "asd"]);
    }

    #[test]
    fn height_capped_at_max() {
        let mut input = InputBox::new();
        let base = input.height();
        for _ in 0..20 {
            input.buffer.add_line();
        }
        assert!(input.height() > base);
        assert!(input.height() <= MAX_INPUT_LINES + 2);
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

        // noop on empty history
        input.history_up();
        input.history_down();
        assert_eq!(input.buffer.value(), "");

        submit_text(&mut input, "a");
        submit_text(&mut input, "b");
        type_text(&mut input, "draft");

        // navigate up through history, clamps at oldest
        input.history_up();
        assert_eq!(input.buffer.value(), "b");
        input.history_up();
        assert_eq!(input.buffer.value(), "a");
        input.history_up();
        assert_eq!(input.buffer.value(), "a");

        // navigate back down, restores draft
        input.history_down();
        assert_eq!(input.buffer.value(), "b");
        input.history_down();
        assert_eq!(input.buffer.value(), "draft");

        // multiline content survives history roundtrip
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

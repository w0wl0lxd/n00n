use crate::theme;

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::widgets::{Block, Borders, Paragraph};
use unicode_width::UnicodeWidthStr;

pub struct InputBox {
    input: String,
    /// Cursor position as a character index (not byte offset)
    cursor_pos: usize,
}

impl InputBox {
    pub fn new() -> Self {
        Self {
            input: String::new(),
            cursor_pos: 0,
        }
    }

    fn byte_offset(&self) -> usize {
        self.input
            .char_indices()
            .nth(self.cursor_pos)
            .map_or(self.input.len(), |(i, _)| i)
    }

    pub fn insert_char(&mut self, c: char) {
        self.input.insert(self.byte_offset(), c);
        self.cursor_pos += 1;
    }

    pub fn backspace(&mut self) {
        if self.cursor_pos > 0 {
            self.cursor_pos -= 1;
            let offset = self.byte_offset();
            let ch = self.input[offset..].chars().next().unwrap();
            self.input.replace_range(offset..offset + ch.len_utf8(), "");
        }
    }

    pub fn move_left(&mut self) {
        self.cursor_pos = self.cursor_pos.saturating_sub(1);
    }

    pub fn move_right(&mut self) {
        let char_count = self.input.chars().count();
        self.cursor_pos = (self.cursor_pos + 1).min(char_count);
    }

    pub fn submit(&mut self) -> Option<String> {
        let text = self.input.trim().to_string();
        if text.is_empty() {
            return None;
        }
        self.input.clear();
        self.cursor_pos = 0;
        Some(text)
    }

    pub fn view(&self, frame: &mut Frame, area: Rect, is_streaming: bool) {
        let indicator = if is_streaming { "..." } else { "> " };
        let input_text = format!("{indicator}{}", self.input);
        let border_style = Style::new().fg(theme::INPUT_BORDER);
        let paragraph = Paragraph::new(input_text)
            .style(Style::new().fg(theme::FOREGROUND))
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_style(border_style),
            );
        frame.render_widget(paragraph, area);

        if !is_streaming {
            let text_before_cursor = &self.input[..self.byte_offset()];
            let display_width = text_before_cursor.width() as u16;
            let cursor_x = area.x + 1 + indicator.len() as u16 + display_width;
            let cursor_y = area.y + 1;
            frame.set_cursor_position((cursor_x, cursor_y));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backspace_and_cursor_movement() {
        let mut input = InputBox::new();
        input.insert_char('a');
        input.insert_char('b');
        input.insert_char('c');
        assert_eq!(input.input, "abc");

        input.move_left();
        assert_eq!(input.cursor_pos, 2);

        input.backspace();
        assert_eq!(input.input, "ac");
        assert_eq!(input.cursor_pos, 1);
    }

    #[test]
    fn submit_returns_trimmed_and_clears() {
        let mut input = InputBox::new();
        input.insert_char(' ');
        input.insert_char('x');
        input.insert_char(' ');

        let result = input.submit();
        assert_eq!(result.as_deref(), Some("x"));
        assert!(input.input.is_empty());
        assert_eq!(input.cursor_pos, 0);
    }

    #[test]
    fn submit_empty_returns_none() {
        let mut input = InputBox::new();
        assert!(input.submit().is_none());

        input.insert_char(' ');
        assert!(input.submit().is_none());
    }

    #[test]
    fn multibyte_insert_move_backspace() {
        let mut input = InputBox::new();
        for c in "café🎉".chars() {
            input.insert_char(c);
        }
        assert_eq!(input.input, "café🎉");

        // move back past emoji and 'é', insert in the middle
        input.move_left();
        input.move_left();
        input.insert_char('X');
        assert_eq!(input.input, "cafXé🎉");

        // backspace the multi-byte 'é' after it
        input.move_right();
        input.backspace();
        assert_eq!(input.input, "cafX🎉");

        // move_right clamps at end
        input.move_right();
        input.move_right();
        input.move_right();
        assert_eq!(input.cursor_pos, input.input.chars().count());
    }
}

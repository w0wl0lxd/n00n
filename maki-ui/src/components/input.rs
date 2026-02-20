use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::widgets::{Block, Borders, Paragraph};

pub struct InputBox {
    input: String,
    cursor_pos: usize,
}

impl InputBox {
    pub fn new() -> Self {
        Self {
            input: String::new(),
            cursor_pos: 0,
        }
    }

    pub fn insert_char(&mut self, c: char) {
        self.input.insert(self.cursor_pos, c);
        self.cursor_pos += 1;
    }

    pub fn backspace(&mut self) {
        if self.cursor_pos > 0 {
            self.cursor_pos -= 1;
            self.input.remove(self.cursor_pos);
        }
    }

    pub fn move_left(&mut self) {
        self.cursor_pos = self.cursor_pos.saturating_sub(1);
    }

    pub fn move_right(&mut self) {
        self.cursor_pos = (self.cursor_pos + 1).min(self.input.len());
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
        let paragraph = Paragraph::new(input_text).block(Block::default().borders(Borders::ALL));
        frame.render_widget(paragraph, area);

        if !is_streaming {
            let cursor_x = area.x + 1 + indicator.len() as u16 + self.cursor_pos as u16;
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
}

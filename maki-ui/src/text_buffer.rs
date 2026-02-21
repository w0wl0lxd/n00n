pub struct TextBuffer {
    lines: Vec<String>,
    raw_x: usize,
    cursor_y: usize,
}

impl TextBuffer {
    pub fn new(input: String) -> Self {
        let lines: Vec<String> = input.split('\n').map(str::to_string).collect();
        Self {
            lines,
            raw_x: 0,
            cursor_y: 0,
        }
    }

    pub fn value(&self) -> String {
        self.lines.join("\n")
    }

    pub fn lines(&self) -> &[String] {
        &self.lines
    }

    pub fn x(&self) -> usize {
        self.raw_x.min(self.current_line_len())
    }

    pub fn y(&self) -> usize {
        self.cursor_y
    }

    pub fn line_count(&self) -> usize {
        self.lines.len()
    }

    fn current_line_len(&self) -> usize {
        self.lines[self.cursor_y].len()
    }

    pub fn push_char(&mut self, c: char) {
        let x = self.x();
        self.lines[self.cursor_y].insert(x, c);
        self.raw_x = x + 1;
    }

    pub fn insert_text(&mut self, text: &str) {
        for (i, chunk) in text.split('\n').enumerate() {
            if i > 0 {
                self.add_line();
            }
            if !chunk.is_empty() {
                let x = self.x();
                self.lines[self.cursor_y].insert_str(x, chunk);
                self.raw_x = x + chunk.len();
            }
        }
    }

    pub fn add_line(&mut self) {
        let x = self.x();
        let (left, right) = self.lines[self.cursor_y].split_at(x);
        let (left, right) = (left.to_string(), right.to_string());
        self.lines[self.cursor_y] = left;
        self.lines.insert(self.cursor_y + 1, right);
        self.raw_x = 0;
        self.cursor_y += 1;
    }

    pub fn remove_char(&mut self) {
        let x = self.x();
        if x == 0 {
            self.merge_with_previous_line();
        } else {
            self.lines[self.cursor_y].remove(x - 1);
            self.raw_x = x - 1;
        }
    }

    pub fn delete_char(&mut self) {
        let x = self.x();
        if x == self.current_line_len() {
            if self.cursor_y + 1 < self.lines.len() {
                let next_line = self.lines.remove(self.cursor_y + 1);
                self.lines[self.cursor_y].push_str(&next_line);
            }
        } else {
            self.lines[self.cursor_y].remove(x);
        }
    }

    fn wrap_to_prev_line(&mut self) -> bool {
        if self.cursor_y > 0 {
            self.cursor_y -= 1;
            self.raw_x = self.lines[self.cursor_y].len();
            true
        } else {
            false
        }
    }

    fn wrap_to_next_line(&mut self) -> bool {
        if self.cursor_y < self.lines.len() - 1 {
            self.cursor_y += 1;
            self.raw_x = 0;
            true
        } else {
            false
        }
    }

    fn find_prev_word_boundary(&self, x: usize) -> usize {
        let line = &self.lines[self.cursor_y];
        let mut new_x = x;
        while new_x > 0 && line.as_bytes()[new_x - 1].is_ascii_whitespace() {
            new_x -= 1;
        }
        while new_x > 0 && !line.as_bytes()[new_x - 1].is_ascii_whitespace() {
            new_x -= 1;
        }
        new_x
    }

    fn find_next_word_boundary(&self, x: usize) -> usize {
        let line = &self.lines[self.cursor_y];
        let len = line.len();
        let mut new_x = x;
        while new_x < len && line.as_bytes()[new_x].is_ascii_whitespace() {
            new_x += 1;
        }
        while new_x < len && !line.as_bytes()[new_x].is_ascii_whitespace() {
            new_x += 1;
        }
        new_x
    }

    pub fn remove_word_before_cursor(&mut self) {
        let x = self.x();
        if x == 0 {
            self.merge_with_previous_line();
            return;
        }
        let new_x = self.find_prev_word_boundary(x);
        self.lines[self.cursor_y].replace_range(new_x..x, "");
        self.raw_x = new_x;
    }

    pub fn move_word_left(&mut self) {
        let x = self.x();
        if x == 0 {
            self.wrap_to_prev_line();
            return;
        }
        self.raw_x = self.find_prev_word_boundary(x);
    }

    pub fn move_word_right(&mut self) {
        let x = self.x();
        if x == self.current_line_len() {
            self.wrap_to_next_line();
            return;
        }
        self.raw_x = self.find_next_word_boundary(x);
    }

    pub fn move_left(&mut self) {
        let x = self.x();
        if x > 0 {
            self.raw_x = x - 1;
        } else {
            self.wrap_to_prev_line();
        }
    }

    pub fn move_right(&mut self) {
        let x = self.x();
        if x < self.current_line_len() {
            self.raw_x = x + 1;
        } else {
            self.wrap_to_next_line();
        }
    }

    pub fn move_up(&mut self) {
        if self.cursor_y > 0 {
            self.cursor_y -= 1;
        }
    }

    pub fn move_down(&mut self) {
        if self.cursor_y < self.lines.len() - 1 {
            self.cursor_y += 1;
        }
    }

    pub fn move_home(&mut self) {
        self.raw_x = 0;
    }

    pub fn move_end(&mut self) {
        self.raw_x = self.current_line_len();
    }

    pub fn clear(&mut self) {
        self.lines = vec![String::new()];
        self.raw_x = 0;
        self.cursor_y = 0;
    }

    pub fn move_to_end(&mut self) {
        self.cursor_y = self.lines.len() - 1;
        self.raw_x = self.current_line_len();
    }

    fn merge_with_previous_line(&mut self) {
        if self.cursor_y == 0 {
            return;
        }
        self.raw_x = self.lines[self.cursor_y - 1].len();
        let line = self.lines.remove(self.cursor_y);
        self.lines[self.cursor_y - 1].push_str(&line);
        self.cursor_y -= 1;
    }
}

#[cfg(test)]
mod tests {
    use super::TextBuffer;

    #[test]
    fn insert_at_middle() {
        let mut buf = TextBuffer::new(String::new());
        buf.push_char('a');
        buf.push_char('c');
        buf.raw_x = 1;
        buf.push_char('b');
        assert_eq!(buf.value(), "abc");
    }

    #[test]
    fn split_then_merge_is_identity() {
        let mut buf = TextBuffer::new("abcd".into());
        buf.raw_x = 2;
        buf.add_line();
        assert_eq!(buf.lines(), &["ab", "cd"]);

        buf.remove_char();
        assert_eq!(buf.value(), "abcd");
    }

    #[test]
    fn delete_char_merges_lines() {
        let mut buf = TextBuffer::new("ab\ncd".into());
        buf.raw_x = 2;
        buf.delete_char();
        assert_eq!(buf.value(), "abcd");
    }

    #[test]
    fn cursor_wraps_across_lines() {
        let mut buf = TextBuffer::new("ab\ncd".into());
        buf.raw_x = 2;
        buf.move_right();
        assert_eq!((buf.y(), buf.x()), (1, 0));

        buf.move_left();
        assert_eq!((buf.y(), buf.x()), (0, 2));
    }

    #[test]
    fn sticky_x_across_short_lines() {
        let mut buf = TextBuffer::new("long\nhi\nlong".into());
        buf.raw_x = 4;
        buf.move_down();
        assert_eq!(buf.x(), 2);
        buf.move_down();
        assert_eq!(buf.x(), 4);
    }

    #[test]
    fn insert_text_multiline() {
        let mut buf = TextBuffer::new(String::new());
        buf.insert_text("line1\nline2\nline3");
        assert_eq!(buf.lines(), &["line1", "line2", "line3"]);
        assert_eq!(buf.y(), 2);
        assert_eq!(buf.x(), 5);
    }

    #[test]
    fn insert_text_at_cursor_middle() {
        let mut buf = TextBuffer::new("abcd".into());
        buf.raw_x = 2;
        buf.insert_text("X\nY");
        assert_eq!(buf.lines(), &["abX", "Ycd"]);
    }

    #[test]
    fn remove_word() {
        let mut buf = TextBuffer::new("hello world".into());
        buf.raw_x = 11;
        buf.remove_word_before_cursor();
        assert_eq!(buf.value(), "hello ");

        buf.remove_word_before_cursor();
        assert_eq!(buf.value(), "");

        // at line start, merges with previous line
        let mut buf = TextBuffer::new("ab\ncd".into());
        buf.cursor_y = 1;
        buf.raw_x = 0;
        buf.remove_word_before_cursor();
        assert_eq!(buf.value(), "abcd");
    }

    #[test]
    fn move_word_left() {
        let mut buf = TextBuffer::new("hello world".into());
        buf.move_to_end();
        buf.move_word_left();
        assert_eq!(buf.x(), 6);
        buf.move_word_left();
        assert_eq!(buf.x(), 0);

        // leading whitespace - same algorithm, just starts at space
        let mut buf = TextBuffer::new("  hello".into());
        buf.move_to_end();
        buf.move_word_left();
        assert_eq!(buf.x(), 2);
    }

    #[test]
    fn move_word_left_wraps_line() {
        let mut buf = TextBuffer::new("ab\ncd".into());
        buf.cursor_y = 1;
        buf.raw_x = 0;
        buf.move_word_left();
        assert_eq!((buf.y(), buf.x()), (0, 2));
    }

    #[test]
    fn move_word_right() {
        let mut buf = TextBuffer::new("hello world".into());
        buf.move_word_right();
        assert_eq!(buf.x(), 5);
        buf.move_word_right();
        assert_eq!(buf.x(), 11);

        // trailing whitespace - same algorithm, ends at space
        let mut buf = TextBuffer::new("hello  ".into());
        buf.move_word_right();
        assert_eq!(buf.x(), 5);
    }

    #[test]
    fn move_word_right_wraps_line() {
        let mut buf = TextBuffer::new("ab\ncd".into());
        buf.raw_x = 2;
        buf.move_word_right();
        assert_eq!((buf.y(), buf.x()), (1, 0));
    }
}

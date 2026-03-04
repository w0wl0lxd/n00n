use crate::components::is_ctrl;
use crate::components::scrollbar::render_vertical_scrollbar;
use crate::selection::inset_border;
use crate::text_buffer::TextBuffer;
use crate::theme;

use crossterm::event::{KeyCode, KeyEvent};
use ratatui::Frame;
use ratatui::layout::{Constraint, Flex, Layout, Position, Rect};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Clear, Paragraph};

pub enum ChatPickerAction {
    Consumed,
    Select(usize),
}

struct State {
    selected: usize,
    original_chat: usize,
    search: TextBuffer,
    scroll_offset: usize,
    viewport_height: usize,
    inner_area: Rect,
}

const NO_MATCHES: &str = "No matches";
const SEARCH_PREFIX: &str = "> ";
const TITLE: &str = " Chats ";
const MIN_WIDTH_PERCENT: u16 = 60;
const MAX_HEIGHT_PERCENT: u16 = 80;
const CHROME_LINES: u16 = 3;

pub struct ChatPicker {
    state: Option<State>,
}

impl ChatPicker {
    pub fn new() -> Self {
        Self { state: None }
    }

    pub fn open(&mut self, active_chat: usize, chat_names: &[String]) {
        let filtered = filter_names("", chat_names);
        let selected = filtered.iter().position(|&i| i == active_chat).unwrap_or(0);
        self.state = Some(State {
            selected,
            original_chat: active_chat,
            search: TextBuffer::new(String::new()),
            scroll_offset: 0,
            viewport_height: 20,
            inner_area: Rect::default(),
        });
    }

    pub fn is_open(&self) -> bool {
        self.state.is_some()
    }

    pub fn handle_key(&mut self, key: KeyEvent, chat_names: &[String]) -> ChatPickerAction {
        let s = match self.state.as_mut() {
            Some(s) => s,
            None => return ChatPickerAction::Consumed,
        };

        if is_ctrl(&key) {
            return match key.code {
                KeyCode::Char('c') => {
                    let orig = s.original_chat;
                    self.state = None;
                    ChatPickerAction::Select(orig)
                }
                KeyCode::Char('w') => {
                    s.search.remove_word_before_cursor();
                    s.clamp_selection(chat_names);
                    ChatPickerAction::Consumed
                }
                _ => ChatPickerAction::Consumed,
            };
        }
        match key.code {
            KeyCode::Up => {
                s.move_up(chat_names);
                ChatPickerAction::Consumed
            }
            KeyCode::Down => {
                s.move_down(chat_names);
                ChatPickerAction::Consumed
            }
            KeyCode::Enter => {
                if let Some(idx) = s.selected_chat(chat_names) {
                    self.state = None;
                    ChatPickerAction::Select(idx)
                } else {
                    ChatPickerAction::Consumed
                }
            }
            KeyCode::Esc => {
                let orig = s.original_chat;
                self.state = None;
                ChatPickerAction::Select(orig)
            }
            KeyCode::Char(c) => {
                s.search.push_char(c);
                s.clamp_selection(chat_names);
                ChatPickerAction::Consumed
            }
            KeyCode::Backspace => {
                s.search.remove_char();
                s.clamp_selection(chat_names);
                ChatPickerAction::Consumed
            }
            KeyCode::Left => {
                s.search.move_left();
                ChatPickerAction::Consumed
            }
            KeyCode::Right => {
                s.search.move_right();
                ChatPickerAction::Consumed
            }
            KeyCode::Home => {
                s.search.move_home();
                ChatPickerAction::Consumed
            }
            KeyCode::End => {
                s.search.move_end();
                ChatPickerAction::Consumed
            }
            _ => ChatPickerAction::Consumed,
        }
    }

    pub fn selected_chat(&self, chat_names: &[String]) -> Option<usize> {
        let s = self.state.as_ref()?;
        s.selected_chat(chat_names)
    }

    pub fn view(&mut self, frame: &mut Frame, area: Rect, chat_names: &[String]) -> Option<Rect> {
        let s = self.state.as_mut()?;

        let filtered = s.filter(chat_names);
        let max_h = (area.height as u32 * MAX_HEIGHT_PERCENT as u32 / 100) as u16;
        let content_rows = if filtered.is_empty() {
            1
        } else {
            filtered.len() as u16
        };
        let total_h = (content_rows + CHROME_LINES)
            .min(max_h)
            .max(CHROME_LINES + 1);
        let viewport_h = total_h.saturating_sub(CHROME_LINES);
        s.viewport_height = viewport_h as usize;

        let [popup] = Layout::vertical([Constraint::Length(total_h)])
            .flex(Flex::Center)
            .areas(area);
        let [popup] = Layout::horizontal([Constraint::Percentage(MIN_WIDTH_PERCENT)])
            .flex(Flex::Center)
            .areas(popup);

        frame.render_widget(Clear, popup);

        let block = Block::bordered()
            .border_type(BorderType::Rounded)
            .border_style(theme::PICKER_BORDER)
            .title(TITLE)
            .title_style(theme::PICKER_TITLE)
            .style(Style::new().bg(theme::BACKGROUND));

        let inner = block.inner(popup);
        frame.render_widget(block, popup);

        let [list_area, search_area] =
            Layout::vertical([Constraint::Min(1), Constraint::Length(1)]).areas(inner);

        render_list(frame, list_area, &filtered, chat_names, s);
        render_search(frame, search_area, s);

        if filtered.len() as u16 > viewport_h {
            render_vertical_scrollbar(
                frame,
                list_area,
                filtered.len() as u16,
                s.scroll_offset as u16,
            );
        }

        s.inner_area = inset_border(popup);
        Some(s.inner_area)
    }

    pub fn close(&mut self) {
        self.state = None;
    }

    pub fn contains(&self, pos: Position) -> bool {
        self.state
            .as_ref()
            .is_some_and(|s| s.inner_area.contains(pos))
    }

    pub fn inner_area(&self) -> Option<Rect> {
        self.state.as_ref().map(|s| s.inner_area)
    }

    pub fn scroll(&mut self, delta: i32, chat_names: &[String]) {
        let s = match self.state.as_mut() {
            Some(s) => s,
            None => return,
        };
        let filtered_len = s.filter(chat_names).len();
        let max_offset = filtered_len.saturating_sub(s.viewport_height);
        if delta > 0 {
            s.scroll_offset = s.scroll_offset.saturating_sub(delta as usize);
        } else {
            s.scroll_offset = (s.scroll_offset + delta.unsigned_abs() as usize).min(max_offset);
        }
    }
}

impl State {
    fn selected_chat(&self, chat_names: &[String]) -> Option<usize> {
        let filtered = self.filter(chat_names);
        filtered.get(self.selected).copied()
    }

    fn filter(&self, chat_names: &[String]) -> Vec<usize> {
        filter_names(self.search.value().as_str(), chat_names)
    }

    fn clamp_selection(&mut self, chat_names: &[String]) {
        let filtered = self.filter(chat_names);
        if filtered.is_empty() {
            self.selected = 0;
            self.scroll_offset = 0;
        } else {
            self.selected = self.selected.min(filtered.len() - 1);
            self.scroll_offset = self.scroll_offset.min(self.selected);
        }
    }

    fn move_up(&mut self, chat_names: &[String]) {
        let filtered = self.filter(chat_names);
        if filtered.is_empty() {
            return;
        }
        self.selected = if self.selected == 0 {
            filtered.len() - 1
        } else {
            self.selected - 1
        };
        self.ensure_visible();
    }

    fn move_down(&mut self, chat_names: &[String]) {
        let filtered = self.filter(chat_names);
        if filtered.is_empty() {
            return;
        }
        self.selected = if self.selected == filtered.len() - 1 {
            0
        } else {
            self.selected + 1
        };
        self.ensure_visible();
    }

    fn ensure_visible(&mut self) {
        if self.selected < self.scroll_offset {
            self.scroll_offset = self.selected;
        }
        if self.selected >= self.scroll_offset + self.viewport_height {
            self.scroll_offset = self.selected - self.viewport_height + 1;
        }
    }
}

fn filter_names(query: &str, chat_names: &[String]) -> Vec<usize> {
    if query.is_empty() {
        return (0..chat_names.len()).collect();
    }
    let query_lower = query.to_ascii_lowercase();
    chat_names
        .iter()
        .enumerate()
        .filter(|(_, name)| name.to_ascii_lowercase().contains(&query_lower))
        .map(|(i, _)| i)
        .collect()
}

fn render_list(
    frame: &mut Frame,
    area: Rect,
    filtered: &[usize],
    chat_names: &[String],
    s: &State,
) {
    if filtered.is_empty() {
        let line = Line::from(Span::styled(NO_MATCHES, theme::CMD_DESC));
        frame.render_widget(Paragraph::new(vec![line]), area);
        return;
    }

    let end = (s.scroll_offset + s.viewport_height).min(filtered.len());
    let visible = &filtered[s.scroll_offset..end];

    let lines: Vec<Line> = visible
        .iter()
        .enumerate()
        .map(|(vi, &chat_idx)| {
            let abs_idx = s.scroll_offset + vi;
            let name = &chat_names[chat_idx];
            let style = if abs_idx == s.selected {
                theme::CMD_SELECTED
            } else {
                theme::CMD_NAME
            };
            Line::from(Span::styled(format!("  {name}"), style))
        })
        .collect();

    frame.render_widget(Paragraph::new(lines), area);
}

fn render_search(frame: &mut Frame, area: Rect, s: &State) {
    let query = s.search.value();
    let cursor_x = s.search.x();
    let chars: Vec<char> = query.chars().collect();
    let before: String = chars[..cursor_x].iter().collect();
    let cursor_char = chars.get(cursor_x).copied().unwrap_or(' ');
    let after_start = cursor_x.saturating_add(1).min(chars.len());
    let after: String = chars[after_start..].iter().collect();

    let line = Line::from(vec![
        Span::styled(SEARCH_PREFIX, theme::PICKER_SEARCH_PREFIX),
        Span::styled(before, theme::FOREGROUND_STYLE),
        Span::styled(cursor_char.to_string(), theme::CURSOR),
        Span::styled(after, theme::FOREGROUND_STYLE),
    ]);
    frame.render_widget(Paragraph::new(vec![line]), area);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::components::{ctrl, key};
    use crossterm::event::KeyCode;
    use test_case::test_case;

    fn names(n: &[&str]) -> Vec<String> {
        n.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn open_sets_initial_state() {
        let mut p = ChatPicker::new();
        let chat_names = names(&["Main", "Explore config", "Run tests"]);
        p.open(1, &chat_names);
        assert!(p.is_open());
        let s = p.state.as_ref().unwrap();
        assert_eq!(s.original_chat, 1);
        assert_eq!(s.selected, 1);
        assert_eq!(s.search.value(), "");
    }

    #[test]
    fn up_down_wraps() {
        let mut p = ChatPicker::new();
        let chat_names = names(&["Main", "Explore config", "Run tests"]);
        p.open(0, &chat_names);

        p.handle_key(key(KeyCode::Up), &chat_names);
        assert_eq!(p.state.as_ref().unwrap().selected, 2);

        p.handle_key(key(KeyCode::Down), &chat_names);
        assert_eq!(p.state.as_ref().unwrap().selected, 0);
    }

    #[test]
    fn enter_confirms_selected() {
        let mut p = ChatPicker::new();
        let chat_names = names(&["Main", "Explore config", "Run tests"]);
        p.open(0, &chat_names);
        p.handle_key(key(KeyCode::Down), &chat_names);

        let action = p.handle_key(key(KeyCode::Enter), &chat_names);
        assert!(matches!(action, ChatPickerAction::Select(1)));
        assert!(!p.is_open());
    }

    #[test_case(key(KeyCode::Esc) ; "escape_returns_original")]
    #[test_case(ctrl('c') ; "ctrl_c_returns_original")]
    fn cancel_returns_original(cancel_key: KeyEvent) {
        let mut p = ChatPicker::new();
        let chat_names = names(&["Main", "Explore config"]);
        p.open(0, &chat_names);
        p.handle_key(key(KeyCode::Down), &chat_names);

        let action = p.handle_key(cancel_key, &chat_names);
        assert!(matches!(action, ChatPickerAction::Select(0)));
        assert!(!p.is_open());
    }

    #[test]
    fn typing_filters_by_substring() {
        let mut p = ChatPicker::new();
        let chat_names = names(&["Main", "Explore config", "Run tests"]);
        p.open(0, &chat_names);

        p.handle_key(key(KeyCode::Char('E')), &chat_names);
        let filtered = p.state.as_ref().unwrap().filter(&chat_names);
        assert_eq!(filtered, vec![1, 2]);
    }

    #[test]
    fn no_matches_enter_consumed() {
        let mut p = ChatPicker::new();
        let chat_names = names(&["Main"]);
        p.open(0, &chat_names);
        p.handle_key(key(KeyCode::Char('z')), &chat_names);

        let action = p.handle_key(key(KeyCode::Enter), &chat_names);
        assert!(matches!(action, ChatPickerAction::Consumed));
        assert!(p.is_open());
    }

    #[test]
    fn selected_clamped_on_filter_shrink() {
        let mut p = ChatPicker::new();
        let chat_names = names(&["Main", "Explore config", "Run tests"]);
        p.open(0, &chat_names);
        p.handle_key(key(KeyCode::Down), &chat_names);
        p.handle_key(key(KeyCode::Down), &chat_names);
        assert_eq!(p.state.as_ref().unwrap().selected, 2);

        p.handle_key(key(KeyCode::Char('M')), &chat_names);
        assert_eq!(p.state.as_ref().unwrap().selected, 0);
    }

    #[test]
    fn ctrl_w_deletes_word() {
        let mut p = ChatPicker::new();
        let chat_names = names(&["Main", "Explore config"]);
        p.open(0, &chat_names);
        p.handle_key(key(KeyCode::Char('h')), &chat_names);
        p.handle_key(key(KeyCode::Char('i')), &chat_names);
        assert_eq!(p.state.as_ref().unwrap().search.value(), "hi");

        p.handle_key(ctrl('w'), &chat_names);
        assert_eq!(p.state.as_ref().unwrap().search.value(), "");
    }

    fn many_names(n: usize) -> Vec<String> {
        (0..n).map(|i| format!("Chat {i}")).collect()
    }

    #[test_case(0, -3, 3  ; "scroll_down")]
    #[test_case(0, 100, 0  ; "clamp_at_top")]
    #[test_case(5, 3, 2    ; "scroll_up")]
    #[test_case(0, -100, 20 ; "clamp_at_bottom")]
    fn scroll_offset(initial: usize, delta: i32, expected: usize) {
        let mut p = ChatPicker::new();
        let chat_names = many_names(30);
        p.open(0, &chat_names);
        let s = p.state.as_mut().unwrap();
        s.viewport_height = 10;
        s.scroll_offset = initial;

        p.scroll(delta, &chat_names);
        assert_eq!(p.state.as_ref().unwrap().scroll_offset, expected);
    }

    #[test]
    fn scroll_noop_when_closed() {
        let mut p = ChatPicker::new();
        let chat_names = many_names(5);
        p.scroll(-3, &chat_names);
        assert!(!p.is_open());
    }
}

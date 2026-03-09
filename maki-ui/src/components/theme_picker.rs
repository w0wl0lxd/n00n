use crate::components::keybindings::key;
use crate::components::modal::Modal;
use crate::components::scrollbar::render_vertical_scrollbar;
use crate::text_buffer::TextBuffer;
use crate::theme;

use crossterm::event::{KeyCode, KeyEvent};
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

pub enum ThemePickerAction {
    Consumed,
    Closed,
}

struct State {
    selected: usize,
    original_theme_name: String,
    search: TextBuffer,
    scroll_offset: usize,
    viewport_height: usize,
}

const NO_MATCHES: &str = "No matches";
const SEARCH_PREFIX: &str = super::CHEVRON;
const TITLE: &str = " Themes ";
const MIN_WIDTH_PERCENT: u16 = 50;
const MAX_HEIGHT_PERCENT: u16 = 80;
const SEARCH_ROW: u16 = 1;

pub struct ThemePicker {
    state: Option<State>,
}

impl ThemePicker {
    pub fn new() -> Self {
        Self { state: None }
    }

    pub fn open(&mut self) {
        let current_name = theme::current_theme_name();
        let themes = theme::BUNDLED_THEMES;
        let selected = themes
            .iter()
            .position(|t| t.name == current_name)
            .unwrap_or(0);
        self.state = Some(State {
            selected,
            original_theme_name: current_name,
            search: TextBuffer::new(String::new()),
            scroll_offset: 0,
            viewport_height: 20,
        });
    }

    pub fn is_open(&self) -> bool {
        self.state.is_some()
    }

    pub fn close(&mut self) {
        self.state = None;
    }

    pub fn handle_key(&mut self, key_event: KeyEvent) -> ThemePickerAction {
        let s = match self.state.as_mut() {
            Some(s) => s,
            None => return ThemePickerAction::Consumed,
        };

        if key::QUIT.matches(key_event) {
            return self.cancel();
        }
        if key::DELETE_WORD.matches(key_event) {
            s.search.remove_word_before_cursor();
            s.clamp_selection();
            s.apply_preview();
            return ThemePickerAction::Consumed;
        }

        match key_event.code {
            KeyCode::Up => {
                s.move_up();
                s.apply_preview();
                ThemePickerAction::Consumed
            }
            KeyCode::Down => {
                s.move_down();
                s.apply_preview();
                ThemePickerAction::Consumed
            }
            KeyCode::Enter => {
                if let Some(entry) = s.selected_theme() {
                    theme::persist_theme(entry.name);
                }
                self.state = None;
                ThemePickerAction::Closed
            }
            KeyCode::Esc => self.cancel(),
            KeyCode::Char(c) => {
                s.search.push_char(c);
                s.clamp_selection();
                s.apply_preview();
                ThemePickerAction::Consumed
            }
            KeyCode::Backspace => {
                s.search.remove_char();
                s.clamp_selection();
                s.apply_preview();
                ThemePickerAction::Consumed
            }
            KeyCode::Left => {
                s.search.move_left();
                ThemePickerAction::Consumed
            }
            KeyCode::Right => {
                s.search.move_right();
                ThemePickerAction::Consumed
            }
            KeyCode::Home => {
                s.search.move_home();
                ThemePickerAction::Consumed
            }
            KeyCode::End => {
                s.search.move_end();
                ThemePickerAction::Consumed
            }
            _ => ThemePickerAction::Consumed,
        }
    }

    fn cancel(&mut self) -> ThemePickerAction {
        if let Some(s) = self.state.take()
            && let Ok(original) = theme::load_by_name(&s.original_theme_name)
        {
            theme::set(original);
        }
        ThemePickerAction::Closed
    }

    pub fn view(&mut self, frame: &mut Frame, area: Rect) {
        let s = match self.state.as_mut() {
            Some(s) => s,
            None => return,
        };

        let filtered = s.filter();
        let content_rows = if filtered.is_empty() {
            1
        } else {
            filtered.len() as u16
        };
        let modal = Modal {
            title: TITLE,
            width_percent: MIN_WIDTH_PERCENT,
            max_height_percent: MAX_HEIGHT_PERCENT,
        };
        let inner = modal.render(frame, area, content_rows + SEARCH_ROW);
        let viewport_h = inner.height.saturating_sub(SEARCH_ROW);
        s.viewport_height = viewport_h as usize;

        let [list_area, search_area] =
            Layout::vertical([Constraint::Min(1), Constraint::Length(1)]).areas(inner);

        render_list(frame, list_area, &filtered, s);
        render_search(frame, search_area, s);

        if filtered.len() as u16 > viewport_h {
            render_vertical_scrollbar(
                frame,
                list_area,
                filtered.len() as u16,
                s.scroll_offset as u16,
            );
        }
    }
}

impl State {
    fn selected_theme(&self) -> Option<&'static theme::ThemeEntry> {
        let filtered = self.filter();
        filtered
            .get(self.selected)
            .map(|&i| &theme::BUNDLED_THEMES[i])
    }

    fn filter(&self) -> Vec<usize> {
        filter_themes(self.search.value().as_str())
    }

    fn clamp_selection(&mut self) {
        let filtered = self.filter();
        if filtered.is_empty() {
            self.selected = 0;
            self.scroll_offset = 0;
        } else {
            self.selected = self.selected.min(filtered.len() - 1);
            self.scroll_offset = self.scroll_offset.min(self.selected);
        }
    }

    fn move_up(&mut self) {
        let filtered = self.filter();
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

    fn move_down(&mut self) {
        let filtered = self.filter();
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

    fn apply_preview(&self) {
        if let Some(entry) = self.selected_theme()
            && let Ok(t) = theme::load_by_name(entry.name)
        {
            theme::set(t);
        }
    }
}

fn filter_themes(query: &str) -> Vec<usize> {
    let themes = theme::BUNDLED_THEMES;
    if query.is_empty() {
        return (0..themes.len()).collect();
    }
    let query_lower = query.to_ascii_lowercase();
    themes
        .iter()
        .enumerate()
        .filter(|(_, t)| t.name.to_ascii_lowercase().contains(&query_lower))
        .map(|(i, _)| i)
        .collect()
}

fn render_list(frame: &mut Frame, area: Rect, filtered: &[usize], s: &State) {
    let themes = theme::BUNDLED_THEMES;
    if filtered.is_empty() {
        let line = Line::from(Span::styled(NO_MATCHES, theme::current().picker_no_match));
        frame.render_widget(Paragraph::new(vec![line]), area);
        return;
    }

    let end = (s.scroll_offset + s.viewport_height).min(filtered.len());
    let visible = &filtered[s.scroll_offset..end];

    let lines: Vec<Line> = visible
        .iter()
        .enumerate()
        .map(|(vi, &theme_idx)| {
            let abs_idx = s.scroll_offset + vi;
            let name = themes[theme_idx].name;
            let style = if abs_idx == s.selected {
                theme::current().cmd_selected
            } else {
                theme::current().picker_item
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
        Span::styled(SEARCH_PREFIX, theme::current().picker_search_prefix),
        Span::styled(before, theme::current().picker_search_text),
        Span::styled(cursor_char.to_string(), theme::current().cursor),
        Span::styled(after, theme::current().picker_search_text),
    ]);
    frame.render_widget(Paragraph::new(vec![line]), area);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::components::key;
    use crate::components::keybindings::key as kb;
    use crossterm::event::KeyCode;
    use test_case::test_case;

    #[test]
    fn open_sets_initial_state() {
        let mut p = ThemePicker::new();
        p.open();
        assert!(p.is_open());
        let s = p.state.as_ref().unwrap();
        assert_eq!(s.search.value(), "");
    }

    #[test]
    fn up_down_wraps() {
        let mut p = ThemePicker::new();
        p.open();

        p.handle_key(key(KeyCode::Up));
        let count = theme::BUNDLED_THEMES.len();
        assert_eq!(p.state.as_ref().unwrap().selected, count - 1);

        p.handle_key(key(KeyCode::Down));
        assert_eq!(p.state.as_ref().unwrap().selected, 0);
    }

    #[test]
    fn enter_closes() {
        let mut p = ThemePicker::new();
        p.open();
        let action = p.handle_key(key(KeyCode::Enter));
        assert!(matches!(action, ThemePickerAction::Closed));
        assert!(!p.is_open());
    }

    #[test_case(key(KeyCode::Esc) ; "escape_restores_and_closes")]
    #[test_case(kb::QUIT.to_key_event() ; "ctrl_c_restores_and_closes")]
    fn cancel_restores(cancel_key: crossterm::event::KeyEvent) {
        let mut p = ThemePicker::new();
        p.open();
        p.handle_key(key(KeyCode::Down));
        let action = p.handle_key(cancel_key);
        assert!(matches!(action, ThemePickerAction::Closed));
        assert!(!p.is_open());
    }

    #[test]
    fn typing_filters() {
        let mut p = ThemePicker::new();
        p.open();
        p.handle_key(key(KeyCode::Char('g')));
        p.handle_key(key(KeyCode::Char('r')));
        let filtered = p.state.as_ref().unwrap().filter();
        assert!(filtered.len() < theme::BUNDLED_THEMES.len());
        assert!(
            filtered
                .iter()
                .any(|&i| theme::BUNDLED_THEMES[i].name == "gruvbox")
        );
    }

    #[test]
    fn filter_empty_query_shows_all() {
        let filtered = filter_themes("");
        assert_eq!(filtered.len(), theme::BUNDLED_THEMES.len());
    }

    #[test]
    fn filter_no_match_returns_empty() {
        let filtered = filter_themes("zzzzzzz");
        assert!(filtered.is_empty());
    }
}

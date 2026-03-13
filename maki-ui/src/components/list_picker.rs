use crate::components::is_ctrl;
use crate::components::keybindings::key;
use crate::components::modal::Modal;
use crate::components::scrollbar::render_vertical_scrollbar;
use crate::text_buffer::TextBuffer;
use crate::theme;

use crossterm::event::{KeyCode, KeyEvent};
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Position, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use unicode_width::UnicodeWidthStr;

pub trait PickerItem {
    fn label(&self) -> &str;
    fn detail(&self) -> Option<&str> {
        None
    }
    fn section(&self) -> Option<&str> {
        None
    }
}

impl PickerItem for String {
    fn label(&self) -> &str {
        self
    }
}

pub enum PickerAction<T> {
    Consumed,
    Select(usize, T),
    Close,
}

const NO_MATCHES: &str = "No matches";
const SEARCH_PREFIX: &str = super::CHEVRON;
const MIN_WIDTH_PERCENT: u16 = 60;
const MAX_HEIGHT_PERCENT: u16 = 80;
const SEARCH_ROW: u16 = 1;

pub struct ListPicker<T> {
    state: Option<State<T>>,
    max_visible: Option<u16>,
    generation: u64,
}

struct State<T> {
    items: Vec<T>,
    filtered: Vec<usize>,
    selected: usize,
    search: TextBuffer,
    scroll_offset: usize,
    viewport_height: usize,
    inner_area: Rect,
    title: &'static str,
}

impl<T: PickerItem> State<T> {
    fn new(items: Vec<T>, title: &'static str) -> Self {
        let filtered = (0..items.len()).collect();
        Self {
            items,
            filtered,
            selected: 0,
            search: TextBuffer::new(String::new()),
            scroll_offset: 0,
            viewport_height: 20,
            inner_area: Rect::default(),
            title,
        }
    }

    fn replace_items(&mut self, items: Vec<T>) {
        self.items = items;
        self.rebuild_filter();
        self.clamp_selection();
    }

    fn rebuild_filter(&mut self) {
        let query = self.search.value();
        if query.is_empty() {
            self.filtered = (0..self.items.len()).collect();
        } else {
            let query_lower = query.to_ascii_lowercase();
            self.filtered = self
                .items
                .iter()
                .enumerate()
                .filter(|(_, item)| item.label().to_ascii_lowercase().contains(&query_lower))
                .map(|(i, _)| i)
                .collect();
        }
    }

    fn clamp_selection(&mut self) {
        if self.filtered.is_empty() {
            self.selected = 0;
            self.scroll_offset = 0;
        } else {
            self.selected = self.selected.min(self.filtered.len() - 1);
            self.scroll_offset = self.scroll_offset.min(self.selected);
        }
    }

    fn update_search_and_clamp(&mut self) {
        self.rebuild_filter();
        self.clamp_selection();
    }

    fn move_up(&mut self) {
        let len = self.filtered.len();
        if len == 0 {
            return;
        }
        self.selected = if self.selected == 0 {
            len - 1
        } else {
            self.selected - 1
        };
        self.ensure_visible();
    }

    fn move_down(&mut self) {
        let len = self.filtered.len();
        if len == 0 {
            return;
        }
        self.selected = if self.selected == len - 1 {
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
        let visual = visual_rows_in_range(
            &self.filtered,
            &self.items,
            self.scroll_offset,
            self.selected + 1,
        );
        if visual > self.viewport_height {
            self.scroll_offset = find_scroll_offset_for(
                &self.filtered,
                &self.items,
                self.selected,
                self.viewport_height,
            );
        }
    }

    fn selected_item_index(&self) -> Option<usize> {
        self.filtered.get(self.selected).copied()
    }
}

impl<T: PickerItem> ListPicker<T> {
    pub fn new() -> Self {
        Self {
            state: None,
            max_visible: None,
            generation: 0,
        }
    }

    pub fn with_max_visible(mut self, max: u16) -> Self {
        self.max_visible = Some(max);
        self
    }

    pub fn open(&mut self, items: Vec<T>, title: &'static str) {
        self.generation += 1;
        self.state = Some(State::new(items, title));
    }

    pub fn select(&mut self, index: usize) {
        if let Some(s) = self.state.as_mut() {
            self.generation += 1;
            s.selected = index.min(s.filtered.len().saturating_sub(1));
            s.ensure_visible();
        }
    }

    pub fn replace_items(&mut self, items: Vec<T>) {
        if let Some(s) = self.state.as_mut() {
            self.generation += 1;
            s.replace_items(items);
        }
    }

    pub fn retain(&mut self, f: impl Fn(&T) -> bool) {
        let Some(s) = self.state.as_mut() else { return };
        self.generation += 1;
        s.items.retain(|item| f(item));
        if s.items.is_empty() {
            self.state = None;
            return;
        }
        s.rebuild_filter();
        s.clamp_selection();
    }

    pub fn is_open(&self) -> bool {
        self.state.is_some()
    }

    pub fn generation(&self) -> u64 {
        self.generation
    }

    pub fn close(&mut self) {
        self.generation += 1;
        self.state = None;
    }

    pub fn contains(&self, pos: Position) -> bool {
        self.state
            .as_ref()
            .is_some_and(|s| s.inner_area.contains(pos))
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> PickerAction<T> {
        let s = match self.state.as_mut() {
            Some(s) => s,
            None => return PickerAction::Close,
        };
        self.generation += 1;

        if key::QUIT.matches(key) {
            self.state = None;
            return PickerAction::Close;
        }
        if key::DELETE_WORD.matches(key) {
            s.search.remove_word_before_cursor();
            s.update_search_and_clamp();
            return PickerAction::Consumed;
        }
        if is_ctrl(&key) {
            return PickerAction::Consumed;
        }
        match key.code {
            KeyCode::Up => {
                s.move_up();
                PickerAction::Consumed
            }
            KeyCode::Down => {
                s.move_down();
                PickerAction::Consumed
            }
            KeyCode::Enter => match s.selected_item_index() {
                Some(idx) => {
                    let mut state = self.state.take().unwrap();
                    PickerAction::Select(idx, state.items.swap_remove(idx))
                }
                None => PickerAction::Consumed,
            },
            KeyCode::Esc => {
                self.state = None;
                PickerAction::Close
            }
            KeyCode::Char(c) => {
                s.search.push_char(c);
                s.update_search_and_clamp();
                PickerAction::Consumed
            }
            KeyCode::Backspace => {
                s.search.remove_char();
                s.update_search_and_clamp();
                PickerAction::Consumed
            }
            KeyCode::Left => {
                s.search.move_left();
                PickerAction::Consumed
            }
            KeyCode::Right => {
                s.search.move_right();
                PickerAction::Consumed
            }
            KeyCode::Home => {
                s.search.move_home();
                PickerAction::Consumed
            }
            KeyCode::End => {
                s.search.move_end();
                PickerAction::Consumed
            }
            _ => PickerAction::Consumed,
        }
    }

    pub fn selected_item(&self) -> Option<&T> {
        let s = self.state.as_ref()?;
        s.selected_item_index().map(|i| &s.items[i])
    }

    pub fn selected_index(&self) -> Option<usize> {
        self.state.as_ref()?.selected_item_index()
    }

    pub fn scroll(&mut self, delta: i32) {
        let s = match self.state.as_mut() {
            Some(s) => s,
            None => return,
        };
        self.generation += 1;
        if delta > 0 {
            s.scroll_offset = s.scroll_offset.saturating_sub(delta as usize);
        } else {
            let total_visual = visual_rows_in_range(&s.filtered, &s.items, 0, s.filtered.len());
            let max_offset = if total_visual <= s.viewport_height {
                0
            } else {
                find_scroll_offset_for_bottom(&s.filtered, &s.items, s.viewport_height)
            };
            s.scroll_offset = (s.scroll_offset + delta.unsigned_abs() as usize).min(max_offset);
        }
    }

    pub fn view(&mut self, frame: &mut Frame, area: Rect) {
        let s = match self.state.as_mut() {
            Some(s) => s,
            None => return,
        };

        let content_rows = if s.filtered.is_empty() {
            1
        } else {
            let rows = visual_rows_in_range(&s.filtered, &s.items, 0, s.filtered.len()) as u16;
            match self.max_visible {
                Some(max) => rows.min(max),
                None => rows,
            }
        };
        let modal = Modal {
            title: s.title,
            width_percent: MIN_WIDTH_PERCENT,
            max_height_percent: MAX_HEIGHT_PERCENT,
        };
        let inner = modal.render(frame, area, content_rows + SEARCH_ROW);
        let viewport_h = inner.height.saturating_sub(SEARCH_ROW);
        s.viewport_height = viewport_h as usize;
        s.ensure_visible();

        let [list_area, search_area] =
            Layout::vertical([Constraint::Min(1), Constraint::Length(1)]).areas(inner);

        render_list(
            frame,
            list_area,
            &s.filtered,
            &s.items,
            s.selected,
            s.scroll_offset,
            s.viewport_height,
        );
        render_search(frame, search_area, &s.search);

        let total_visual = visual_rows_in_range(&s.filtered, &s.items, 0, s.filtered.len());
        if total_visual as u16 > viewport_h {
            let visual_offset = visual_rows_in_range(&s.filtered, &s.items, 0, s.scroll_offset);
            render_vertical_scrollbar(frame, list_area, total_visual as u16, visual_offset as u16);
        }

        s.inner_area = inner;
    }
}

fn section_gap<T: PickerItem>(filtered: &[usize], items: &[T], idx: usize) -> usize {
    let item = &items[filtered[idx]];
    let is_break = match item.section() {
        None => false,
        Some(sec) => {
            idx == 0
                || items[filtered[idx - 1]]
                    .section()
                    .is_none_or(|prev| prev != sec)
        }
    };
    if !is_break {
        return 0;
    }
    if idx == 0 { 1 } else { 2 }
}

fn visual_rows_in_range<T: PickerItem>(
    filtered: &[usize],
    items: &[T],
    start: usize,
    end: usize,
) -> usize {
    let item_count = end.saturating_sub(start);
    let section_rows: usize = (start..end).map(|i| section_gap(filtered, items, i)).sum();
    item_count + section_rows
}

fn find_scroll_offset_for<T: PickerItem>(
    filtered: &[usize],
    items: &[T],
    target: usize,
    viewport_height: usize,
) -> usize {
    for start in (0..=target).rev() {
        let rows = visual_rows_in_range(filtered, items, start, target + 1);
        if rows > viewport_height {
            return (start + 1).min(target);
        }
    }
    0
}

fn find_scroll_offset_for_bottom<T: PickerItem>(
    filtered: &[usize],
    items: &[T],
    viewport_height: usize,
) -> usize {
    let len = filtered.len();
    if len == 0 {
        return 0;
    }
    find_scroll_offset_for(filtered, items, len - 1, viewport_height)
}

fn detail_padding(label: &str, detail: &str, area_width: u16) -> usize {
    area_width.saturating_sub(label.width() as u16 + detail.width() as u16 + 1) as usize
}

fn render_list<T: PickerItem>(
    frame: &mut Frame,
    area: Rect,
    filtered: &[usize],
    items: &[T],
    selected: usize,
    scroll_offset: usize,
    viewport_height: usize,
) {
    if filtered.is_empty() {
        let line = Line::from(Span::styled(NO_MATCHES, theme::current().cmd_desc));
        frame.render_widget(Paragraph::new(vec![line]), area);
        return;
    }

    let mut lines: Vec<Line> = Vec::new();
    let mut i = scroll_offset;
    let mut last_section: Option<&str> = if scroll_offset > 0 {
        items[filtered[scroll_offset - 1]].section()
    } else {
        None
    };

    while lines.len() < viewport_height && i < filtered.len() {
        let item_idx = filtered[i];
        let item = &items[item_idx];

        if let Some(sec) = item.section()
            && last_section.is_none_or(|prev| prev != sec)
        {
            if !lines.is_empty() && lines.len() < viewport_height {
                lines.push(Line::raw(""));
            }
            if lines.len() < viewport_height {
                lines.push(Line::from(Span::styled(
                    format!("  {sec}"),
                    theme::current().keybind_section,
                )));
            }
            last_section = Some(sec);
        }

        if lines.len() >= viewport_height {
            break;
        }

        let style = if i == selected {
            theme::current().cmd_selected
        } else {
            theme::current().cmd_name
        };
        let label = format!("  {}", item.label());
        let line = match item.detail() {
            Some(detail) => {
                let pad = detail_padding(&label, detail, area.width);
                let detail_style = if i == selected {
                    style
                } else {
                    theme::current().cmd_desc
                };
                Line::from(vec![
                    Span::styled(label, style),
                    Span::styled(" ".repeat(pad), style),
                    Span::styled(detail.to_string(), detail_style),
                ])
            }
            None => Line::from(Span::styled(label, style)),
        };
        lines.push(line);
        i += 1;
    }

    frame.render_widget(Paragraph::new(lines), area);
}

fn render_search(frame: &mut Frame, area: Rect, search: &TextBuffer) {
    let query = search.value();
    let cursor_x = search.x();
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

    struct Entry {
        label: String,
        detail: Option<String>,
    }

    impl Entry {
        fn new(label: &str) -> Self {
            Self {
                label: label.into(),
                detail: None,
            }
        }
    }

    impl PickerItem for Entry {
        fn label(&self) -> &str {
            &self.label
        }
        fn detail(&self) -> Option<&str> {
            self.detail.as_deref()
        }
    }

    fn entries(names: &[&str]) -> Vec<Entry> {
        names.iter().map(|n| Entry::new(n)).collect()
    }

    #[test]
    fn navigation_wraps_around() {
        let mut p = ListPicker::new();
        p.open(entries(&["A", "B", "C"]), " Test ");

        p.handle_key(key(KeyCode::Up));
        assert_eq!(p.state.as_ref().unwrap().selected, 2);

        p.handle_key(key(KeyCode::Down));
        assert_eq!(p.state.as_ref().unwrap().selected, 0);
    }

    #[test]
    fn search_filters_progressively() {
        let mut p = ListPicker::new();
        p.open(entries(&["Alpha", "Beta"]), " Test ");
        assert_eq!(p.state.as_ref().unwrap().filtered, vec![0, 1]);

        p.handle_key(key(KeyCode::Char('a')));
        assert_eq!(p.state.as_ref().unwrap().filtered, vec![0, 1]);

        p.handle_key(key(KeyCode::Char('l')));
        assert_eq!(p.state.as_ref().unwrap().filtered, vec![0]);
    }

    #[test]
    fn enter_returns_selected_item() {
        let mut p = ListPicker::new();
        p.open(entries(&["A", "B", "C"]), " Test ");
        p.handle_key(key(KeyCode::Down));

        let action = p.handle_key(key(KeyCode::Enter));
        assert!(matches!(action, PickerAction::Select(1, ref e) if e.label == "B"));
        assert!(!p.is_open());
    }

    #[test_case(key(KeyCode::Esc) ; "esc_returns_close")]
    #[test_case(kb::QUIT.to_key_event() ; "ctrl_c_returns_close")]
    fn cancel_returns_close(cancel_key: KeyEvent) {
        let mut p = ListPicker::new();
        p.open(entries(&["A", "B"]), " Test ");

        let action = p.handle_key(cancel_key);
        assert!(matches!(action, PickerAction::Close));
        assert!(!p.is_open());
    }

    #[test]
    fn enter_on_empty_results_consumed() {
        let mut p = ListPicker::new();
        p.open(entries(&["Alpha"]), " Test ");
        p.handle_key(key(KeyCode::Char('z')));

        let action = p.handle_key(key(KeyCode::Enter));
        assert!(matches!(action, PickerAction::Consumed));
    }

    #[test_case(0, -3, 3  ; "scroll_down")]
    #[test_case(0, 100, 0  ; "clamp_at_top")]
    #[test_case(5, 3, 2    ; "scroll_up")]
    #[test_case(0, -100, 20 ; "clamp_at_bottom")]
    fn scroll_bounds(initial: usize, delta: i32, expected: usize) {
        let items: Vec<Entry> = (0..30).map(|i| Entry::new(&format!("Item {i}"))).collect();
        let mut p = ListPicker::new();
        p.open(items, " Test ");
        let s = p.state.as_mut().unwrap();
        s.viewport_height = 10;
        s.scroll_offset = initial;

        p.scroll(delta);
        assert_eq!(p.state.as_ref().unwrap().scroll_offset, expected);
    }

    #[test]
    fn ctrl_w_deletes_word() {
        let mut p = ListPicker::new();
        p.open(entries(&["A", "B"]), " Test ");
        p.handle_key(key(KeyCode::Char('h')));
        p.handle_key(key(KeyCode::Char('i')));
        assert_eq!(p.state.as_ref().unwrap().search.value(), "hi");

        p.handle_key(kb::DELETE_WORD.to_key_event());
        assert_eq!(p.state.as_ref().unwrap().search.value(), "");
    }

    struct SectionEntry {
        label: String,
        section: &'static str,
    }

    impl PickerItem for SectionEntry {
        fn label(&self) -> &str {
            &self.label
        }
        fn section(&self) -> Option<&str> {
            Some(self.section)
        }
    }

    fn section_entries() -> Vec<SectionEntry> {
        vec![
            SectionEntry {
                label: "a1".into(),
                section: "A",
            },
            SectionEntry {
                label: "a2".into(),
                section: "A",
            },
            SectionEntry {
                label: "b1".into(),
                section: "B",
            },
        ]
    }

    #[test]
    fn section_headers_counted_in_visual_rows() {
        let items = section_entries();
        let filtered: Vec<usize> = (0..items.len()).collect();
        let rows = visual_rows_in_range(&filtered, &items, 0, items.len());
        assert_eq!(rows, 6);
    }

    #[test]
    fn section_navigation_accounts_for_headers() {
        let mut p = ListPicker::new();
        p.open(section_entries(), " Test ");
        let s = p.state.as_mut().unwrap();
        s.viewport_height = 3;

        s.selected = 2;
        s.ensure_visible();
        assert_eq!(s.scroll_offset, 2);
    }

    #[test]
    fn detail_padding_consistent_with_multibyte_chars() {
        let detail = "2h ago";
        let width = 60;
        let pad_ascii = detail_padding("  abcdefghijk", detail, width);
        let pad_ellipsis = detail_padding("  abcdefgh\u{2026}", detail, width);
        assert_eq!(pad_ellipsis - pad_ascii, 2);
    }
}

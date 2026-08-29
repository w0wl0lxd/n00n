use std::sync::Arc;

use crate::components::Overlay;
use crate::components::list_picker::{ListPicker, PickerAction, PickerItem};
use crate::theme::{self, ThemeEngine};

use crossterm::event::KeyEvent;
use ratatui::Frame;
use ratatui::layout::Rect;

const TITLE: &str = " Themes ";
const MAX_VISIBLE: u16 = 15;

pub enum ThemePickerAction {
    Consumed,
    Closed,
}

struct ThemeEntry {
    name: &'static str,
}

impl PickerItem for ThemeEntry {
    fn label(&self) -> &str {
        self.name
    }
}

pub struct ThemePicker {
    picker: ListPicker<ThemeEntry>,
    original_theme_name: Option<String>,
    theme_engine: Arc<ThemeEngine>,
}

impl ThemePicker {
    pub fn new(theme_engine: Arc<ThemeEngine>) -> Self {
        Self {
            picker: ListPicker::new(Arc::clone(&theme_engine)).with_max_visible(MAX_VISIBLE),
            original_theme_name: None,
            theme_engine,
        }
    }

    pub fn open(&mut self) {
        let current_name = ThemeEngine::current_theme_name();
        let entries: Vec<ThemeEntry> = theme::BUNDLED_THEMES
            .iter()
            .map(|t| ThemeEntry { name: t.name })
            .collect();
        let current_idx = entries
            .iter()
            .position(|e| e.name == current_name)
            .unwrap_or_else(|| 0);
        self.original_theme_name = Some(current_name);
        self.picker.open(entries, TITLE);
        self.picker.select(current_idx);
    }

    pub fn is_open(&self) -> bool {
        self.picker.is_open()
    }

    pub fn close(&mut self) {
        self.picker.close();
        self.original_theme_name = None;
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> ThemePickerAction {
        match self.picker.handle_key(key) {
            PickerAction::Consumed => {
                self.apply_preview();
                ThemePickerAction::Consumed
            }
            PickerAction::Select(_, entry) => {
                ThemeEngine::persist_theme(entry.name);
                self.original_theme_name = None;
                ThemePickerAction::Closed
            }
            PickerAction::Close => {
                self.restore_original();
                self.original_theme_name = None;
                ThemePickerAction::Closed
            }
            PickerAction::Toggle(..) => ThemePickerAction::Consumed,
        }
    }

    pub fn view(&mut self, frame: &mut Frame, area: Rect) -> Rect {
        self.picker.view(frame, area)
    }

    pub fn handle_paste(&mut self, text: &str) -> bool {
        let consumed = self.picker.handle_paste(text);
        if consumed {
            self.apply_preview();
        }
        consumed
    }

    fn apply_preview(&self) {
        if let Some(entry) = self.picker.selected_item()
            && let Ok(t) = ThemeEngine::load_by_name(entry.name)
        {
            self.theme_engine.set(t);
        }
    }

    fn restore_original(&self) {
        if let Some(ref name) = self.original_theme_name
            && let Ok(t) = ThemeEngine::load_by_name(name)
        {
            self.theme_engine.set(t);
        }
    }
}

impl Overlay for ThemePicker {
    fn is_open(&self) -> bool {
        self.is_open()
    }

    fn close(&mut self) {
        self.close();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::components::key;
    use crate::components::keybindings::key as kb;
    use crate::theme::test_engine;
    use crossterm::event::KeyCode;
    use test_case::test_case;

    #[test]
    fn enter_closes() {
        let mut p = ThemePicker::new(test_engine());
        p.open();
        let action = p.handle_key(key(KeyCode::Enter));
        assert!(matches!(action, ThemePickerAction::Closed));
        assert!(!p.is_open());
    }

    #[test_case(key(KeyCode::Esc) ; "escape_restores_and_closes")]
    #[test_case(kb::QUIT.to_key_event() ; "ctrl_c_restores_and_closes")]
    fn cancel_restores(cancel_key: crossterm::event::KeyEvent) {
        let mut p = ThemePicker::new(test_engine());
        p.open();
        p.handle_key(key(KeyCode::Down));
        let action = p.handle_key(cancel_key);
        assert!(matches!(action, ThemePickerAction::Closed));
        assert!(!p.is_open());
    }
}

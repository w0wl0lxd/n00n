use crate::components::list_picker::{ListPicker, PickerAction};

use crossterm::event::KeyEvent;
use ratatui::Frame;
use ratatui::layout::{Position, Rect};

const TITLE: &str = " Chats ";

pub enum ChatPickerAction {
    Consumed,
    Select(usize),
}

pub struct ChatPicker {
    picker: ListPicker<String>,
    original_chat: Option<usize>,
}

impl ChatPicker {
    pub fn new() -> Self {
        Self {
            picker: ListPicker::new(),
            original_chat: None,
        }
    }

    pub fn open(&mut self, active_chat: usize, chat_names: &[String]) {
        self.original_chat = Some(active_chat);
        self.picker.open(chat_names.to_vec(), TITLE);
    }

    pub fn is_open(&self) -> bool {
        self.picker.is_open()
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> ChatPickerAction {
        match self.picker.handle_key(key) {
            PickerAction::Consumed => ChatPickerAction::Consumed,
            PickerAction::Select(idx, _) => {
                self.original_chat = None;
                ChatPickerAction::Select(idx)
            }
            PickerAction::Close => {
                let original = self.original_chat.take().unwrap_or(0);
                ChatPickerAction::Select(original)
            }
        }
    }

    pub fn selected_chat(&self) -> Option<usize> {
        self.picker.selected_index()
    }

    pub fn view(&mut self, frame: &mut Frame, area: Rect) {
        self.picker.view(frame, area);
    }

    pub fn close(&mut self) {
        self.picker.close();
        self.original_chat = None;
    }

    pub fn contains(&self, pos: Position) -> bool {
        self.picker.contains(pos)
    }

    pub fn scroll(&mut self, delta: i32) {
        self.picker.scroll(delta);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::components::key;
    use crate::components::keybindings::key as kb;
    use crossterm::event::{KeyCode, KeyEvent};
    use test_case::test_case;

    fn names(n: &[&str]) -> Vec<String> {
        n.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn enter_confirms_selected() {
        let mut p = ChatPicker::new();
        let chat_names = names(&["Main", "Explore config", "Run tests"]);
        p.open(0, &chat_names);
        p.handle_key(key(KeyCode::Down));

        let action = p.handle_key(key(KeyCode::Enter));
        assert!(matches!(action, ChatPickerAction::Select(1)));
        assert!(!p.is_open());
    }

    #[test_case(key(KeyCode::Esc) ; "escape_returns_original")]
    #[test_case(kb::QUIT.to_key_event() ; "ctrl_c_returns_original")]
    fn cancel_returns_original(cancel_key: KeyEvent) {
        let mut p = ChatPicker::new();
        let chat_names = names(&["Main", "Explore config"]);
        p.open(0, &chat_names);
        p.handle_key(key(KeyCode::Down));

        let action = p.handle_key(cancel_key);
        assert!(matches!(action, ChatPickerAction::Select(0)));
        assert!(!p.is_open());
    }
}

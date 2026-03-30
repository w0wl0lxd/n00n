use crate::components::Overlay;
use crate::components::is_ctrl;
use crate::components::keybindings::key;
use crate::components::list_picker::{ListPicker, PickerAction, PickerItem};

use crossterm::event::{KeyCode, KeyEvent};
use ratatui::Frame;
use ratatui::layout::{Position, Rect};

const TITLE: &str = " Memory Files ";
const FOOTER_HINTS: &[(&str, &str)] = &[("Enter", "open"), (key::DELETE.label, "delete")];

#[derive(Debug)]
pub enum MemoryModalAction {
    Consumed,
    Close,
    OpenFile(String),
    DeleteFile(String),
    ConfirmDelete,
}

pub struct MemoryEntry {
    pub name: String,
    detail: String,
}

impl MemoryEntry {
    pub fn new(name: String, size: u64) -> Self {
        let detail = format!("({size} bytes)");
        Self { name, detail }
    }
}

impl PickerItem for MemoryEntry {
    fn label(&self) -> &str {
        &self.name
    }

    fn detail(&self) -> Option<&str> {
        Some(&self.detail)
    }
}

pub struct MemoryModal {
    picker: ListPicker<MemoryEntry>,
    confirming: Option<(String, u64)>,
}

impl MemoryModal {
    pub fn new() -> Self {
        Self {
            picker: ListPicker::new().with_footer(FOOTER_HINTS),
            confirming: None,
        }
    }

    pub fn open(&mut self, entries: Vec<MemoryEntry>) {
        self.picker.open(entries, TITLE);
        self.confirming = None;
    }

    pub fn is_open(&self) -> bool {
        self.picker.is_open()
    }

    pub fn close(&mut self) {
        self.picker.close();
        self.confirming = None;
    }

    pub fn contains(&self, pos: Position) -> bool {
        self.picker.contains(pos)
    }

    pub fn scroll(&mut self, delta: i32) {
        self.picker.scroll(delta);
    }

    pub fn handle_paste(&mut self, text: &str) -> bool {
        self.picker.handle_paste(text)
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> MemoryModalAction {
        if is_ctrl(&key) && key.code == KeyCode::Char('d') {
            return self.handle_delete_key();
        }

        self.confirming = None;

        if key.code == KeyCode::Enter || key::OPEN_EDITOR.matches(key) {
            return match self.picker.selected_item() {
                Some(entry) => MemoryModalAction::OpenFile(entry.name.clone()),
                None => MemoryModalAction::Consumed,
            };
        }

        match self.picker.handle_key(key) {
            PickerAction::Consumed => MemoryModalAction::Consumed,
            PickerAction::Select(_, entry) => MemoryModalAction::OpenFile(entry.name),
            PickerAction::Close => MemoryModalAction::Close,
            PickerAction::Toggle(..) => MemoryModalAction::Consumed,
        }
    }

    fn handle_delete_key(&mut self) -> MemoryModalAction {
        let Some(selected) = self.picker.selected_item() else {
            return MemoryModalAction::Consumed;
        };

        let generation = self.picker.generation();
        if self
            .confirming
            .as_ref()
            .is_some_and(|(name, g)| name == &selected.name && *g == generation)
        {
            let name = selected.name.clone();
            self.confirming = None;
            return MemoryModalAction::DeleteFile(name);
        }

        self.confirming = Some((selected.name.clone(), generation));
        MemoryModalAction::ConfirmDelete
    }

    #[cfg(test)]
    fn is_confirming(&self) -> bool {
        self.confirming.is_some()
    }

    pub fn retain(&mut self, f: impl Fn(&MemoryEntry) -> bool) {
        self.picker.retain(f);
    }

    pub fn update_size(&mut self, name: &str, size: u64) {
        self.picker.with_item_mut(name, |entry| {
            entry.detail = format!("({size} bytes)");
        });
    }

    pub fn view(&mut self, frame: &mut Frame, area: Rect) -> Rect {
        self.picker.view(frame, area)
    }
}

impl Overlay for MemoryModal {
    fn is_open(&self) -> bool {
        self.is_open()
    }

    fn close(&mut self) {
        self.close()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::components::key as key_ev;
    use crossterm::event::{KeyCode, KeyModifiers};

    fn ctrl_d() -> KeyEvent {
        KeyEvent::new(KeyCode::Char('d'), KeyModifiers::CONTROL)
    }

    fn sample_entries() -> Vec<MemoryEntry> {
        vec![
            MemoryEntry::new("a.md".into(), 10),
            MemoryEntry::new("b.md".into(), 20),
        ]
    }

    #[test]
    fn enter_opens_selected_file_and_stays_open() {
        let mut modal = MemoryModal::new();
        modal.open(sample_entries());
        modal.handle_key(key_ev(KeyCode::Down));
        match modal.handle_key(key_ev(KeyCode::Enter)) {
            MemoryModalAction::OpenFile(f) => assert_eq!(f, "b.md"),
            other => panic!("expected OpenFile, got {other:?}"),
        }
        assert!(modal.is_open());
    }

    #[test]
    fn enter_on_empty_is_consumed() {
        let mut modal = MemoryModal::new();
        modal.open(vec![]);
        assert!(matches!(
            modal.handle_key(ctrl_d()),
            MemoryModalAction::Consumed
        ));
    }

    #[test]
    fn delete_confirmation_flow() {
        let mut modal = MemoryModal::new();
        modal.open(sample_entries());

        assert!(matches!(
            modal.handle_key(ctrl_d()),
            MemoryModalAction::ConfirmDelete
        ));
        assert!(modal.is_confirming());

        modal.handle_key(key_ev(KeyCode::Down));
        assert!(!modal.is_confirming(), "moving selection resets confirm");

        assert!(matches!(
            modal.handle_key(ctrl_d()),
            MemoryModalAction::ConfirmDelete
        ));
        match modal.handle_key(ctrl_d()) {
            MemoryModalAction::DeleteFile(name) => assert_eq!(name, "b.md"),
            other => panic!("expected DeleteFile, got {other:?}"),
        }
    }

    #[test]
    fn ctrl_d_on_empty_is_consumed() {
        let mut modal = MemoryModal::new();
        modal.open(vec![]);
        assert!(matches!(
            modal.handle_key(ctrl_d()),
            MemoryModalAction::Consumed
        ));
        assert!(!modal.is_confirming());
    }

    #[test]
    fn update_size_changes_detail() {
        let mut modal = MemoryModal::new();
        modal.open(sample_entries());
        modal.update_size("a.md", 999);
        assert_eq!(
            modal.picker.selected_item().unwrap().detail(),
            Some("(999 bytes)")
        );
    }
}

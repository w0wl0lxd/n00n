use std::sync::Arc;

use arc_swap::ArcSwapOption;
use crossterm::event::KeyEvent;
use ratatui::Frame;
use ratatui::layout::{Position, Rect};

use maki_providers::provider::ProviderKind;

use crate::components::list_picker::{ListPicker, PickerAction, PickerItem};

pub enum ModelPickerAction {
    Consumed,
    Select(String),
    Close,
}

struct ModelEntry {
    spec: String,
    id: String,
    provider_display: &'static str,
    tier: String,
}

impl PickerItem for ModelEntry {
    fn label(&self) -> &str {
        &self.id
    }

    fn detail(&self) -> Option<&str> {
        Some(&self.tier)
    }

    fn section(&self) -> Option<&str> {
        Some(self.provider_display)
    }
}

const TITLE: &str = " Models ";

pub struct ModelPicker {
    picker: ListPicker<ModelEntry>,
    models: Arc<ArcSwapOption<Vec<String>>>,
    last_spec_count: usize,
}

impl ModelPicker {
    pub fn new(models: Arc<ArcSwapOption<Vec<String>>>) -> Self {
        Self {
            picker: ListPicker::new(),
            models,
            last_spec_count: 0,
        }
    }

    pub fn open(&mut self) {
        let guard = self.models.load();
        let specs = guard.as_deref();
        self.last_spec_count = specs.map_or(0, Vec::len);
        let entries = specs
            .map(|s| s.iter().filter_map(|s| parse_model_entry(s)).collect())
            .unwrap_or_default();
        self.picker.open(entries, TITLE);
    }

    fn try_refresh(&mut self) {
        if !self.picker.is_open() {
            return;
        }
        let guard = self.models.load();
        let spec_count = guard.as_deref().map_or(0, Vec::len);
        if spec_count == self.last_spec_count {
            return;
        }
        self.last_spec_count = spec_count;
        let entries: Vec<ModelEntry> = guard
            .as_deref()
            .unwrap()
            .iter()
            .filter_map(|s| parse_model_entry(s))
            .collect();
        self.picker.replace_items(entries);
    }

    pub fn is_open(&self) -> bool {
        self.picker.is_open()
    }

    pub fn close(&mut self) {
        self.picker.close();
    }

    pub fn contains(&self, pos: Position) -> bool {
        self.picker.contains(pos)
    }

    pub fn scroll(&mut self, delta: i32) {
        self.picker.scroll(delta);
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> ModelPickerAction {
        match self.picker.handle_key(key) {
            PickerAction::Consumed => ModelPickerAction::Consumed,
            PickerAction::Select(_, entry) => ModelPickerAction::Select(entry.spec),
            PickerAction::Close => ModelPickerAction::Close,
        }
    }

    pub fn view(&mut self, frame: &mut Frame, area: Rect) {
        self.try_refresh();
        self.picker.view(frame, area);
    }
}

fn parse_model_entry(spec: &str) -> Option<ModelEntry> {
    let (provider_str, model_id) = spec.split_once('/')?;
    let provider_kind: ProviderKind = provider_str.parse().ok()?;
    let tier = match maki_providers::Model::from_spec(spec) {
        Ok(m) => m.tier.to_string(),
        Err(_) => String::new(),
    };
    Some(ModelEntry {
        spec: spec.to_string(),
        id: model_id.to_string(),
        provider_display: provider_kind.display_name(),
        tier,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::components::key;
    use crate::components::keybindings::key as kb;
    use crossterm::event::{KeyCode, KeyEvent};
    use test_case::test_case;

    fn test_models() -> Arc<ArcSwapOption<Vec<String>>> {
        let models = Arc::new(ArcSwapOption::empty());
        models.store(Some(Arc::new(vec![
            "anthropic/claude-sonnet-4-20250514".into(),
            "anthropic/claude-opus-4-6-20260101".into(),
            "zai/glm-5".into(),
        ])));
        models
    }

    #[test]
    fn select_returns_full_spec() {
        let mut p = ModelPicker::new(test_models());
        p.open();
        let action = p.handle_key(key(KeyCode::Enter));
        assert!(
            matches!(action, ModelPickerAction::Select(ref s) if s == "anthropic/claude-sonnet-4-20250514")
        );
    }

    #[test_case(key(KeyCode::Esc)          ; "esc_closes")]
    #[test_case(kb::QUIT.to_key_event()    ; "ctrl_c_closes")]
    fn close_keys(cancel_key: KeyEvent) {
        let mut p = ModelPicker::new(test_models());
        p.open();
        let action = p.handle_key(cancel_key);
        assert!(matches!(action, ModelPickerAction::Close));
        assert!(!p.is_open());
    }

    #[test]
    fn open_with_no_models_still_opens() {
        let models = Arc::new(ArcSwapOption::empty());
        let mut p = ModelPicker::new(models);
        p.open();
        assert!(p.is_open());
    }

    #[test]
    fn refresh_populates_when_models_arrive() {
        let models = Arc::new(ArcSwapOption::empty());
        let mut p = ModelPicker::new(models.clone());
        p.open();
        assert_eq!(p.last_spec_count, 0);

        models.store(Some(Arc::new(vec![
            "anthropic/claude-sonnet-4-20250514".into(),
        ])));
        p.try_refresh();
        assert_eq!(p.last_spec_count, 1);
    }

    #[test]
    fn refresh_updates_items_and_preserves_search() {
        let models = Arc::new(ArcSwapOption::empty());
        models.store(Some(Arc::new(vec![
            "anthropic/claude-sonnet-4-20250514".into(),
        ])));
        let mut p = ModelPicker::new(models.clone());
        p.open();

        p.handle_key(key(KeyCode::Char('o')));
        p.handle_key(key(KeyCode::Char('p')));

        models.store(Some(Arc::new(vec![
            "anthropic/claude-sonnet-4-20250514".into(),
            "anthropic/claude-opus-4-6-20260101".into(),
        ])));
        p.try_refresh();

        assert_eq!(p.last_spec_count, 2);
        let action = p.handle_key(key(KeyCode::Enter));
        assert!(
            matches!(action, ModelPickerAction::Select(ref s) if s.contains("opus")),
            "after refresh, 'op' filter should match opus"
        );
    }

    #[test]
    fn parse_model_entry_valid() {
        let entry = parse_model_entry("anthropic/claude-sonnet-4-20250514").unwrap();
        assert_eq!(entry.id, "claude-sonnet-4-20250514");
        assert_eq!(entry.provider_display, "Anthropic");
        assert!(!entry.tier.is_empty());
    }

    #[test]
    fn parse_model_entry_no_slash() {
        assert!(parse_model_entry("no-slash").is_none());
    }
}

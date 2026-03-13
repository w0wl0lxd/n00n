use std::collections::VecDeque;
use std::fs;

use crate::{DataDir, StorageError, atomic_write};

const HISTORY_FILE: &str = "input_history.json";
const MAX_ENTRIES: usize = 100;

#[derive(Debug, Default)]
pub struct InputHistory(VecDeque<String>);

impl InputHistory {
    pub fn load(dir: &DataDir) -> Self {
        let path = dir.path().join(HISTORY_FILE);
        let data = match fs::read(&path) {
            Ok(d) => d,
            Err(_) => return Self(VecDeque::new()),
        };
        let entries: VecDeque<String> = serde_json::from_slice(&data).unwrap_or_default();
        let mut history = Self(VecDeque::with_capacity(MAX_ENTRIES));
        for entry in entries {
            history.push_inner(entry);
        }
        history
    }

    pub fn save(&self, dir: &DataDir) -> Result<(), StorageError> {
        let data = serde_json::to_vec(&self.0)?;
        atomic_write(&dir.path().join(HISTORY_FILE), &data)
    }

    pub fn push(&mut self, entry: String) {
        let trimmed = entry.trim().to_string();
        if trimmed.is_empty() {
            return;
        }
        self.push_inner(trimmed);
    }

    fn push_inner(&mut self, entry: String) {
        if self.0.back().is_some_and(|last| *last == entry) {
            return;
        }
        if self.0.len() == MAX_ENTRIES {
            self.0.pop_front();
        }
        self.0.push_back(entry);
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn get(&self, index: usize) -> Option<&str> {
        self.0.get(index).map(String::as_str)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use test_case::test_case;

    fn tmp_dir() -> (tempfile::TempDir, DataDir) {
        let tmp = tempfile::tempdir().unwrap();
        let dir = DataDir::from_path(tmp.path().to_path_buf());
        (tmp, dir)
    }

    #[test]
    fn roundtrip() {
        let (_tmp, dir) = tmp_dir();
        let mut history = InputHistory::load(&dir);
        history.push("a".into());
        history.push("b".into());
        history.push("c".into());
        history.save(&dir).unwrap();
        let loaded = InputHistory::load(&dir);
        assert_eq!(loaded.len(), 3);
        assert_eq!(loaded.get(0), Some("a"));
        assert_eq!(loaded.get(2), Some("c"));
    }

    #[test]
    fn truncates_to_max_entries() {
        let mut history = InputHistory::default();
        for i in 0..150 {
            history.push(format!("entry{i}"));
        }
        assert_eq!(history.len(), MAX_ENTRIES);
        assert_eq!(history.get(0), Some("entry50"));
        assert_eq!(history.get(MAX_ENTRIES - 1), Some("entry149"));
    }

    #[test]
    fn rejects_consecutive_duplicates() {
        let mut history = InputHistory::default();
        history.push("a".into());
        history.push("a".into());
        history.push("b".into());
        history.push("b".into());
        history.push("a".into());
        assert_eq!(history.len(), 3);
        assert_eq!(history.get(0), Some("a"));
        assert_eq!(history.get(1), Some("b"));
        assert_eq!(history.get(2), Some("a"));
    }

    #[test]
    fn push_trims_and_rejects_blank() {
        let mut history = InputHistory::default();
        history.push("".into());
        history.push("   ".into());
        history.push("\n".into());
        assert!(history.is_empty());

        history.push("  hello  ".into());
        assert_eq!(history.get(0), Some("hello"));
    }

    #[test_case(None      ; "missing_file")]
    #[test_case(Some(b"not json" as &[u8]) ; "corrupt_file")]
    fn load_bad_state_returns_empty(content: Option<&[u8]>) {
        let (_tmp, dir) = tmp_dir();
        if let Some(data) = content {
            fs::write(dir.path().join(HISTORY_FILE), data).unwrap();
        }
        let history = InputHistory::load(&dir);
        assert!(history.is_empty());
    }
}

use crate::components::list_picker::{ListPicker, PickerAction, PickerItem};

use crate::AppSession;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use jiff::Timestamp;
use maki_storage::DataDir;
use ratatui::Frame;
use ratatui::layout::{Position, Rect};

const TITLE: &str = " Sessions ";
const NO_SESSIONS_MSG: &str = "No previous sessions";

pub enum SessionPickerAction {
    Consumed,
    Select(String),
    ConfirmDelete,
    Delete(String),
    Close,
}

struct SessionEntry {
    id: String,
    title: String,
    relative_time: String,
}

impl PickerItem for SessionEntry {
    fn label(&self) -> &str {
        &self.title
    }
    fn detail(&self) -> Option<&str> {
        Some(&self.relative_time)
    }
}

pub struct SessionPicker {
    picker: ListPicker<SessionEntry>,
    confirming: Option<(String, u64)>,
}

impl SessionPicker {
    pub fn new() -> Self {
        Self {
            picker: ListPicker::new(),
            confirming: None,
        }
    }

    pub fn open(
        &mut self,
        cwd: &str,
        current_session_id: &str,
        dir: &DataDir,
    ) -> Result<(), String> {
        let summaries =
            AppSession::list(cwd, dir).map_err(|e| format!("Failed to list sessions: {e}"))?;
        let entries: Vec<SessionEntry> = summaries
            .into_iter()
            .filter(|s| s.id != current_session_id)
            .map(|s| SessionEntry {
                id: s.id,
                title: s.title,
                relative_time: format_relative_time(s.updated_at),
            })
            .collect();
        if entries.is_empty() {
            return Err(NO_SESSIONS_MSG.into());
        }
        self.picker.open(entries, TITLE);
        Ok(())
    }

    pub fn is_open(&self) -> bool {
        self.picker.is_open()
    }

    pub fn close(&mut self) {
        self.picker.close();
    }

    pub fn remove_entry(&mut self, id: &str) {
        self.picker.retain(|e| e.id != id);
    }

    pub fn contains(&self, pos: Position) -> bool {
        self.picker.contains(pos)
    }

    pub fn scroll(&mut self, delta: i32) {
        self.picker.scroll(delta);
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> SessionPickerAction {
        if key.modifiers.contains(KeyModifiers::CONTROL)
            && !key.modifiers.contains(KeyModifiers::ALT)
            && key.code == KeyCode::Char('d')
        {
            return self.handle_delete_key();
        }

        match self.picker.handle_key(key) {
            PickerAction::Consumed => SessionPickerAction::Consumed,
            PickerAction::Select(_, entry) => SessionPickerAction::Select(entry.id),
            PickerAction::Close => SessionPickerAction::Close,
        }
    }

    fn handle_delete_key(&mut self) -> SessionPickerAction {
        let Some(selected) = self.picker.selected_item() else {
            return SessionPickerAction::Consumed;
        };

        let generation = self.picker.generation();
        if self
            .confirming
            .as_ref()
            .is_some_and(|(id, g)| id == &selected.id && *g == generation)
        {
            return SessionPickerAction::Delete(selected.id.clone());
        }

        self.confirming = Some((selected.id.clone(), generation));
        SessionPickerAction::ConfirmDelete
    }

    pub fn view(&mut self, frame: &mut Frame, area: Rect) {
        self.picker.view(frame, area);
    }
}

fn format_relative_time(epoch_secs: u64) -> String {
    let ts = Timestamp::from_second(epoch_secs as i64).unwrap_or(Timestamp::UNIX_EPOCH);
    let now = Timestamp::now();
    let secs = now.as_second().saturating_sub(ts.as_second()).max(0) as u64;
    humanize_secs(secs)
}

fn humanize_secs(secs: u64) -> String {
    const MINUTE: u64 = 60;
    const HOUR: u64 = 3600;
    const DAY: u64 = 86400;
    const WEEK: u64 = 604800;
    const MONTH: u64 = 2592000;
    const YEAR: u64 = 31536000;

    match secs {
        0..MINUTE => "just now".into(),
        MINUTE..HOUR => format!("{}m ago", secs / MINUTE),
        HOUR..DAY => format!("{}h ago", secs / HOUR),
        DAY..WEEK => format!("{}d ago", secs / DAY),
        WEEK..MONTH => format!("{}w ago", secs / WEEK),
        MONTH..YEAR => format!("{}mo ago", secs / MONTH),
        _ => format!("{}y ago", secs / YEAR),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use test_case::test_case;

    #[test_case(0, "just now" ; "below_minute")]
    #[test_case(60, "1m ago" ; "minute_boundary")]
    #[test_case(3600, "1h ago" ; "hour_boundary")]
    #[test_case(86400, "1d ago" ; "day_boundary")]
    #[test_case(604800, "1w ago" ; "week_boundary")]
    #[test_case(2592000, "1mo ago" ; "month_boundary")]
    #[test_case(31536000, "1y ago" ; "year_boundary")]
    fn relative_time_formatting(secs: u64, expected: &str) {
        assert_eq!(humanize_secs(secs), expected);
    }
}

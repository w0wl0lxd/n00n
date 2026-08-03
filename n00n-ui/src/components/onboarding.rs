use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use n00n_storage::{StateDir, StorageError, atomic_write};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Wrap};

use crate::components::Overlay;
use crate::components::modal::Modal;
use crate::theme;

const MARKER: &str = "welcome-seen";
const TITLE: &str = " Welcome to n00n ";

pub struct Onboarding {
    open: bool,
}

impl Onboarding {
    pub fn new(storage: &StateDir) -> Self {
        Self {
            open: !has_seen(storage),
        }
    }

    pub fn is_open(&self) -> bool {
        self.open
    }

    pub fn open(&mut self) {
        self.open = true;
    }

    pub fn close(&mut self) {
        self.open = false;
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> bool {
        if !self.open {
            return false;
        }
        if key
            .modifiers
            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
        {
            return true;
        }
        match key.code {
            KeyCode::Enter | KeyCode::Char(' ' | 'q') | KeyCode::Esc => {
                self.close();
                true
            }
            _ => true,
        }
    }

    pub fn mark_seen(storage: &StateDir) -> Result<(), StorageError> {
        atomic_write(&storage.path().join(MARKER), b"welcome version 1\n")
    }

    pub fn view(&self, frame: &mut Frame, area: Rect) -> Rect {
        if !self.open {
            return Rect::default();
        }
        let t = theme::current();
        let compact = area.width < 80;
        let lines = if compact {
            vec![
                Line::raw(""),
                Line::from(Span::styled("  Safe, clear tool use", t.foreground)),
                Line::raw(""),
                Line::from(Span::styled(
                    "  Prompts show scope before tools run.",
                    t.tool_dim,
                )),
                Line::from(Span::styled("  Type to search pickers.", t.tool_dim)),
                Line::from(Span::styled("  Enter selects. Esc closes.", t.tool_dim)),
                Line::raw(""),
                Line::from(Span::styled(
                    "  Enter to start · /welcome to reopen",
                    t.accent,
                )),
                Line::raw(""),
            ]
        } else {
            vec![
                Line::raw(""),
                Line::from(Span::styled(
                    "  A short guide to safe, clear tool use.",
                    t.foreground,
                )),
                Line::raw(""),
                Line::from(vec![
                    Span::styled("  Permission prompts  ", t.keybind_section),
                    Span::styled(
                        "show what a tool will do and where. You choose each time.",
                        t.tool_dim,
                    ),
                ]),
                Line::from(vec![
                    Span::styled("  Pickers              ", t.keybind_section),
                    Span::styled(
                        "accept typing to search; Enter selects; Esc closes.",
                        t.tool_dim,
                    ),
                ]),
                Line::from(vec![
                    Span::styled("  Tool output          ", t.keybind_section),
                    Span::styled(
                        "keeps full names and lets you expand long sections.",
                        t.tool_dim,
                    ),
                ]),
                Line::raw(""),
                Line::from(Span::styled(
                    "  Press Enter or Space to start. Reopen this guide with /welcome.",
                    t.accent,
                )),
                Line::raw(""),
            ]
        };
        let width_percent = if compact { 90 } else { 72 };
        let content_width =
            crate::cast::u32_to_u16(u32::from(area.width) * u32::from(width_percent) / 100)
                .saturating_sub(2)
                .max(1);
        let paragraph = Paragraph::new(lines).wrap(Wrap { trim: false });
        let height = match u16::try_from(paragraph.line_count(content_width)) {
            Ok(height) => height,
            Err(error) => {
                tracing::warn!(%error, "welcome guide height exceeds terminal limits");
                u16::MAX
            }
        };
        let modal = Modal {
            title: TITLE,
            width_percent,
            max_height_percent: 80,
        };
        let (popup, inner) = modal.render(frame, area, height);
        frame.render_widget(paragraph, inner);
        popup
    }
}

impl Overlay for Onboarding {
    fn is_open(&self) -> bool {
        self.is_open()
    }

    fn close(&mut self) {
        self.close();
    }
}

fn has_seen(storage: &StateDir) -> bool {
    match std::fs::metadata(storage.path().join(MARKER)) {
        Ok(metadata) => metadata.is_file(),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
        Err(error) => {
            tracing::warn!(error = %error, "cannot read onboarding marker; showing welcome guide");
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::KeyEvent;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use tempfile::TempDir;
    use test_case::test_case;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    #[test]
    fn first_run_is_open_and_dismissal_persists() {
        let temp = TempDir::new().expect("temp dir");
        let storage = StateDir::from_path(temp.path().to_path_buf());
        let mut onboarding = Onboarding::new(&storage);
        assert!(onboarding.is_open());
        assert!(onboarding.handle_key(key(KeyCode::Enter)));
        Onboarding::mark_seen(&storage).expect("marker writes");
        assert!(!Onboarding::new(&storage).is_open());
    }

    #[test_case(40 ; "narrow_width")]
    #[test_case(79 ; "compact_width")]
    #[test_case(80 ; "standard_width")]
    #[test_case(100 ; "wide_width")]
    fn wrapped_content_keeps_reopen_hint_visible(width: u16) {
        let temp = TempDir::new().expect("temp dir");
        let storage = StateDir::from_path(temp.path().to_path_buf());
        let onboarding = Onboarding::new(&storage);
        let mut terminal = Terminal::new(TestBackend::new(width, 40)).expect("terminal");

        terminal
            .draw(|frame| {
                onboarding.view(frame, frame.area());
            })
            .expect("render");

        let rendered = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(ratatui::buffer::Cell::symbol)
            .collect::<String>();
        assert!(rendered.contains("/welcome"));
    }

    #[test]
    fn keyboard_dismissal_is_safe_on_narrow_layout() {
        let temp = TempDir::new().expect("temp dir");
        let storage = StateDir::from_path(temp.path().to_path_buf());
        let mut onboarding = Onboarding::new(&storage);
        assert!(onboarding.handle_key(key(KeyCode::Esc)));
        assert!(!onboarding.is_open());
    }
}

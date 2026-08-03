use crate::animation::{animation_elapsed_ms, spinner_frame};
use crate::theme;
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};

const MAX_LINES: u16 = 4;
const MIN_LINES: u16 = 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActivityState {
    Running,
    Complete,
    Error,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActivityItem {
    pub label: String,
    pub detail: String,
    pub state: ActivityState,
}

pub struct ActivityShelf;

impl ActivityShelf {
    #[must_use]
    pub fn height(item_count: usize, width: u16) -> u16 {
        if item_count == 0 || width == 0 {
            return 0;
        }
        let item_lines = match u16::try_from(item_count) {
            Ok(lines) => lines,
            Err(_) => u16::MAX,
        };
        MIN_LINES.saturating_add(item_lines).min(MAX_LINES)
    }

    pub fn view(frame: &mut Frame, area: Rect, items: &[ActivityItem]) {
        if area.width == 0 || area.height == 0 || items.is_empty() {
            return;
        }
        let t = theme::current();
        let block = Block::default()
            .borders(Borders::TOP)
            .border_style(t.border);
        let inner = block.inner(area);
        frame.render_widget(block, area);
        if inner.height == 0 {
            return;
        }

        let title = Line::from(Span::styled(" Activity", t.panel_title));
        frame.render_widget(
            Paragraph::new(title),
            Rect::new(inner.x, inner.y, inner.width, 1),
        );
        if inner.height < 2 {
            return;
        }

        let available = inner.height.saturating_sub(1);
        let lines: Vec<Line<'_>> = items
            .iter()
            .take(usize::from(available))
            .map(|item| {
                let (marker, style) = match item.state {
                    ActivityState::Running => {
                        let marker = if theme::reduced_motion() {
                            "*"
                        } else {
                            match spinner_frame(animation_elapsed_ms()) {
                                '⠋' | '⠙' | '⠹' | '⠸' | '⠼' => "*",
                                _ => ".",
                            }
                        };
                        (marker, t.activity_running)
                    }
                    ActivityState::Complete => ("+", t.activity_success),
                    ActivityState::Error => ("!", t.activity_error),
                };
                Line::from(vec![
                    Span::styled(format!(" {marker} "), style),
                    Span::styled(item.label.clone(), t.text_primary),
                    Span::styled(format!("  {}", item.detail), t.text_secondary),
                ])
            })
            .collect();
        frame.render_widget(
            Paragraph::new(lines),
            Rect::new(inner.x, inner.y + 1, inner.width, available),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::{Terminal, backend::TestBackend, style::Style};

    fn item(state: ActivityState) -> ActivityItem {
        ActivityItem {
            label: "Agent".into(),
            detail: "processing".into(),
            state,
        }
    }

    #[test]
    fn shelf_is_two_to_four_lines() {
        assert_eq!(ActivityShelf::height(0, 100), 0);
        assert_eq!(ActivityShelf::height(1, 100), 3);
        assert_eq!(ActivityShelf::height(5, 100), 4);
    }

    #[test]
    fn shelf_renders_state_text_without_animation_requirement() {
        let backend = TestBackend::new(40, 4);
        let mut terminal = Terminal::new(backend).expect("test terminal");
        terminal
            .draw(|frame| ActivityShelf::view(frame, frame.area(), &[item(ActivityState::Running)]))
            .expect("draw shelf");
        let text: String = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(ratatui::buffer::Cell::symbol)
            .collect();
        assert!(text.contains("Activity"));
        assert!(text.contains("processing"));
    }

    #[test]
    fn activity_states_have_distinct_styles() {
        let t = theme::current();
        assert_ne!(t.activity_running, Style::default());
        assert_ne!(t.activity_success, t.activity_error);
    }
}

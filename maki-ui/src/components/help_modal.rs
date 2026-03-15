use crate::components::keybindings::{ALL_CONTEXTS, KEYBINDS, key};
use crate::components::modal::Modal;
use crate::components::scrollbar::render_vertical_scrollbar;
use crate::theme;

use crossterm::event::{KeyCode, KeyEvent};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

const TITLE: &str = " Keybindings ";
const KEY_COL_WIDTH: usize = 16;
const HALF_PAGE: u16 = 12;

pub struct HelpModal {
    open: bool,
    scroll_offset: u16,
}

impl HelpModal {
    pub fn new() -> Self {
        Self {
            open: false,
            scroll_offset: 0,
        }
    }

    pub fn is_open(&self) -> bool {
        self.open
    }

    pub fn toggle(&mut self) {
        self.open = !self.open;
        self.scroll_offset = 0;
    }

    pub fn close(&mut self) {
        self.open = false;
        self.scroll_offset = 0;
    }

    /// Returns `true` if the key was consumed by the modal.
    pub fn handle_key(&mut self, key_event: KeyEvent) -> bool {
        let close = key_event.code == KeyCode::Esc
            || key::HELP.matches(key_event)
            || key::QUIT.matches(key_event);
        if close {
            self.close();
            return true;
        }
        match key_event.code {
            KeyCode::Up => {
                self.scroll_offset = self.scroll_offset.saturating_sub(1);
                true
            }
            KeyCode::Down => {
                self.scroll_offset = self.scroll_offset.saturating_add(1);
                true
            }
            _ if key::SCROLL_HALF_UP.matches(key_event) => {
                self.scroll_offset = self.scroll_offset.saturating_sub(HALF_PAGE);
                true
            }
            _ if key::SCROLL_HALF_DOWN.matches(key_event) => {
                self.scroll_offset = self.scroll_offset.saturating_add(HALF_PAGE);
                true
            }
            _ => true,
        }
    }

    pub fn view(&self, frame: &mut Frame, area: Rect) {
        if !self.open {
            return;
        }

        let mut lines: Vec<Line> = Vec::new();
        let theme = theme::current();

        let mut first = true;
        for &ctx in ALL_CONTEXTS {
            if ctx.parent().is_some() {
                continue;
            }
            if !first {
                lines.push(Line::default());
            }
            first = false;

            lines.push(Line::from(Span::styled(
                format!("  {}", ctx.label()),
                theme.keybind_section,
            )));

            for kb in KEYBINDS.iter().filter(|kb| kb.context == ctx) {
                lines.push(Line::from(vec![
                    Span::styled(format!("  {:KEY_COL_WIDTH$}", kb.key), theme.keybind_key),
                    Span::styled(kb.description, theme.keybind_desc),
                ]));
            }

            for &child in ALL_CONTEXTS {
                if child.parent() != Some(ctx) {
                    continue;
                }
                let child_binds: Vec<_> =
                    KEYBINDS.iter().filter(|kb| kb.context == child).collect();
                if child_binds.is_empty() {
                    continue;
                }
                lines.push(Line::default());
                lines.push(Line::from(Span::styled(
                    format!("    {}", child.label()),
                    theme.keybind_section,
                )));
                for kb in child_binds {
                    lines.push(Line::from(vec![
                        Span::styled(
                            format!("    {:width$}", kb.key, width = KEY_COL_WIDTH - 2),
                            theme.keybind_key,
                        ),
                        Span::styled(kb.description, theme.keybind_desc),
                    ]));
                }
            }
        }

        let total = lines.len() as u16;
        let modal = Modal {
            title: TITLE,
            width_percent: 50,
            max_height_percent: 80,
        };
        let inner = modal.render(frame, area, total);
        let viewport_h = inner.height;
        let max_scroll = total.saturating_sub(viewport_h);
        let scroll = self.scroll_offset.min(max_scroll);

        let paragraph = Paragraph::new(lines).scroll((scroll, 0));
        frame.render_widget(paragraph, inner);

        if total > viewport_h {
            render_vertical_scrollbar(frame, inner, total, scroll);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::components::key as key_ev;
    use crossterm::event::KeyCode;
    use test_case::test_case;

    #[test_case(key_ev(KeyCode::Esc)       ; "esc_closes")]
    #[test_case(key::QUIT.to_key_event()    ; "ctrl_c_closes")]
    #[test_case(key::HELP.to_key_event()    ; "ctrl_h_closes")]
    fn handle_key_closes(k: KeyEvent) {
        let mut modal = HelpModal::new();
        modal.toggle();
        assert!(modal.handle_key(k));
        assert!(!modal.is_open());
    }

    #[test]
    fn handle_key_consumes_all() {
        let mut modal = HelpModal::new();
        modal.toggle();
        assert!(modal.handle_key(key_ev(KeyCode::Char('a'))));
        assert!(modal.is_open());
    }
}

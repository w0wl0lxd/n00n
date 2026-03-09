use crate::components::keybindings::{KeybindContext, active_keybinds, key};
use crate::components::modal::Modal;
use crate::theme;

use crossterm::event::{KeyCode, KeyEvent};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

const TITLE: &str = " Keybindings ";
const KEY_COL_WIDTH: usize = 16;

pub struct HelpModal {
    open: bool,
}

impl HelpModal {
    pub fn new() -> Self {
        Self { open: false }
    }

    pub fn is_open(&self) -> bool {
        self.open
    }

    pub fn toggle(&mut self) {
        self.open = !self.open;
    }

    pub fn close(&mut self) {
        self.open = false;
    }

    /// Returns `true` if the key closed the modal.
    pub fn handle_key(&mut self, key: KeyEvent) -> bool {
        let close = key.code == KeyCode::Esc || key::HELP.matches(key) || key::QUIT.matches(key);
        if close {
            self.open = false;
        }
        close
    }

    pub fn view(&self, frame: &mut Frame, area: Rect, contexts: &[KeybindContext]) {
        if !self.open {
            return;
        }

        let keybinds = active_keybinds(contexts);
        let mut lines: Vec<Line> = Vec::new();

        let mut current_ctx: Option<KeybindContext> = None;
        for kb in &keybinds {
            if current_ctx != Some(kb.context) {
                if current_ctx.is_some() {
                    lines.push(Line::default());
                }
                lines.push(Line::from(Span::styled(
                    format!("── {} ──", kb.context.label()),
                    theme::current().keybind_section,
                )));
                current_ctx = Some(kb.context);
            }

            let key_display = format!("{:width$}", kb.key, width = KEY_COL_WIDTH);
            lines.push(Line::from(vec![
                Span::styled(key_display, theme::current().keybind_key),
                Span::styled(kb.description, theme::current().keybind_desc),
            ]));
        }

        let modal = Modal {
            title: TITLE,
            width_percent: 50,
            max_height_percent: 80,
        };
        let inner = modal.render(frame, area, lines.len() as u16);

        let paragraph = Paragraph::new(lines);
        frame.render_widget(paragraph, inner);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::components::key as key_ev;
    use crate::components::keybindings::key as kb;
    use crossterm::event::{KeyCode, KeyEvent};
    use test_case::test_case;

    #[test_case(key_ev(KeyCode::Esc),        true  ; "esc_closes")]
    #[test_case(kb::QUIT.to_key_event(),     true  ; "ctrl_c_closes")]
    #[test_case(kb::HELP.to_key_event(),     true  ; "ctrl_h_closes")]
    #[test_case(key_ev(KeyCode::Char('a')),  false ; "other_key_stays_open")]
    fn handle_key_close_behavior(key: KeyEvent, should_close: bool) {
        let mut modal = HelpModal::new();
        modal.toggle();
        assert_eq!(modal.handle_key(key), should_close);
        assert_eq!(modal.is_open(), !should_close);
    }
}

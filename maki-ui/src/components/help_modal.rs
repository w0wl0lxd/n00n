use crate::components::keybindings::{KeybindContext, active_keybinds};
use crate::theme;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Frame;
use ratatui::layout::{Constraint, Flex, Layout, Rect};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Clear, Paragraph};

const TITLE: &str = " Keybindings ";
const MIN_WIDTH_PERCENT: u16 = 50;
const MAX_HEIGHT_PERCENT: u16 = 80;
const CHROME_LINES: u16 = 2;
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
        let close = key.code == KeyCode::Esc
            || (key.modifiers.contains(KeyModifiers::CONTROL)
                && matches!(key.code, KeyCode::Char('h') | KeyCode::Char('c')));
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
                    theme::KEYBIND_SECTION,
                )));
                current_ctx = Some(kb.context);
            }

            let key_display = format!("{:width$}", kb.key, width = KEY_COL_WIDTH);
            lines.push(Line::from(vec![
                Span::styled(key_display, theme::KEYBIND_KEY),
                Span::styled(kb.description, theme::FOREGROUND_STYLE),
            ]));
        }

        let max_h = (area.height as u32 * MAX_HEIGHT_PERCENT as u32 / 100) as u16;
        let content_rows = lines.len() as u16;
        let total_h = (content_rows + CHROME_LINES)
            .min(max_h)
            .max(CHROME_LINES + 1);

        let [popup] = Layout::vertical([Constraint::Length(total_h)])
            .flex(Flex::Center)
            .areas(area);
        let [popup] = Layout::horizontal([Constraint::Percentage(MIN_WIDTH_PERCENT)])
            .flex(Flex::Center)
            .areas(popup);

        frame.render_widget(Clear, popup);

        let block = Block::bordered()
            .border_type(BorderType::Rounded)
            .border_style(theme::PANEL_BORDER)
            .title(TITLE)
            .title_style(theme::PANEL_TITLE)
            .style(Style::new().bg(theme::BACKGROUND));

        let inner = block.inner(popup);
        frame.render_widget(block, popup);

        let paragraph = Paragraph::new(lines);
        frame.render_widget(paragraph, inner);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::components::{ctrl, key};
    use crossterm::event::KeyCode;

    #[test]
    fn esc_closes() {
        let mut modal = HelpModal::new();
        modal.toggle();
        assert!(modal.handle_key(key(KeyCode::Esc)));
        assert!(!modal.is_open());
    }

    #[test]
    fn ctrl_c_closes() {
        let mut modal = HelpModal::new();
        modal.toggle();
        assert!(modal.handle_key(ctrl('c')));
        assert!(!modal.is_open());
    }

    #[test]
    fn ctrl_h_closes() {
        let mut modal = HelpModal::new();
        modal.toggle();
        assert!(modal.handle_key(ctrl('h')));
        assert!(!modal.is_open());
    }

    #[test]
    fn other_keys_consumed() {
        let mut modal = HelpModal::new();
        modal.toggle();
        assert!(!modal.handle_key(key(KeyCode::Char('a'))));
        assert!(modal.is_open());
    }
}

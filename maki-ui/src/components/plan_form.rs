use crate::components::Overlay;
use crate::components::keybindings::key;
use crate::theme;

use crossterm::event::{KeyCode, KeyEvent};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Paragraph, Wrap};

const FORM_LABEL: &str = " Plan complete ";
const HINT_BAR: &str = " ↑↓ select  Enter confirm  Ctrl+O edit plan  Esc dismiss";

struct MenuItem {
    label: &'static str,
    desc: &'static str,
    action: fn() -> PlanFormAction,
}

const MENU: &[MenuItem] = &[
    MenuItem {
        label: "Clear context and implement",
        desc: "  Start fresh session, then implement the plan",
        action: || PlanFormAction::ClearAndImplement,
    },
    MenuItem {
        label: "Implement plan",
        desc: "  Keep current context, implement the plan",
        action: || PlanFormAction::Implement,
    },
    MenuItem {
        label: "Continue iterating",
        desc: "  Stay in plan mode for more refinement",
        action: || PlanFormAction::Continue,
    },
];

// 2 borders + 1 empty line + 1 hint bar
const CHROME_LINES: u16 = 4;
const FORM_HEIGHT: u16 = MENU.len() as u16 + CHROME_LINES;

pub enum PlanFormAction {
    Consumed,
    ClearAndImplement,
    Implement,
    Continue,
    OpenEditor,
    Dismiss,
}

pub struct PlanForm {
    visible: bool,
    selected: usize,
}

impl PlanForm {
    pub fn new() -> Self {
        Self {
            visible: false,
            selected: 0,
        }
    }

    pub fn is_visible(&self) -> bool {
        self.visible
    }

    pub fn open(&mut self) {
        self.visible = true;
        self.selected = 0;
    }

    pub fn close(&mut self) {
        self.visible = false;
    }

    pub fn height() -> u16 {
        FORM_HEIGHT
    }

    pub fn handle_key(&mut self, key_event: KeyEvent) -> PlanFormAction {
        if key::QUIT.matches(key_event) || key_event.code == KeyCode::Esc {
            return PlanFormAction::Dismiss;
        }
        if key::OPEN_EDITOR.matches(key_event) {
            return PlanFormAction::OpenEditor;
        }
        match key_event.code {
            KeyCode::Up => {
                self.selected = self.selected.saturating_sub(1);
                PlanFormAction::Consumed
            }
            KeyCode::Down => {
                self.selected = (self.selected + 1).min(MENU.len() - 1);
                PlanFormAction::Consumed
            }
            KeyCode::Enter => (MENU[self.selected].action)(),
            _ => PlanFormAction::Consumed,
        }
    }

    pub fn view(&self, frame: &mut Frame, area: Rect) {
        if !self.visible {
            return;
        }

        let t = theme::current();
        let mut lines: Vec<Line<'static>> = Vec::with_capacity(MENU.len() + 1);

        for (i, item) in MENU.iter().enumerate() {
            let (prefix, style) = if i == self.selected {
                ("▸ ", t.form_active)
            } else {
                ("  ", Style::new().fg(t.foreground))
            };
            lines.push(Line::from(vec![
                Span::styled(prefix, t.form_arrow),
                Span::styled(item.label, style),
                Span::styled(item.desc, t.form_description),
            ]));
        }

        lines.push(Line::default());
        lines.push(Line::from(Span::styled(HINT_BAR, t.form_hint)));

        let block = Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(t.panel_border)
            .title_top(Line::from(FORM_LABEL).left_aligned())
            .title_style(t.panel_title);

        let paragraph = Paragraph::new(lines)
            .style(Style::new().fg(t.foreground))
            .wrap(Wrap { trim: false })
            .block(block);

        frame.render_widget(paragraph, area);
    }
}

impl Overlay for PlanForm {
    fn is_open(&self) -> bool {
        self.visible
    }

    fn close(&mut self) {
        self.visible = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyEventKind, KeyEventState, KeyModifiers};
    use test_case::test_case;

    fn k(code: KeyCode) -> KeyEvent {
        KeyEvent {
            code,
            modifiers: KeyModifiers::NONE,
            kind: KeyEventKind::Press,
            state: KeyEventState::NONE,
        }
    }

    #[test]
    fn open_resets_selected() {
        let mut form = PlanForm::new();
        form.selected = 2;
        form.open();
        assert!(form.is_visible());
        assert_eq!(form.selected, 0);
    }

    #[test_case(0, KeyCode::Up,   0 ; "up_at_zero_stays")]
    #[test_case(0, KeyCode::Down, 1 ; "down_from_zero")]
    #[test_case(2, KeyCode::Down, 2 ; "down_at_max_stays")]
    #[test_case(2, KeyCode::Up,   1 ; "up_from_max")]
    fn navigation(start: usize, code: KeyCode, expected: usize) {
        let mut form = PlanForm::new();
        form.open();
        form.selected = start;
        let action = form.handle_key(k(code));
        assert!(matches!(action, PlanFormAction::Consumed));
        assert_eq!(form.selected, expected);
    }

    #[test_case(0, "ClearAndImplement" ; "enter_at_0")]
    #[test_case(1, "Implement"         ; "enter_at_1")]
    #[test_case(2, "Continue"          ; "enter_at_2")]
    fn enter_dispatches(selected: usize, expected: &str) {
        let mut form = PlanForm::new();
        form.open();
        form.selected = selected;
        let action = form.handle_key(k(KeyCode::Enter));
        let name = match action {
            PlanFormAction::ClearAndImplement => "ClearAndImplement",
            PlanFormAction::Implement => "Implement",
            PlanFormAction::Continue => "Continue",
            _ => "other",
        };
        assert_eq!(name, expected);
    }

    #[test]
    fn esc_dismisses() {
        let mut form = PlanForm::new();
        form.open();
        assert!(matches!(
            form.handle_key(k(KeyCode::Esc)),
            PlanFormAction::Dismiss
        ));
    }

    #[test]
    fn ctrl_c_dismisses() {
        let mut form = PlanForm::new();
        form.open();
        assert!(matches!(
            form.handle_key(key::QUIT.to_key_event()),
            PlanFormAction::Dismiss
        ));
    }

    #[test]
    fn ctrl_o_opens_editor() {
        let mut form = PlanForm::new();
        form.open();
        assert!(matches!(
            form.handle_key(key::OPEN_EDITOR.to_key_event()),
            PlanFormAction::OpenEditor
        ));
    }

    #[test]
    fn unknown_key_consumed() {
        let mut form = PlanForm::new();
        form.open();
        assert!(matches!(
            form.handle_key(k(KeyCode::Char('x'))),
            PlanFormAction::Consumed
        ));
    }

    #[test]
    fn height_constant() {
        assert_eq!(PlanForm::height(), FORM_HEIGHT);
    }
}

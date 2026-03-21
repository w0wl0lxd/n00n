use crate::components::form::{overlay_impl, render_form, selected_prefix};
use crate::components::hint_line;
use crate::components::keybindings::key;
use crate::theme;

use crossterm::event::{KeyCode, KeyEvent};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};

const FORM_LABEL: &str = " Plan complete ";
const HINT_PAIRS: &[(&str, &str)] = &[
    ("↑↓", "select"),
    ("Enter", "confirm"),
    ("Ctrl+O", "edit plan"),
    ("Esc", "dismiss"),
];

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

#[derive(Debug, PartialEq)]
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
            let (prefix, style) = selected_prefix(&t, i == self.selected);
            lines.push(Line::from(vec![
                Span::styled(prefix, t.form_arrow),
                Span::styled(item.label, style),
                Span::styled(item.desc, t.form_description),
            ]));
        }

        lines.push(Line::default());
        lines.push(hint_line(HINT_PAIRS));

        render_form(&t, FORM_LABEL, frame, area, lines, (0, 0));
    }
}

overlay_impl!(PlanForm);

#[cfg(test)]
mod tests {
    use super::*;
    use crate::components::key;
    use test_case::test_case;

    const LAST: usize = MENU.len() - 1;

    #[test]
    fn open_resets_selected() {
        let mut form = PlanForm::new();
        form.selected = 2;
        form.open();
        assert!(form.is_visible());
        assert_eq!(form.selected, 0);
    }

    #[test_case(0, KeyCode::Up,   0    ; "up_at_zero_stays")]
    #[test_case(0, KeyCode::Down, 1    ; "down_from_zero")]
    #[test_case(LAST, KeyCode::Down, LAST ; "down_at_max_stays")]
    #[test_case(LAST, KeyCode::Up, LAST - 1 ; "up_from_max")]
    fn navigation(start: usize, code: KeyCode, expected: usize) {
        let mut form = PlanForm::new();
        form.open();
        form.selected = start;
        assert_eq!(form.handle_key(key(code)), PlanFormAction::Consumed);
        assert_eq!(form.selected, expected);
    }

    #[test_case(0, PlanFormAction::ClearAndImplement ; "enter_at_0")]
    #[test_case(1, PlanFormAction::Implement         ; "enter_at_1")]
    #[test_case(2, PlanFormAction::Continue           ; "enter_at_2")]
    fn enter_dispatches(selected: usize, expected: PlanFormAction) {
        let mut form = PlanForm::new();
        form.open();
        form.selected = selected;
        assert_eq!(form.handle_key(key(KeyCode::Enter)), expected);
    }

    #[test_case(key(KeyCode::Esc)           ; "esc")]
    #[test_case(key::QUIT.to_key_event() ; "ctrl_c")]
    fn dismiss(k: KeyEvent) {
        let mut form = PlanForm::new();
        form.open();
        assert_eq!(form.handle_key(k), PlanFormAction::Dismiss);
    }

    #[test]
    fn ctrl_o_opens_editor() {
        let mut form = PlanForm::new();
        form.open();
        assert_eq!(
            form.handle_key(key::OPEN_EDITOR.to_key_event()),
            PlanFormAction::OpenEditor
        );
    }

    #[test]
    fn unknown_key_consumed() {
        let mut form = PlanForm::new();
        form.open();
        assert_eq!(
            form.handle_key(key(KeyCode::Char('x'))),
            PlanFormAction::Consumed
        );
    }
}

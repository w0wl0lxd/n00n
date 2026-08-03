use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use crate::theme;

struct FooterHint {
    key: &'static str,
    action: &'static str,
}

const DEFAULT_HINTS: &[FooterHint] = &[
    FooterHint {
        key: "Enter",
        action: "send",
    },
    FooterHint {
        key: "/",
        action: "commands",
    },
    FooterHint {
        key: "Ctrl+H",
        action: "help",
    },
    FooterHint {
        key: "Esc",
        action: "cancel",
    },
    FooterHint {
        key: "Ctrl+C",
        action: "exit",
    },
];

const PICKER_HINTS: &[FooterHint] = &[
    FooterHint {
        key: "↑↓",
        action: "move",
    },
    FooterHint {
        key: "type",
        action: "filter",
    },
    FooterHint {
        key: "Enter",
        action: "select",
    },
    FooterHint {
        key: "Esc",
        action: "close",
    },
];

const PERMISSION_HINTS: &[FooterHint] = &[
    FooterHint {
        key: "y",
        action: "allow once",
    },
    FooterHint {
        key: "a",
        action: "allow project",
    },
    FooterHint {
        key: "s",
        action: "allow session",
    },
    FooterHint {
        key: "n",
        action: "deny",
    },
];

const PERMISSION_CONFIRM_HINTS: &[FooterHint] = &[
    FooterHint {
        key: "Enter",
        action: "confirm",
    },
    FooterHint {
        key: "Esc",
        action: "back",
    },
];

const PERMISSION_EDIT_HINTS: &[FooterHint] = &[
    FooterHint {
        key: "Enter",
        action: "deny with guidance",
    },
    FooterHint {
        key: "Esc",
        action: "back",
    },
];

const MODAL_HINTS: &[FooterHint] = &[FooterHint {
    key: "Esc",
    action: "close",
}];

const HELP_HINTS: &[FooterHint] = &[
    FooterHint {
        key: "/",
        action: "filter",
    },
    FooterHint {
        key: "Tab",
        action: "section",
    },
    FooterHint {
        key: "Esc",
        action: "close",
    },
];

const WELCOME_HINTS: &[FooterHint] = &[
    FooterHint {
        key: "Enter",
        action: "start",
    },
    FooterHint {
        key: "Esc",
        action: "close",
    },
];

const FORM_HINTS: &[FooterHint] = &[
    FooterHint {
        key: "Enter",
        action: "submit",
    },
    FooterHint {
        key: "Esc",
        action: "cancel",
    },
];

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum FooterContext {
    #[default]
    Default,
    Picker,
    Permission,
    PermissionConfirm,
    PermissionEdit,
    Help,
    Welcome,
    Form,
    Modal,
}

fn visible_hints(width: u16, context: FooterContext) -> &'static [FooterHint] {
    let hints = match context {
        FooterContext::Default => DEFAULT_HINTS,
        FooterContext::Picker => PICKER_HINTS,
        FooterContext::Permission => PERMISSION_HINTS,
        FooterContext::PermissionConfirm => PERMISSION_CONFIRM_HINTS,
        FooterContext::PermissionEdit => PERMISSION_EDIT_HINTS,
        FooterContext::Help => HELP_HINTS,
        FooterContext::Welcome => WELCOME_HINTS,
        FooterContext::Form => FORM_HINTS,
        FooterContext::Modal => MODAL_HINTS,
    };
    let limit = match width {
        0..=39 => 1,
        40..=59 => 2,
        60..=79 => 3,
        _ => 5,
    };
    &hints[..hints.len().min(limit)]
}

pub fn view(frame: &mut Frame, area: Rect, context: FooterContext) {
    if area.is_empty() {
        return;
    }
    let mut spans = Vec::new();
    for (index, hint) in visible_hints(area.width, context).iter().enumerate() {
        if index > 0 {
            spans.push(Span::styled(
                " · ",
                theme::semantic_style(theme::SemanticRole::Muted),
            ));
        }
        spans.push(Span::styled(
            hint.key,
            theme::semantic_style(theme::SemanticRole::Accent),
        ));
        spans.push(Span::raw(" "));
        spans.push(Span::styled(
            hint.action,
            theme::semantic_style(theme::SemanticRole::Muted),
        ));
    }
    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn footer_collapses_low_priority_hints_as_width_shrinks() {
        assert_eq!(visible_hints(30, FooterContext::Default).len(), 1);
        assert_eq!(visible_hints(70, FooterContext::Default).len(), 3);
        assert_eq!(visible_hints(120, FooterContext::Default).len(), 5);
        assert_eq!(visible_hints(30, FooterContext::Default)[0].action, "send");
    }

    #[test]
    fn overlay_footer_uses_picker_actions() {
        let hints = visible_hints(120, FooterContext::Picker);
        assert_eq!(hints.len(), 4);
        assert_eq!(hints[0].key, "↑↓");
        assert_eq!(hints[1].action, "filter");
        assert_eq!(hints[3].action, "close");
    }
}

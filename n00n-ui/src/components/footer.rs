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

const OVERLAY_HINTS: &[FooterHint] = &[
    FooterHint {
        key: "↑↓",
        action: "move",
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

fn visible_hints(width: u16, overlay_open: bool) -> &'static [FooterHint] {
    let hints = if overlay_open {
        OVERLAY_HINTS
    } else {
        DEFAULT_HINTS
    };
    let limit = match width {
        0..=39 => 1,
        40..=59 => 2,
        60..=79 => 3,
        _ => 5,
    };
    &hints[..hints.len().min(limit)]
}

pub fn view(frame: &mut Frame, area: Rect, overlay_open: bool) {
    if area.is_empty() {
        return;
    }
    let mut spans = Vec::new();
    for (index, hint) in visible_hints(area.width, overlay_open).iter().enumerate() {
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
        assert_eq!(visible_hints(30, false).len(), 1);
        assert_eq!(visible_hints(70, false).len(), 3);
        assert_eq!(visible_hints(120, false).len(), 5);
        assert_eq!(visible_hints(30, false)[0].action, "send");
    }

    #[test]
    fn overlay_footer_uses_picker_actions() {
        let hints = visible_hints(120, true);
        assert_eq!(hints.len(), 3);
        assert_eq!(hints[0].key, "↑↓");
        assert_eq!(hints[2].action, "close");
    }
}

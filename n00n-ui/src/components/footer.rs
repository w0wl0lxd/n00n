use crate::components::keybindings::KeybindContext;
use crate::theme;
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

const COMPACT_WIDTH: u16 = 80;
const MAX_HINTS: usize = 5;
const COMPACT_HINTS: usize = 3;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FooterHint {
    pub key: &'static str,
    pub description: &'static str,
}

pub struct Footer;

impl Footer {
    #[must_use]
    pub fn hints(contexts: &[KeybindContext]) -> Vec<FooterHint> {
        let mut hints = Vec::with_capacity(MAX_HINTS);
        push_unique(
            &mut hints,
            FooterHint {
                key: "Ctrl+H",
                description: "help",
            },
        );
        if contexts.contains(&KeybindContext::Streaming) {
            push_unique(
                &mut hints,
                FooterHint {
                    key: "Esc",
                    description: "stop",
                },
            );
            push_unique(
                &mut hints,
                FooterHint {
                    key: "Ctrl+X",
                    description: "tasks",
                },
            );
        } else if contexts.iter().any(|context| context.parent().is_some()) {
            push_unique(
                &mut hints,
                FooterHint {
                    key: "Esc",
                    description: "close",
                },
            );
            push_unique(
                &mut hints,
                FooterHint {
                    key: "Enter",
                    description: "select",
                },
            );
        } else {
            push_unique(
                &mut hints,
                FooterHint {
                    key: "Enter",
                    description: "send",
                },
            );
            push_unique(
                &mut hints,
                FooterHint {
                    key: "Ctrl+X",
                    description: "tasks",
                },
            );
        }
        push_unique(
            &mut hints,
            FooterHint {
                key: "Ctrl+C",
                description: "quit",
            },
        );
        hints
    }

    pub fn view(frame: &mut Frame, area: Rect, contexts: &[KeybindContext]) {
        if area.width == 0 || area.height == 0 {
            return;
        }
        let compact = area.width < COMPACT_WIDTH;
        let hints = Self::hints(contexts);
        let limit = if compact { COMPACT_HINTS } else { MAX_HINTS };
        let mut spans = Vec::with_capacity(limit * 3);
        let mut used = 0usize;
        for hint in hints.into_iter().take(limit) {
            let separator = if used == 0 { "" } else { "   " };
            let width = separator.len() + hint.key.len() + 1 + hint.description.len();
            if used > 0 && width + spans_width(&spans) > usize::from(area.width) {
                break;
            }
            spans.push(Span::styled(separator, theme::current().text_muted));
            spans.push(Span::styled(hint.key, theme::current().keybind_key));
            spans.push(Span::styled(
                format!(" {}", hint.description),
                theme::current().text_secondary,
            ));
            used += 1;
        }
        frame.render_widget(Paragraph::new(Line::from(spans)), area);
    }
}

fn push_unique(hints: &mut Vec<FooterHint>, hint: FooterHint) {
    if !hints.iter().any(|existing| existing.key == hint.key) && hints.len() < MAX_HINTS {
        hints.push(hint);
    }
}

fn spans_width(spans: &[Span<'_>]) -> usize {
    spans.iter().map(Span::width).sum()
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::{Terminal, backend::TestBackend};

    #[test]
    fn footer_has_at_most_five_hints() {
        let hints = Footer::hints(&[KeybindContext::General, KeybindContext::Editing]);
        assert!(hints.len() <= MAX_HINTS);
    }

    #[test]
    fn narrow_footer_uses_compact_fallback() {
        let backend = TestBackend::new(40, 1);
        let mut terminal = Terminal::new(backend).expect("test terminal");
        terminal
            .draw(|frame| Footer::view(frame, frame.area(), &[KeybindContext::Editing]))
            .expect("draw footer");
        let text: String = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(ratatui::buffer::Cell::symbol)
            .collect();
        assert!(text.contains("help"));
        assert!(
            !text.contains("Enter send"),
            "compact footer should stay short: {text:?}"
        );
    }

    #[test]
    fn keybind_contexts_are_not_ignored() {
        let editing = Footer::hints(&[KeybindContext::Editing]);
        let streaming = Footer::hints(&[KeybindContext::Streaming]);
        assert_ne!(editing, streaming);
    }
}

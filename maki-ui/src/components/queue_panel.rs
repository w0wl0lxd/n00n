use crate::theme;

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Paragraph};

const ELLIPSIS: &str = "...";
const QUEUE_LABEL: &str = " Queue ";

pub fn height(queue_len: usize) -> u16 {
    if queue_len == 0 {
        0
    } else {
        queue_len as u16 + 2
    }
}

pub fn view(frame: &mut Frame, area: Rect, messages: &[&str]) {
    if messages.is_empty() {
        return;
    }
    let content_width = area.width.saturating_sub(2) as usize;
    let lines: Vec<Line> = messages
        .iter()
        .map(|msg| {
            let flat = msg.replace('\n', " ");
            Line::from(truncate_line(&flat, content_width))
        })
        .collect();

    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(theme::INPUT_BORDER))
        .title_bottom(Line::from(QUEUE_LABEL).left_aligned());

    let paragraph = Paragraph::new(lines)
        .style(Style::new().fg(theme::FOREGROUND))
        .block(block);

    frame.render_widget(paragraph, area);
}

fn truncate_line(text: &str, max_width: usize) -> Vec<Span<'static>> {
    if text.len() <= max_width {
        return vec![Span::raw(text.to_string())];
    }
    let truncated_len = max_width.saturating_sub(ELLIPSIS.len());
    vec![
        Span::raw(text[..truncated_len].to_string()),
        Span::styled(ELLIPSIS, Style::new().fg(theme::COMMENT)),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    use test_case::test_case;

    #[test]
    fn height_includes_borders() {
        assert_eq!(height(0), 0);
        assert_eq!(height(1), 3);
        assert_eq!(height(3), 5);
    }

    #[test_case("hello", 10, &[("hello", false)]                    ; "short_text_unchanged")]
    #[test_case("abcdefghij", 7, &[("abcd", false), (ELLIPSIS, true)] ; "long_text_with_ellipsis")]
    #[test_case("abcde", 5, &[("abcde", false)]                     ; "at_exact_width")]
    #[test_case("abcdef", 2, &[("", false), (ELLIPSIS, true)]        ; "tiny_width")]
    fn truncate_line_cases(input: &str, width: usize, expected: &[(&str, bool)]) {
        let spans = truncate_line(input, width);
        assert_eq!(spans.len(), expected.len());
        for (span, &(text, is_styled)) in spans.iter().zip(expected) {
            assert_eq!(span.content, text);
            if is_styled {
                assert_eq!(span.style, Style::new().fg(theme::COMMENT));
            }
        }
    }
}

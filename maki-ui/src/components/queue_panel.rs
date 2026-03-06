use crate::theme;

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Paragraph};

const ELLIPSIS: &str = "...";
const QUEUE_LABEL: &str = " Queue ";
const DELETE_HINT: &str = " - Enter to delete";

pub struct QueueEntry<'a> {
    pub text: &'a str,
    pub color: ratatui::style::Color,
}

pub fn height(queue_len: usize) -> u16 {
    if queue_len == 0 {
        0
    } else {
        queue_len as u16 + 2
    }
}

pub fn view(frame: &mut Frame, area: Rect, entries: &[QueueEntry], focus: Option<usize>) {
    if entries.is_empty() {
        return;
    }
    let content_width = area.width.saturating_sub(2) as usize;
    let lines: Vec<Line> = entries
        .iter()
        .enumerate()
        .map(|(i, entry)| {
            let flat = entry.text.replace('\n', " ");
            if focus == Some(i) {
                truncate_line_selected(&flat, content_width)
            } else {
                Line::from(truncate_line(&flat, content_width, entry.color))
            }
        })
        .collect();

    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(if focus.is_some() {
            Style::new().fg(theme::RED)
        } else {
            theme::PANEL_BORDER
        })
        .title_top(Line::from(QUEUE_LABEL).left_aligned())
        .title_style(theme::PANEL_TITLE);

    let paragraph = Paragraph::new(lines)
        .style(Style::new().fg(theme::FOREGROUND))
        .block(block);

    frame.render_widget(paragraph, area);
}

fn truncate_line(text: &str, max_width: usize, color: ratatui::style::Color) -> Vec<Span<'static>> {
    let style = Style::new().fg(color);
    if text.len() <= max_width {
        return vec![Span::styled(text.to_string(), style)];
    }
    let truncated_len = text.floor_char_boundary(max_width.saturating_sub(ELLIPSIS.len()));
    vec![
        Span::styled(text[..truncated_len].to_string(), style),
        Span::styled(ELLIPSIS, Style::new().fg(theme::COMMENT)),
    ]
}

fn truncate_line_selected(text: &str, max_width: usize) -> Line<'static> {
    let hint_len = DELETE_HINT.len();
    let available = max_width.saturating_sub(hint_len);
    let selected_style = Style::new().fg(theme::RED).add_modifier(Modifier::BOLD);
    let hint_style = Style::new().fg(theme::COMMENT);

    if text.len() <= available {
        return Line::from(vec![
            Span::styled(text.to_string(), selected_style),
            Span::styled(DELETE_HINT, hint_style),
        ]);
    }
    let truncated_len = text.floor_char_boundary(available.saturating_sub(ELLIPSIS.len()));
    Line::from(vec![
        Span::styled(text[..truncated_len].to_string(), selected_style),
        Span::styled(ELLIPSIS, hint_style),
        Span::styled(DELETE_HINT, hint_style),
    ])
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

    #[test_case("hello", 10, &["hello"]                ; "short_text_unchanged")]
    #[test_case("abcdefghij", 7, &["abcd", ELLIPSIS]    ; "long_text_with_ellipsis")]
    #[test_case("abcde", 5, &["abcde"]                  ; "at_exact_width")]
    #[test_case("abcdef", 2, &["", ELLIPSIS]              ; "tiny_width")]
    #[test_case("●abc", 5, &["", ELLIPSIS]                ; "multibyte_too_narrow")]
    #[test_case("●●●", 8, &["●", ELLIPSIS]              ; "multibyte_fits_one")]
    fn truncate_line_cases(input: &str, width: usize, expected: &[&str]) {
        let spans = truncate_line(input, width, theme::FOREGROUND);
        let texts: Vec<&str> = spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(texts, expected);
    }
}

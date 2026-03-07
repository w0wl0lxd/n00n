use crate::theme;

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Paragraph};

const ELLIPSIS: &str = "...";
const QUEUE_LABEL: &str = " Queue ";
const FOCUSED_HINT: &str = " - Enter to delete";
const UNFOCUSED_HINT: &str = " - Ctrl-q to delete";

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
            let (style, hint) = if focus == Some(i) {
                (
                    Style::new().fg(theme::RED).add_modifier(Modifier::BOLD),
                    FOCUSED_HINT,
                )
            } else if i == 0 {
                (Style::new().fg(entry.color), UNFOCUSED_HINT)
            } else {
                (Style::new().fg(entry.color), "")
            };
            truncate_line(&flat, content_width, style, hint)
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

fn truncate_line(text: &str, max_width: usize, style: Style, hint: &'static str) -> Line<'static> {
    let hint_style = Style::new().fg(theme::COMMENT);
    let available = max_width.saturating_sub(hint.len());

    let (text_span, ellipsis) = if text.len() <= available {
        (Span::styled(text.to_string(), style), None)
    } else {
        let truncated_len = text.floor_char_boundary(available.saturating_sub(ELLIPSIS.len()));
        (
            Span::styled(text[..truncated_len].to_string(), style),
            Some(Span::styled(ELLIPSIS, hint_style)),
        )
    };

    let mut spans = vec![text_span];
    spans.extend(ellipsis);
    if !hint.is_empty() {
        spans.push(Span::styled(hint, hint_style));
    }
    Line::from(spans)
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

    const HINT: &str = " - hint";
    const NO_HINT: &str = "";
    fn style() -> Style {
        Style::new().fg(theme::FOREGROUND)
    }
    fn span_texts<'a>(line: &'a Line<'a>) -> Vec<&'a str> {
        line.spans.iter().map(|s| s.content.as_ref()).collect()
    }

    #[test_case("hello", 10, NO_HINT, &["hello"]                                     ; "no_hint_short")]
    #[test_case("abcdefghij", 7, NO_HINT, &["abcd", ELLIPSIS]                        ; "no_hint_truncated")]
    #[test_case("abcde", 5, NO_HINT, &["abcde"]                                      ; "no_hint_exact_width")]
    #[test_case("abcdef", 2, NO_HINT, &["", ELLIPSIS]                                ; "no_hint_tiny_width")]
    #[test_case("●abc", 5, NO_HINT, &["", ELLIPSIS]                                  ; "no_hint_multibyte_narrow")]
    #[test_case("●●●", 8, NO_HINT, &["●", ELLIPSIS]                                 ; "no_hint_multibyte_fits_one")]
    #[test_case("hello", 20, HINT, &["hello", HINT]                                  ; "hint_short")]
    #[test_case("abcdefghijklmnopqrstuvwxyz", 18, HINT, &["abcdefgh", ELLIPSIS, HINT] ; "hint_truncated")]
    #[test_case("ab", 9, HINT, &["ab", HINT]                                         ; "hint_exact_fit")]
    fn truncate_line_cases(input: &str, width: usize, hint: &'static str, expected: &[&str]) {
        assert_eq!(
            span_texts(&truncate_line(input, width, style(), hint)),
            expected
        );
    }
}

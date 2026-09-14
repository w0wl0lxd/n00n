use std::borrow::Cow;

use crate::components::keybindings::key;
use crate::theme;

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Paragraph};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

const ELLIPSIS: &str = "...";
const QUEUE_LABEL: &str = " Queue ";
const FOCUSED_HINT: &str = "  ↵ edit · del";

pub struct QueueEntry<'a> {
    pub text: Cow<'a, str>,
    pub color: ratatui::style::Color,
}

pub fn height(queue_len: usize) -> u16 {
    if queue_len == 0 {
        0
    } else {
        u16::try_from(queue_len).unwrap_or_else(|_| u16::MAX) + 2
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
            let (style, hint_parts) = if focus == Some(i) {
                (theme::current().queue_delete, ("", FOCUSED_HINT, ""))
            } else if i == 0 {
                (
                    Style::new().fg(entry.color),
                    ("  · ", key::POP_QUEUE.label, " pop"),
                )
            } else {
                (Style::new().fg(entry.color), ("", "", ""))
            };
            truncate_line(&flat, content_width, style, hint_parts)
        })
        .collect();

    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(if focus.is_some() {
            theme::current().queue_delete
        } else {
            theme::current().panel_border
        })
        .title_top(Line::from(QUEUE_LABEL).left_aligned())
        .title_style(theme::current().panel_title);

    let paragraph = Paragraph::new(lines)
        .style(Style::new().fg(theme::current().foreground))
        .block(block);

    frame.render_widget(paragraph, area);
}

fn truncate_line(
    text: &str,
    max_width: usize,
    style: Style,
    hint: (&'static str, &'static str, &'static str),
) -> Line<'static> {
    let hint_style = theme::current().tool_dim;
    let hint_len = UnicodeWidthStr::width(hint.0)
        + UnicodeWidthStr::width(hint.1)
        + UnicodeWidthStr::width(hint.2);
    let available = max_width.saturating_sub(hint_len);

    let (text_span, ellipsis) = if text.width() <= available {
        (Span::styled(text.to_string(), style), None)
    } else {
        let target = available.saturating_sub(UnicodeWidthStr::width(ELLIPSIS));
        let mut width = 0;
        let mut end = 0;
        for (idx, ch) in text.char_indices() {
            let ch_width = ch.width().unwrap_or_else(|| 1);
            if width + ch_width > target {
                break;
            }
            width += ch_width;
            end = idx + ch.len_utf8();
        }
        (
            Span::styled(text[..end].to_string(), style),
            Some(Span::styled(ELLIPSIS, hint_style)),
        )
    };

    let mut spans = vec![text_span];
    spans.extend(ellipsis);
    if hint_len > 0 {
        spans.push(Span::styled(hint.0, hint_style));
        spans.push(Span::styled(hint.1, hint_style));
        spans.push(Span::styled(hint.2, hint_style));
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

    const HINT: (&str, &str, &str) = (" - hint", "", "");
    const NO_HINT: (&str, &str, &str) = ("", "", "");
    fn style() -> Style {
        Style::new().fg(theme::current().foreground)
    }
    fn span_texts<'a>(line: &'a Line<'a>) -> Vec<&'a str> {
        line.spans
            .iter()
            .map(|s| s.content.as_ref())
            .filter(|s| !s.is_empty())
            .collect()
    }

    const HINT_STR: &str = " - hint";
    #[test_case("hello", 10, NO_HINT, &["hello"]                                          ; "no_hint_short")]
    #[test_case("abcdefghij", 7, NO_HINT, &["abcd", ELLIPSIS]                             ; "no_hint_truncated")]
    #[test_case("abcde", 5, NO_HINT, &["abcde"]                                           ; "no_hint_exact_width")]
    #[test_case("abcdef", 2, NO_HINT, &[ELLIPSIS]                                     ; "no_hint_tiny_width")]
    #[test_case("●abc", 5, NO_HINT, &["●abc"]                                        ; "no_hint_multibyte_fits_exactly")]
    #[test_case("●●●", 8, NO_HINT, &["●●●"]                                          ; "no_hint_multibyte_fits")]
    #[test_case("ab日本cd", 6, NO_HINT, &["ab", ELLIPSIS]                             ; "no_hint_cjk_truncated")]
    #[test_case("hello", 20, HINT, &["hello", HINT_STR]                                   ; "hint_short")]
    #[test_case("abcdefghijklmnopqrstuvwxyz", 18, HINT, &["abcdefgh", ELLIPSIS, HINT_STR]  ; "hint_truncated")]
    #[test_case("ab", 9, HINT, &["ab", HINT_STR]                                          ; "hint_exact_fit")]
    fn truncate_line_cases(
        input: &str,
        width: usize,
        hint: (&'static str, &'static str, &'static str),
        expected: &[&str],
    ) {
        assert_eq!(
            span_texts(&truncate_line(input, width, style(), hint)),
            expected
        );
    }
}

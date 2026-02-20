use std::borrow::Cow;

use crate::highlight;

use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

pub const BOLD_STYLE: Style = Style::new()
    .fg(ratatui::style::Color::Cyan)
    .add_modifier(Modifier::BOLD);
pub const CODE_STYLE: Style = Style::new().fg(ratatui::style::Color::Magenta);

struct Delimiter {
    open: &'static str,
    style: Style,
}

const DELIMITERS: [Delimiter; 2] = [
    Delimiter {
        open: "**",
        style: BOLD_STYLE,
    },
    Delimiter {
        open: "`",
        style: CODE_STYLE,
    },
];

pub fn parse_inline_markdown<'a>(text: &'a str, base_style: Style) -> Vec<Span<'a>> {
    let mut spans = Vec::new();
    let mut remaining = text;

    while !remaining.is_empty() {
        let next = DELIMITERS
            .iter()
            .filter_map(|d| remaining.find(d.open).map(|pos| (pos, d)))
            .min_by_key(|(pos, _)| *pos);

        let Some((pos, delim)) = next else {
            spans.push(Span::styled(remaining, base_style));
            break;
        };

        if pos > 0 {
            spans.push(Span::styled(&remaining[..pos], base_style));
        }

        let after_open = &remaining[pos + delim.open.len()..];
        if let Some(close) = after_open.find(delim.open) {
            spans.push(Span::styled(&after_open[..close], delim.style));
            remaining = &after_open[close + delim.open.len()..];
        } else {
            spans.push(Span::styled(&remaining[pos..], base_style));
            break;
        }
    }

    spans
}

enum TextBlock<'a> {
    Normal(&'a str),
    Code { lang: &'a str, code: &'a str },
}

fn parse_blocks(text: &str) -> Vec<TextBlock<'_>> {
    let mut blocks = Vec::new();
    let mut rest = text;

    while let Some(fence_start) = rest.find("```") {
        let before = &rest[..fence_start];
        if !before.is_empty() {
            blocks.push(TextBlock::Normal(
                before.strip_suffix('\n').unwrap_or(before),
            ));
        }

        let after_fence = &rest[fence_start + 3..];
        let lang_end = after_fence.find('\n').unwrap_or(after_fence.len());
        let lang = after_fence[..lang_end].trim();

        let code_start_offset = lang_end + 1;
        if code_start_offset > after_fence.len() {
            rest = "";
            break;
        }
        let code_region = &after_fence[code_start_offset..];

        if let Some(close) = code_region.find("```") {
            let code = code_region[..close]
                .strip_suffix('\n')
                .unwrap_or(&code_region[..close]);
            blocks.push(TextBlock::Code { lang, code });
            let after_close = &code_region[close + 3..];
            rest = after_close.strip_prefix('\n').unwrap_or(after_close);
        } else {
            let code = code_region;
            blocks.push(TextBlock::Code { lang, code });
            rest = "";
            break;
        }
    }

    if !rest.is_empty() {
        blocks.push(TextBlock::Normal(rest));
    }

    blocks
}

pub fn text_to_lines(text: &str, prefix: &str, base_style: Style) -> Vec<Line<'static>> {
    let prefix_style = base_style.add_modifier(Modifier::BOLD);
    let blocks = parse_blocks(text);
    let mut lines: Vec<Line<'static>> = Vec::new();
    let mut first_line = true;

    for block in blocks {
        match block {
            TextBlock::Normal(content) => {
                for line in content.split('\n') {
                    let mut spans: Vec<Span<'static>> = Vec::new();
                    if first_line {
                        spans.push(Span::styled(prefix.to_owned(), prefix_style));
                        first_line = false;
                    }
                    spans.extend(
                        parse_inline_markdown(line, base_style)
                            .into_iter()
                            .map(|s| Span::styled(s.content.into_owned(), s.style)),
                    );
                    lines.push(Line::from(spans));
                }
            }
            TextBlock::Code { lang, code } => {
                if first_line {
                    lines.push(Line::from(Span::styled(prefix.to_owned(), prefix_style)));
                    first_line = false;
                }
                lines.extend(highlight::highlight_code(lang, code));
            }
        }
    }

    if lines.is_empty() {
        lines.push(Line::from(Span::styled(prefix.to_owned(), prefix_style)));
    }

    lines
}

pub fn truncate_lines(s: &str, max_lines: usize) -> Cow<'_, str> {
    match s.match_indices('\n').nth(max_lines.saturating_sub(1)) {
        Some((i, _)) => Cow::Owned(format!("{}\n...", &s[..i])),
        None => Cow::Borrowed(s),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use test_case::test_case;

    #[test_case("a **bold** b", &[("a ", None), ("bold", Some(BOLD_STYLE)), (" b", None)] ; "bold")]
    #[test_case("use `foo` here", &[("use ", None), ("foo", Some(CODE_STYLE)), (" here", None)] ; "inline_code")]
    #[test_case("a `code` then **bold**", &[("a ", None), ("code", Some(CODE_STYLE)), (" then ", None), ("bold", Some(BOLD_STYLE))] ; "code_before_bold")]
    #[test_case("a **unclosed", &[("a ", None), ("**unclosed", None)] ; "unclosed_delimiter")]
    fn parse_inline_markdown_cases(input: &str, expected: &[(&str, Option<Style>)]) {
        let base = Style::default();
        let spans = parse_inline_markdown(input, base);
        assert_eq!(spans.len(), expected.len());
        for (span, (text, style)) in spans.iter().zip(expected) {
            assert_eq!(span.content, *text);
            assert_eq!(span.style, style.unwrap_or(base));
        }
    }

    #[test]
    fn text_to_lines_splits_newlines() {
        let style = Style::default();
        let lines = text_to_lines("line1\nline2\nline3", "p> ", style);
        assert_eq!(lines.len(), 3);
        assert_eq!(lines[0].spans[0].content, "p> ");
        assert_eq!(lines[1].spans.len(), 1);
    }

    #[test_case("a\nb\nc", 5, "a\nb\nc" ; "under_limit")]
    #[test_case("a\nb\nc\nd", 2, "a\nb\n..." ; "over_limit")]
    #[test_case("single", 1, "single" ; "single_line")]
    fn truncate_lines_cases(input: &str, max: usize, expected: &str) {
        assert_eq!(truncate_lines(input, max), expected);
    }

    fn block_summary<'a>(blocks: &'a [TextBlock<'a>]) -> Vec<(&'a str, Option<&'a str>)> {
        blocks
            .iter()
            .map(|b| match b {
                TextBlock::Normal(t) => (*t, None),
                TextBlock::Code { lang, code } => (*code, Some(*lang)),
            })
            .collect()
    }

    #[test_case(
        "hello world\nsecond line",
        &[("hello world\nsecond line", None)]
        ; "no_fences"
    )]
    #[test_case(
        "before\n```rust\nfn main() {}\n```\nafter",
        &[("before", None), ("fn main() {}", Some("rust")), ("after", None)]
        ; "single_code_block"
    )]
    #[test_case(
        "a\n```py\nx=1\n```\nb\n```js\ny=2\n```\nc",
        &[("a", None), ("x=1", Some("py")), ("b", None), ("y=2", Some("js")), ("c", None)]
        ; "multiple_code_blocks"
    )]
    #[test_case(
        "before\n```rust\nfn main() {}",
        &[("before", None), ("fn main() {}", Some("rust"))]
        ; "unclosed_fence"
    )]
    #[test_case(
        "a\n```rs\n```\nb",
        &[("a", None), ("", Some("rs")), ("b", None)]
        ; "empty_code_block"
    )]
    #[test_case(
        "```\ncode\n```",
        &[("code", Some(""))]
        ; "no_language_tag"
    )]
    fn parse_blocks_cases(input: &str, expected: &[(&str, Option<&str>)]) {
        let blocks = parse_blocks(input);
        assert_eq!(block_summary(&blocks), expected);
    }
}

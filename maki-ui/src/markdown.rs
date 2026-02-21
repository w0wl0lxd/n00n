use std::borrow::Cow;

use crate::highlight::{self, CodeHighlighter};
use crate::theme;

use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

pub const BOLD_STYLE: Style = theme::BOLD;
pub const CODE_STYLE: Style = theme::INLINE_CODE;
pub const BOLD_CODE_STYLE: Style = theme::BOLD_CODE;

const BOLD_DELIM: &str = "**";
const CODE_DELIM: &str = "`";

fn find_earliest_delim(text: &str) -> Option<(usize, &'static str, Style)> {
    [(BOLD_DELIM, BOLD_STYLE), (CODE_DELIM, CODE_STYLE)]
        .into_iter()
        .filter_map(|(d, s)| text.find(d).map(|pos| (pos, d, s)))
        .min_by_key(|(pos, _, _)| *pos)
}

fn parse_inner<'a>(
    content: &'a str,
    outer_style: Style,
    nested_delim: &str,
    spans: &mut Vec<Span<'a>>,
) {
    let mut remaining = content;

    while !remaining.is_empty() {
        let Some(pos) = remaining.find(nested_delim) else {
            spans.push(Span::styled(remaining, outer_style));
            return;
        };
        let after_open = &remaining[pos + nested_delim.len()..];
        let Some(close) = after_open.find(nested_delim) else {
            spans.push(Span::styled(remaining, outer_style));
            return;
        };
        if pos > 0 {
            spans.push(Span::styled(&remaining[..pos], outer_style));
        }
        spans.push(Span::styled(&after_open[..close], BOLD_CODE_STYLE));
        remaining = &after_open[close + nested_delim.len()..];
    }
}

pub fn parse_inline_markdown<'a>(text: &'a str, base_style: Style) -> Vec<Span<'a>> {
    let mut spans = Vec::new();
    let mut remaining = text;

    while !remaining.is_empty() {
        let Some((pos, delim, style)) = find_earliest_delim(remaining) else {
            spans.push(Span::styled(remaining, base_style));
            break;
        };

        if pos > 0 {
            spans.push(Span::styled(&remaining[..pos], base_style));
        }

        let after_open = &remaining[pos + delim.len()..];
        let Some(close) = after_open.find(delim) else {
            spans.push(Span::styled(&remaining[pos..], base_style));
            break;
        };

        let nested_delim = if delim == BOLD_DELIM {
            CODE_DELIM
        } else {
            BOLD_DELIM
        };
        parse_inner(&after_open[..close], style, nested_delim, &mut spans);
        remaining = &after_open[close + delim.len()..];
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

pub fn text_to_lines(
    text: &str,
    prefix: &str,
    base_style: Style,
    mut highlighters: Option<&mut Vec<CodeHighlighter>>,
) -> Vec<Line<'static>> {
    let prefix_style = base_style.add_modifier(Modifier::BOLD);
    let text = text.trim_start_matches('\n');
    let blocks = parse_blocks(text);
    let mut lines: Vec<Line<'static>> = Vec::new();
    let mut first_line = true;
    let mut code_idx = 0;

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
                } else {
                    lines.push(Line::default());
                }
                if let Some(ref mut hl) = highlighters {
                    if code_idx >= hl.len() {
                        hl.push(CodeHighlighter::new(lang));
                    }
                    lines.extend(hl[code_idx].update(code));
                } else {
                    lines.extend(highlight::highlight_code(lang, code));
                }
                code_idx += 1;
            }
        }
    }

    if let Some(hl) = highlighters {
        hl.truncate(code_idx);
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
    #[test_case("**bold `code` bold**", &[("bold ", Some(BOLD_STYLE)), ("code", Some(BOLD_CODE_STYLE)), (" bold", Some(BOLD_STYLE))] ; "code_inside_bold")]
    #[test_case("`code **bold** code`", &[("code ", Some(CODE_STYLE)), ("bold", Some(BOLD_CODE_STYLE)), (" code", Some(CODE_STYLE))] ; "bold_inside_code")]
    #[test_case("**`all`**", &[("all", Some(BOLD_CODE_STYLE))] ; "entire_bold_is_code")]
    #[test_case("`**all**`", &[("all", Some(BOLD_CODE_STYLE))] ; "entire_code_is_bold")]
    #[test_case("**bold `unclosed**", &[("bold `unclosed", Some(BOLD_STYLE))] ; "unclosed_nested_code_in_bold")]
    #[test_case("`code **unclosed`", &[("code **unclosed", Some(CODE_STYLE))] ; "unclosed_nested_bold_in_code")]
    fn parse_inline_markdown_cases(input: &str, expected: &[(&str, Option<Style>)]) {
        let base = Style::default();
        let spans = parse_inline_markdown(input, base);
        assert_eq!(spans.len(), expected.len());
        for (span, (text, style)) in spans.iter().zip(expected) {
            assert_eq!(span.content, *text);
            assert_eq!(span.style, style.unwrap_or(base));
        }
    }

    #[test_case("line1\nline2\nline3", 3, "line1" ; "splits_newlines")]
    #[test_case("\n\nfirst line\nsecond", 2, "first line" ; "strips_leading_newlines")]
    fn text_to_lines_cases(input: &str, expected_lines: usize, first_text: &str) {
        let style = Style::default();
        let lines = text_to_lines(input, "p> ", style, None);
        assert_eq!(lines.len(), expected_lines);
        assert_eq!(lines[0].spans[0].content, "p> ");
        let text: String = lines[0].spans[1..]
            .iter()
            .map(|s| s.content.as_ref())
            .collect();
        assert_eq!(text, first_text);
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

    fn lines_text(lines: &[Line<'_>]) -> Vec<String> {
        lines
            .iter()
            .map(|l| {
                l.spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>()
            })
            .collect()
    }

    #[test]
    fn incremental_matches_non_incremental() {
        let style = Style::default();
        let text = "hello\n```rust\nfn main() {}\n```\nbye";
        let full = text_to_lines(text, "p> ", style, None);
        let mut hl = Vec::new();
        let inc = text_to_lines(text, "p> ", style, Some(&mut hl));
        assert_eq!(lines_text(&full), lines_text(&inc));
    }
}

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

fn find_opening_fence(text: &str) -> Option<(usize, usize)> {
    let mut search_from = 0;
    while search_from < text.len() {
        let pos = text[search_from..].find("```")?;
        let abs = search_from + pos;
        if abs == 0 || text.as_bytes()[abs - 1] == b'\n' {
            let fence_len = 3 + text[abs + 3..].bytes().take_while(|&b| b == b'`').count();
            return Some((abs, fence_len));
        }
        search_from = abs + 3;
    }
    None
}

fn find_closing_fence(text: &str, fence_len: usize) -> Option<(usize, usize)> {
    let fence_pat = "`".repeat(fence_len);
    let mut offset = 0;
    for line in text.split('\n') {
        let trimmed = line.trim_end();
        if trimmed.starts_with(&fence_pat) && !trimmed[fence_len..].starts_with('`') {
            return Some((offset, line.len()));
        }
        offset += line.len() + 1;
    }
    None
}

fn parse_blocks(text: &str) -> Vec<TextBlock<'_>> {
    let mut blocks = Vec::new();
    let mut rest = text;

    while let Some((fence_start, fence_len)) = find_opening_fence(rest) {
        let before = &rest[..fence_start];
        if !before.is_empty() {
            blocks.push(TextBlock::Normal(
                before.strip_suffix('\n').unwrap_or(before),
            ));
        }

        let after_fence = &rest[fence_start + fence_len..];
        let lang_end = after_fence.find('\n').unwrap_or(after_fence.len());
        let lang = after_fence[..lang_end].trim();

        let code_start_offset = lang_end + 1;
        if code_start_offset > after_fence.len() {
            rest = &rest[fence_start..];
            break;
        }
        let code_region = &after_fence[code_start_offset..];

        if let Some((close_offset, close_line_len)) = find_closing_fence(code_region, fence_len) {
            let raw = &code_region[..close_offset];
            let code = raw.strip_suffix('\n').unwrap_or(raw);
            blocks.push(TextBlock::Code { lang, code });
            let after_close = &code_region[close_offset + close_line_len..];
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

fn is_blank_line(line: &Line<'_>) -> bool {
    line.spans.is_empty() || line.spans.iter().all(|s| s.content.is_empty())
}

fn ensure_blank_line(lines: &mut Vec<Line<'static>>) {
    if !lines.last().is_some_and(is_blank_line) {
        lines.push(Line::default());
    }
}

fn prefix_span(prefix: &str, style: Style) -> Span<'static> {
    Span::styled(prefix.to_owned(), style.add_modifier(Modifier::BOLD))
}

pub fn plain_lines(
    text: &str,
    prefix: &str,
    text_style: Style,
    prefix_style: Style,
) -> Vec<Line<'static>> {
    let text = text.trim_start_matches('\n');
    let mut lines: Vec<Line<'static>> = Vec::new();
    let mut first_line = true;

    for line in text.split('\n') {
        let mut spans: Vec<Span<'static>> = Vec::new();
        if first_line {
            spans.push(prefix_span(prefix, prefix_style));
            first_line = false;
        }
        spans.push(Span::styled(line.to_owned(), text_style));
        lines.push(Line::from(spans));
    }

    if lines.is_empty() {
        lines.push(Line::from(prefix_span(prefix, prefix_style)));
    }

    lines
}

pub fn text_to_lines(
    text: &str,
    prefix: &str,
    text_style: Style,
    prefix_style: Style,
    mut highlighters: Option<&mut Vec<CodeHighlighter>>,
) -> Vec<Line<'static>> {
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
                        spans.push(prefix_span(prefix, prefix_style));
                        first_line = false;
                    }
                    spans.extend(
                        parse_inline_markdown(line, text_style)
                            .into_iter()
                            .map(|s| Span::styled(s.content.into_owned(), s.style)),
                    );
                    lines.push(Line::from(spans));
                }
            }
            TextBlock::Code { lang, code } => {
                if first_line {
                    lines.push(Line::from(prefix_span(prefix, prefix_style)));
                    first_line = false;
                }
                ensure_blank_line(&mut lines);
                if let Some(ref mut hl) = highlighters {
                    if code_idx >= hl.len() {
                        hl.push(CodeHighlighter::new(lang));
                    }
                    lines.extend(hl[code_idx].update(code));
                } else {
                    lines.extend(highlight::highlight_code(lang, code));
                }
                ensure_blank_line(&mut lines);
                code_idx += 1;
            }
        }
    }

    if let Some(hl) = highlighters {
        hl.truncate(code_idx);
    }

    while lines.last().is_some_and(is_blank_line) {
        lines.pop();
    }

    if lines.is_empty() {
        lines.push(Line::from(prefix_span(prefix, prefix_style)));
    }

    lines
}

pub fn truncate_lines(s: &str, max_lines: usize) -> Cow<'_, str> {
    match s.match_indices('\n').nth(max_lines.saturating_sub(1)) {
        Some((i, _)) => Cow::Owned(format!("{}\n...", &s[..i])),
        None => Cow::Borrowed(s),
    }
}

pub fn tail_lines(s: &str, max_lines: usize) -> Cow<'_, str> {
    match s.rmatch_indices('\n').nth(max_lines.saturating_sub(1)) {
        Some((i, _)) => Cow::Owned(format!("...\n{}", &s[i + 1..])),
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
    #[test_case("plain text", &[("plain text", None)] ; "no_delimiters")]
    #[test_case("", &[] ; "empty_string")]
    #[test_case("`", &[("`", None)] ; "lone_backtick")]
    #[test_case("**", &[("**", None)] ; "lone_double_star")]
    #[test_case("``", &[] ; "empty_code_span")]
    #[test_case("****", &[] ; "empty_bold_span")]
    #[test_case("a * b", &[("a * b", None)] ; "single_star_not_bold")]
    #[test_case("a*b*c", &[("a*b*c", None)] ; "single_stars_not_parsed")]
    #[test_case("`a` `b`", &[("a", Some(CODE_STYLE)), (" ", None), ("b", Some(CODE_STYLE))] ; "two_code_spans")]
    #[test_case("**a** **b**", &[("a", Some(BOLD_STYLE)), (" ", None), ("b", Some(BOLD_STYLE))] ; "two_bold_spans")]
    #[test_case("`a` **b**", &[("a", Some(CODE_STYLE)), (" ", None), ("b", Some(BOLD_STYLE))] ; "code_then_bold")]
    #[test_case("**a** `b`", &[("a", Some(BOLD_STYLE)), (" ", None), ("b", Some(CODE_STYLE))] ; "bold_then_code")]
    #[test_case("`a` middle `b`", &[("a", Some(CODE_STYLE)), (" middle ", None), ("b", Some(CODE_STYLE))] ; "two_code_spans_with_text")]
    #[test_case("a `unclosed", &[("a ", None), ("`unclosed", None)] ; "unclosed_backtick")]
    #[test_case("a `b` c `unclosed", &[("a ", None), ("b", Some(CODE_STYLE)), (" c ", None), ("`unclosed", None)] ; "code_then_unclosed_backtick")]
    #[test_case("a **b** c **unclosed", &[("a ", None), ("b", Some(BOLD_STYLE)), (" c ", None), ("**unclosed", None)] ; "bold_then_unclosed_bold")]
    #[test_case("**a `b** c`", &[("a `b", Some(BOLD_STYLE)), (" c", None), ("`", None)] ; "interleaved_bold_code")]
    #[test_case("`a **b` c**", &[("a **b", Some(CODE_STYLE)), (" c", None), ("**", None)] ; "interleaved_code_bold")]
    #[test_case("***bold***", &[("*bold", Some(BOLD_STYLE)), ("*", None)] ; "triple_star_treated_as_bold_with_star")]
    #[test_case("**`**`", &[("`", Some(BOLD_STYLE)), ("`", None)] ; "bold_delim_greedily_matches_before_code")]
    fn parse_inline_markdown_cases(input: &str, expected: &[(&str, Option<Style>)]) {
        let base = Style::default();
        let spans = parse_inline_markdown(input, base);
        assert_eq!(
            spans.len(),
            expected.len(),
            "span count mismatch for {input:?}: got {spans:?}"
        );
        for (span, (text, style)) in spans.iter().zip(expected) {
            assert_eq!(span.content, *text);
            assert_eq!(span.style, style.unwrap_or(base));
        }
    }

    #[test_case("plain text" ; "plain")]
    #[test_case("a **bold** b" ; "simple_bold")]
    #[test_case("use `foo` here" ; "simple_code")]
    #[test_case("**bold `code` bold**" ; "nested_code_in_bold")]
    #[test_case("`code **bold** code`" ; "nested_bold_in_code")]
    #[test_case("a **unclosed" ; "unclosed_bold")]
    #[test_case("a `unclosed" ; "unclosed_code")]
    #[test_case("**bold `unclosed**" ; "unclosed_nested_code")]
    #[test_case("`code **unclosed`" ; "unclosed_nested_bold")]
    #[test_case("**a `b** c`" ; "interleaved_1")]
    #[test_case("`a **b` c**" ; "interleaved_2")]
    #[test_case("***bold***" ; "triple_star")]
    #[test_case("**`**`" ; "bold_before_code")]
    #[test_case("a `b` c `d` e" ; "multiple_code")]
    #[test_case("a **b** c **d** e" ; "multiple_bold")]
    #[test_case("``" ; "empty_code")]
    #[test_case("****" ; "empty_bold")]
    #[test_case("" ; "empty")]
    #[test_case("`" ; "lone_backtick")]
    #[test_case("**" ; "lone_stars")]
    #[test_case("**`all`**" ; "bold_code_combined")]
    #[test_case("`**all**`" ; "code_bold_combined")]
    #[test_case("here is `/home/tony/file.rs` path" ; "path_in_backticks")]
    #[test_case("use `fn main()` and **important**" ; "code_and_bold_real_content")]
    #[test_case("**`/home/tony/c/maki/src/tools/read.rs:23-38`**" ; "bold_code_path")]
    #[test_case("### 1. Data ` Types` — How Output" ; "heading_with_stray_backtick")]
    #[test_case("**/ Diffs Are Structured" ; "unclosed_bold_with_slash")]
    #[test_case("`a` `b` `c` `d` `e`" ; "many_code_spans")]
    #[test_case("**a** `b` **c** `d`" ; "alternating_bold_code")]
    #[test_case("text `code` more **bold** end `code2` fin" ; "mixed_inline")]
    fn inline_parse_invariants(input: &str) {
        let base = Style::default();
        let spans = parse_inline_markdown(input, base);
        let reconstructed: String = spans.iter().map(|s| s.content.as_ref()).collect();

        let mut input_chars = input.chars().peekable();
        for ch in reconstructed.chars() {
            loop {
                match input_chars.next() {
                    Some(c) if c == ch => break,
                    Some(_) => continue,
                    None => panic!(
                        "output not a subsequence of input\n  input: {input:?}\n  output: {reconstructed:?}"
                    ),
                }
            }
        }

        let strip = |s: &str| -> String { s.chars().filter(|c| *c != '`' && *c != '*').collect() };
        assert_eq!(
            strip(&reconstructed),
            strip(input),
            "non-delimiter content lost or reordered\n  input: {input:?}\n  output: {reconstructed:?}"
        );
    }

    #[test_case("line1\nline2\nline3", 3, "line1" ; "splits_newlines")]
    #[test_case("\n\nfirst line\nsecond", 2, "first line" ; "strips_leading_newlines")]
    fn text_to_lines_cases(input: &str, expected_lines: usize, first_text: &str) {
        let style = Style::default();
        let lines = text_to_lines(input, "p> ", style, style, None);
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

    #[test_case("a\nb\nc", 5, "a\nb\nc" ; "under_limit")]
    #[test_case("a\nb\nc\nd", 2, "...\nc\nd" ; "over_limit")]
    #[test_case("single", 1, "single" ; "single_line")]
    #[test_case("a\nb\nc\nd\ne", 3, "...\nc\nd\ne" ; "keeps_last_three")]
    fn tail_lines_cases(input: &str, max: usize, expected: &str) {
        assert_eq!(tail_lines(input, max), expected);
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
    #[test_case(
        "inline ```code``` here\ntext with ``` inside\nand more",
        &[("inline ```code``` here\ntext with ``` inside\nand more", None)]
        ; "mid_line_backticks_not_a_fence"
    )]
    #[test_case(
        "before\n````markdown\n```rust\nfn main() {}\n```\n````\nafter",
        &[("before", None), ("```rust\nfn main() {}\n```", Some("markdown")), ("after", None)]
        ; "four_backtick_fence_nests_three"
    )]
    #[test_case(
        "before\n```md\nuse ``` in code\n```\nafter",
        &[("before", None), ("use ``` in code", Some("md")), ("after", None)]
        ; "backticks_inside_code_block_not_closing_fence"
    )]
    #[test_case(
        "before\n```rs\ncode\n```trailing\nmore",
        &[("before", None), ("code", Some("rs")), ("more", None)]
        ; "closing_fence_with_trailing_text"
    )]
    #[test_case(
        "before\n```rust",
        &[("before", None), ("```rust", None)]
        ; "partial_fence_no_newline_after_lang"
    )]
    #[test_case(
        "before\n```",
        &[("before", None), ("```", None)]
        ; "partial_fence_no_lang_no_newline"
    )]
    #[test_case(
        "```rust",
        &[("```rust", None)]
        ; "only_partial_fence"
    )]
    #[test_case(
        "a\n```\n",
        &[("a", None), ("", Some(""))]
        ; "fence_with_newline_then_eof"
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
        let full = text_to_lines(text, "p> ", style, style, None);
        let mut hl = Vec::new();
        let inc = text_to_lines(text, "p> ", style, style, Some(&mut hl));
        assert_eq!(lines_text(&full), lines_text(&inc));
    }

    #[test_case(
        "Here is **bold** and `code` text.\nLine2 has `more` stuff."
        ; "streaming_mixed_markdown"
    )]
    #[test_case(
        "### 1. Data Types\n\nHere is `/home/file.rs` path\n**bold** end"
        ; "streaming_heading_with_code"
    )]
    #[test_case(
        "**`/home/tony/c/maki/src/tools/read.rs:23-38`**\n\nSome text after"
        ; "streaming_bold_code_path"
    )]
    #[test_case(
        "Before\n```rust\nfn main() {}\n```\nAfter with **bold**"
        ; "streaming_code_block_then_inline"
    )]
    #[test_case(
        "a `b` c **d** e\n`f` **g**\nh"
        ; "streaming_multiline_inline"
    )]
    fn streaming_never_garbles(input: &str) {
        let style = Style::default();
        for end in 1..=input.len() {
            if !input.is_char_boundary(end) {
                continue;
            }
            let prefix = &input[..end];
            let mut hl = Vec::new();
            let lines = text_to_lines(prefix, "", style, style, Some(&mut hl));
            let rendered: String = lines
                .iter()
                .map(|l| {
                    l.spans
                        .iter()
                        .map(|s| s.content.as_ref())
                        .collect::<String>()
                })
                .collect::<Vec<_>>()
                .join("\n");

            for line in rendered.split('\n') {
                if line.is_empty() {
                    continue;
                }
                let line_stripped: String =
                    line.chars().filter(|c| *c != '`' && *c != '*').collect();
                let input_stripped: String =
                    prefix.chars().filter(|c| *c != '`' && *c != '*').collect();
                assert!(
                    input_stripped.contains(&line_stripped),
                    "rendered line not found in input at prefix len={end}\n  prefix: {prefix:?}\n  rendered line: {line:?}\n  full rendered: {rendered:?}"
                );
            }
        }
    }

    #[test_case(
        "before\n```rust\nfn main() {}\n```\nafter",
        &["before", "", "fn main() {}", "", "after"]
        ; "margin_around_code_block"
    )]
    #[test_case(
        "before\n\n```rust\ncode\n```\n\nafter",
        &["before", "", "code", "", "", "after"]
        ; "existing_blanks_preserved"
    )]
    #[test_case(
        "hello\n```rust\ncode\n```",
        &["hello", "", "code"]
        ; "no_trailing_blank_after_final_code_block"
    )]
    fn code_block_margins(input: &str, expected: &[&str]) {
        let style = Style::default();
        let lines = text_to_lines(input, "", style, style, None);
        assert_eq!(lines_text(&lines), expected);
    }

    #[test_case(
        "**bold** `code` ```fences```",
        &["p> **bold** `code` ```fences```"]
        ; "plain_ignores_all_markdown"
    )]
    #[test_case(
        "before\n```rust\nfn main() {}\n```\nafter",
        &["p> before", "```rust", "fn main() {}", "```", "after"]
        ; "plain_preserves_code_fences_literally"
    )]
    #[test_case(
        "line1\nline2",
        &["p> line1", "line2"]
        ; "plain_splits_lines"
    )]
    fn plain_content(input: &str, expected: &[&str]) {
        let base = Style::new().fg(ratatui::style::Color::Cyan);
        let lines = plain_lines(input, "p> ", base, base);
        assert_eq!(lines_text(&lines), expected);
        for line in &lines {
            for span in &line.spans {
                assert!(
                    span.style == base || span.style == base.add_modifier(Modifier::BOLD),
                    "unexpected style on {:?}",
                    span.content
                );
            }
        }
    }
}

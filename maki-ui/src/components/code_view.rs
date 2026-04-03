use crate::highlight::{
    fallback_span, highlight_code_plain, highlight_line, highlighter_for_path,
    highlighter_for_syntax, highlighter_for_token, syntax_for_path,
};
use crate::markdown::truncation_notice;
use crate::theme;

use maki_agent::{
    DiffHunk, DiffLine, DiffSpan, GrepFileEntry, InstructionBlock, ToolInput, ToolOutput,
};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use syntect::easy::HighlightLines;

const MAX_CODE_EXECUTION_LINES: usize = 100;
pub(crate) const MAX_INSTRUCTION_LINES: usize = 15;

pub(crate) fn instruction_limit(expanded: bool) -> usize {
    if expanded {
        usize::MAX
    } else {
        MAX_INSTRUCTION_LINES
    }
}

fn nr_width(max_nr: usize) -> usize {
    max_nr.max(1).ilog10() as usize + 1
}

fn gutter(nr_str: &str) -> Span<'static> {
    Span::styled(format!("{nr_str} "), theme::current().diff_line_nr)
}

fn gap_ellipsis() -> Line<'static> {
    Line::from(vec![
        Span::styled("...".to_owned(), theme::current().tool_dim),
        Span::raw("  ".to_owned()),
    ])
}

fn truncation_line(truncated: usize) -> Line<'static> {
    Line::from(Span::styled(
        truncation_notice(truncated),
        theme::current().tool_dim,
    ))
}

fn code_spans(
    hl: &mut Option<syntect::easy::HighlightLines<'_>>,
    text: &str,
) -> Vec<Span<'static>> {
    match hl {
        Some(h) => highlight_spans(h, text),
        None => vec![fallback_span(text)],
    }
}

fn highlight_spans(hl: &mut HighlightLines<'_>, text: &str) -> Vec<Span<'static>> {
    let with_nl = format!("{text}\n");
    highlight_line(hl, &with_nl)
        .into_iter()
        .map(|(style, chunk)| Span::styled(chunk, style))
        .collect()
}

fn render_code(
    mut hl: Option<HighlightLines<'static>>,
    start_line: usize,
    code_lines: &[String],
    total_count: usize,
    max_lines: usize,
) -> (Vec<Line<'static>>, bool) {
    let display_count = code_lines.len().min(max_lines);
    let max_nr = start_line + display_count.saturating_sub(1);
    let w = nr_width(max_nr);

    let mut lines: Vec<Line<'static>> = code_lines
        .iter()
        .take(display_count)
        .enumerate()
        .map(|(i, text)| {
            let nr = start_line + i;
            let mut spans = vec![gutter(&format!("{nr:>w$}"))];
            spans.extend(code_spans(&mut hl, text));
            Line::from(spans)
        })
        .collect();

    let hidden = total_count.saturating_sub(display_count);
    let has_truncation = hidden > 0;
    if hidden > 0 {
        lines.push(truncation_line(hidden));
    }
    (lines, has_truncation)
}

fn render_diff(path: Option<&str>, hunks: &[DiffHunk]) -> Vec<Line<'static>> {
    let max_line_nr = hunks
        .iter()
        .map(|h| {
            let numbered = h
                .lines
                .iter()
                .filter(|l| !matches!(l, DiffLine::Added(_)))
                .count();
            h.start_line + numbered.saturating_sub(1)
        })
        .max()
        .unwrap_or(1);
    let w = nr_width(max_line_nr);

    let mut lines = Vec::new();
    for (i, hunk) in hunks.iter().enumerate() {
        if i > 0 {
            lines.push(gap_ellipsis());
        }
        let mut hl = path.map(highlighter_for_path);
        let mut line_nr = hunk.start_line;
        for dl in &hunk.lines {
            let show_nr = !matches!(dl, DiffLine::Added(_));
            let nr_str = if show_nr {
                let s = format!("{line_nr:>w$}");
                line_nr += 1;
                s
            } else {
                " ".repeat(w)
            };
            let mut spans = vec![gutter(&nr_str)];
            match dl {
                DiffLine::Unchanged(t) => {
                    spans.push(Span::raw("  ".to_owned()));
                    spans.extend(code_spans(&mut hl, t));
                }
                DiffLine::Removed(ds) | DiffLine::Added(ds) => {
                    let is_add = matches!(dl, DiffLine::Added(_));
                    let (prefix, base, emph) = if is_add {
                        (
                            "+ ",
                            theme::current().diff_new,
                            theme::current().diff_new_emphasis,
                        )
                    } else {
                        (
                            "- ",
                            theme::current().diff_old,
                            theme::current().diff_old_emphasis,
                        )
                    };
                    spans.push(Span::styled(
                        prefix,
                        base.patch(theme::current().code_fallback),
                    ));
                    let full: String = ds.iter().map(|s| s.text.as_str()).collect();
                    if let Some(ref mut h) = hl {
                        let with_nl = format!("{full}\n");
                        let syn = highlight_line(h, &with_nl);
                        spans.extend(merge_syntax_with_diff(&syn, ds, base, emph));
                    } else {
                        spans.push(Span::styled(
                            crate::highlight::normalize_text(&full),
                            base.patch(theme::current().code_fallback),
                        ));
                    }
                }
            }
            lines.push(Line::from(spans));
        }
    }
    lines
}

fn render_grep_results(
    entries: &[GrepFileEntry],
    max_lines: usize,
    highlight: bool,
) -> (Vec<Line<'static>>, bool) {
    let mut out = Vec::new();
    let mut budget = max_lines;
    let total_matches: usize = entries.iter().map(|e| e.match_count()).sum();
    let mut rendered_matches: usize = 0;

    let global_max_nr = entries
        .iter()
        .flat_map(|e| {
            e.groups
                .iter()
                .flat_map(|g| g.lines.iter().map(|l| l.line_nr))
        })
        .max()
        .unwrap_or(1);
    let w = nr_width(global_max_nr);
    let multi = entries.len() > 1;
    let dim = theme::current().tool_dim;

    for entry in entries {
        if budget == 0 {
            break;
        }

        if multi {
            out.push(Line::from(Span::styled(
                entry.path.clone(),
                theme::current().tool_path,
            )));
        }

        let syntax = highlight.then(|| syntax_for_path(&entry.path));
        let has_context = entry.groups.iter().any(|g| g.lines.len() > 1);

        for (gi, group) in entry.groups.iter().enumerate() {
            if budget == 0 {
                break;
            }
            if gi > 0 && has_context {
                out.push(Line::from(Span::styled("  --".to_owned(), dim)));
                budget -= 1;
            }
            for line in &group.lines {
                if budget == 0 {
                    break;
                }
                let mut spans = vec![gutter(&format!("{:>w$}", line.line_nr))];
                let text_spans = if let Some(syn) = syntax {
                    highlight_spans(&mut highlighter_for_syntax(syn), &line.text)
                } else if line.is_match {
                    vec![fallback_span(&line.text)]
                } else {
                    vec![Span::styled(line.text.clone(), dim)]
                };
                if line.is_match {
                    spans.extend(text_spans);
                    rendered_matches += 1;
                } else {
                    spans.extend(
                        text_spans
                            .into_iter()
                            .map(|s| Span::styled(s.content, theme::dim_style(s.style, 0.3))),
                    );
                }
                out.push(Line::from(spans));
                budget -= 1;
            }
        }
    }
    let truncated = budget == 0 && rendered_matches < total_matches;
    if truncated {
        let hidden = total_matches - rendered_matches;
        out.push(truncation_line(hidden));
    }
    (out, truncated)
}

pub(crate) fn render_instructions(
    blocks: &[InstructionBlock],
    lines: &mut Vec<Line<'static>>,
    max_lines: usize,
    highlight: bool,
) -> bool {
    let dim = theme::current().tool_dim;
    let mut used = 0;
    let mut truncated = false;
    let multi = blocks.len() > 1;

    for (i, block) in blocks.iter().enumerate() {
        if used >= max_lines {
            truncated = true;
            break;
        }

        if multi {
            lines.push(Line::from(Span::styled(block.path.clone(), dim)));
            used += 1;
            if i > 0 && used >= max_lines {
                truncated = true;
                break;
            }
        }

        if block.content.is_empty() {
            continue;
        }

        let code_lines: Vec<String> = block.content.lines().map(String::from).collect();
        let total = code_lines.len();
        let remaining = max_lines.saturating_sub(used);
        let hl = highlight.then(|| highlighter_for_path(&block.path));
        let (rendered, was_truncated) = render_code(hl, 1, &code_lines, total, remaining);
        used += rendered.len();
        truncated |= was_truncated;
        lines.extend(rendered);
    }
    truncated
}

pub struct ToolContent {
    pub lines: Vec<Line<'static>>,
    pub has_truncation: bool,
}

pub fn render_tool_content(
    input: Option<&ToolInput>,
    output: Option<&ToolOutput>,
    highlight: bool,
    max_lines: usize,
) -> ToolContent {
    let mut lines = Vec::new();
    let mut has_truncation = false;
    match input {
        Some(ToolInput::Script { language, code }) => {
            let code_lines: Vec<String> = code
                .trim_end_matches('\n')
                .lines()
                .map(String::from)
                .collect();
            let total = code_lines.len();
            let hl = highlight.then(|| highlighter_for_token(language));
            let (code_result, trunc) =
                render_code(hl, 1, &code_lines, total, MAX_CODE_EXECUTION_LINES);
            has_truncation |= trunc;
            lines.extend(code_result);
        }
        Some(ToolInput::Code { language, code }) => {
            if highlight {
                for line in highlight_code_plain(language, code) {
                    lines.push(line);
                }
            } else {
                for text in code.trim_end_matches('\n').lines() {
                    lines.push(Line::from(fallback_span(text)));
                }
            }
        }
        None => {}
    }
    let (output_lines, output_trunc) = match output {
        Some(ToolOutput::ReadCode {
            path,
            start_line,
            lines: code_lines,
            ..
        }) => render_code(
            highlight.then(|| highlighter_for_path(path)),
            *start_line,
            code_lines,
            code_lines.len(),
            max_lines,
        ),
        Some(
            ToolOutput::WriteCode {
                path,
                lines: code_lines,
                ..
            }
            | ToolOutput::MemoryWrite {
                path,
                lines: code_lines,
            }
            | ToolOutput::MemoryRead {
                path,
                lines: code_lines,
            },
        ) => render_code(
            highlight.then(|| highlighter_for_path(path)),
            1,
            code_lines,
            code_lines.len(),
            max_lines,
        ),
        Some(ToolOutput::Diff { path, hunks, .. }) => (
            render_diff(highlight.then_some(path.as_str()), hunks),
            false,
        ),
        Some(ToolOutput::GrepResult { entries }) => {
            render_grep_results(entries, max_lines, highlight)
        }
        Some(ToolOutput::Instructions { blocks }) => {
            let mut instruction_lines = Vec::new();
            let trunc = render_instructions(blocks, &mut instruction_lines, max_lines, highlight);
            (instruction_lines, trunc)
        }
        Some(ToolOutput::ReadDir { .. }) => (Vec::new(), false),
        _ => (Vec::new(), false),
    };
    has_truncation |= output_trunc;
    if !lines.is_empty() && !output_lines.is_empty() {
        lines.push(Line::default());
    }
    lines.extend(output_lines);
    ToolContent {
        lines,
        has_truncation,
    }
}

fn merge_syntax_with_diff(
    syntax_spans: &[(Style, String)],
    diff_spans: &[DiffSpan],
    base: Style,
    emphasis: Style,
) -> Vec<Span<'static>> {
    let mut result = Vec::new();
    let mut syn_off = 0;
    let mut syn_idx = 0;
    let mut diff_off = 0;
    let mut diff_idx = 0;

    while syn_idx < syntax_spans.len() {
        let (ref syn_style, ref syn_text) = syntax_spans[syn_idx];
        let syn_rem = &syn_text[syn_off..];
        if syn_rem.is_empty() {
            syn_idx += 1;
            syn_off = 0;
            continue;
        }

        let (bg, diff_rem) = if diff_idx < diff_spans.len() {
            let ds = &diff_spans[diff_idx];
            let rem = &ds.text[diff_off..];
            if rem.is_empty() {
                diff_idx += 1;
                diff_off = 0;
                continue;
            }
            let bg = if ds.emphasized { emphasis } else { base };
            (bg, rem.len())
        } else {
            (base, syn_rem.len())
        };

        let take = syn_rem.len().min(diff_rem);
        let take = syn_rem.floor_char_boundary(take);
        result.push(Span::styled(
            syn_rem[..take].to_owned(),
            syn_style.patch(bg),
        ));
        syn_off += take;
        diff_off += take;

        if syn_off >= syn_text.len() {
            syn_idx += 1;
            syn_off = 0;
        }
        if diff_idx < diff_spans.len() && diff_off >= diff_spans[diff_idx].text.len() {
            diff_idx += 1;
            diff_off = 0;
        }
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::markdown::TRUNCATION_PREFIX;
    use maki_agent::{DiffSpan, GrepLine, GrepMatchGroup};
    use test_case::test_case;

    use ratatui::style::Color;

    const READ_MAX_LINES: usize = 5;

    #[test_case(20, 20, READ_MAX_LINES + 1 ; "truncates_with_ellipsis")]
    #[test_case(3,  3,  3                    ; "no_truncation_when_short")]
    #[test_case(5,  50, 5 + 1                ; "total_exceeds_available_lines")]
    fn render_code_line_count(input_lines: usize, total: usize, expected: usize) {
        let code_lines: Vec<String> = (0..input_lines).map(|i| format!("line {i}")).collect();
        let (result, _) = render_code(
            Some(highlighter_for_path("test.rs")),
            1,
            &code_lines,
            total,
            READ_MAX_LINES,
        );
        assert_eq!(result.len(), expected);
    }

    #[test]
    fn merge_syntax_with_diff_emphasis_split() {
        let base = Style::new().bg(Color::Red);
        let emph = Style::new().bg(Color::Green);
        let syn = vec![(Style::new().fg(Color::White), "abcde".to_owned())];
        let diff = vec![
            DiffSpan::plain("abc".into()),
            DiffSpan {
                text: "de".into(),
                emphasized: true,
            },
        ];
        let result = merge_syntax_with_diff(&syn, &diff, base, emph);
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].content.as_ref(), "abc");
        assert_eq!(result[0].style.fg, Some(Color::White));
        assert_eq!(result[0].style.bg, Some(Color::Red));
        assert_eq!(result[1].content.as_ref(), "de");
        assert_eq!(result[1].style.bg, Some(Color::Green));
    }

    #[test]
    fn merge_syntax_longer_than_diff_preserves_trailing() {
        let base = Style::new().bg(Color::Red);
        let syn = vec![
            (Style::new().fg(Color::Blue), "ab".to_owned()),
            (Style::new().fg(Color::Cyan), "cd".to_owned()),
        ];
        let diff = vec![DiffSpan::plain("ab".into())];
        let result = merge_syntax_with_diff(&syn, &diff, base, Style::default());
        let text: String = result.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(text, "abcd");
    }

    fn grep_entries(files: &[(&str, &[usize])]) -> Vec<GrepFileEntry> {
        files
            .iter()
            .map(|(path, nrs)| GrepFileEntry {
                path: path.to_string(),
                groups: nrs
                    .iter()
                    .map(|&n| GrepMatchGroup::single(n, format!("code at {path}:{n}")))
                    .collect(),
            })
            .collect()
    }

    #[test_case(&[("a.rs", &[1,2,3,4,5,6,7,8,9,10_usize] as &[usize])], 3, 4  ; "truncates_with_ellipsis")]
    #[test_case(&[("a.rs", &[1_usize,2])],                                5, 2  ; "no_truncation_when_fits")]
    #[test_case(&[("a.rs", &[1_usize,2,3]), ("b.rs", &[10,20])],          4, 7  ; "multi_file_budget_with_ellipsis")]
    fn render_grep_line_count(files: &[(&str, &[usize])], max: usize, expected: usize) {
        let entries = grep_entries(files);
        assert_eq!(render_grep_results(&entries, max, true).0.len(), expected);
    }

    fn line_text(line: &Line) -> String {
        line.spans.iter().map(|s| s.content.as_ref()).collect()
    }

    #[test]
    fn multi_file_grep_headers_and_alignment() {
        let entries = grep_entries(&[("a.rs", &[1]), ("b.rs", &[100])]);
        let (lines, _) = render_grep_results(&entries, 10, false);

        let texts: Vec<String> = lines.iter().map(line_text).collect();
        assert!(texts.iter().any(|t| t.contains("a.rs")));
        assert!(texts.iter().any(|t| t.contains("b.rs")));

        let gutter_width =
            |line: &str| line.find(|c: char| c.is_alphabetic()).unwrap_or(usize::MAX);
        let content_gutters: Vec<usize> = texts
            .iter()
            .filter(|t| !t.contains(".rs"))
            .map(|t| gutter_width(t))
            .collect();
        assert!(
            content_gutters.windows(2).all(|w| w[0] == w[1]),
            "gutter widths should be uniform across files: {content_gutters:?}"
        );
    }

    #[test]
    fn merge_syntax_interleaved_splits_at_emphasis_boundary() {
        let base = Style::default();
        let emph = Style::new().bg(Color::Green);
        let syn = vec![
            (Style::new().fg(Color::Red), "ab".to_owned()),
            (Style::new().fg(Color::Blue), "cd".to_owned()),
        ];
        let diff = vec![
            DiffSpan::plain("a".into()),
            DiffSpan {
                text: "bcd".into(),
                emphasized: true,
            },
        ];
        let result = merge_syntax_with_diff(&syn, &diff, base, emph);
        let text: String = result.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(text, "abcd");
        assert_eq!(result[0].content.as_ref(), "a");
        assert_eq!(result[0].style.fg, Some(Color::Red));
        assert_eq!(result[1].content.as_ref(), "b");
        assert_eq!(result[1].style.bg, Some(Color::Green));
        assert_eq!(result[2].content.as_ref(), "cd");
        assert_eq!(result[2].style.bg, Some(Color::Green));
    }

    #[test]
    fn grep_highlights_match_and_context_lines_independently() {
        let entries = vec![GrepFileEntry {
            path: "test.rs".into(),
            groups: vec![GrepMatchGroup {
                lines: vec![
                    GrepLine {
                        line_nr: 1,
                        text: "let x = \"open string".into(),
                        is_match: false,
                    },
                    GrepLine::matched(2, "let y = 42;"),
                ],
            }],
        }];
        let (lines, _) = render_grep_results(&entries, 100, true);

        let distinct_styles = |line: &Line| -> usize {
            let styles: Vec<Style> = line.spans[1..].iter().map(|s| s.style).collect();
            styles
                .iter()
                .collect::<std::collections::HashSet<_>>()
                .len()
        };

        assert!(
            distinct_styles(&lines[0]) > 1,
            "context line should have multiple distinct (dimmed) syntax styles"
        );
        assert!(
            distinct_styles(&lines[1]) > 1,
            "match line after unclosed string context should be highlighted independently"
        );
    }

    #[test]
    fn render_instructions_single_block() {
        let blocks = vec![InstructionBlock {
            path: "/src/AGENTS.md".into(),
            content: "# Title\n\nSome rules here".into(),
        }];
        let mut lines = Vec::new();
        let truncated = render_instructions(&blocks, &mut lines, MAX_INSTRUCTION_LINES, false);
        assert!(!truncated);
        let text: Vec<String> = lines.iter().map(line_text).collect();
        assert!(text.iter().any(|l| l.contains("Title")));
        assert!(text.iter().any(|l| l.contains("Some rules here")));
    }

    #[test_case(MAX_INSTRUCTION_LINES, true,  true  ; "collapsed_truncates")]
    #[test_case(usize::MAX,             false, false ; "expanded_shows_all")]
    fn render_instructions_truncation(
        max_lines: usize,
        expect_truncated: bool,
        expect_notice: bool,
    ) {
        let long_content: String = (0..30)
            .map(|i| format!("line {i}"))
            .collect::<Vec<_>>()
            .join("\n");
        let blocks = vec![InstructionBlock {
            path: "AGENTS.md".into(),
            content: long_content,
        }];
        let mut lines = Vec::new();
        let truncated = render_instructions(&blocks, &mut lines, max_lines, false);
        assert_eq!(truncated, expect_truncated);
        let has_notice = lines
            .iter()
            .any(|l| line_text(l).contains(TRUNCATION_PREFIX));
        assert_eq!(has_notice, expect_notice);
    }

    #[test]
    fn render_instructions_empty_content() {
        let blocks = vec![InstructionBlock {
            path: "AGENTS.md".into(),
            content: String::new(),
        }];
        let mut lines = Vec::new();
        let truncated = render_instructions(&blocks, &mut lines, MAX_INSTRUCTION_LINES, false);
        assert!(!truncated);
        assert_eq!(lines.len(), 0);
    }

    #[test_case("héllo", &["hé", "llo"]       ; "multibyte_accented")]
    #[test_case("🦀x",   &["🦀", "x"]         ; "emoji_boundary")]
    fn merge_syntax_with_diff_multibyte(input: &str, parts: &[&str]) {
        let base = Style::new().bg(Color::Red);
        let emph = Style::new().bg(Color::Green);
        let syn = vec![(Style::new().fg(Color::White), input.to_owned())];
        let diff: Vec<DiffSpan> = parts
            .iter()
            .enumerate()
            .map(|(i, &t)| DiffSpan {
                text: t.into(),
                emphasized: i == 0,
            })
            .collect();
        let result = merge_syntax_with_diff(&syn, &diff, base, emph);
        let full: String = result.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(full, input);
    }
}

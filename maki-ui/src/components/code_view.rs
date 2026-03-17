use crate::highlight::{
    fallback_span, highlight_code_plain, highlight_line, highlighter_for_path,
    highlighter_for_syntax, highlighter_for_token, syntax_for_path,
};
use crate::markdown::{Keep, text_to_lines, truncate_lines, truncation_notice};
use crate::theme;

use maki_agent::{
    DiffHunk, DiffLine, DiffSpan, GrepFileEntry, InstructionBlock, ToolInput, ToolOutput,
};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use syntect::easy::HighlightLines;

const MAX_CODE_LINES: usize = 7;
const MAX_WRITE_LINES: usize = 30;
const MAX_GREP_LINES: usize = 15;
const MAX_CODE_EXECUTION_LINES: usize = 100;
const MAX_INSTRUCTION_LINES: usize = 15;

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

#[cfg(test)]
fn highlight_standalone(path: &str, text: &str) -> Vec<Span<'static>> {
    highlight_spans(&mut highlighter_for_syntax(syntax_for_path(path)), text)
}

fn render_code(
    mut hl: Option<HighlightLines<'static>>,
    start_line: usize,
    code_lines: &[String],
    total_count: usize,
    max_lines: usize,
) -> Vec<Line<'static>> {
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
    if hidden > 0 {
        lines.push(truncation_line(hidden));
    }
    lines
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
                        let syn = highlight_line(h, &full);
                        spans.extend(merge_syntax_with_diff(&syn, ds, base, emph));
                    } else {
                        spans.push(Span::styled(
                            full,
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
) -> Vec<Line<'static>> {
    let mut out = Vec::new();
    let mut budget = max_lines;
    let total: usize = entries.iter().map(|e| e.matches.len()).sum();

    let global_max_nr = entries
        .iter()
        .flat_map(|e| e.matches.iter().map(|m| m.line_nr))
        .max()
        .unwrap_or(1);
    let w = nr_width(global_max_nr);
    let multi = entries.len() > 1;

    for entry in entries {
        if budget == 0 {
            break;
        }
        let take = entry.matches.len().min(budget);

        if multi {
            out.push(Line::from(Span::styled(
                entry.path.clone(),
                theme::current().tool_path,
            )));
        }

        let syntax = highlight.then(|| syntax_for_path(&entry.path));

        for m in entry.matches.iter().take(take) {
            let mut spans = vec![gutter(&format!("{:>w$}", m.line_nr))];
            if let Some(syn) = syntax {
                spans.extend(highlight_spans(&mut highlighter_for_syntax(syn), &m.text));
            } else {
                spans.push(fallback_span(&m.text));
            }
            out.push(Line::from(spans));
            budget -= 1;
        }
    }
    if total > max_lines {
        out.push(truncation_line(total - max_lines));
    }
    out
}

fn render_instructions(blocks: &[InstructionBlock], lines: &mut Vec<Line<'static>>, width: u16) {
    let style = theme::current().assistant;
    let dim = theme::current().tool_dim;
    for block in blocks {
        let header = format!("Instructions from: {}", block.path);
        lines.push(Line::from(Span::styled(header, dim)));
        if !block.content.is_empty() {
            let tr = truncate_lines(&block.content, MAX_INSTRUCTION_LINES, Keep::Head);
            lines.extend(text_to_lines(tr.kept, "", style, style, None, width));
            if let Some(notice) = tr.notice_line() {
                lines.push(notice);
            }
        }
    }
}

pub fn render_tool_content(
    input: Option<&ToolInput>,
    output: Option<&ToolOutput>,
    highlight: bool,
    width: u16,
) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    match input {
        Some(ToolInput::Script { language, code }) => {
            let code_lines: Vec<String> = code
                .trim_end_matches('\n')
                .lines()
                .map(String::from)
                .collect();
            let total = code_lines.len();
            let hl = highlight.then(|| highlighter_for_token(language));
            lines.extend(render_code(
                hl,
                1,
                &code_lines,
                total,
                MAX_CODE_EXECUTION_LINES,
            ));
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
    let output_lines = match output {
        Some(ToolOutput::ReadCode {
            path,
            start_line,
            lines: code_lines,
            instructions,
            ..
        }) => {
            let mut result = render_code(
                highlight.then(|| highlighter_for_path(path)),
                *start_line,
                code_lines,
                code_lines.len(),
                MAX_CODE_LINES,
            );
            if let Some(inst) = instructions {
                result.push(Line::default());
                render_instructions(inst, &mut result, width);
            }
            result
        }
        Some(ToolOutput::WriteCode {
            path,
            lines: code_lines,
            ..
        }) => render_code(
            highlight.then(|| highlighter_for_path(path)),
            1,
            code_lines,
            code_lines.len(),
            MAX_WRITE_LINES,
        ),
        Some(ToolOutput::Diff { path, hunks, .. }) => {
            render_diff(highlight.then_some(path.as_str()), hunks)
        }
        Some(ToolOutput::GrepResult { entries }) => {
            render_grep_results(entries, MAX_GREP_LINES, highlight)
        }
        _ => Vec::new(),
    };
    if !lines.is_empty() && !output_lines.is_empty() {
        lines.push(Line::default());
    }
    lines.extend(output_lines);
    lines
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
    use maki_agent::{DiffSpan, GrepMatch};
    use test_case::test_case;

    use ratatui::style::Color;

    #[test_case(1, 1 ; "single_digit")]
    #[test_case(10, 2 ; "ten")]
    #[test_case(100, 3 ; "hundred")]
    #[test_case(0, 1 ; "zero_clamped")]
    fn nr_width_cases(input: usize, expected: usize) {
        assert_eq!(nr_width(input), expected);
    }

    #[test_case(20, 20, MAX_CODE_LINES + 1 ; "truncates_with_ellipsis")]
    #[test_case(3,  3,  3                    ; "no_truncation_when_short")]
    #[test_case(5,  50, 5 + 1                ; "total_exceeds_available_lines")]
    fn render_code_line_count(input_lines: usize, total: usize, expected: usize) {
        let code_lines: Vec<String> = (0..input_lines).map(|i| format!("line {i}")).collect();
        let result = render_code(
            Some(highlighter_for_path("test.rs")),
            1,
            &code_lines,
            total,
            MAX_CODE_LINES,
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
    fn merge_syntax_with_diff_syntax_longer_than_diff() {
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
                matches: nrs
                    .iter()
                    .map(|&n| GrepMatch {
                        line_nr: n,
                        text: format!("code at {path}:{n}"),
                    })
                    .collect(),
            })
            .collect()
    }

    #[test_case(&[("a.rs", &[1,2,3,4,5,6,7,8,9,10_usize] as &[usize])], 3, 4  ; "truncates_with_ellipsis")]
    #[test_case(&[("a.rs", &[1_usize,2])],                                5, 2  ; "no_truncation_when_fits")]
    #[test_case(&[("a.rs", &[1_usize,2,3]), ("b.rs", &[10,20])],          4, 7  ; "multi_file_budget_with_ellipsis")]
    fn render_grep_line_count(files: &[(&str, &[usize])], max: usize, expected: usize) {
        let entries = grep_entries(files);
        assert_eq!(render_grep_results(&entries, max, true).len(), expected);
    }

    fn line_text(line: &Line) -> String {
        line.spans.iter().map(|s| s.content.as_ref()).collect()
    }

    #[test]
    fn multi_file_grep_headers_and_alignment() {
        let entries = grep_entries(&[("a.rs", &[1]), ("b.rs", &[100])]);
        let lines = render_grep_results(&entries, 10, false);

        let texts: Vec<String> = lines.iter().map(line_text).collect();
        assert!(texts[0].contains("a.rs"));
        assert!(texts[2].contains("b.rs"));

        assert!(
            lines[0]
                .spans
                .iter()
                .any(|s| s.style == theme::current().tool_path)
        );

        let gutter_width = |line: &str| line.find(|c: char| c.is_alphabetic()).unwrap_or(0);
        assert_eq!(gutter_width(&texts[1]), gutter_width(&texts[3]));
    }

    #[test]
    fn merge_syntax_with_diff_interleaved_boundaries() {
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

    fn span_styles(line: &Line) -> Vec<Style> {
        line.spans.iter().map(|s| s.style).collect()
    }

    #[test]
    fn grep_each_line_highlighted_independently() {
        let entries = vec![GrepFileEntry {
            path: "test.rs".into(),
            matches: vec![
                GrepMatch {
                    line_nr: 1,
                    text: "let x = \"open string".into(),
                },
                GrepMatch {
                    line_nr: 50,
                    text: "let y = 42;".into(),
                },
            ],
        }];
        let lines = render_grep_results(&entries, 100, true);
        let standalone = highlight_standalone("test.rs", "let y = 42;");
        let second_line_spans: Vec<_> = lines[1].spans[1..].to_vec();
        assert_eq!(
            span_styles(&Line::from(second_line_spans)),
            span_styles(&Line::from(standalone))
        );
    }

    #[test]
    fn render_instructions_single_block() {
        let blocks = vec![InstructionBlock {
            path: "/src/AGENTS.md".into(),
            content: "# Title\n\nSome rules here".into(),
        }];
        let mut lines = Vec::new();
        render_instructions(&blocks, &mut lines, 80);
        let text: Vec<String> = lines.iter().map(line_text).collect();
        assert!(text[0].contains("Instructions from:"));
        assert!(text.iter().any(|l| l.contains("Title")));
        assert!(text.iter().any(|l| l.contains("Some rules here")));
    }

    #[test]
    fn render_instructions_truncates_with_dim_notice() {
        let long_content: String = (0..30)
            .map(|i| format!("line {i}"))
            .collect::<Vec<_>>()
            .join("\n");
        let blocks = vec![InstructionBlock {
            path: "AGENTS.md".into(),
            content: long_content,
        }];
        let mut lines = Vec::new();
        render_instructions(&blocks, &mut lines, 80);
        let content_lines = lines.len() - 1;
        assert!(content_lines <= MAX_INSTRUCTION_LINES + 1);
        let last = lines.last().unwrap();
        assert!(line_text(last).starts_with(TRUNCATION_PREFIX));
        assert_eq!(last.spans[0].style, theme::current().tool_dim);
    }

    #[test]
    fn render_instructions_empty_content() {
        let blocks = vec![InstructionBlock {
            path: "AGENTS.md".into(),
            content: String::new(),
        }];
        let mut lines = Vec::new();
        render_instructions(&blocks, &mut lines, 80);
        assert_eq!(lines.len(), 1);
    }
}

use crate::highlight::{highlight_line, highlighter_for_path};
use crate::theme;

use maki_providers::{DiffHunk, DiffLine, GrepFileEntry};
use ratatui::style::Style;
use ratatui::text::{Line, Span};

const INDENT: &str = "  ";
const MAX_DISPLAY_LINES: usize = 7;

fn nr_width(max_nr: usize) -> usize {
    max_nr.max(1).ilog10() as usize + 1
}

fn gutter(nr_str: &str) -> Span<'static> {
    Span::styled(format!("{INDENT}{nr_str} "), theme::DIFF_LINE_NR)
}

fn ellipsis(nr_width: usize) -> Line<'static> {
    Line::from(Span::styled(
        format!("{INDENT}{:>nr_width$}  ...", ""),
        theme::DIFF_LINE_NR,
    ))
}

fn syntax_spans(hl: &mut syntect::easy::HighlightLines<'_>, text: &str) -> Vec<Span<'static>> {
    let mut spans = vec![Span::raw(INDENT)];
    for (style, chunk) in highlight_line(hl, text) {
        spans.push(Span::styled(chunk, style));
    }
    spans
}

pub fn render_code(
    path: &str,
    start_line: usize,
    code_lines: &[String],
    max_lines: usize,
) -> Vec<Line<'static>> {
    let display_count = code_lines.len().min(max_lines);
    let max_nr = start_line + display_count.saturating_sub(1);
    let w = nr_width(max_nr);
    let mut hl = highlighter_for_path(path);

    let mut lines: Vec<Line<'static>> = code_lines
        .iter()
        .take(display_count)
        .enumerate()
        .map(|(i, text)| {
            let nr = start_line + i;
            let mut spans = vec![gutter(&format!("{nr:>w$}"))];
            spans.extend(syntax_spans(&mut hl, text));
            Line::from(spans)
        })
        .collect();

    if code_lines.len() > max_lines {
        lines.push(ellipsis(w));
    }
    lines
}

pub fn render_read_code(
    path: &str,
    start_line: usize,
    code_lines: &[String],
) -> Vec<Line<'static>> {
    render_code(path, start_line, code_lines, MAX_DISPLAY_LINES)
}

pub fn render_diff(path: &str, hunks: &[DiffHunk]) -> Vec<Line<'static>> {
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
            lines.push(ellipsis(w));
        }
        let mut hl = highlighter_for_path(path);
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
                    spans.extend(syntax_spans(&mut hl, t));
                }
                DiffLine::Removed(ds) | DiffLine::Added(ds) => {
                    let is_add = matches!(dl, DiffLine::Added(_));
                    let (prefix, base, emph) = if is_add {
                        ("+ ", theme::DIFF_NEW, theme::DIFF_NEW_EMPHASIS)
                    } else {
                        ("- ", theme::DIFF_OLD, theme::DIFF_OLD_EMPHASIS)
                    };
                    spans.push(Span::styled(prefix, base.fg(theme::FOREGROUND)));
                    let full: String = ds.iter().map(|s| s.text.as_str()).collect();
                    let syn = highlight_line(&mut hl, &full);
                    spans.extend(merge_syntax_with_diff(&syn, ds, base, emph));
                }
            }
            lines.push(Line::from(spans));
        }
    }
    lines
}

fn merge_syntax_with_diff(
    syntax_spans: &[(Style, String)],
    diff_spans: &[maki_providers::DiffSpan],
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

pub fn render_grep_results(entries: &[GrepFileEntry], max_lines: usize) -> Vec<Line<'static>> {
    let mut out = Vec::new();
    let mut budget = max_lines;
    let total: usize = entries.iter().map(|e| e.matches.len()).sum();
    for entry in entries {
        if budget == 0 {
            break;
        }
        let take = entry.matches.len().min(budget);
        let max_nr = entry
            .matches
            .iter()
            .take(take)
            .map(|m| m.line_nr)
            .max()
            .unwrap_or(1);
        let w = nr_width(max_nr);
        let mut hl = highlighter_for_path(&entry.path);
        for m in entry.matches.iter().take(take) {
            let mut spans = vec![gutter(&format!("{:>w$}", m.line_nr))];
            spans.extend(syntax_spans(&mut hl, &m.text));
            out.push(Line::from(spans));
            budget -= 1;
        }
    }
    if total > max_lines {
        out.push(ellipsis(nr_width(1)));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use maki_providers::{DiffSpan, GrepMatch};
    use test_case::test_case;

    use ratatui::style::Color;

    #[test_case(1, 1 ; "single_digit")]
    #[test_case(10, 2 ; "ten")]
    #[test_case(100, 3 ; "hundred")]
    #[test_case(0, 1 ; "zero_clamped")]
    fn nr_width_cases(input: usize, expected: usize) {
        assert_eq!(nr_width(input), expected);
    }

    #[test_case(20, MAX_DISPLAY_LINES + 1 ; "truncates_with_ellipsis")]
    #[test_case(3,  3                      ; "no_truncation_when_short")]
    fn render_read_code_line_count(input_lines: usize, expected: usize) {
        let code_lines: Vec<String> = (0..input_lines).map(|i| format!("line {i}")).collect();
        let result = render_read_code("test.rs", 1, &code_lines);
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
    #[test_case(&[("a.rs", &[1_usize,2,3]), ("b.rs", &[10,20])],          4, 5  ; "multi_file_budget_with_ellipsis")]
    fn render_grep_line_count(files: &[(&str, &[usize])], max: usize, expected: usize) {
        let entries = grep_entries(files);
        assert_eq!(render_grep_results(&entries, max).len(), expected);
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
}

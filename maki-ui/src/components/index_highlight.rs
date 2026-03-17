use crate::theme;
use ratatui::text::{Line, Span};

const KEYWORDS: &[&str] = &[
    "pub",
    "fn",
    "struct",
    "enum",
    "trait",
    "type",
    "impl",
    "mod",
    "const",
    "static",
    "async",
    "class",
    "interface",
    "export",
    "macro_rules!",
];

fn is_section_header(line: &str) -> bool {
    let trimmed = line.trim_end();
    trimmed.ends_with(':') || (trimmed.ends_with(']') && trimmed.contains(": ["))
}

fn split_trailing_range(line: &str) -> Option<(&str, &str)> {
    let bracket_start = line.rfind('[')?;
    if !line.ends_with(']') || bracket_start == 0 {
        return None;
    }
    let inside = &line[bracket_start + 1..line.len() - 1];
    if inside
        .chars()
        .all(|c| c.is_ascii_digit() || c == '-' || c == ',' || c == ' ')
        && inside.chars().any(|c| c.is_ascii_digit())
    {
        Some((&line[..bracket_start], &line[bracket_start..]))
    } else {
        None
    }
}

pub(crate) fn push_index_lines(lines: &mut Vec<Line<'static>>, text: &str, indent: &str) {
    let t = theme::current();
    for line in text.lines() {
        if is_section_header(line) {
            let mut spans = vec![Span::raw(indent.to_owned())];
            if let Some((before, range)) = split_trailing_range(line) {
                spans.push(Span::styled(before.to_owned(), t.index_section));
                spans.push(Span::styled(range.to_owned(), t.index_line_nr));
            } else {
                spans.push(Span::styled(line.to_owned(), t.index_section));
            }
            lines.push(Line::from(spans));
            continue;
        }

        let (leading, content) = match line.strip_prefix("  ") {
            Some(c) => ("  ", c),
            None => ("", line),
        };

        let mut spans = vec![Span::raw(format!("{indent}{leading}"))];

        let keyword = KEYWORDS
            .iter()
            .find(|&&kw| content.starts_with(kw) && content[kw.len()..].starts_with([' ', '(']))
            .copied();

        let rest = if let Some(kw) = keyword {
            spans.push(Span::styled(kw.to_owned(), t.index_keyword));
            &content[kw.len()..]
        } else {
            content
        };

        if let Some((before, range)) = split_trailing_range(rest) {
            spans.push(Span::styled(before.to_owned(), t.tool));
            spans.push(Span::styled(range.to_owned(), t.index_line_nr));
        } else {
            spans.push(Span::styled(rest.to_owned(), t.tool));
        }

        lines.push(Line::from(spans));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use test_case::test_case;

    #[test_case("fns:",               true  ; "plain_colon")]
    #[test_case("types:",             true  ; "plain_types")]
    #[test_case("imports: [1-5]",     true  ; "with_range")]
    #[test_case("tests: [42,50-60]",  true  ; "with_multi_range")]
    #[test_case("  pub fn foo() [10]", false ; "indented_fn")]
    #[test_case("  std::io",          false ; "module_path")]
    fn section_header(input: &str, expected: bool) {
        assert_eq!(is_section_header(input), expected);
    }

    #[test_case("pub fn foo() [10-20]", Some(("pub fn foo() ", "[10-20]")) ; "fn_with_range")]
    #[test_case("imports: [1,3-5]",     Some(("imports: ", "[1,3-5]"))    ; "section_with_range")]
    #[test_case("[*.rs]",               None                              ; "glob_pattern")]
    #[test_case("no brackets",          None                              ; "plain_text")]
    fn trailing_range(input: &str, expected: Option<(&str, &str)>) {
        assert_eq!(split_trailing_range(input), expected);
    }
}

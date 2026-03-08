use std::sync::LazyLock;

use crate::theme;

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use syntect::easy::HighlightLines;
use syntect::highlighting::{FontStyle, HighlightState, Highlighter};
use syntect::parsing::{ParseState, ScopeStack, SyntaxReference, SyntaxSet};
use syntect::util::LinesWithEndings;

static SYNTAX_SET: LazyLock<SyntaxSet> = LazyLock::new(two_face::syntax::extra_newlines);

const DRACULA_TMTHEME: &[u8] = include_bytes!("dracula.tmTheme");
static THEME: LazyLock<syntect::highlighting::Theme> = LazyLock::new(|| {
    let mut cursor = std::io::Cursor::new(DRACULA_TMTHEME);
    syntect::highlighting::ThemeSet::load_from_reader(&mut cursor).expect("embedded Dracula theme")
});

const TOKEN_ALIASES: &[(&str, &str)] = &[("jsx", "js")];

pub fn highlighter_for_path(path: &str) -> HighlightLines<'static> {
    let syntax = SYNTAX_SET
        .find_syntax_for_file(path)
        .ok()
        .flatten()
        .unwrap_or_else(|| {
            let ext = path.rsplit('.').next().unwrap_or(path);
            syntax_for_token(ext)
        });
    HighlightLines::new(syntax, &THEME)
}

pub fn highlight_line(hl: &mut HighlightLines<'_>, text: &str) -> Vec<(Style, String)> {
    let ss = &*SYNTAX_SET;
    match hl.highlight_line(text, ss) {
        Ok(ranges) => ranges
            .into_iter()
            .map(|(style, text)| (convert_style(style), text.trim_end_matches('\n').to_owned()))
            .collect(),
        Err(_) => vec![(theme::CODE_FALLBACK, text.to_owned())],
    }
}

pub fn highlighter_for_token(lang: &str) -> HighlightLines<'static> {
    HighlightLines::new(syntax_for_token(lang), &THEME)
}

fn syntax_for_token(lang: &str) -> &'static SyntaxReference {
    let ss = &*SYNTAX_SET;
    ss.find_syntax_by_token(lang)
        .or_else(|| {
            TOKEN_ALIASES
                .iter()
                .find(|(from, _)| *from == lang)
                .and_then(|(_, to)| ss.find_syntax_by_token(to))
        })
        .unwrap_or_else(|| ss.find_syntax_plain_text())
}

pub fn highlight_code_plain(lang: &str, code: &str) -> Vec<Line<'static>> {
    let ss = &*SYNTAX_SET;
    let mut h = HighlightLines::new(syntax_for_token(lang), &THEME);
    LinesWithEndings::from(code)
        .map(|raw| highlight_single_line(&mut h, raw, ss))
        .collect()
}

pub struct CodeHighlighter {
    lines: Vec<Line<'static>>,
    checkpoint_parse: ParseState,
    checkpoint_highlight: HighlightState,
    completed_lines: usize,
}

impl CodeHighlighter {
    pub fn new(lang: &str) -> Self {
        let syntax = syntax_for_token(lang);
        let highlighter = Highlighter::new(&THEME);
        Self {
            lines: Vec::new(),
            checkpoint_parse: ParseState::new(syntax),
            checkpoint_highlight: HighlightState::new(&highlighter, ScopeStack::new()),
            completed_lines: 0,
        }
    }

    pub fn update(&mut self, code: &str) -> &[Line<'static>] {
        let ss = &*SYNTAX_SET;
        let raw_lines: Vec<&str> = LinesWithEndings::from(code).collect();
        let total = raw_lines.len();
        if total == 0 {
            self.lines.clear();
            self.completed_lines = 0;
            return &[];
        }

        let new_completed = if code.ends_with('\n') {
            total
        } else {
            total - 1
        };

        if new_completed > self.completed_lines {
            let mut hl = HighlightLines::from_state(
                &THEME,
                self.checkpoint_highlight.clone(),
                self.checkpoint_parse.clone(),
            );

            for raw in &raw_lines[self.completed_lines..new_completed] {
                let line = highlight_single_line(&mut hl, raw, ss);
                self.set_or_push(self.completed_lines, line);
                self.completed_lines += 1;
            }

            let (hs, ps) = hl.state();
            self.checkpoint_parse = ps;
            self.checkpoint_highlight = hs;
        }

        let line_count = new_completed + usize::from(new_completed < total);
        self.lines.truncate(line_count);

        if new_completed < total {
            let mut hl = HighlightLines::from_state(
                &THEME,
                self.checkpoint_highlight.clone(),
                self.checkpoint_parse.clone(),
            );
            let partial = highlight_single_line(&mut hl, raw_lines[new_completed], ss);
            self.set_or_push(new_completed, partial);
        }

        &self.lines
    }

    fn set_or_push(&mut self, index: usize, line: Line<'static>) {
        if index < self.lines.len() {
            self.lines[index] = line;
        } else {
            self.lines.push(line);
        }
    }
}

fn highlight_to_spans(
    hl: &mut HighlightLines<'_>,
    text: &str,
    ss: &SyntaxSet,
) -> Vec<Span<'static>> {
    match hl.highlight_line(text, ss) {
        Ok(ranges) => ranges
            .into_iter()
            .map(|(style, text)| {
                Span::styled(text.trim_end_matches('\n').to_owned(), convert_style(style))
            })
            .collect(),
        Err(_) => vec![Span::styled(
            text.trim_end_matches('\n').to_owned(),
            theme::CODE_FALLBACK,
        )],
    }
}

fn highlight_single_line(hl: &mut HighlightLines<'_>, raw: &str, ss: &SyntaxSet) -> Line<'static> {
    Line::from(highlight_to_spans(hl, raw, ss))
}

pub fn highlight_regex_inline(pattern: &str) -> Vec<Span<'static>> {
    let ss = &*SYNTAX_SET;
    let Some(syntax) = ss.find_syntax_by_token("re") else {
        return vec![Span::styled(pattern.to_owned(), theme::CODE_FALLBACK)];
    };
    let mut hl = HighlightLines::new(syntax, &THEME);
    highlight_to_spans(&mut hl, pattern, ss)
}

fn convert_style(s: syntect::highlighting::Style) -> Style {
    let f = s.foreground;
    let mut style = Style::new().fg(Color::Rgb(f.r, f.g, f.b));
    if s.font_style.contains(FontStyle::BOLD) {
        style = style.add_modifier(Modifier::BOLD);
    }
    if s.font_style.contains(FontStyle::ITALIC) {
        style = style.add_modifier(Modifier::ITALIC);
    }
    if s.font_style.contains(FontStyle::UNDERLINE) {
        style = style.add_modifier(Modifier::UNDERLINED);
    }
    style
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spans_text(lines: &[Line<'_>]) -> Vec<String> {
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
    fn incremental_matches_full_highlight() {
        let code = "fn main() {\n    println!(\"hi\");\n}\n";
        let full = highlight_code_plain("rust", code);
        let mut ch = CodeHighlighter::new("rust");
        let incremental = ch.update(code);
        assert_eq!(spans_text(&full), spans_text(incremental));
    }

    #[test]
    fn incremental_streaming_matches_full() {
        let mut ch = CodeHighlighter::new("py");
        ch.update("x = ");
        ch.update("x = 1\ny");
        let result = ch.update("x = 1\ny = 2\n");
        let full = highlight_code_plain("py", "x = 1\ny = 2\n");
        assert_eq!(spans_text(&full), spans_text(result));
    }

    #[test]
    fn highlighter_for_path_falls_back_on_unknown_extension() {
        let mut hl = highlighter_for_path("data.xyznonexistent");
        highlight_line(&mut hl, "hello");
    }

    #[test]
    fn tsx_and_typescript_syntaxes_resolve() {
        let ss = &*SYNTAX_SET;
        assert!(ss.find_syntax_for_file("foo.tsx").ok().flatten().is_some());
        assert!(ss.find_syntax_for_file("foo.ts").ok().flatten().is_some());
        assert!(ss.find_syntax_by_token("tsx").is_some());
        assert!(ss.find_syntax_by_token("typescript").is_some());
    }

    #[test]
    fn jsx_falls_back_to_javascript() {
        let token_syntax = syntax_for_token("jsx");
        let js = SYNTAX_SET.find_syntax_by_token("js").unwrap();
        assert_eq!(token_syntax.name, js.name);
    }

    #[test]
    fn highlight_line_strips_trailing_newline() {
        let mut hl = highlighter_for_path("test.rs");
        let spans = highlight_line(&mut hl, "let x = 1;\n");
        let text: String = spans.iter().map(|(_, t)| t.as_str()).collect();
        assert!(!text.ends_with('\n'));
    }
}

use std::sync::{LazyLock, Mutex};

use crate::theme;

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use syntect::easy::HighlightLines;
use syntect::highlighting::{FontStyle, HighlightState, Highlighter};
use syntect::parsing::{ParseState, ScopeStack, SyntaxReference, SyntaxSet};
use syntect::util::LinesWithEndings;

const TOKEN_ALIASES: &[(&str, &str)] = &[("jsx", "js")];
const TAB_SPACES: &str = "  ";

static SYNTAX_SET: LazyLock<SyntaxSet> = LazyLock::new(two_face::syntax::extra_newlines);

static LEAKED_SYNTAX_THEME: LazyLock<Mutex<&'static syntect::highlighting::Theme>> =
    LazyLock::new(|| {
        let theme = theme::current();
        Mutex::new(Box::leak(Box::new(theme.syntax.clone())))
    });

pub(crate) fn refresh_syntax_theme() {
    let theme = theme::current();
    let leaked: &'static syntect::highlighting::Theme = Box::leak(Box::new(theme.syntax.clone()));
    *LEAKED_SYNTAX_THEME.lock().unwrap() = leaked;
}

fn syntax_theme() -> &'static syntect::highlighting::Theme {
    *LEAKED_SYNTAX_THEME.lock().unwrap()
}

fn normalize_text(text: &str) -> String {
    text.trim_end_matches('\n').replace('\t', TAB_SPACES)
}

pub fn highlighter_for_path(path: &str) -> HighlightLines<'static> {
    let syntax = SYNTAX_SET
        .find_syntax_for_file(path)
        .ok()
        .flatten()
        .unwrap_or_else(|| {
            let ext = path.rsplit('.').next().unwrap_or(path);
            syntax_for_token(ext)
        });
    HighlightLines::new(syntax, syntax_theme())
}

pub fn highlight_line(hl: &mut HighlightLines<'_>, text: &str) -> Vec<(Style, String)> {
    let ss = &*SYNTAX_SET;
    match hl.highlight_line(text, ss) {
        Ok(ranges) => ranges
            .into_iter()
            .map(|(style, text)| (convert_style(style), normalize_text(text)))
            .collect(),
        Err(_) => vec![(theme::current().code_fallback, normalize_text(text))],
    }
}

pub fn highlighter_for_token(lang: &str) -> HighlightLines<'static> {
    HighlightLines::new(syntax_for_token(lang), syntax_theme())
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
    let mut h = HighlightLines::new(syntax_for_token(lang), syntax_theme());
    LinesWithEndings::from(code)
        .map(|raw| highlight_single_line(&mut h, raw))
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
        let highlighter = Highlighter::new(syntax_theme());
        Self {
            lines: Vec::new(),
            checkpoint_parse: ParseState::new(syntax),
            checkpoint_highlight: HighlightState::new(&highlighter, ScopeStack::new()),
            completed_lines: 0,
        }
    }

    pub fn update(&mut self, code: &str) -> &[Line<'static>] {
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
                syntax_theme(),
                self.checkpoint_highlight.clone(),
                self.checkpoint_parse.clone(),
            );

            for raw in &raw_lines[self.completed_lines..new_completed] {
                let line = highlight_single_line(&mut hl, raw);
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
                syntax_theme(),
                self.checkpoint_highlight.clone(),
                self.checkpoint_parse.clone(),
            );
            let partial = highlight_single_line(&mut hl, raw_lines[new_completed]);
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

fn highlight_to_spans(hl: &mut HighlightLines<'_>, text: &str) -> Vec<Span<'static>> {
    highlight_line(hl, text)
        .into_iter()
        .map(|(style, text)| Span::styled(text, style))
        .collect()
}

fn highlight_single_line(hl: &mut HighlightLines<'_>, raw: &str) -> Line<'static> {
    Line::from(highlight_to_spans(hl, raw))
}

pub fn highlight_regex_inline(pattern: &str) -> Vec<Span<'static>> {
    let Some(syntax) = SYNTAX_SET.find_syntax_by_token("re") else {
        return vec![fallback_span(pattern)];
    };
    let mut hl = HighlightLines::new(syntax, syntax_theme());
    highlight_to_spans(&mut hl, pattern)
}

pub fn fallback_span(text: &str) -> Span<'static> {
    Span::styled(normalize_text(text), theme::current().code_fallback)
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
    fn tsx_and_typescript_syntaxes_resolve() {
        for token in ["tsx", "typescript"] {
            assert!(SYNTAX_SET.find_syntax_by_token(token).is_some(), "{token}");
        }
    }

    #[test]
    fn jsx_falls_back_to_javascript() {
        let token_syntax = syntax_for_token("jsx");
        let js = SYNTAX_SET.find_syntax_by_token("js").unwrap();
        assert_eq!(token_syntax.name, js.name);
    }

    #[test]
    fn normalize_text_strips_newlines_and_expands_tabs() {
        let mut hl = highlighter_for_path("test.go");
        let text: String = highlight_line(&mut hl, "\tvalue\n")
            .iter()
            .map(|(_, t)| t.as_str())
            .collect();
        assert!(text.starts_with(TAB_SPACES), "tab not expanded: {text:?}");
        assert!(!text.contains('\t'));
        assert!(!text.ends_with('\n'));
    }
}

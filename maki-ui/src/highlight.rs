use std::sync::LazyLock;

use crate::theme;

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use syntect::easy::HighlightLines;
use syntect::highlighting::{FontStyle, HighlightState, Highlighter};
use syntect::parsing::{ParseState, ScopeStack, SyntaxSet};
use syntect::util::LinesWithEndings;

static SYNTAX_SET: LazyLock<SyntaxSet> = LazyLock::new(SyntaxSet::load_defaults_newlines);

const DRACULA_TMTHEME: &[u8] = include_bytes!("dracula.tmTheme");
static THEME: LazyLock<syntect::highlighting::Theme> = LazyLock::new(|| {
    let mut cursor = std::io::Cursor::new(DRACULA_TMTHEME);
    syntect::highlighting::ThemeSet::load_from_reader(&mut cursor).expect("embedded Dracula theme")
});

const FALLBACK_STYLE: Style = theme::CODE_FALLBACK;

pub fn highlighter_for_path(path: &str) -> HighlightLines<'static> {
    let ss = &*SYNTAX_SET;
    let syntax = ss
        .find_syntax_for_file(path)
        .ok()
        .flatten()
        .unwrap_or_else(|| ss.find_syntax_plain_text());
    HighlightLines::new(syntax, &THEME)
}

pub fn highlight_line(hl: &mut HighlightLines<'_>, text: &str) -> Vec<(Style, String)> {
    let ss = &*SYNTAX_SET;
    match hl.highlight_line(text, ss) {
        Ok(ranges) => ranges
            .into_iter()
            .map(|(style, text)| (convert_style(style), text.trim_end_matches('\n').to_owned()))
            .collect(),
        Err(_) => vec![(FALLBACK_STYLE, text.to_owned())],
    }
}

pub fn highlight_code(lang: &str, code: &str) -> Vec<Line<'static>> {
    let ss = &*SYNTAX_SET;
    let syntax = ss
        .find_syntax_by_token(lang)
        .unwrap_or_else(|| ss.find_syntax_plain_text());

    let mut h = HighlightLines::new(syntax, &THEME);
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
        let ss = &*SYNTAX_SET;
        let syntax = ss
            .find_syntax_by_token(lang)
            .unwrap_or_else(|| ss.find_syntax_plain_text());
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
                if self.completed_lines < self.lines.len() {
                    self.lines[self.completed_lines] = line;
                } else {
                    self.lines.push(line);
                }
                self.completed_lines += 1;
            }

            let (hs, ps) = hl.state();
            self.checkpoint_parse = ps;
            self.checkpoint_highlight = hs;
        }

        self.lines
            .truncate(new_completed + usize::from(new_completed < total));

        if new_completed < total {
            let mut hl = HighlightLines::from_state(
                &THEME,
                self.checkpoint_highlight.clone(),
                self.checkpoint_parse.clone(),
            );
            let partial = highlight_single_line(&mut hl, raw_lines[new_completed], ss);
            if new_completed < self.lines.len() {
                self.lines[new_completed] = partial;
            } else {
                self.lines.push(partial);
            }
        }

        &self.lines
    }
}

fn highlight_single_line(hl: &mut HighlightLines<'_>, raw: &str, ss: &SyntaxSet) -> Line<'static> {
    let spans = match hl.highlight_line(raw, ss) {
        Ok(ranges) => ranges
            .into_iter()
            .map(|(style, text)| {
                Span::styled(text.trim_end_matches('\n').to_owned(), convert_style(style))
            })
            .collect(),
        Err(_) => vec![Span::styled(
            raw.trim_end_matches('\n').to_owned(),
            FALLBACK_STYLE,
        )],
    };
    Line::from(spans)
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

    #[test]
    fn known_language_produces_output() {
        let lines = highlight_code("rust", "fn main() {}");
        assert_eq!(lines.len(), 1);
        assert!(!lines[0].spans.is_empty());
    }

    #[test]
    fn unknown_language_falls_back_without_panic() {
        let lines = highlight_code("nonexistent_lang_xyz", "some code");
        assert_eq!(lines.len(), 1);
        assert!(!lines[0].spans.is_empty());
    }

    #[test]
    fn multiline_code_produces_correct_line_count() {
        let lines = highlight_code("py", "x = 1\ny = 2\nz = 3\n");
        assert_eq!(lines.len(), 3);
    }

    #[test]
    fn empty_code_produces_no_lines() {
        let lines = highlight_code("rust", "");
        assert!(lines.is_empty());
    }

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
        let full = highlight_code("rust", code);
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
        let full = highlight_code("py", "x = 1\ny = 2\n");
        assert_eq!(spans_text(&full), spans_text(result));
    }

    #[test]
    fn incremental_partial_line_produces_output() {
        let mut ch = CodeHighlighter::new("rust");
        assert_eq!(ch.update("let x").len(), 1);
    }

    #[test]
    fn highlighter_for_path_known_extension() {
        let mut hl = highlighter_for_path("main.rs");
        let spans = highlight_line(&mut hl, "fn main() {}");
        let text: String = spans.iter().map(|(_, t)| t.as_str()).collect();
        assert_eq!(text, "fn main() {}");
    }

    #[test]
    fn highlighter_for_path_unknown_extension_falls_back() {
        let mut hl = highlighter_for_path("data.xyznonexistent");
        let spans = highlight_line(&mut hl, "hello");
        assert!(!spans.is_empty());
    }

    #[test]
    fn highlight_line_strips_trailing_newline() {
        let mut hl = highlighter_for_path("test.rs");
        let spans = highlight_line(&mut hl, "let x = 1;\n");
        let text: String = spans.iter().map(|(_, t)| t.as_str()).collect();
        assert!(!text.ends_with('\n'));
    }

    #[test]
    fn convert_style_maps_rgb_and_modifiers() {
        let s = syntect::highlighting::Style {
            foreground: syntect::highlighting::Color {
                r: 255,
                g: 128,
                b: 0,
                a: 255,
            },
            background: syntect::highlighting::Color {
                r: 0,
                g: 0,
                b: 0,
                a: 255,
            },
            font_style: FontStyle::BOLD | FontStyle::ITALIC,
        };
        let result = convert_style(s);
        assert_eq!(result.fg, Some(Color::Rgb(255, 128, 0)));
        assert!(result.add_modifier.contains(Modifier::BOLD));
        assert!(result.add_modifier.contains(Modifier::ITALIC));
    }
}

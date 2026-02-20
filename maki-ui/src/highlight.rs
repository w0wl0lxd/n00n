use std::sync::LazyLock;

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use syntect::easy::HighlightLines;
use syntect::highlighting::FontStyle;
use syntect::parsing::SyntaxSet;
use syntect::util::LinesWithEndings;

static SYNTAX_SET: LazyLock<SyntaxSet> = LazyLock::new(SyntaxSet::load_defaults_newlines);

const DRACULA_TMTHEME: &[u8] = include_bytes!("dracula.tmTheme");
static THEME: LazyLock<syntect::highlighting::Theme> = LazyLock::new(|| {
    let mut cursor = std::io::Cursor::new(DRACULA_TMTHEME);
    syntect::highlighting::ThemeSet::load_from_reader(&mut cursor).expect("embedded Dracula theme")
});

const FALLBACK_STYLE: Style = Style::new().fg(Color::Magenta);

pub fn highlight_code(lang: &str, code: &str) -> Vec<Line<'static>> {
    let ss = &*SYNTAX_SET;
    let syntax = ss
        .find_syntax_by_token(lang)
        .unwrap_or_else(|| ss.find_syntax_plain_text());

    let mut h = HighlightLines::new(syntax, &THEME);

    LinesWithEndings::from(code)
        .map(|line| {
            let spans = match h.highlight_line(line, ss) {
                Ok(ranges) => ranges
                    .into_iter()
                    .map(|(style, text)| {
                        Span::styled(text.trim_end_matches('\n').to_owned(), convert_style(style))
                    })
                    .collect(),
                Err(_) => vec![Span::styled(
                    line.trim_end_matches('\n').to_owned(),
                    FALLBACK_STYLE,
                )],
            };
            Line::from(spans)
        })
        .collect()
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

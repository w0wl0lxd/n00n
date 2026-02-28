use std::sync::LazyLock;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc;
use std::thread;

use crate::theme;

use maki_providers::{ToolInput, ToolOutput};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use syntect::easy::HighlightLines;
use syntect::highlighting::{FontStyle, HighlightState, Highlighter};
use syntect::parsing::{ParseState, ScopeStack, SyntaxReference, SyntaxSet};
use syntect::util::LinesWithEndings;

static SYNTAX_SET: LazyLock<SyntaxSet> = LazyLock::new(SyntaxSet::load_defaults_newlines);

const DRACULA_TMTHEME: &[u8] = include_bytes!("dracula.tmTheme");
static THEME: LazyLock<syntect::highlighting::Theme> = LazyLock::new(|| {
    let mut cursor = std::io::Cursor::new(DRACULA_TMTHEME);
    syntect::highlighting::ThemeSet::load_from_reader(&mut cursor).expect("embedded Dracula theme")
});

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
        Err(_) => vec![(theme::CODE_FALLBACK, text.to_owned())],
    }
}

fn syntax_for_token(lang: &str) -> &'static SyntaxReference {
    let ss = &*SYNTAX_SET;
    ss.find_syntax_by_token(lang)
        .unwrap_or_else(|| ss.find_syntax_plain_text())
}

pub fn highlight_code_plain(lang: &str, code: &str) -> Vec<Line<'static>> {
    let ss = &*SYNTAX_SET;
    let mut h = HighlightLines::new(syntax_for_token(lang), &THEME);
    LinesWithEndings::from(code)
        .map(|raw| highlight_single_line(&mut h, raw, ss))
        .collect()
}

pub fn highlight_code(lang: &str, code: &str) -> Vec<Line<'static>> {
    let mut lines = highlight_code_plain(lang, code);
    for line in &mut lines {
        prepend_code_bar(line);
    }
    lines
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
                let mut line = highlight_single_line(&mut hl, raw, ss);
                prepend_code_bar(&mut line);
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
            let mut partial = highlight_single_line(&mut hl, raw_lines[new_completed], ss);
            prepend_code_bar(&mut partial);
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

pub(crate) const CODE_BAR: &str = "│ ";

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

fn prepend_code_bar(line: &mut Line<'static>) {
    line.spans
        .insert(0, Span::styled(CODE_BAR, theme::CODE_BAR_STYLE));
}

struct HighlightJob {
    id: u64,
    tool_input: Option<ToolInput>,
    tool_output: Option<ToolOutput>,
}

pub struct HighlightResult {
    pub id: u64,
    pub lines: Vec<Line<'static>>,
}

static NEXT_JOB_ID: AtomicU64 = AtomicU64::new(0);

pub struct HighlightWorker {
    tx: mpsc::Sender<HighlightJob>,
    rx: mpsc::Receiver<HighlightResult>,
}

impl HighlightWorker {
    pub fn new() -> Self {
        let (req_tx, req_rx) = mpsc::channel::<HighlightJob>();
        let (res_tx, res_rx) = mpsc::channel::<HighlightResult>();

        thread::Builder::new()
            .name("highlight".into())
            .spawn(move || {
                use crate::components::code_view;
                while let Ok(job) = req_rx.recv() {
                    let lines = code_view::render_tool_content(
                        job.tool_input.as_ref(),
                        job.tool_output.as_ref(),
                        true,
                    );
                    if res_tx.send(HighlightResult { id: job.id, lines }).is_err() {
                        break;
                    }
                }
            })
            .expect("spawn highlight thread");

        Self {
            tx: req_tx,
            rx: res_rx,
        }
    }

    pub fn send(&self, tool_input: Option<ToolInput>, tool_output: Option<ToolOutput>) -> u64 {
        let id = NEXT_JOB_ID.fetch_add(1, Ordering::Relaxed);
        let _ = self.tx.send(HighlightJob {
            id,
            tool_input,
            tool_output,
        });
        id
    }

    pub fn try_recv(&self) -> Option<HighlightResult> {
        self.rx.try_recv().ok()
    }
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

    #[test]
    fn unknown_language_falls_back_without_panic() {
        highlight_code("nonexistent_lang_xyz", "some code");
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
    fn all_lines_have_bar_prefix() {
        let expected = Span::styled(CODE_BAR, theme::CODE_BAR_STYLE);
        let lines = highlight_code("rust", "fn main() {}\nlet x = 1;\n");
        for line in &lines {
            assert_eq!(line.spans[0], expected);
        }
    }

    #[test]
    fn highlighter_for_path_falls_back_on_unknown_extension() {
        let mut hl = highlighter_for_path("data.xyznonexistent");
        highlight_line(&mut hl, "hello");
    }

    #[test]
    fn highlight_line_strips_trailing_newline() {
        let mut hl = highlighter_for_path("test.rs");
        let spans = highlight_line(&mut hl, "let x = 1;\n");
        let text: String = spans.iter().map(|(_, t)| t.as_str()).collect();
        assert!(!text.ends_with('\n'));
    }
}

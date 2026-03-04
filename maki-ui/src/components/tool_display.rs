use super::{DisplayMessage, ToolStatus};

use super::code_view;
use crate::animation::spinner_frame;
use crate::markdown::TRUNCATION_PREFIX;
use crate::theme;

use std::time::Instant;

use jiff::Timestamp;
use jiff::tz::TimeZone;

use maki_agent::tools::{
    BASH_TOOL_NAME, EDIT_TOOL_NAME, GLOB_TOOL_NAME, GREP_TOOL_NAME, MULTIEDIT_TOOL_NAME,
    READ_TOOL_NAME, WEBFETCH_TOOL_NAME, WEBSEARCH_TOOL_NAME, WRITE_TOOL_NAME,
};
use maki_providers::{BatchToolStatus, ToolInput, ToolOutput};
use ratatui::style::Style;
use ratatui::text::{Line, Span};

use crate::highlight::highlight_regex_inline;
use crate::render_worker::RenderWorker;

pub const TOOL_INDICATOR: &str = "● ";
pub const TOOL_OUTPUT_MAX_LINES: usize = 7;
pub const BASH_OUTPUT_MAX_LINES: usize = 10;
pub const TOOL_BODY_INDENT: &str = "  ";
const TIMESTAMP_LEN: usize = 8;

pub fn tool_summary_annotation(tool: &str, text: &str) -> Option<String> {
    match tool {
        GLOB_TOOL_NAME => Some(format!("{} files", text.lines().count())),
        WEBFETCH_TOOL_NAME | WEBSEARCH_TOOL_NAME => Some(format!("{} lines", text.lines().count())),
        _ => {
            let n = text.lines().count();
            (n > BASH_OUTPUT_MAX_LINES).then(|| format!("{n} lines"))
        }
    }
}

fn extract_path_suffix(s: &str) -> Option<(&str, &str)> {
    let i = s.rfind(" in ")?;
    let path = s[i + 4..].split('"').next().unwrap();
    Some((&s[..i], path))
}

fn split_trailing_annotation(s: &str) -> (&str, Option<&str>) {
    if let Some(i) = s.rfind(" (")
        && s.ends_with(')')
    {
        return (&s[..i], Some(&s[i..]));
    }
    (s, None)
}

fn style_grep_header(header: &str) -> Vec<Span<'static>> {
    let (pattern, rest) = match header.find(" [") {
        Some(i) => (&header[..i], &header[i..]),
        None => match header.rfind(" in ") {
            Some(i) => (&header[..i], &header[i..]),
            None => (header, ""),
        },
    };

    let mut spans = highlight_regex_inline(pattern);

    let after_pattern = if let Some(bracket_end) = rest.find(']') {
        let filter = &rest[..bracket_end + 1];
        spans.push(Span::styled(filter.to_owned(), theme::TOOL_ANNOTATION));
        &rest[bracket_end + 1..]
    } else {
        rest
    };

    if let Some((_, path)) = extract_path_suffix(after_pattern) {
        spans.push(Span::styled(format!(" {path}"), theme::TOOL_PATH));
    }

    spans
}

fn style_tool_header(tool: &str, header: &str) -> Vec<Span<'static>> {
    if PATH_FIRST_TOOLS.contains(&tool) {
        return vec![Span::styled(header.to_owned(), theme::TOOL_PATH)];
    }
    if tool == GREP_TOOL_NAME {
        return style_grep_header(header);
    }
    if IN_PATH_TOOLS.contains(&tool)
        && let Some((cmd, path)) = extract_path_suffix(header)
    {
        return vec![
            Span::styled(format!("{cmd} "), theme::TOOL),
            Span::styled(path.to_owned(), theme::TOOL_PATH),
        ];
    }
    vec![Span::styled(header.to_owned(), theme::TOOL)]
}

const PATH_FIRST_TOOLS: &[&str] = &[
    READ_TOOL_NAME,
    EDIT_TOOL_NAME,
    WRITE_TOOL_NAME,
    MULTIEDIT_TOOL_NAME,
];
const IN_PATH_TOOLS: &[&str] = &[BASH_TOOL_NAME, GLOB_TOOL_NAME];

pub struct RoleStyle {
    pub prefix: &'static str,
    pub text_style: Style,
    pub prefix_style: Style,
    pub use_markdown: bool,
}

pub const ASSISTANT_STYLE: RoleStyle = RoleStyle {
    prefix: "maki> ",
    text_style: theme::ASSISTANT,
    prefix_style: theme::ASSISTANT_PREFIX,
    use_markdown: true,
};

pub const USER_STYLE: RoleStyle = RoleStyle {
    prefix: "you> ",
    text_style: theme::ASSISTANT,
    prefix_style: theme::USER,
    use_markdown: true,
};

pub const THINKING_STYLE: RoleStyle = RoleStyle {
    prefix: "thinking> ",
    text_style: theme::THINKING,
    prefix_style: theme::THINKING,
    use_markdown: true,
};

pub const ERROR_STYLE: RoleStyle = RoleStyle {
    prefix: "",
    text_style: theme::ERROR,
    prefix_style: theme::ERROR,
    use_markdown: false,
};

pub struct ToolLines {
    pub lines: Vec<Line<'static>>,
    pub highlight: Option<HighlightRequest>,
}

pub struct HighlightRequest {
    pub range: (usize, usize),
    pub input: Option<ToolInput>,
    pub output: Option<ToolOutput>,
}

impl HighlightRequest {
    fn new(
        range: (usize, usize),
        input: Option<ToolInput>,
        output: Option<ToolOutput>,
    ) -> Option<Self> {
        if range.0 == range.1 {
            return None;
        }
        let output = output.and_then(|o| match o {
            ToolOutput::ReadCode { .. }
            | ToolOutput::WriteCode { .. }
            | ToolOutput::Diff { .. }
            | ToolOutput::GrepResult { .. } => Some(o),
            ToolOutput::Plain(_)
            | ToolOutput::TodoList(_)
            | ToolOutput::Batch { .. }
            | ToolOutput::QuestionAnswers(_) => None,
        });
        Some(Self {
            range,
            input,
            output,
        })
    }
}

impl ToolLines {
    pub fn send_highlight(&self, worker: &RenderWorker) -> Option<u64> {
        let hl = self.highlight.as_ref()?;
        Some(worker.send(hl.input.clone(), hl.output.clone()))
    }
}

pub fn format_timestamp_now() -> String {
    let zoned = Timestamp::now().to_zoned(TimeZone::system());
    zoned.strftime("%H:%M:%S").to_string()
}

pub fn append_timestamp(line: &mut Line<'static>, timestamp: &str, width: u16) {
    let header_width: usize = line.spans.iter().map(|s| s.content.len()).sum();
    let w = width as usize;
    if header_width + 1 + TIMESTAMP_LEN <= w {
        let pad = w - header_width - TIMESTAMP_LEN;
        line.spans.push(Span::raw(" ".repeat(pad)));
        line.spans
            .push(Span::styled(timestamp.to_owned(), theme::COMMENT_STYLE));
    }
}

pub fn build_tool_lines(
    msg: &DisplayMessage,
    status: ToolStatus,
    started_at: Instant,
) -> ToolLines {
    let header = msg
        .text
        .split_once('\n')
        .map_or(msg.text.as_str(), |(h, _)| h);
    let (header, annotation) = split_trailing_annotation(header);
    let tool_name = msg.role.tool_name().unwrap_or("?");
    let mut header_spans = vec![Span::styled(format!("{tool_name}> "), theme::TOOL_PREFIX)];
    header_spans.extend(style_tool_header(tool_name, header));
    if let Some(ann) = annotation {
        header_spans.push(Span::styled(ann.to_owned(), theme::TOOL_ANNOTATION));
    }
    let mut lines = vec![Line::from(header_spans)];

    let (indicator, indicator_style) = match status {
        ToolStatus::InProgress => {
            let ch = spinner_frame(started_at.elapsed().as_millis());
            (format!("{ch} "), theme::TOOL_IN_PROGRESS)
        }
        ToolStatus::Success => (TOOL_INDICATOR.into(), theme::TOOL_SUCCESS),
        ToolStatus::Error => (TOOL_INDICATOR.into(), theme::TOOL_ERROR),
    };
    lines[0]
        .spans
        .insert(0, Span::styled(indicator, indicator_style));

    let content =
        code_view::render_tool_content(msg.tool_input.as_ref(), msg.tool_output.as_ref(), false);
    let content_start = lines.len();
    lines.extend(content);
    let content_end = lines.len();

    match msg.tool_output.as_ref() {
        None | Some(ToolOutput::Plain(_)) => {
            if let Some((_, body)) = msg.text.split_once('\n') {
                for line in body.lines() {
                    let style = if line.starts_with(TRUNCATION_PREFIX) {
                        theme::TOOL_ANNOTATION
                    } else {
                        theme::TOOL
                    };
                    lines.push(Line::from(Span::styled(
                        format!("{TOOL_BODY_INDENT}{line}"),
                        style,
                    )));
                }
            }
        }
        Some(ToolOutput::TodoList(items)) => {
            for item in items {
                let style = match item.status {
                    maki_providers::TodoStatus::Completed => theme::TODO_COMPLETED,
                    maki_providers::TodoStatus::InProgress => theme::TODO_IN_PROGRESS,
                    maki_providers::TodoStatus::Pending => theme::TODO_PENDING,
                    maki_providers::TodoStatus::Cancelled => theme::TODO_CANCELLED,
                };
                lines.push(Line::from(Span::styled(
                    format!(
                        "{TOOL_BODY_INDENT}{} {}",
                        item.status.marker(),
                        item.content
                    ),
                    style,
                )));
            }
        }
        Some(ToolOutput::Batch { entries, .. }) => {
            for entry in entries {
                let (indicator, style) = match entry.status {
                    BatchToolStatus::Pending => ("○ ".into(), theme::TOOL_DIM),
                    BatchToolStatus::InProgress => {
                        let ch = spinner_frame(started_at.elapsed().as_millis());
                        (format!("{ch} "), theme::TOOL_IN_PROGRESS)
                    }
                    BatchToolStatus::Success => (TOOL_INDICATOR.into(), theme::TOOL_SUCCESS),
                    BatchToolStatus::Error => (TOOL_INDICATOR.into(), theme::TOOL_ERROR),
                };
                let mut spans = vec![
                    Span::styled(TOOL_BODY_INDENT.to_owned(), style),
                    Span::styled(indicator, style),
                    Span::styled(format!("{}> ", entry.tool), theme::TOOL_PREFIX),
                ];
                spans.extend(style_tool_header(&entry.tool, &entry.summary));
                lines.push(Line::from(spans));
            }
        }
        Some(ToolOutput::QuestionAnswers(pairs)) => {
            for pair in pairs {
                lines.push(Line::from(vec![
                    Span::styled(format!("{TOOL_BODY_INDENT}❯ "), theme::TOOL_ANNOTATION),
                    Span::styled(pair.question.clone(), theme::QUESTION_LABEL),
                    Span::styled(" → ", theme::TOOL_ANNOTATION),
                    Span::styled(pair.answer.clone(), theme::QUESTION_ANSWER),
                ]));
            }
        }
        _ => {}
    }

    let highlight = HighlightRequest::new(
        (content_start, content_end),
        msg.tool_input.clone(),
        msg.tool_output.clone(),
    );

    ToolLines { lines, highlight }
}

pub fn truncate_to_header(text: &mut String) {
    let end = text.find('\n').unwrap_or(text.len());
    text.truncate(end);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::components::DisplayRole;
    use maki_agent::tools::{BASH_TOOL_NAME, WRITE_TOOL_NAME};
    use maki_providers::{ToolInput, ToolOutput};
    use test_case::test_case;

    #[test_case(GLOB_TOOL_NAME, "src/a.rs\nsrc/b.rs\nsrc/c.rs", Some("3 files") ; "glob_file_count")]
    #[test_case(WEBFETCH_TOOL_NAME, "line1\nline2\nline3", Some("3 lines") ; "webfetch_line_count")]
    #[test_case(WEBSEARCH_TOOL_NAME, "result1\nresult2", Some("2 lines") ; "websearch_line_count")]
    #[test_case("bash", "ok", None ; "short_output_no_annotation")]
    #[test_case("bash", &(0..20).map(|i| format!("line {i}")).collect::<Vec<_>>().join("\n"), Some("20 lines") ; "long_output_line_count")]
    fn summary_annotation(tool: &str, output: &str, expected: Option<&str>) {
        assert_eq!(tool_summary_annotation(tool, output).as_deref(), expected);
    }

    fn code_input() -> Option<ToolInput> {
        Some(ToolInput::Code {
            language: "sh",
            code: "echo hi\n".into(),
        })
    }

    fn code_output() -> Option<ToolOutput> {
        Some(ToolOutput::ReadCode {
            path: "test.rs".into(),
            start_line: 1,
            lines: vec!["fn main() {}".into()],
        })
    }

    fn plain_output() -> Option<ToolOutput> {
        Some(ToolOutput::Plain("ok".into()))
    }

    #[test_case(code_input(),  plain_output(),  true,  false ; "code_input_strips_plain_output")]
    #[test_case(code_input(),  code_output(),   true,  true  ; "code_input_keeps_code_output")]
    #[test_case(None,          code_output(),   true,  true  ; "code_output_only")]
    #[test_case(None,          plain_output(),  false, false ; "no_content_no_highlight")]
    fn highlight_request(
        input: Option<ToolInput>,
        output: Option<ToolOutput>,
        expect_highlight: bool,
        expect_output: bool,
    ) {
        let msg = DisplayMessage {
            role: DisplayRole::Tool {
                id: "t1".into(),
                status: ToolStatus::Success,
                name: BASH_TOOL_NAME,
            },
            text: "header\nbody".into(),
            tool_input: input,
            tool_output: output,
            plan_path: None,
            timestamp: None,
        };
        let tl = build_tool_lines(&msg, ToolStatus::Success, Instant::now());
        assert_eq!(tl.highlight.is_some(), expect_highlight);
        if let Some(hl) = &tl.highlight {
            assert_eq!(hl.output.is_some(), expect_output);
        }
    }

    #[test_case("foo (3 files)", "foo", Some(" (3 files)") ; "with_parens")]
    #[test_case("foo bar",       "foo bar", None            ; "without_parens")]
    fn split_trailing_annotation_cases(
        input: &str,
        expected_header: &str,
        expected_ann: Option<&str>,
    ) {
        let (header, ann) = split_trailing_annotation(input);
        assert_eq!(header, expected_header);
        assert_eq!(ann, expected_ann);
    }

    fn spans_text(spans: &[Span<'_>]) -> String {
        spans.iter().map(|s| s.content.as_ref()).collect()
    }

    fn has_styled_span(spans: &[Span<'_>], text: &str, style: Style) -> bool {
        spans
            .iter()
            .any(|s| s.content.contains(text) && s.style == style)
    }

    #[test]
    fn style_tool_header_path_first() {
        let spans = style_tool_header(WRITE_TOOL_NAME, "src/main.rs");
        assert_eq!(spans_text(&spans), "src/main.rs");
    }

    #[test]
    fn style_tool_header_in_path() {
        let spans = style_tool_header(BASH_TOOL_NAME, "echo hi in /tmp");
        let text = spans_text(&spans);
        assert!(text.contains("echo hi"));
        assert!(has_styled_span(&spans, "/tmp", theme::TOOL_PATH));
    }

    #[test]
    fn style_tool_header_truncates_json_in_path() {
        let spans = style_tool_header(
            GREP_TOOL_NAME,
            "STRIKETHROUGH_STYLE in /home/tony/c/maki2\", \"pattern\": \"STRIKETHROUGH_STYLE\"}",
        );
        let text = spans_text(&spans);
        assert!(text.contains("STRIKETHROUGH_STYLE"));
        assert!(text.contains("/home/tony/c/maki2"));
        assert!(!text.contains("pattern"));
    }

    #[test_case("TODO",                       "TODO"                        ; "pattern_only")]
    #[test_case("TODO [*.rs]",                "TODO [*.rs]"                 ; "with_include")]
    #[test_case("TODO in src/",               "TODO src/"                ; "with_path")]
    #[test_case("\\b(fn|pub)\\s+ [*.rs] in src/", "\\b(fn|pub)\\s+ [*.rs] src/" ; "with_include_and_path")]
    fn grep_header_text_roundtrips(input: &str, expected: &str) {
        assert_eq!(spans_text(&style_grep_header(input)), expected);
    }

    #[test]
    fn grep_header_styles_filter_and_path() {
        let spans = style_grep_header("TODO [*.rs] in src/");
        assert!(has_styled_span(&spans, "[*.rs]", theme::TOOL_ANNOTATION));
        assert!(has_styled_span(&spans, "src/", theme::TOOL_PATH));
    }

    fn lines_text(tl: &ToolLines) -> String {
        tl.lines
            .iter()
            .flat_map(|l| l.spans.iter())
            .map(|s| s.content.as_ref())
            .collect::<Vec<_>>()
            .join("")
    }

    #[test_case(ToolStatus::InProgress, None           ; "live_streaming_shows_body")]
    #[test_case(ToolStatus::Success,    plain_output() ; "done_with_plain_output_shows_body")]
    fn bash_body_visible(status: ToolStatus, output: Option<ToolOutput>) {
        let msg = DisplayMessage {
            role: DisplayRole::Tool {
                id: "t1".into(),
                status,
                name: BASH_TOOL_NAME,
            },
            text: "echo hi\nline1\nline2".into(),
            tool_input: code_input(),
            tool_output: output,
            plan_path: None,
            timestamp: None,
        };
        let tl = build_tool_lines(&msg, status, Instant::now());
        let text = lines_text(&tl);
        assert!(text.contains("line1"));
        assert!(text.contains("line2"));
    }

    #[test_case("header\nbody\nmore", "header" ; "multiline")]
    #[test_case("header",            "header" ; "single_line")]
    fn truncate_to_header_cases(input: &str, expected: &str) {
        let mut text = input.to_string();
        truncate_to_header(&mut text);
        assert_eq!(text, expected);
    }

    fn tool_msg() -> DisplayMessage {
        DisplayMessage {
            role: DisplayRole::Tool {
                id: "t1".into(),
                status: ToolStatus::Success,
                name: BASH_TOOL_NAME,
            },
            text: "cmd".into(),
            tool_input: None,
            tool_output: None,
            plan_path: None,
            timestamp: None,
        }
    }

    #[test_case(80, true  ; "shown_when_width_sufficient")]
    #[test_case(10, false ; "hidden_when_too_narrow")]
    fn append_timestamp_visibility(width: u16, expect_timestamp: bool) {
        let msg = tool_msg();
        let mut tl = build_tool_lines(&msg, ToolStatus::Success, Instant::now());
        let span_count_before = tl.lines[0].spans.len();
        append_timestamp(&mut tl.lines[0], "12:34:56", width);
        let last = tl.lines[0].spans.last().unwrap();
        if expect_timestamp {
            assert_eq!(last.style, theme::COMMENT_STYLE);
            assert_eq!(spans_text(&tl.lines[0].spans).len(), width as usize,);
        } else {
            assert_eq!(tl.lines[0].spans.len(), span_count_before);
        }
    }
}

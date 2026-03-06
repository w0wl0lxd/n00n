use super::{DisplayMessage, ToolStatus};

use super::code_view;
use crate::animation::spinner_frame;
use crate::markdown::TRUNCATION_PREFIX;
use crate::theme;

use std::time::Instant;

use jiff::Timestamp;
use jiff::tz::TimeZone;

use crate::markdown::{Keep, truncate_lines};
use maki_agent::tools::{
    BASH_TOOL_NAME, EDIT_TOOL_NAME, GLOB_TOOL_NAME, GREP_TOOL_NAME, MULTIEDIT_TOOL_NAME,
    READ_TOOL_NAME, WEBFETCH_TOOL_NAME, WEBSEARCH_TOOL_NAME, WRITE_TOOL_NAME,
};
use maki_agent::{BatchToolEntry, BatchToolStatus, TodoStatus, ToolInput, ToolOutput};
use ratatui::style::Style;
use ratatui::text::{Line, Span};

use crate::highlight::highlight_regex_inline;
use crate::render_worker::RenderWorker;

pub const TOOL_INDICATOR: &str = "● ";
pub const TOOL_OUTPUT_MAX_LINES: usize = 7;
pub const BASH_OUTPUT_MAX_LINES: usize = 10;
pub const TOOL_BODY_INDENT: &str = "  ";
const TIMESTAMP_LEN: usize = 8;
const PLAIN_ANNOTATION_THRESHOLD: usize = 10;
const ALWAYS_ANNOTATE_TOOLS: &[&str] = &[WEBFETCH_TOOL_NAME, WEBSEARCH_TOOL_NAME];

pub(crate) fn tool_output_annotation(output: &ToolOutput, tool: &str) -> Option<String> {
    match output {
        ToolOutput::ReadCode { lines, .. } => Some(format!("{} lines", lines.len())),
        ToolOutput::WriteCode { byte_count, .. } => Some(format!("{byte_count} bytes")),
        ToolOutput::GrepResult { entries, .. } => Some(format!("{} files", entries.len())),
        ToolOutput::GlobResult { files } if !files.is_empty() => {
            Some(format!("{} files", files.len()))
        }
        ToolOutput::Plain(text) => {
            let n = text.lines().count();
            if ALWAYS_ANNOTATE_TOOLS.contains(&tool) || n > PLAIN_ANNOTATION_THRESHOLD {
                Some(format!("{n} lines"))
            } else {
                None
            }
        }
        _ => None,
    }
}

fn extract_path_suffix(s: &str) -> Option<(&str, &str)> {
    let i = s.rfind(" in ")?;
    let path = s[i + 4..].split('"').next().unwrap();
    Some((&s[..i], path))
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
    pub spinner_lines: Vec<usize>,
    pub content_indent: &'static str,
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
            | ToolOutput::GlobResult { .. }
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

fn push_text_output_lines(lines: &mut Vec<Line<'static>>, text: &str, indent: &str) {
    for line in text.lines() {
        let style = if line.starts_with(TRUNCATION_PREFIX) {
            theme::TOOL_ANNOTATION
        } else {
            theme::TOOL
        };
        lines.push(Line::from(Span::styled(format!("{indent}{line}"), style)));
    }
}

fn push_structured_output_lines(
    lines: &mut Vec<Line<'static>>,
    output: Option<&ToolOutput>,
    indent: &str,
) {
    match output {
        Some(ToolOutput::TodoList(items)) => {
            for item in items {
                let style = match item.status {
                    TodoStatus::Completed => theme::TODO_COMPLETED,
                    TodoStatus::InProgress => theme::TODO_IN_PROGRESS,
                    TodoStatus::Pending => theme::TODO_PENDING,
                    TodoStatus::Cancelled => theme::TODO_CANCELLED,
                };
                lines.push(Line::from(Span::styled(
                    format!("{indent}{} {}", item.status.marker(), item.content),
                    style,
                )));
            }
        }
        Some(ToolOutput::QuestionAnswers(pairs)) => {
            for pair in pairs {
                lines.push(Line::from(vec![
                    Span::styled(format!("{indent}❯ "), theme::TOOL_ANNOTATION),
                    Span::styled(pair.question.clone(), theme::QUESTION_LABEL),
                    Span::styled(" → ", theme::TOOL_ANNOTATION),
                    Span::styled(pair.answer.clone(), theme::QUESTION_ANSWER),
                ]));
            }
        }
        _ => {}
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
    let tool_name = msg.role.tool_name().unwrap_or("?");
    let mut header_spans = vec![Span::styled(format!("{tool_name}> "), theme::TOOL_PREFIX)];
    header_spans.extend(style_tool_header(tool_name, header));
    if let Some(ann) = &msg.annotation {
        header_spans.push(Span::styled(format!(" ({ann})"), theme::TOOL_ANNOTATION));
    }
    let mut lines = vec![Line::from(header_spans)];

    let mut spinner_lines = Vec::new();

    let (indicator, indicator_style) = match status {
        ToolStatus::InProgress => {
            spinner_lines.push(0);
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
    for mut line in content {
        line.spans.insert(0, Span::raw(TOOL_BODY_INDENT.to_owned()));
        lines.push(line);
    }
    let content_end = lines.len();

    match msg.tool_output.as_ref() {
        None | Some(ToolOutput::Plain(_)) | Some(ToolOutput::GlobResult { .. }) => {
            if let Some((_, body)) = msg.text.split_once('\n') {
                push_text_output_lines(&mut lines, body, TOOL_BODY_INDENT);
            }
        }
        Some(ToolOutput::Batch { .. }) => {}
        other => push_structured_output_lines(&mut lines, other, TOOL_BODY_INDENT),
    }

    let highlight = HighlightRequest::new(
        (content_start, content_end),
        msg.tool_input.clone(),
        msg.tool_output.clone(),
    );

    ToolLines {
        lines,
        highlight,
        spinner_lines,
        content_indent: TOOL_BODY_INDENT,
    }
}

pub fn truncate_to_header(text: &mut String) {
    let end = text.find('\n').unwrap_or(text.len());
    text.truncate(end);
}

const BATCH_INDENT: &str = "  ";
const BATCH_CONTENT_INDENT: &str = "    ";
const BATCH_SEPARATOR: &str = "─";

fn indent_lines(lines: &mut [Line<'static>]) {
    for line in lines {
        line.spans.insert(0, Span::raw(BATCH_INDENT.to_owned()));
    }
}

pub fn build_batch_entry_lines(
    entry: &BatchToolEntry,
    index: usize,
    started_at: Instant,
) -> ToolLines {
    let mut header_spans = vec![Span::styled(
        format!("{}> ", entry.tool),
        theme::TOOL_PREFIX,
    )];
    header_spans.extend(style_tool_header(&entry.tool, &entry.summary));
    if let Some(ann) = entry
        .output
        .as_ref()
        .and_then(|o| tool_output_annotation(o, &entry.tool))
    {
        header_spans.push(Span::styled(format!(" ({ann})"), theme::TOOL_ANNOTATION));
    }
    let mut lines = vec![Line::from(header_spans)];

    let mut spinner_lines = Vec::new();

    let (indicator, indicator_style) = match entry.status {
        BatchToolStatus::Pending => ("○ ".into(), theme::TOOL_DIM),
        BatchToolStatus::InProgress => {
            spinner_lines.push(0);
            let ch = spinner_frame(started_at.elapsed().as_millis());
            (format!("{ch} "), theme::TOOL_IN_PROGRESS)
        }
        BatchToolStatus::Success => (TOOL_INDICATOR.into(), theme::TOOL_SUCCESS),
        BatchToolStatus::Error => (TOOL_INDICATOR.into(), theme::TOOL_ERROR),
    };
    lines[0]
        .spans
        .insert(0, Span::styled(indicator, indicator_style));

    let content =
        code_view::render_tool_content(entry.input.as_ref(), entry.output.as_ref(), false);
    let mut content_start = lines.len();
    for mut line in content {
        line.spans.insert(0, Span::raw(TOOL_BODY_INDENT.to_owned()));
        lines.push(line);
    }
    let mut content_end = lines.len();

    match entry.output.as_ref() {
        None => {}
        Some(ToolOutput::Plain(text)) => {
            let (max, keep) = if entry.tool == BASH_TOOL_NAME {
                (BASH_OUTPUT_MAX_LINES, Keep::Tail)
            } else {
                (TOOL_OUTPUT_MAX_LINES, Keep::Head)
            };
            push_text_output_lines(
                &mut lines,
                &truncate_lines(text, max, keep),
                TOOL_BODY_INDENT,
            );
        }
        Some(ToolOutput::GlobResult { files }) => {
            let joined = files.join("\n");
            push_text_output_lines(
                &mut lines,
                &truncate_lines(&joined, TOOL_OUTPUT_MAX_LINES, Keep::Head),
                TOOL_BODY_INDENT,
            );
        }
        other => push_structured_output_lines(&mut lines, other, TOOL_BODY_INDENT),
    }

    indent_lines(&mut lines);

    if index > 0 {
        lines.insert(
            0,
            Line::from(Span::styled(
                format!("{BATCH_INDENT}{}", BATCH_SEPARATOR.repeat(40)),
                theme::TOOL_DIM,
            )),
        );
        let offset = 1;
        spinner_lines.iter_mut().for_each(|l| *l += offset);
        content_start += offset;
        content_end += offset;
    }

    let highlight = HighlightRequest::new(
        (content_start, content_end),
        entry.input.clone(),
        entry.output.clone(),
    );

    ToolLines {
        lines,
        highlight,
        spinner_lines,
        content_indent: BATCH_CONTENT_INDENT,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::components::DisplayRole;
    use maki_agent::tools::{BASH_TOOL_NAME, WRITE_TOOL_NAME};
    use maki_agent::{BatchToolEntry, BatchToolStatus, GrepFileEntry, ToolInput, ToolOutput};
    use test_case::test_case;

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
            annotation: None,
            plan_path: None,
            timestamp: None,
        };
        let tl = build_tool_lines(&msg, ToolStatus::Success, Instant::now());
        assert_eq!(tl.highlight.is_some(), expect_highlight);
        if let Some(hl) = &tl.highlight {
            assert_eq!(hl.output.is_some(), expect_output);
        }
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
            annotation: None,
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
            annotation: None,
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

    #[test]
    fn batch_entry_annotation_rendered() {
        let entry = BatchToolEntry {
            tool: "read".into(),
            summary: "src/main.rs".into(),
            status: BatchToolStatus::Success,
            input: None,
            output: Some(ToolOutput::ReadCode {
                path: "src/main.rs".into(),
                start_line: 1,
                lines: vec!["x".into(); 42],
            }),
        };
        let tl = build_batch_entry_lines(&entry, 0, Instant::now());
        let text = lines_text(&tl);
        assert!(text.contains("(42 lines)"));
    }

    #[test]
    fn batch_entry_code_input_rendered() {
        let entry = BatchToolEntry {
            tool: "bash".into(),
            summary: "echo hi".into(),
            status: BatchToolStatus::Success,
            input: Some(ToolInput::Code {
                language: "bash",
                code: "echo hi\n".into(),
            }),
            output: None,
        };
        let tl = build_batch_entry_lines(&entry, 0, Instant::now());
        let text = lines_text(&tl);
        assert!(text.contains("echo hi"));
    }

    #[test_case(BatchToolStatus::InProgress, &[0]    ; "in_progress_has_spinner")]
    #[test_case(BatchToolStatus::Pending,    &[]     ; "pending_no_spinner")]
    #[test_case(BatchToolStatus::Success,    &[]     ; "success_no_spinner")]
    fn batch_entry_spinner(status: BatchToolStatus, expected: &[usize]) {
        let entry = BatchToolEntry {
            tool: "bash".into(),
            summary: "test".into(),
            status,
            input: None,
            output: None,
        };
        let tl = build_batch_entry_lines(&entry, 0, Instant::now());
        assert_eq!(tl.spinner_lines, expected);
    }

    #[test]
    fn batch_entry_separator_on_nonzero_index() {
        let entry = BatchToolEntry {
            tool: "bash".into(),
            summary: "test".into(),
            status: BatchToolStatus::Success,
            input: None,
            output: None,
        };
        let first = build_batch_entry_lines(&entry, 0, Instant::now());
        let second = build_batch_entry_lines(&entry, 1, Instant::now());
        assert!(second.lines.len() > first.lines.len());
        assert!(spans_text(&second.lines[0].spans).contains(BATCH_SEPARATOR));
    }

    #[test]
    fn batch_entry_spinner_offset_with_separator() {
        let entry = BatchToolEntry {
            tool: "bash".into(),
            summary: "test".into(),
            status: BatchToolStatus::InProgress,
            input: None,
            output: None,
        };
        let tl = build_batch_entry_lines(&entry, 1, Instant::now());
        assert_eq!(tl.spinner_lines, &[1]);
    }

    #[test]
    fn batch_entry_plain_output_rendered() {
        let entry = BatchToolEntry {
            tool: "bash".into(),
            summary: "echo hello".into(),
            status: BatchToolStatus::Success,
            input: None,
            output: Some(ToolOutput::Plain("hello world".into())),
        };
        let tl = build_batch_entry_lines(&entry, 0, Instant::now());
        let text = lines_text(&tl);
        assert!(text.contains("hello world"));
    }

    #[test_case("bash",  ToolOutput::Plain("ok".into()),                      None                ; "plain_short_no_annotation")]
    #[test_case("bash",  ToolOutput::Plain((0..20).map(|i| format!("line {i}")).collect::<Vec<_>>().join("\n")), Some("20 lines") ; "plain_long_annotates")]
    #[test_case("webfetch", ToolOutput::Plain("a\nb".into()),                 Some("2 lines")     ; "webfetch_always_annotates")]
    #[test_case("websearch", ToolOutput::Plain("r".into()),                   Some("1 lines")     ; "websearch_always_annotates")]
    #[test_case("read",  ToolOutput::ReadCode { path: "a.rs".into(), start_line: 1, lines: vec!["x".into(); 5] }, Some("5 lines") ; "read_code_lines")]
    #[test_case("write", ToolOutput::WriteCode { path: "a.rs".into(), byte_count: 99, lines: vec![] }, Some("99 bytes") ; "write_code_bytes")]
    #[test_case("grep",  ToolOutput::GrepResult { entries: vec![GrepFileEntry { path: "a.rs".into(), matches: vec![] }] }, Some("1 files") ; "grep_file_count")]
    #[test_case("glob",  ToolOutput::GlobResult { files: vec!["a".into(), "b".into()] }, Some("2 files") ; "glob_file_count")]
    #[test_case("glob",  ToolOutput::GlobResult { files: vec![] },            None                ; "glob_empty_no_annotation")]
    #[test_case("edit",  ToolOutput::Diff { path: "a.rs".into(), hunks: vec![], summary: "ok".into() }, None ; "diff_no_annotation")]
    fn annotation_cases(tool: &str, output: ToolOutput, expected: Option<&str>) {
        assert_eq!(tool_output_annotation(&output, tool).as_deref(), expected);
    }
}

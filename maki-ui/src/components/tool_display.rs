use super::{DisplayMessage, ToolStatus};

use super::code_view;
use crate::animation::spinner_frame;
use crate::markdown::TRUNCATION_PREFIX;
use crate::theme;

use std::fmt::Write;
use std::time::Instant;

use jiff::Timestamp;
use jiff::tz::TimeZone;

use crate::markdown::{Keep, truncate_lines};
use maki_agent::tools::{
    BASH_TOOL_NAME, CODE_EXECUTION_TOOL_NAME, EDIT_TOOL_NAME, GLOB_TOOL_NAME, GREP_TOOL_NAME,
    MULTIEDIT_TOOL_NAME, READ_TOOL_NAME, WEBFETCH_TOOL_NAME, WEBSEARCH_TOOL_NAME, WRITE_TOOL_NAME,
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
pub const CODE_EXECUTION_OUTPUT_MAX_LINES: usize = 30;

pub(crate) fn output_limits(tool: &str) -> (usize, Keep) {
    match tool {
        BASH_TOOL_NAME => (BASH_OUTPUT_MAX_LINES, Keep::Tail),
        CODE_EXECUTION_TOOL_NAME => (CODE_EXECUTION_OUTPUT_MAX_LINES, Keep::Tail),
        _ => (TOOL_OUTPUT_MAX_LINES, Keep::Head),
    }
}
const TIMESTAMP_LEN: usize = 8;
const PLAIN_ANNOTATION_THRESHOLD: usize = 10;
const BASH_OUTPUT_LABEL: &str = "· Output:";
const BASH_WAITING_LABEL: &str = "· Waiting for output...";
const BASH_NO_OUTPUT_LABEL: &str = "· No output.";
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

fn style_command_with_path(header: &str) -> Vec<Span<'static>> {
    match extract_path_suffix(header) {
        Some((cmd, path)) => vec![
            Span::styled(format!("{cmd} "), theme::current().tool),
            Span::styled(path.to_owned(), theme::current().tool_path),
        ],
        None => vec![Span::styled(header.to_owned(), theme::current().tool)],
    }
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
        spans.push(Span::styled(
            filter.to_owned(),
            theme::current().tool_annotation,
        ));
        &rest[bracket_end + 1..]
    } else {
        rest
    };

    if let Some((_, path)) = extract_path_suffix(after_pattern) {
        spans.push(Span::styled(format!(" {path}"), theme::current().tool_path));
    }

    spans
}

fn style_tool_header(tool: &str, header: &str) -> Vec<Span<'static>> {
    match tool {
        READ_TOOL_NAME | EDIT_TOOL_NAME | WRITE_TOOL_NAME | MULTIEDIT_TOOL_NAME => {
            vec![Span::styled(header.to_owned(), theme::current().tool_path)]
        }
        BASH_TOOL_NAME | GLOB_TOOL_NAME => style_command_with_path(header),
        GREP_TOOL_NAME => style_grep_header(header),
        CODE_EXECUTION_TOOL_NAME => vec![Span::styled(header.to_owned(), theme::current().tool)],
        _ => vec![Span::styled(header.to_owned(), theme::current().tool)],
    }
}

pub struct RoleStyle {
    pub prefix: &'static str,
    pub text_style: Style,
    pub prefix_style: Style,
    pub use_markdown: bool,
}

pub fn assistant_style() -> RoleStyle {
    RoleStyle {
        prefix: "maki> ",
        text_style: theme::current().assistant,
        prefix_style: theme::current().assistant_prefix,
        use_markdown: true,
    }
}

pub fn user_style() -> RoleStyle {
    RoleStyle {
        prefix: "you> ",
        text_style: theme::current().assistant,
        prefix_style: theme::current().user,
        use_markdown: true,
    }
}

pub fn thinking_style() -> RoleStyle {
    RoleStyle {
        prefix: "thinking> ",
        text_style: theme::current().thinking,
        prefix_style: theme::current().thinking,
        use_markdown: true,
    }
}

pub fn error_style() -> RoleStyle {
    RoleStyle {
        prefix: "",
        text_style: theme::current().error,
        prefix_style: theme::current().tool_error,
        use_markdown: false,
    }
}

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
    let w = width as usize + 1;
    if header_width + 1 + TIMESTAMP_LEN <= w {
        let pad = w - header_width - TIMESTAMP_LEN;
        line.spans.push(Span::raw(" ".repeat(pad)));
        line.spans.push(Span::styled(
            timestamp.to_owned(),
            theme::current().timestamp,
        ));
    }
}

enum Indicator {
    Pending,
    InProgress,
    Success,
    Error,
}

impl From<ToolStatus> for Indicator {
    fn from(s: ToolStatus) -> Self {
        match s {
            ToolStatus::InProgress => Self::InProgress,
            ToolStatus::Success => Self::Success,
            ToolStatus::Error => Self::Error,
        }
    }
}

impl From<BatchToolStatus> for Indicator {
    fn from(s: BatchToolStatus) -> Self {
        match s {
            BatchToolStatus::Pending => Self::Pending,
            BatchToolStatus::InProgress => Self::InProgress,
            BatchToolStatus::Success => Self::Success,
            BatchToolStatus::Error => Self::Error,
        }
    }
}

enum OutputMode<'a> {
    Fallback {
        body: Option<&'a str>,
        tool: &'a str,
        is_done: bool,
    },
    Truncated {
        tool: &'a str,
        is_done: bool,
    },
}

struct ToolLineBuilder {
    lines: Vec<Line<'static>>,
    spinner_lines: Vec<usize>,
    content_range: (usize, usize),
}

impl ToolLineBuilder {
    fn new() -> Self {
        Self {
            lines: Vec::new(),
            spinner_lines: Vec::new(),
            content_range: (0, 0),
        }
    }

    fn push_header(&mut self, tool_name: &str, header: &str, annotation: Option<&str>) {
        let mut spans = vec![Span::styled(
            format!("{tool_name}> "),
            theme::current().tool_prefix,
        )];
        spans.extend(style_tool_header(tool_name, header));
        if let Some(ann) = annotation {
            spans.push(Span::styled(
                format!(" ({ann})"),
                theme::current().tool_annotation,
            ));
        }
        self.lines.push(Line::from(spans));
    }

    fn prepend_indicator(&mut self, indicator: Indicator, started_at: Instant) {
        let (text, style) = match indicator {
            Indicator::Pending => ("○ ".into(), theme::current().tool_dim),
            Indicator::InProgress => {
                self.spinner_lines.push(0);
                let ch = spinner_frame(started_at.elapsed().as_millis());
                (format!("{ch} "), theme::current().spinner)
            }
            Indicator::Success => (TOOL_INDICATOR.into(), theme::current().tool_success),
            Indicator::Error => (TOOL_INDICATOR.into(), theme::current().tool_error),
        };
        self.lines[0].spans.insert(0, Span::styled(text, style));
    }

    fn push_code_content(&mut self, input: Option<&ToolInput>, output: Option<&ToolOutput>) {
        let content = code_view::render_tool_content(input, output, false);
        let start = self.lines.len();
        for mut line in content {
            line.spans.insert(0, Span::raw(TOOL_BODY_INDENT.to_owned()));
            self.lines.push(line);
        }
        self.content_range = (start, self.lines.len());
    }

    fn push_output(&mut self, output: Option<&ToolOutput>, mode: OutputMode<'_>) {
        match mode {
            OutputMode::Fallback {
                body,
                tool,
                is_done,
            } => self.push_output_fallback(output, body, tool, is_done),
            OutputMode::Truncated { tool, is_done } => {
                self.push_output_truncated(output, tool, is_done)
            }
        }
    }

    fn push_output_fallback(
        &mut self,
        output: Option<&ToolOutput>,
        body: Option<&str>,
        tool: &str,
        is_done: bool,
    ) {
        match output {
            None | Some(ToolOutput::Plain(_)) | Some(ToolOutput::GlobResult { .. }) => {
                let has_code = self.content_range.1 > self.content_range.0;
                if has_code && tool == BASH_TOOL_NAME {
                    self.push_bash_output_label(TOOL_BODY_INDENT, is_done, body.is_some());
                } else if has_code && body.is_some() {
                    self.push_code_output_separator(tool, TOOL_BODY_INDENT);
                }
                if let Some(text) = body {
                    push_text_lines(&mut self.lines, text, TOOL_BODY_INDENT);
                }
            }
            Some(ToolOutput::Batch { .. }) => {}
            other => push_structured_lines(&mut self.lines, other, TOOL_BODY_INDENT),
        }
    }

    fn push_bash_output_label(&mut self, indent: &str, is_done: bool, has_output: bool) {
        let label = match (has_output, is_done) {
            (true, _) => BASH_OUTPUT_LABEL,
            (false, false) => BASH_WAITING_LABEL,
            (false, true) => BASH_NO_OUTPUT_LABEL,
        };
        self.lines.push(Line::from(Span::styled(
            format!("{indent}{label}"),
            theme::current().tool_dim,
        )));
    }

    fn push_code_output_separator(&mut self, tool: &str, indent: &str) {
        if tool == CODE_EXECUTION_TOOL_NAME {
            self.lines.push(Line::from(Span::styled(
                format!("{indent}{}", super::TOOL_SEPARATOR),
                theme::current().tool_dim,
            )));
        }
    }

    fn push_output_truncated(&mut self, output: Option<&ToolOutput>, tool: &str, is_done: bool) {
        let has_code = self.content_range.1 > self.content_range.0;
        if has_code && tool == BASH_TOOL_NAME {
            let has_output = matches!(output, Some(ToolOutput::Plain(t)) if !t.is_empty());
            self.push_bash_output_label(TOOL_BODY_INDENT, is_done, has_output);
        } else if has_code {
            self.push_code_output_separator(tool, TOOL_BODY_INDENT);
        }
        match output {
            None => {}
            Some(ToolOutput::Plain(text)) => {
                let (max, keep) = output_limits(tool);
                push_text_lines(
                    &mut self.lines,
                    &truncate_lines(text, max, keep),
                    TOOL_BODY_INDENT,
                );
            }
            Some(ToolOutput::GlobResult { files }) => {
                let joined = files.join("\n");
                push_text_lines(
                    &mut self.lines,
                    &truncate_lines(&joined, TOOL_OUTPUT_MAX_LINES, Keep::Head),
                    TOOL_BODY_INDENT,
                );
            }
            other => push_structured_lines(&mut self.lines, other, TOOL_BODY_INDENT),
        }
    }

    fn indent_all(&mut self, prefix: &str) {
        for line in &mut self.lines {
            line.spans.insert(0, Span::raw(prefix.to_owned()));
        }
    }

    fn prepend_separator(&mut self, index: usize) {
        if index == 0 {
            return;
        }
        self.lines.insert(
            0,
            Line::from(Span::styled(
                format!("{BATCH_INDENT}{}", super::TOOL_SEPARATOR),
                theme::current().tool_dim,
            )),
        );
        self.spinner_lines.iter_mut().for_each(|l| *l += 1);
        self.content_range.0 += 1;
        self.content_range.1 += 1;
    }

    fn finish(
        self,
        input: Option<ToolInput>,
        output: Option<ToolOutput>,
        content_indent: &'static str,
    ) -> ToolLines {
        let highlight = HighlightRequest::new(self.content_range, input, output);
        ToolLines {
            lines: self.lines,
            highlight,
            spinner_lines: self.spinner_lines,
            content_indent,
        }
    }
}

fn push_text_lines(lines: &mut Vec<Line<'static>>, text: &str, indent: &str) {
    for line in text.lines() {
        let style = if line.starts_with(TRUNCATION_PREFIX) {
            theme::current().tool_dim
        } else {
            theme::current().tool
        };
        lines.push(Line::from(Span::styled(format!("{indent}{line}"), style)));
    }
}

fn push_structured_lines(
    lines: &mut Vec<Line<'static>>,
    output: Option<&ToolOutput>,
    indent: &str,
) {
    match output {
        Some(ToolOutput::TodoList(items)) => {
            for item in items {
                let style = match item.status {
                    TodoStatus::Completed => theme::current().todo_completed,
                    TodoStatus::InProgress => theme::current().todo_in_progress,
                    TodoStatus::Pending => theme::current().todo_pending,
                    TodoStatus::Cancelled => theme::current().todo_cancelled,
                };
                lines.push(Line::from(Span::styled(
                    format!("{indent}{} {}", item.status.marker(), item.content),
                    style,
                )));
            }
        }
        Some(ToolOutput::QuestionAnswers(_)) => {}
        _ => {}
    }
}

pub fn build_tool_lines(
    msg: &DisplayMessage,
    status: ToolStatus,
    started_at: Instant,
) -> ToolLines {
    let tool_name = msg.role.tool_name().unwrap_or("?");
    let (header, body) = match msg.text.split_once('\n') {
        Some((h, b)) => (h, Some(b)),
        None => (msg.text.as_str(), None),
    };

    let mut b = ToolLineBuilder::new();
    b.push_header(tool_name, header, msg.annotation.as_deref());
    b.prepend_indicator(status.into(), started_at);
    b.push_code_content(msg.tool_input.as_ref(), msg.tool_output.as_ref());
    let is_done = status != ToolStatus::InProgress;
    b.push_output(
        msg.tool_output.as_ref(),
        OutputMode::Fallback {
            body,
            tool: tool_name,
            is_done,
        },
    );
    b.finish(
        msg.tool_input.clone(),
        msg.tool_output.clone(),
        TOOL_BODY_INDENT,
    )
}

pub fn truncate_to_header(text: &mut String) {
    let end = text.find('\n').unwrap_or(text.len());
    text.truncate(end);
}

const BATCH_INDENT: &str = "  ";
const BATCH_CONTENT_INDENT: &str = "    ";

pub fn build_batch_entry_lines(
    entry: &BatchToolEntry,
    index: usize,
    started_at: Instant,
) -> ToolLines {
    let mut annotation = entry.annotation.clone();
    if let Some(suffix) = entry
        .output
        .as_ref()
        .and_then(|o| tool_output_annotation(o, &entry.tool))
    {
        append_annotation(&mut annotation, &suffix);
    }

    let mut b = ToolLineBuilder::new();
    b.push_header(&entry.tool, &entry.summary, annotation.as_deref());
    b.prepend_indicator(entry.status.into(), started_at);
    b.push_code_content(entry.input.as_ref(), entry.output.as_ref());
    let is_done = matches!(
        entry.status,
        BatchToolStatus::Success | BatchToolStatus::Error
    );
    b.push_output(
        entry.output.as_ref(),
        OutputMode::Truncated {
            tool: &entry.tool,
            is_done,
        },
    );
    b.indent_all(BATCH_INDENT);
    b.prepend_separator(index);
    b.finish(
        entry.input.clone(),
        entry.output.clone(),
        BATCH_CONTENT_INDENT,
    )
}

pub(crate) fn append_annotation(ann: &mut Option<String>, suffix: &str) {
    match ann {
        Some(a) => write!(a, " · {suffix}").unwrap(),
        None => *ann = Some(suffix.to_owned()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::components::{DisplayRole, TOOL_SEPARATOR};
    use maki_agent::tools::{BASH_TOOL_NAME, WRITE_TOOL_NAME};
    use maki_agent::{BatchToolEntry, BatchToolStatus, GrepFileEntry, ToolInput, ToolOutput};
    use test_case::test_case;

    fn code_input() -> Option<ToolInput> {
        Some(ToolInput::Code {
            language: "sh".into(),
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
        assert!(has_styled_span(&spans, "/tmp", theme::current().tool_path));
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
        assert!(has_styled_span(
            &spans,
            "[*.rs]",
            theme::current().tool_annotation
        ));
        assert!(has_styled_span(&spans, "src/", theme::current().tool_path));
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

    fn line_has_styled(tl: &ToolLines, text: &str, style: Style) -> bool {
        tl.lines
            .iter()
            .any(|l| has_styled_span(&l.spans, text, style))
    }

    #[test]
    fn bash_output_label_when_has_output() {
        let msg = DisplayMessage {
            role: DisplayRole::Tool {
                id: "t1".into(),
                status: ToolStatus::Success,
                name: BASH_TOOL_NAME,
            },
            text: "echo hi\nhello".into(),
            tool_input: code_input(),
            tool_output: plain_output(),
            annotation: None,
            plan_path: None,
            timestamp: None,
        };
        let tl = build_tool_lines(&msg, ToolStatus::Success, Instant::now());
        assert!(line_has_styled(
            &tl,
            BASH_OUTPUT_LABEL,
            theme::current().tool_dim
        ));
    }

    #[test]
    fn bash_no_label_without_code_input() {
        let msg = DisplayMessage {
            role: DisplayRole::Tool {
                id: "t1".into(),
                status: ToolStatus::Success,
                name: BASH_TOOL_NAME,
            },
            text: "echo hi\nhello".into(),
            tool_input: None,
            tool_output: plain_output(),
            annotation: None,
            plan_path: None,
            timestamp: None,
        };
        let tl = build_tool_lines(&msg, ToolStatus::Success, Instant::now());
        assert!(!line_has_styled(
            &tl,
            BASH_OUTPUT_LABEL,
            theme::current().tool_dim
        ));
    }

    #[test]
    fn bash_waiting_label_when_in_progress_no_output() {
        let msg = DisplayMessage {
            role: DisplayRole::Tool {
                id: "t1".into(),
                status: ToolStatus::InProgress,
                name: BASH_TOOL_NAME,
            },
            text: "echo hi".into(),
            tool_input: code_input(),
            tool_output: None,
            annotation: None,
            plan_path: None,
            timestamp: None,
        };
        let tl = build_tool_lines(&msg, ToolStatus::InProgress, Instant::now());
        assert!(line_has_styled(
            &tl,
            BASH_WAITING_LABEL,
            theme::current().tool_dim
        ));
    }

    #[test]
    fn bash_no_output_label_when_done_empty() {
        let msg = DisplayMessage {
            role: DisplayRole::Tool {
                id: "t1".into(),
                status: ToolStatus::Success,
                name: BASH_TOOL_NAME,
            },
            text: "echo hi".into(),
            tool_input: code_input(),
            tool_output: Some(ToolOutput::Plain(String::new())),
            annotation: None,
            plan_path: None,
            timestamp: None,
        };
        let tl = build_tool_lines(&msg, ToolStatus::Success, Instant::now());
        assert!(line_has_styled(
            &tl,
            BASH_NO_OUTPUT_LABEL,
            theme::current().tool_dim
        ));
    }

    #[test]
    fn batch_bash_output_label_between_code_and_output() {
        let entry = BatchToolEntry {
            tool: "bash".into(),
            summary: "echo hi".into(),
            status: BatchToolStatus::Success,
            input: code_input(),
            output: Some(ToolOutput::Plain("hello".into())),
            annotation: None,
        };
        let tl = build_batch_entry_lines(&entry, 0, Instant::now());
        assert!(line_has_styled(
            &tl,
            BASH_OUTPUT_LABEL,
            theme::current().tool_dim
        ));
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
            assert_eq!(last.style, theme::current().timestamp);
            assert_eq!(spans_text(&tl.lines[0].spans).len(), width as usize + 1);
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
            annotation: None,
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
                language: "bash".into(),
                code: "echo hi\n".into(),
            }),
            output: None,
            annotation: None,
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
            annotation: None,
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
            annotation: None,
        };
        let first = build_batch_entry_lines(&entry, 0, Instant::now());
        let second = build_batch_entry_lines(&entry, 1, Instant::now());
        assert!(second.lines.len() > first.lines.len());
        assert!(spans_text(&second.lines[0].spans).contains(TOOL_SEPARATOR));
    }

    #[test]
    fn batch_entry_spinner_offset_with_separator() {
        let entry = BatchToolEntry {
            tool: "bash".into(),
            summary: "test".into(),
            status: BatchToolStatus::InProgress,
            input: None,
            output: None,
            annotation: None,
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
            annotation: None,
        };
        let tl = build_batch_entry_lines(&entry, 0, Instant::now());
        let text = lines_text(&tl);
        assert!(text.contains("hello world"));
    }

    #[test]
    fn annotation_rendered_on_header() {
        let mut msg = tool_msg();
        msg.annotation = Some("2m timeout".into());
        let tl = build_tool_lines(&msg, ToolStatus::Success, Instant::now());
        let text = lines_text(&tl);
        assert!(text.contains("(2m timeout)"));
    }

    #[test]
    fn batch_entry_stored_annotation_rendered() {
        let entry = BatchToolEntry {
            tool: "task".into(),
            summary: "research".into(),
            status: BatchToolStatus::Success,
            input: None,
            output: None,
            annotation: Some("anthropic/claude-haiku-4-20250414".into()),
        };
        let tl = build_batch_entry_lines(&entry, 0, Instant::now());
        let text = lines_text(&tl);
        assert!(text.contains("(anthropic/claude-haiku-4-20250414)"));
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

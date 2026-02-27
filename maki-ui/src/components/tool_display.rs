use super::{DisplayMessage, ToolStatus};

use super::code_view;
use crate::animation::spinner_frame;
use crate::markdown::TRUNCATION_PREFIX;
use crate::theme;

use std::time::Instant;

use maki_agent::tools::{
    BASH_TOOL_NAME, EDIT_TOOL_NAME, GLOB_TOOL_NAME, GREP_TOOL_NAME, MULTIEDIT_TOOL_NAME,
    READ_TOOL_NAME, WEBFETCH_TOOL_NAME, WRITE_TOOL_NAME,
};
use maki_providers::{ToolInput, ToolOutput};
use ratatui::style::Style;
use ratatui::text::{Line, Span};

use crate::highlight::HighlightWorker;

pub const TOOL_INDICATOR: &str = "● ";
pub const TOOL_OUTPUT_MAX_LINES: usize = 7;
pub const BASH_OUTPUT_MAX_LINES: usize = 10;
pub const TOOL_BODY_INDENT: &str = "  ";

pub fn tool_summary_annotation(tool: &str, text: &str) -> Option<String> {
    match tool {
        GLOB_TOOL_NAME => Some(format!("{} files", text.lines().count())),
        WEBFETCH_TOOL_NAME => Some(format!("{} lines", text.lines().count())),
        _ => {
            let n = text.lines().count();
            (n > BASH_OUTPUT_MAX_LINES).then(|| format!("{n} lines"))
        }
    }
}

const PATH_FIRST_TOOLS: &[&str] = &[
    READ_TOOL_NAME,
    EDIT_TOOL_NAME,
    WRITE_TOOL_NAME,
    MULTIEDIT_TOOL_NAME,
];
const IN_PATH_TOOLS: &[&str] = &[BASH_TOOL_NAME, GLOB_TOOL_NAME, GREP_TOOL_NAME];

fn split_trailing_annotation(s: &str) -> (&str, Option<&str>) {
    if let Some(i) = s.rfind(" (")
        && s.ends_with(')')
    {
        return (&s[..i], Some(&s[i..]));
    }
    (s, None)
}

fn style_tool_header(tool: &str, header: &str) -> Vec<Span<'static>> {
    if PATH_FIRST_TOOLS.contains(&tool) {
        return vec![Span::styled(header.to_owned(), theme::TOOL_PATH)];
    }
    if IN_PATH_TOOLS.contains(&tool)
        && let Some(i) = header.rfind(" in ")
    {
        let (cmd, rest) = header.split_at(i);
        return vec![
            Span::styled(format!("{cmd} in "), theme::TOOL),
            Span::styled(rest[4..].to_owned(), theme::TOOL_PATH),
        ];
    }
    vec![Span::styled(header.to_owned(), theme::TOOL)]
}

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

impl ToolLines {
    pub fn send_highlight(&self, worker: &HighlightWorker) -> Option<u64> {
        let hl = self.highlight.as_ref()?;
        Some(worker.send(hl.input.clone(), hl.output.clone()))
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
    let prefix = format!("{tool_name}> ");
    let mut header_spans = vec![Span::styled(prefix, theme::TOOL_PREFIX)];
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
    let has_content = !content.is_empty();

    let content_start = lines.len();
    lines.extend(content);
    let content_end = lines.len();

    let show_body = !has_content || msg.tool_output.is_none();
    if show_body {
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
                    let style = if entry.is_error {
                        theme::TOOL_ERROR
                    } else {
                        theme::TOOL_SUCCESS
                    };
                    let mut spans = vec![
                        Span::styled(TOOL_BODY_INDENT.to_owned(), style),
                        Span::styled(TOOL_INDICATOR, style),
                        Span::styled(format!("{}> ", entry.tool), theme::TOOL_PREFIX),
                    ];
                    spans.extend(style_tool_header(&entry.tool, &entry.summary));
                    lines.push(Line::from(spans));
                }
            }
            _ => {}
        }
    }

    let highlight = has_content.then(|| HighlightRequest {
        range: (content_start, content_end),
        input: msg.tool_input.clone(),
        output: msg.tool_output.clone(),
    });

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

    #[test_case(code_input(),  plain_output(),  true  ; "input_code_needs_highlight")]
    #[test_case(None,          code_output(),   true  ; "code_output_needs_highlight")]
    #[test_case(code_input(),  code_output(),   true  ; "both_input_and_output_need_highlight")]
    #[test_case(None,          plain_output(),  false ; "plain_no_input_skips_highlight")]
    fn highlight_job_presence(
        input: Option<ToolInput>,
        output: Option<ToolOutput>,
        expect_highlight: bool,
    ) {
        let msg = DisplayMessage {
            role: DisplayRole::Tool {
                id: "t1".into(),
                status: ToolStatus::Success,
                name: "bash",
            },
            text: "header\nbody".into(),
            tool_input: input,
            tool_output: output,
        };
        let tl = build_tool_lines(&msg, ToolStatus::Success, Instant::now());
        assert_eq!(tl.highlight.is_some(), expect_highlight);
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

    #[test]
    fn style_tool_header_path_first() {
        let spans = style_tool_header(WRITE_TOOL_NAME, "src/main.rs");
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].content, "src/main.rs");
    }

    #[test]
    fn style_tool_header_in_path() {
        let spans = style_tool_header(BASH_TOOL_NAME, "echo hi in /tmp");
        assert_eq!(spans.len(), 2);
        assert!(spans[0].content.contains("echo hi in "));
        assert_eq!(spans[1].content, "/tmp");
    }

    #[test]
    fn bash_live_output_shown_with_code_input() {
        let msg = DisplayMessage {
            role: DisplayRole::Tool {
                id: "t1".into(),
                status: ToolStatus::InProgress,
                name: BASH_TOOL_NAME,
            },
            text: "echo hi\nline1\nline2".into(),
            tool_input: code_input(),
            tool_output: None,
        };
        let tl = build_tool_lines(&msg, ToolStatus::InProgress, Instant::now());
        let text: String = tl
            .lines
            .iter()
            .flat_map(|l| l.spans.iter())
            .map(|s| s.content.as_ref())
            .collect::<Vec<_>>()
            .join("");
        assert!(text.contains("line1"), "live output line1 missing");
        assert!(text.contains("line2"), "live output line2 missing");
    }

    #[test]
    fn bash_done_hides_body_when_code_output_present() {
        let msg = DisplayMessage {
            role: DisplayRole::Tool {
                id: "t1".into(),
                status: ToolStatus::Success,
                name: BASH_TOOL_NAME,
            },
            text: "echo hi\nstale body".into(),
            tool_input: code_input(),
            tool_output: plain_output(),
        };
        let tl = build_tool_lines(&msg, ToolStatus::Success, Instant::now());
        let text: String = tl
            .lines
            .iter()
            .flat_map(|l| l.spans.iter())
            .map(|s| s.content.as_ref())
            .collect::<Vec<_>>()
            .join("");
        assert!(
            !text.contains("stale body"),
            "body should be hidden once tool_output is set"
        );
    }

    #[test_case("header\nbody\nmore", "header" ; "multiline")]
    #[test_case("header",            "header" ; "single_line")]
    fn truncate_to_header_cases(input: &str, expected: &str) {
        let mut text = input.to_string();
        truncate_to_header(&mut text);
        assert_eq!(text, expected);
    }
}

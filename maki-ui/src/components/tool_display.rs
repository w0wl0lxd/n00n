use super::status_bar::format_tokens;
use super::{DisplayMessage, ToolStatus};

use super::{code_view, index_highlight};
use crate::animation::spinner_frame;
use crate::theme;

use std::fmt::Write;
use std::time::Instant;

use unicode_width::UnicodeWidthStr;

use maki_providers::{ModelPricing, TokenUsage};

use jiff::Timestamp;
use jiff::tz::TimeZone;

use crate::markdown::{Keep, text_to_lines, truncate_output, truncation_notice};
use maki_agent::tools::{
    BASH_TOOL_NAME, CODE_EXECUTION_TOOL_NAME, EDIT_TOOL_NAME, GLOB_TOOL_NAME, GREP_TOOL_NAME,
    INDEX_TOOL_NAME, MULTIEDIT_TOOL_NAME, READ_TOOL_NAME, TASK_TOOL_NAME, WEBFETCH_TOOL_NAME,
    WEBSEARCH_TOOL_NAME, WRITE_TOOL_NAME,
};
use maki_agent::{
    BatchToolEntry, BatchToolStatus, InstructionBlock, TodoStatus, ToolInput, ToolOutput,
};
use ratatui::style::Style;
use ratatui::text::{Line, Span};

use crate::highlight::highlight_regex_inline;
use crate::render_worker::RenderWorker;

pub const TOOL_INDICATOR: &str = "● ";
pub const TOOL_BODY_INDENT: &str = "  ";
const TOOL_SEPARATOR: &str = "──────────────────";
const TOOL_OUTPUT_MAX_LINES: usize = 7;
const BASH_OUTPUT_MAX_LINES: usize = 10;
const CODE_EXECUTION_OUTPUT_SEPARATOR: &str = "────────────";
const INSTRUCTION_SEPARATOR: &str = "────────────";
const CODE_EXECUTION_OUTPUT_MAX_LINES: usize = 30;
const TASK_OUTPUT_MAX_LINES: usize = 30;
const INDEX_OUTPUT_MAX_LINES: usize = 15;
const GREP_OUTPUT_MAX_LINES: usize = 15;
const BASH_WAITING_LABEL: &str = "Waiting for output...";
const BASH_NO_OUTPUT_LABEL: &str = "No output.";
const BASH_OUTPUT_SEPARATOR: &str = "──────";
const ALWAYS_ANNOTATE_TOOLS: &[&str] = &[WEBFETCH_TOOL_NAME, WEBSEARCH_TOOL_NAME, INDEX_TOOL_NAME];
const PLAIN_ANNOTATION_THRESHOLD: usize = 10;
const BATCH_INDENT: &str = "  ";
const BATCH_CONTENT_INDENT: &str = "    ";

pub(crate) fn output_limits(tool: &str) -> (usize, Keep) {
    match tool {
        BASH_TOOL_NAME => (BASH_OUTPUT_MAX_LINES, Keep::Tail),
        CODE_EXECUTION_TOOL_NAME => (CODE_EXECUTION_OUTPUT_MAX_LINES, Keep::Tail),
        TASK_TOOL_NAME => (TASK_OUTPUT_MAX_LINES, Keep::Head),
        INDEX_TOOL_NAME => (INDEX_OUTPUT_MAX_LINES, Keep::Head),
        GREP_TOOL_NAME => (GREP_OUTPUT_MAX_LINES, Keep::Head),
        _ => (TOOL_OUTPUT_MAX_LINES, Keep::Head),
    }
}

fn renders_markdown(tool: &str) -> bool {
    tool == TASK_TOOL_NAME
}

pub(crate) fn tool_output_annotation(output: &ToolOutput, tool: &str) -> Option<String> {
    match output {
        ToolOutput::ReadCode {
            lines, total_lines, ..
        } => {
            let shown = lines.len();
            if *total_lines > shown {
                Some(format!("{shown} of {} lines", total_lines))
            } else {
                Some(format!("{shown} lines"))
            }
        }
        ToolOutput::WriteCode { byte_count, .. } => Some(format!("{byte_count} bytes")),
        ToolOutput::GrepResult { entries } => Some(format!("{} files", entries.len())),
        ToolOutput::GlobResult { files } if !files.is_empty() => {
            Some(format!("{} files", files.len()))
        }
        ToolOutput::ReadDir { text, .. } => {
            let n = text.lines().count();
            Some(format!("{n} entries"))
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
        READ_TOOL_NAME | EDIT_TOOL_NAME | WRITE_TOOL_NAME | MULTIEDIT_TOOL_NAME
        | INDEX_TOOL_NAME => {
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

pub fn done_style() -> RoleStyle {
    RoleStyle {
        prefix: "",
        text_style: theme::current()
            .tool_success
            .add_modifier(ratatui::style::Modifier::BOLD),
        prefix_style: theme::current().tool_success,
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
    pub width: u16,
}

impl HighlightRequest {
    fn new(
        range: (usize, usize),
        input: Option<ToolInput>,
        output: Option<ToolOutput>,
        width: u16,
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
            | ToolOutput::ReadDir { .. }
            | ToolOutput::TodoList(_)
            | ToolOutput::Batch { .. }
            | ToolOutput::GlobResult { .. }
            | ToolOutput::QuestionAnswers(_) => None,
        });
        Some(Self {
            range,
            input,
            output,
            width,
        })
    }
}

impl ToolLines {
    pub fn send_highlight(&self, worker: &RenderWorker) -> Option<u64> {
        let hl = self.highlight.as_ref()?;
        Some(worker.send(hl.input.clone(), hl.output.clone(), hl.width))
    }
}

pub fn format_timestamp_now() -> String {
    let zoned = Timestamp::now().to_zoned(TimeZone::system());
    zoned.strftime("%H:%M:%S").to_string()
}

pub fn format_turn_usage(usage: &TokenUsage, pricing: &ModelPricing) -> String {
    let cost = usage.cost(pricing);
    format!(
        "{}↑ {}↓ ${cost:.3}",
        format_tokens(usage.total_input()),
        format_tokens(usage.output),
    )
}

pub fn append_right_info(
    line: &mut Line<'static>,
    usage: Option<&str>,
    timestamp: Option<&str>,
    width: u16,
) {
    if usage.is_none() && timestamp.is_none() {
        return;
    }
    let separator = if usage.is_some() && timestamp.is_some() {
        2
    } else {
        0
    };
    let suffix_len =
        usage.map_or(0, UnicodeWidthStr::width) + timestamp.map_or(0, str::len) + separator;
    let header_width: usize = line.spans.iter().map(|s| s.content.len()).sum();
    let w = width as usize + 1;
    if header_width + 1 + suffix_len > w {
        return;
    }
    let pad = w - header_width - suffix_len;
    line.spans.push(Span::raw(" ".repeat(pad)));
    if let Some(u) = usage {
        line.spans
            .push(Span::styled(u.to_owned(), theme::current().tool_dim));
        if timestamp.is_some() {
            line.spans.push(Span::raw("  "));
        }
    }
    if let Some(ts) = timestamp {
        line.spans
            .push(Span::styled(ts.to_owned(), theme::current().timestamp));
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
        pre_truncated: usize,
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
    width: u16,
    outer_indent: &'static str,
}

impl ToolLineBuilder {
    fn new(width: u16, outer_indent: &'static str) -> Self {
        Self {
            lines: Vec::new(),
            spinner_lines: Vec::new(),
            content_range: (0, 0),
            width: width.saturating_sub(outer_indent.len() as u16),
            outer_indent,
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
        let content_width = self.width.saturating_sub(TOOL_BODY_INDENT.len() as u16);
        let content = code_view::render_tool_content(input, output, false, content_width);
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
                pre_truncated,
            } => self.push_output_fallback(output, body, tool, is_done, pre_truncated),
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
        pre_truncated: usize,
    ) {
        match output {
            None
            | Some(ToolOutput::Plain(_))
            | Some(ToolOutput::ReadDir { .. })
            | Some(ToolOutput::GlobResult { .. }) => {
                if renders_markdown(tool) {
                    let text = match output {
                        Some(ToolOutput::Plain(t)) => Some(t.as_str()),
                        _ => body,
                    };
                    if let Some(text) = text {
                        let (max, keep) = output_limits(tool);
                        let tr = truncate_output(text, max, keep);
                        self.push_markdown_body(&tr.kept);
                        let skipped = if tr.skipped > 0 {
                            tr.skipped
                        } else {
                            pre_truncated
                        };
                        self.push_truncation_count(skipped);
                    }
                    return;
                }
                if tool == INDEX_TOOL_NAME {
                    if let Some(ToolOutput::Plain(text)) = output {
                        let (max, keep) = output_limits(tool);
                        let tr = truncate_output(text, max, keep);
                        self.push_index_body(&tr.kept);
                        self.push_truncation_count(tr.skipped);
                    } else if let Some(text) = body {
                        self.push_index_body(text);
                        self.push_truncation_count(pre_truncated);
                    }
                    return;
                }
                let has_code = self.content_range.1 > self.content_range.0;
                if has_code && tool == BASH_TOOL_NAME {
                    self.push_bash_output_label(TOOL_BODY_INDENT, is_done, body.is_some());
                } else if has_code && body.is_some() {
                    self.push_code_output_separator(tool, TOOL_BODY_INDENT);
                }
                if let Some(text) = body {
                    let (_, keep) = output_limits(tool);
                    if matches!(keep, Keep::Tail) {
                        self.push_truncation_count(pre_truncated);
                    }
                    push_text_lines(&mut self.lines, text, TOOL_BODY_INDENT);
                    if matches!(keep, Keep::Head) {
                        self.push_truncation_count(pre_truncated);
                    }
                }
                if let Some(ToolOutput::ReadDir { instructions, .. }) = output {
                    self.maybe_push_instructions(instructions.as_deref());
                }
            }
            Some(ToolOutput::Batch { .. }) => {}
            other => push_structured_lines(&mut self.lines, other, TOOL_BODY_INDENT),
        }
    }

    fn push_markdown_body(&mut self, text: &str) {
        let style = theme::current().assistant;
        let indent = TOOL_BODY_INDENT.len() as u16;
        let md_lines = text_to_lines(
            text,
            "",
            style,
            style,
            None,
            self.width.saturating_sub(indent),
        );
        for mut line in md_lines {
            line.spans.insert(0, Span::raw(TOOL_BODY_INDENT.to_owned()));
            self.lines.push(line);
        }
    }

    fn push_truncation_count(&mut self, skipped: usize) {
        if skipped > 0 {
            let text = truncation_notice(skipped);
            let mut line = Line::from(Span::styled(text, theme::current().tool_dim));
            line.spans.insert(0, Span::raw(TOOL_BODY_INDENT.to_owned()));
            self.lines.push(line);
        }
    }

    fn push_bash_output_label(&mut self, indent: &str, is_done: bool, has_output: bool) {
        self.lines.push(Line::from(Span::styled(
            format!("{indent}{BASH_OUTPUT_SEPARATOR}"),
            theme::current().tool_dim,
        )));
        let label = match (has_output, is_done) {
            (true, _) => return,
            (false, false) => BASH_WAITING_LABEL,
            (false, true) => BASH_NO_OUTPUT_LABEL,
        };
        self.lines.push(Line::from(Span::styled(
            format!("{indent}{label}"),
            theme::current().tool_dim,
        )));
    }

    fn push_index_body(&mut self, text: &str) {
        index_highlight::push_index_lines(&mut self.lines, text, TOOL_BODY_INDENT);
    }

    fn push_code_output_separator(&mut self, tool: &str, indent: &str) {
        if tool == CODE_EXECUTION_TOOL_NAME {
            self.lines.push(Line::from(Span::styled(
                format!("{indent}{}", CODE_EXECUTION_OUTPUT_SEPARATOR),
                theme::current().tool_dim,
            )));
        }
    }

    fn maybe_push_instructions(&mut self, blocks: Option<&[InstructionBlock]>) {
        if let Some(blocks) = blocks {
            self.push_instruction_separator(TOOL_BODY_INDENT);
            self.push_instructions(blocks);
        }
    }

    fn push_instruction_separator(&mut self, indent: &str) {
        self.lines.push(Line::from(Span::styled(
            format!("{indent}{INSTRUCTION_SEPARATOR}"),
            theme::current().tool_dim,
        )));
    }

    fn push_instructions(&mut self, blocks: &[InstructionBlock]) {
        let content_width = self.width.saturating_sub(TOOL_BODY_INDENT.len() as u16);
        let start = self.lines.len();
        code_view::render_instructions(blocks, &mut self.lines, content_width);
        for line in &mut self.lines[start..] {
            line.spans.insert(0, Span::raw(TOOL_BODY_INDENT.to_owned()));
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
                let tr = truncate_output(text, max, keep);
                if matches!(keep, Keep::Tail) {
                    self.push_truncation_count(tr.skipped);
                }
                if renders_markdown(tool) {
                    self.push_markdown_body(&tr.kept);
                } else if tool == INDEX_TOOL_NAME {
                    self.push_index_body(&tr.kept);
                } else {
                    push_text_lines(&mut self.lines, &tr.kept, TOOL_BODY_INDENT);
                }
                if matches!(keep, Keep::Head) {
                    self.push_truncation_count(tr.skipped);
                }
            }
            Some(ToolOutput::GlobResult { .. }) => {
                let text = output.unwrap().as_display_text();
                let (max, keep) = output_limits(tool);
                let tr = truncate_output(&text, max, keep);
                push_text_lines(&mut self.lines, &tr.kept, TOOL_BODY_INDENT);
                self.push_truncation_count(tr.skipped);
            }
            Some(ToolOutput::ReadDir { text, instructions }) => {
                let (max, keep) = output_limits(tool);
                let tr = truncate_output(text, max, keep);
                push_text_lines(&mut self.lines, &tr.kept, TOOL_BODY_INDENT);
                self.push_truncation_count(tr.skipped);
                self.maybe_push_instructions(instructions.as_deref());
            }
            other => push_structured_lines(&mut self.lines, other, TOOL_BODY_INDENT),
        }
    }

    fn prepend_separator(&mut self, index: usize) {
        if index == 0 {
            return;
        }
        let sep = [
            Line::default(),
            Line::from(Span::styled(
                TOOL_SEPARATOR.to_owned(),
                theme::current().tool_dim,
            )),
            Line::default(),
        ];
        self.lines.splice(0..0, sep);
        self.spinner_lines.iter_mut().for_each(|l| *l += 3);
        self.content_range.0 += 3;
        self.content_range.1 += 3;
    }

    fn finish(
        mut self,
        input: Option<ToolInput>,
        output: Option<ToolOutput>,
        content_indent: &'static str,
    ) -> ToolLines {
        if !self.outer_indent.is_empty() {
            for line in &mut self.lines {
                line.spans
                    .insert(0, Span::raw(self.outer_indent.to_owned()));
            }
        }
        let full_width = self.width + self.outer_indent.len() as u16;
        let content_width = full_width.saturating_sub(content_indent.len() as u16);
        let highlight = HighlightRequest::new(self.content_range, input, output, content_width);
        ToolLines {
            lines: self.lines,
            highlight,
            spinner_lines: self.spinner_lines,
            content_indent,
        }
    }
}

fn push_text_lines(lines: &mut Vec<Line<'static>>, text: &str, indent: &str) {
    let style = theme::current().tool;
    for line in text.lines() {
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
    width: u16,
) -> ToolLines {
    let tool_name = msg.role.tool_name().unwrap_or("?");
    let (header, body) = match msg.text.split_once('\n') {
        Some((h, b)) => (h, Some(b)),
        None => (msg.text.as_str(), None),
    };

    let mut b = ToolLineBuilder::new(width, "");
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
            pre_truncated: msg.truncated_lines,
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

pub fn build_batch_entry_lines(
    entry: &BatchToolEntry,
    index: usize,
    started_at: Instant,
    width: u16,
) -> ToolLines {
    let mut annotation = entry.annotation.clone();
    if let Some(suffix) = entry
        .output
        .as_ref()
        .and_then(|o| tool_output_annotation(o, &entry.tool))
    {
        append_annotation(&mut annotation, &suffix);
    }

    let mut b = ToolLineBuilder::new(width, BATCH_INDENT);
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
    use crate::components::DisplayRole;
    use crate::markdown::TRUNCATION_PREFIX;
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
            total_lines: 1,
            instructions: None,
        })
    }

    fn plain_output() -> Option<ToolOutput> {
        Some(ToolOutput::Plain("ok".into()))
    }

    fn bash_msg(
        text: &str,
        status: ToolStatus,
        input: Option<ToolInput>,
        output: Option<ToolOutput>,
    ) -> DisplayMessage {
        DisplayMessage {
            role: DisplayRole::Tool {
                id: "t1".into(),
                status,
                name: BASH_TOOL_NAME,
            },
            text: text.into(),
            tool_input: input,
            tool_output: output,
            annotation: None,
            plan_path: None,
            truncated_lines: 0,
            timestamp: None,
            turn_usage: None,
        }
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
        let msg = bash_msg("header\nbody", ToolStatus::Success, input, output);
        let tl = build_tool_lines(&msg, ToolStatus::Success, Instant::now(), 80);
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
        let msg = bash_msg("echo hi\nline1\nline2", status, code_input(), output);
        let tl = build_tool_lines(&msg, status, Instant::now(), 80);
        let text = lines_text(&tl);
        assert!(text.contains("line1"));
        assert!(text.contains("line2"));
    }

    fn line_has_styled(tl: &ToolLines, text: &str, style: Style) -> bool {
        tl.lines
            .iter()
            .any(|l| has_styled_span(&l.spans, text, style))
    }

    #[test_case(code_input(), true  ; "shown_with_code_input")]
    #[test_case(None,         false ; "hidden_without_code_input")]
    fn bash_separator(input: Option<ToolInput>, expected: bool) {
        let msg = bash_msg("echo hi\nhello", ToolStatus::Success, input, plain_output());
        let tl = build_tool_lines(&msg, ToolStatus::Success, Instant::now(), 80);
        assert_eq!(
            line_has_styled(&tl, BASH_OUTPUT_SEPARATOR, theme::current().tool_dim),
            expected,
        );
    }

    #[test_case(ToolStatus::InProgress, None,                                     BASH_WAITING_LABEL   ; "waiting_when_in_progress")]
    #[test_case(ToolStatus::Success,    Some(ToolOutput::Plain(String::new())),    BASH_NO_OUTPUT_LABEL ; "no_output_when_done_empty")]
    fn bash_status_label(status: ToolStatus, output: Option<ToolOutput>, label: &str) {
        let msg = bash_msg("echo hi", status, code_input(), output);
        let tl = build_tool_lines(&msg, status, Instant::now(), 80);
        assert!(line_has_styled(&tl, label, theme::current().tool_dim));
    }

    #[test]
    fn batch_bash_separator_between_code_and_output() {
        let entry = BatchToolEntry {
            tool: "bash".into(),
            summary: "echo hi".into(),
            status: BatchToolStatus::Success,
            input: code_input(),
            output: Some(ToolOutput::Plain("hello".into())),
            annotation: None,
        };
        let tl = build_batch_entry_lines(&entry, 0, Instant::now(), 80);
        assert!(line_has_styled(
            &tl,
            BASH_OUTPUT_SEPARATOR,
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
        bash_msg("cmd", ToolStatus::Success, None, None)
    }

    #[test_case(80, true  ; "shown_when_width_sufficient")]
    #[test_case(10, false ; "hidden_when_too_narrow")]
    fn append_right_info_timestamp_visibility(width: u16, expect_timestamp: bool) {
        let msg = tool_msg();
        let mut tl = build_tool_lines(&msg, ToolStatus::Success, Instant::now(), 80);
        let span_count_before = tl.lines[0].spans.len();
        append_right_info(&mut tl.lines[0], None, Some("12:34:56"), width);
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
                total_lines: 42,
                instructions: None,
            }),
            annotation: None,
        };
        let tl = build_batch_entry_lines(&entry, 0, Instant::now(), 80);
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
        let tl = build_batch_entry_lines(&entry, 0, Instant::now(), 80);
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
        let tl = build_batch_entry_lines(&entry, 0, Instant::now(), 80);
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
        let first = build_batch_entry_lines(&entry, 0, Instant::now(), 80);
        let second = build_batch_entry_lines(&entry, 1, Instant::now(), 80);
        assert!(second.lines.len() > first.lines.len());
        assert!(spans_text(&second.lines[1].spans).contains(TOOL_SEPARATOR));
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
        let without_sep = build_batch_entry_lines(&entry, 0, Instant::now(), 80);
        let with_sep = build_batch_entry_lines(&entry, 1, Instant::now(), 80);
        let offset = with_sep.lines.len() - without_sep.lines.len();
        let expected: Vec<usize> = without_sep
            .spinner_lines
            .iter()
            .map(|l| l + offset)
            .collect();
        assert_eq!(with_sep.spinner_lines, expected);
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
        let tl = build_batch_entry_lines(&entry, 0, Instant::now(), 80);
        let text = lines_text(&tl);
        assert!(text.contains("hello world"));
    }

    #[test]
    fn annotation_rendered_on_header() {
        let mut msg = tool_msg();
        msg.annotation = Some("2m timeout".into());
        let tl = build_tool_lines(&msg, ToolStatus::Success, Instant::now(), 80);
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
        let tl = build_batch_entry_lines(&entry, 0, Instant::now(), 80);
        let text = lines_text(&tl);
        assert!(text.contains("(anthropic/claude-haiku-4-20250414)"));
    }

    #[test_case("bash",  ToolOutput::Plain("ok".into()),                      None                ; "plain_short_no_annotation")]
    #[test_case("bash",  ToolOutput::Plain((0..20).map(|i| format!("line {i}")).collect::<Vec<_>>().join("\n")), Some("20 lines") ; "plain_long_annotates")]
    #[test_case("webfetch", ToolOutput::Plain("a\nb".into()),                 Some("2 lines")     ; "webfetch_always_annotates")]
    #[test_case("websearch", ToolOutput::Plain("r".into()),                   Some("1 lines")     ; "websearch_always_annotates")]
    #[test_case("read",  ToolOutput::ReadCode { path: "a.rs".into(), start_line: 1, lines: vec!["x".into(); 5], total_lines: 5, instructions: None }, Some("5 lines") ; "read_code_full_file")]
    #[test_case("read",  ToolOutput::ReadCode { path: "a.rs".into(), start_line: 10, lines: vec!["x".into(); 5], total_lines: 100, instructions: None }, Some("5 of 100 lines") ; "read_code_partial")]
    #[test_case("write", ToolOutput::WriteCode { path: "a.rs".into(), byte_count: 99, lines: vec![] }, Some("99 bytes") ; "write_code_bytes")]
    #[test_case("grep",  ToolOutput::GrepResult { entries: vec![GrepFileEntry { path: "a.rs".into(), matches: vec![] }] }, Some("1 files") ; "grep_file_count")]
    #[test_case("glob",  ToolOutput::GlobResult { files: vec!["a".into(), "b".into()] }, Some("2 files") ; "glob_file_count")]
    #[test_case("glob",  ToolOutput::GlobResult { files: vec![] },            None                ; "glob_empty_no_annotation")]
    #[test_case("edit",  ToolOutput::Diff { path: "a.rs".into(), hunks: vec![], summary: "ok".into() }, None ; "diff_no_annotation")]
    fn annotation_cases(tool: &str, output: ToolOutput, expected: Option<&str>) {
        assert_eq!(tool_output_annotation(&output, tool).as_deref(), expected);
    }

    #[test]
    fn task_output_body_visible() {
        let msg = task_msg("**bold** and `code`".into());
        let tl = build_tool_lines(&msg, ToolStatus::Success, Instant::now(), 80);
        let text = lines_text(&tl);
        assert!(text.contains("bold"));
        assert!(text.contains("code"));
    }

    fn task_msg(output: String) -> DisplayMessage {
        DisplayMessage {
            role: DisplayRole::Tool {
                id: "t1".into(),
                status: ToolStatus::Success,
                name: TASK_TOOL_NAME,
            },
            text: "Find auth".into(),
            tool_input: None,
            tool_output: Some(ToolOutput::Plain(output)),
            annotation: None,
            plan_path: None,
            timestamp: None,
            turn_usage: None,
            truncated_lines: 0,
        }
    }

    fn n_lines(n: usize) -> String {
        (0..n)
            .map(|i| format!("line {i}"))
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn assert_truncation_styled(tl: &ToolLines) {
        let last = tl.lines.last().unwrap();
        let span = last
            .spans
            .iter()
            .find(|s| s.content.contains(TRUNCATION_PREFIX));
        assert!(span.is_some(), "expected truncation prefix");
        assert_eq!(span.unwrap().style, theme::current().tool_dim);
    }

    fn task_truncation_tl(output: String) -> ToolLines {
        let msg = task_msg(output);
        build_tool_lines(&msg, ToolStatus::Success, Instant::now(), 80)
    }

    #[test_case(n_lines(200)                                             ; "plain_lines")]
    #[test_case(format!("```rust\nfn main() {{}}\n```\n{}", n_lines(40)) ; "after_code_block")]
    #[test_case(format!("```rust\n{}", n_lines(40))                      ; "inside_code_block")]
    fn task_truncation_styled_dim(output: String) {
        assert_truncation_styled(&task_truncation_tl(output));
    }

    #[test]
    fn task_output_truncated_at_max_lines() {
        let tl = task_truncation_tl(n_lines(200));
        let body_lines = tl.lines.len() - 1;
        assert!(
            body_lines <= TASK_OUTPUT_MAX_LINES + 1,
            "expected at most {} body lines, got {body_lines}",
            TASK_OUTPUT_MAX_LINES + 1,
        );
    }

    #[test]
    fn task_hr_fits_within_indented_width() {
        let width: u16 = 60;
        let msg = task_msg("before\n\n---\n\nafter".into());
        let tl = build_tool_lines(&msg, ToolStatus::Success, Instant::now(), width);
        let hr_line = tl
            .lines
            .iter()
            .find(|l| l.spans.iter().any(|s| s.content.contains('─')));
        assert!(hr_line.is_some());
        let total_width: usize = hr_line
            .unwrap()
            .spans
            .iter()
            .map(|s| s.content.chars().count())
            .sum();
        assert!(
            total_width <= width as usize,
            "HR ({total_width} chars) should fit in {width} cols"
        );
    }

    #[test]
    fn batch_task_hr_fits_within_width() {
        let width: u16 = 60;
        let entry = BatchToolEntry {
            tool: TASK_TOOL_NAME.to_string(),
            summary: "research".into(),
            status: BatchToolStatus::Success,
            input: None,
            output: Some(ToolOutput::Plain("before\n\n---\n\nafter".into())),
            annotation: None,
        };
        let tl = build_batch_entry_lines(&entry, 0, Instant::now(), width);
        for line in &tl.lines {
            let total: usize = line.spans.iter().map(|s| s.content.chars().count()).sum();
            assert!(
                total <= width as usize,
                "line ({total} chars) should fit in {width} cols: {line:?}"
            );
        }
    }

    fn index_msg(body: &str) -> DisplayMessage {
        DisplayMessage {
            role: DisplayRole::Tool {
                id: "t1".into(),
                status: ToolStatus::Success,
                name: INDEX_TOOL_NAME,
            },
            text: format!("src/lib.rs\n{body}"),
            tool_input: None,
            tool_output: Some(ToolOutput::Plain(body.to_owned())),
            annotation: None,
            plan_path: None,
            timestamp: None,
            turn_usage: None,
            truncated_lines: 0,
        }
    }

    #[test]
    fn index_output_truncated_at_max_lines() {
        let body: String = (0..150).map(|i| format!("  line_{i}\n")).collect();
        let msg = index_msg(&body);
        let tl = build_tool_lines(&msg, ToolStatus::Success, Instant::now(), 80);
        let text = lines_text(&tl);
        assert!(text.contains("line_0"));
        assert!(!text.contains("line_149"));
        assert!(text.contains(TRUNCATION_PREFIX));
    }

    #[test]
    fn index_output_styles_all_elements() {
        let body = "imports: [1-5]\n  std::io\n\nfns:\n  pub fn main() [10-20]";
        let msg = index_msg(body);
        let tl = build_tool_lines(&msg, ToolStatus::Success, Instant::now(), 80);
        let t = theme::current();
        assert!(line_has_styled(&tl, "imports:", t.index_section));
        assert!(line_has_styled(&tl, "fns:", t.index_section));
        assert!(line_has_styled(&tl, "[10-20]", t.index_line_nr));
        assert!(line_has_styled(&tl, "pub", t.index_keyword));
    }

    #[test]
    fn index_header_uses_path_style() {
        let spans = style_tool_header(INDEX_TOOL_NAME, "src/lib.rs");
        assert!(has_styled_span(
            &spans,
            "src/lib.rs",
            theme::current().tool_path
        ));
    }
}

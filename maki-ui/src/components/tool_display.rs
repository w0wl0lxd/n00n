use super::status_bar::format_tokens;
use super::{DisplayMessage, ToolStatus};

use super::{code_view, index_highlight};
use crate::animation::spinner_frame;
use crate::theme;

use std::borrow::Cow;
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
use maki_agent::{BatchToolEntry, BatchToolStatus, InstructionBlock, ToolInput, ToolOutput};
use ratatui::style::Style;
use ratatui::text::{Line, Span};

use crate::highlight::highlight_regex_inline;
use crate::render_worker::RenderWorker;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ToolKind {
    Bash,
    CodeExecution,
    Task,
    Index,
    Grep,
    Read,
    Write,
    WebFetch,
    WebSearch,
    Other,
}

impl ToolKind {
    pub fn from_name(name: &str) -> Self {
        match name {
            BASH_TOOL_NAME => Self::Bash,
            CODE_EXECUTION_TOOL_NAME => Self::CodeExecution,
            TASK_TOOL_NAME => Self::Task,
            INDEX_TOOL_NAME => Self::Index,
            GREP_TOOL_NAME => Self::Grep,
            READ_TOOL_NAME => Self::Read,
            WRITE_TOOL_NAME | EDIT_TOOL_NAME | MULTIEDIT_TOOL_NAME => Self::Write,
            WEBFETCH_TOOL_NAME => Self::WebFetch,
            WEBSEARCH_TOOL_NAME => Self::WebSearch,
            _ => Self::Other,
        }
    }

    #[cfg(test)]
    pub const fn output_limits(self) -> OutputLimits {
        let max_lines = match self {
            Self::Bash => 10,
            Self::CodeExecution | Self::Task => 15,
            Self::Index => 7,
            Self::Grep | Self::Read => 5,
            Self::Write => 30,
            Self::WebFetch | Self::WebSearch | Self::Other => 3,
        };
        self.output_limits_with(max_lines)
    }

    pub const fn output_limits_with(self, max_lines: usize) -> OutputLimits {
        let keep = match self {
            Self::Bash | Self::CodeExecution => Keep::Tail,
            _ => Keep::Head,
        };
        OutputLimits { max_lines, keep }
    }

    fn always_annotate(self) -> bool {
        matches!(self, Self::WebFetch | Self::WebSearch | Self::Index)
    }
}

pub(crate) struct OutputLimits {
    pub max_lines: usize,
    pub keep: Keep,
}

pub const TOOL_INDICATOR: &str = "● ";
pub const TOOL_BODY_INDENT: &str = "  ";

const TOOL_SEPARATOR: &str = "──────────────────";
const CODE_EXECUTION_OUTPUT_SEPARATOR: &str = "────────────";
const INSTRUCTION_SEPARATOR: &str = "────────────";
const BASH_WAITING_LABEL: &str = "Waiting for output...";
const BASH_NO_OUTPUT_LABEL: &str = "No output.";
const BASH_OUTPUT_SEPARATOR: &str = "──────";

const PLAIN_ANNOTATION_THRESHOLD: usize = 10;
const BATCH_INDENT: &str = "  ";
const BATCH_CONTENT_INDENT: &str = "    ";

pub(crate) fn tool_output_annotation(output: &ToolOutput, kind: ToolKind) -> Option<String> {
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
            if kind.always_annotate() || n > PLAIN_ANNOTATION_THRESHOLD {
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
    pub has_truncation: bool,
}

pub struct HighlightRequest {
    pub range: (usize, usize),
    pub input: Option<ToolInput>,
    pub output: Option<ToolOutput>,
    pub width: u16,
    pub max_lines: usize,
    pub expanded: bool,
}

impl HighlightRequest {
    fn new(
        range: (usize, usize),
        input: Option<ToolInput>,
        output: Option<ToolOutput>,
        width: u16,
        max_lines: usize,
        expanded: bool,
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
            max_lines,
            expanded,
        })
    }
}

impl ToolLines {
    pub fn send_highlight(&self, worker: &RenderWorker) -> Option<u64> {
        let hl = self.highlight.as_ref()?;
        Some(worker.send(
            hl.input.clone(),
            hl.output.clone(),
            hl.width,
            hl.max_lines,
            hl.expanded,
        ))
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

struct ResolvedOutput<'a> {
    text: Option<Cow<'a, str>>,
    skipped: usize,
    instructions: Option<&'a [InstructionBlock]>,
}

fn resolve_output<'a>(
    output: Option<&'a ToolOutput>,
    body: Option<&'a str>,
    pre_truncated: usize,
    max_lines: usize,
    keep: Keep,
    expanded: bool,
) -> ResolvedOutput<'a> {
    let instructions = match output {
        Some(ToolOutput::ReadDir { instructions, .. }) => instructions.as_deref(),
        _ => None,
    };

    if let Some(ToolOutput::Batch { .. } | ToolOutput::QuestionAnswers(_)) = output {
        return ResolvedOutput {
            text: None,
            skipped: 0,
            instructions,
        };
    }

    let use_body = body.filter(|_| !expanded);
    let (raw_text, already_truncated) = match (use_body, output) {
        (Some(b), _) => (Some(Cow::Borrowed(b)), pre_truncated),
        (None, Some(ToolOutput::Plain(t))) => (Some(Cow::Borrowed(t.as_str())), 0),
        (None, Some(ToolOutput::ReadDir { text, .. })) => (Some(Cow::Borrowed(text.as_str())), 0),
        (None, Some(output @ ToolOutput::GlobResult { .. })) => {
            (Some(Cow::Owned(output.as_display_text())), 0)
        }
        (None, None) => (None, 0),
        _ => {
            return ResolvedOutput {
                text: None,
                skipped: 0,
                instructions,
            };
        }
    };

    let (text, skipped) = match raw_text {
        Some(t) if !t.is_empty() => {
            let tr = truncate_output(&t, max_lines, keep);
            let s = if tr.skipped > 0 {
                tr.skipped
            } else {
                already_truncated
            };
            (Some(Cow::Owned(tr.kept.into_owned())), s)
        }
        _ => (None, already_truncated),
    };

    ResolvedOutput {
        text,
        skipped,
        instructions,
    }
}

struct ToolLineBuilder {
    lines: Vec<Line<'static>>,
    spinner_lines: Vec<usize>,
    content_range: (usize, usize),
    width: u16,
    outer_indent: &'static str,
    has_truncation: bool,
    expanded: bool,
    max_lines: usize,
    keep: Keep,
}

impl ToolLineBuilder {
    fn new(width: u16, outer_indent: &'static str, expanded: bool, limits: OutputLimits) -> Self {
        let max_lines = if expanded {
            usize::MAX
        } else {
            limits.max_lines
        };
        Self {
            lines: Vec::new(),
            spinner_lines: Vec::new(),
            content_range: (0, 0),
            width: width.saturating_sub(outer_indent.len() as u16),
            outer_indent,
            has_truncation: false,
            expanded,
            max_lines,
            keep: limits.keep,
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
        let content = code_view::render_tool_content(
            input,
            output,
            false,
            content_width,
            self.max_lines,
            self.expanded,
        );
        self.has_truncation |= content.has_truncation;
        let start = self.lines.len();
        for mut line in content.lines {
            line.spans.insert(0, Span::raw(TOOL_BODY_INDENT.to_owned()));
            self.lines.push(line);
        }
        self.content_range = (start, self.lines.len());
    }

    fn push_resolved_output(
        &mut self,
        resolved: &ResolvedOutput<'_>,
        kind: ToolKind,
        is_done: bool,
    ) {
        let has_code = self.content_range.1 > self.content_range.0;

        if resolved.text.is_none() && resolved.instructions.is_none() {
            if has_code && kind == ToolKind::Bash {
                self.push_bash_output_label(TOOL_BODY_INDENT, is_done, false);
            }
            return;
        }

        if has_code {
            if kind == ToolKind::Bash {
                self.push_bash_output_label(TOOL_BODY_INDENT, is_done, resolved.text.is_some());
            } else if resolved.text.is_some() {
                self.push_code_output_separator(kind, TOOL_BODY_INDENT);
            }
        }

        if let Some(text) = &resolved.text {
            if matches!(self.keep, Keep::Tail) {
                self.push_truncation_count(resolved.skipped);
            }
            match kind {
                ToolKind::Task => self.push_markdown_body(text),
                ToolKind::Index => self.push_index_body(text),
                _ => push_text_lines(&mut self.lines, text, TOOL_BODY_INDENT),
            }
            if matches!(self.keep, Keep::Head) {
                self.push_truncation_count(resolved.skipped);
            }
        }

        self.maybe_push_instructions(resolved.instructions);
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
            self.has_truncation = true;
            let text = truncation_notice(skipped);
            let mut line = Line::from(Span::styled(text, theme::current().tool_dim));
            line.spans.insert(0, Span::raw(TOOL_BODY_INDENT.to_owned()));
            self.lines.push(line);
        } else if self.expanded {
            self.has_truncation = true;
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

    fn push_code_output_separator(&mut self, kind: ToolKind, indent: &str) {
        if kind == ToolKind::CodeExecution {
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
        let highlight = HighlightRequest::new(
            self.content_range,
            input,
            output,
            content_width,
            self.max_lines,
            self.expanded,
        );
        ToolLines {
            lines: self.lines,
            highlight,
            spinner_lines: self.spinner_lines,
            content_indent,
            has_truncation: self.has_truncation,
        }
    }
}

fn push_text_lines(lines: &mut Vec<Line<'static>>, text: &str, indent: &str) {
    let style = theme::current().tool;
    for line in text.lines() {
        lines.push(Line::from(Span::styled(format!("{indent}{line}"), style)));
    }
}

pub fn build_tool_lines(
    msg: &DisplayMessage,
    status: ToolStatus,
    started_at: Instant,
    width: u16,
    expanded: bool,
    tool_output_lines: usize,
) -> ToolLines {
    let tool_name = msg.role.tool_name().unwrap_or("?");
    let kind = ToolKind::from_name(tool_name);
    let limits = kind.output_limits_with(tool_output_lines);
    let (header, body) = match msg.text.split_once('\n') {
        Some((h, b)) => (h, Some(b)),
        None => (msg.text.as_str(), None),
    };

    let mut b = ToolLineBuilder::new(width, "", expanded, limits);
    b.push_header(tool_name, header, msg.annotation.as_deref());
    b.prepend_indicator(status.into(), started_at);
    b.push_code_content(msg.tool_input.as_ref(), msg.tool_output.as_ref());
    let is_done = status != ToolStatus::InProgress;
    let resolved = resolve_output(
        msg.tool_output.as_ref(),
        body,
        msg.truncated_lines,
        b.max_lines,
        b.keep,
        expanded,
    );
    b.push_resolved_output(&resolved, kind, is_done);
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
    expanded: bool,
    tool_output_lines: usize,
) -> ToolLines {
    let kind = ToolKind::from_name(&entry.tool);
    let limits = kind.output_limits_with(tool_output_lines);
    let mut annotation = entry.annotation.clone();
    if let Some(suffix) = entry
        .output
        .as_ref()
        .and_then(|o| tool_output_annotation(o, kind))
    {
        append_annotation(&mut annotation, &suffix);
    }

    let mut b = ToolLineBuilder::new(width, BATCH_INDENT, expanded, limits);
    b.push_header(&entry.tool, &entry.summary, annotation.as_deref());
    b.prepend_indicator(entry.status.into(), started_at);
    b.push_code_content(entry.input.as_ref(), entry.output.as_ref());
    let is_done = matches!(
        entry.status,
        BatchToolStatus::Success | BatchToolStatus::Error
    );
    let resolved = resolve_output(
        entry.output.as_ref(),
        None,
        0,
        b.max_lines,
        b.keep,
        expanded,
    );
    b.push_resolved_output(&resolved, kind, is_done);
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
    use maki_agent::tools::{BASH_TOOL_NAME, READ_TOOL_NAME, WRITE_TOOL_NAME};
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
        let tl = build_tool_lines(&msg, ToolStatus::Success, Instant::now(), 80, false, 10);
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
        let tl = build_tool_lines(&msg, status, Instant::now(), 80, false, 10);
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
        let tl = build_tool_lines(&msg, ToolStatus::Success, Instant::now(), 80, false, 10);
        assert_eq!(
            line_has_styled(&tl, BASH_OUTPUT_SEPARATOR, theme::current().tool_dim),
            expected,
        );
    }

    #[test_case(ToolStatus::InProgress, None,                                     BASH_WAITING_LABEL   ; "waiting_when_in_progress")]
    #[test_case(ToolStatus::Success,    Some(ToolOutput::Plain(String::new())),    BASH_NO_OUTPUT_LABEL ; "no_output_when_done_empty")]
    fn bash_status_label(status: ToolStatus, output: Option<ToolOutput>, label: &str) {
        let msg = bash_msg("echo hi", status, code_input(), output);
        let tl = build_tool_lines(&msg, status, Instant::now(), 80, false, 10);
        assert!(line_has_styled(&tl, label, theme::current().tool_dim));
    }

    #[test]
    fn batch_bash_separator_between_code_and_output() {
        let entry = batch_entry(
            "bash",
            BatchToolStatus::Success,
            code_input(),
            Some(ToolOutput::Plain("hello".into())),
        );
        let tl = build_batch_entry_lines(&entry, 0, Instant::now(), 80, false, 10);
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

    fn batch_entry(
        tool: &str,
        status: BatchToolStatus,
        input: Option<ToolInput>,
        output: Option<ToolOutput>,
    ) -> BatchToolEntry {
        BatchToolEntry {
            tool: tool.into(),
            summary: "test".into(),
            status,
            input,
            output,
            annotation: None,
        }
    }

    #[test_case(80, true  ; "shown_when_width_sufficient")]
    #[test_case(10, false ; "hidden_when_too_narrow")]
    fn append_right_info_timestamp_visibility(width: u16, expect_timestamp: bool) {
        let msg = tool_msg();
        let mut tl = build_tool_lines(&msg, ToolStatus::Success, Instant::now(), 80, false, 10);
        let span_count_before = tl.lines[0].spans.len();
        append_right_info(&mut tl.lines[0], None, Some("12:34:56"), width);
        if expect_timestamp {
            let last = tl.lines[0].spans.last().unwrap();
            assert_eq!(last.style, theme::current().timestamp);
            assert!(tl.lines[0].spans.len() > span_count_before);
        } else {
            assert_eq!(tl.lines[0].spans.len(), span_count_before);
        }
    }

    #[test]
    fn batch_entry_annotation_rendered() {
        let mut entry = batch_entry(
            "read",
            BatchToolStatus::Success,
            None,
            Some(ToolOutput::ReadCode {
                path: "src/main.rs".into(),
                start_line: 1,
                lines: vec!["x".into(); 42],
                total_lines: 42,
                instructions: None,
            }),
        );
        entry.summary = "src/main.rs".into();
        let tl = build_batch_entry_lines(&entry, 0, Instant::now(), 80, false, 10);
        assert!(lines_text(&tl).contains("(42 lines)"));
    }

    #[test]
    fn batch_entry_code_input_rendered() {
        let entry = batch_entry("bash", BatchToolStatus::Success, code_input(), None);
        let tl = build_batch_entry_lines(&entry, 0, Instant::now(), 80, false, 10);
        assert!(lines_text(&tl).contains("echo hi"));
    }

    #[test_case(BatchToolStatus::InProgress, &[0]    ; "in_progress_has_spinner")]
    #[test_case(BatchToolStatus::Pending,    &[]     ; "pending_no_spinner")]
    #[test_case(BatchToolStatus::Success,    &[]     ; "success_no_spinner")]
    fn batch_entry_spinner(status: BatchToolStatus, expected: &[usize]) {
        let entry = batch_entry("bash", status, None, None);
        let tl = build_batch_entry_lines(&entry, 0, Instant::now(), 80, false, 10);
        assert_eq!(tl.spinner_lines, expected);
    }

    #[test]
    fn batch_entry_separator_on_nonzero_index() {
        let entry = batch_entry("bash", BatchToolStatus::Success, None, None);
        let first = build_batch_entry_lines(&entry, 0, Instant::now(), 80, false, 10);
        let second = build_batch_entry_lines(&entry, 1, Instant::now(), 80, false, 10);
        assert!(second.lines.len() > first.lines.len());
        assert!(spans_text(&second.lines[1].spans).contains(TOOL_SEPARATOR));
    }

    #[test]
    fn batch_entry_spinner_offset_with_separator() {
        let entry = batch_entry("bash", BatchToolStatus::InProgress, None, None);
        let without_sep = build_batch_entry_lines(&entry, 0, Instant::now(), 80, false, 10);
        let with_sep = build_batch_entry_lines(&entry, 1, Instant::now(), 80, false, 10);
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
        let entry = batch_entry(
            "bash",
            BatchToolStatus::Success,
            None,
            Some(ToolOutput::Plain("hello world".into())),
        );
        let tl = build_batch_entry_lines(&entry, 0, Instant::now(), 80, false, 10);
        assert!(lines_text(&tl).contains("hello world"));
    }

    #[test]
    fn annotation_rendered_on_header() {
        let mut msg = tool_msg();
        msg.annotation = Some("2m timeout".into());
        let tl = build_tool_lines(&msg, ToolStatus::Success, Instant::now(), 80, false, 10);
        let text = lines_text(&tl);
        assert!(text.contains("(2m timeout)"));
    }

    #[test]
    fn batch_entry_stored_annotation_rendered() {
        let mut entry = batch_entry("task", BatchToolStatus::Success, None, None);
        entry.annotation = Some("anthropic/claude-haiku-4-20250414".into());
        let tl = build_batch_entry_lines(&entry, 0, Instant::now(), 80, false, 10);
        assert!(lines_text(&tl).contains("(anthropic/claude-haiku-4-20250414)"));
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
        assert_eq!(
            tool_output_annotation(&output, ToolKind::from_name(tool)).as_deref(),
            expected
        );
    }

    #[test]
    fn task_output_body_visible() {
        let msg = task_msg("**bold** and `code`".into());
        let tl = build_tool_lines(&msg, ToolStatus::Success, Instant::now(), 80, false, 10);
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
        build_tool_lines(&msg, ToolStatus::Success, Instant::now(), 80, false, 10)
    }

    #[test_case(n_lines(200)                                             ; "plain_lines")]
    #[test_case(format!("```rust\nfn main() {{}}\n```\n{}", n_lines(40)) ; "after_code_block")]
    #[test_case(format!("```rust\n{}", n_lines(40))                      ; "inside_code_block")]
    fn task_truncation_styled_dim(output: String) {
        assert_truncation_styled(&task_truncation_tl(output));
    }

    #[test]
    fn task_output_truncated_at_max_lines() {
        const TASK_MAX: usize = ToolKind::Task.output_limits().max_lines;
        let tl = task_truncation_tl(n_lines(200));
        let body_lines = tl.lines.len() - 1;
        assert!(
            body_lines <= TASK_MAX + 1,
            "expected at most {} body lines, got {body_lines}",
            TASK_MAX + 1,
        );
    }

    #[test]
    fn task_hr_fits_within_indented_width() {
        let width: u16 = 60;
        let msg = task_msg("before\n\n---\n\nafter".into());
        let tl = build_tool_lines(&msg, ToolStatus::Success, Instant::now(), width, false, 10);
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
        let entry = batch_entry(
            TASK_TOOL_NAME,
            BatchToolStatus::Success,
            None,
            Some(ToolOutput::Plain("before\n\n---\n\nafter".into())),
        );
        let tl = build_batch_entry_lines(&entry, 0, Instant::now(), width, false, 10);
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
        let tl = build_tool_lines(&msg, ToolStatus::Success, Instant::now(), 80, false, 10);
        let text = lines_text(&tl);
        assert!(text.contains("line_0"));
        assert!(!text.contains("line_149"));
        assert!(text.contains(TRUNCATION_PREFIX));
    }

    #[test]
    fn index_output_styles_all_elements() {
        let body = "imports: [1-5]\n  std::io\n\nfns:\n  pub fn main() [10-20]";
        let msg = index_msg(body);
        let tl = build_tool_lines(&msg, ToolStatus::Success, Instant::now(), 80, false, 10);
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

    #[test_case(None,       None,    "bash",  false, false ; "none_output_none_body")]
    #[test_case(None,       Some("hello"), "bash", true, false ; "none_output_with_body")]
    #[test_case(
        Some(ToolOutput::Plain("world".into())), None, "bash", true, false
        ; "plain_no_body_uses_plain"
    )]
    #[test_case(
        Some(ToolOutput::Plain("world".into())), Some("override"), "bash", true, false
        ; "body_takes_priority_over_plain"
    )]
    #[test_case(
        Some(ToolOutput::Plain(String::new())), None, "bash", false, false
        ; "empty_plain_resolves_to_none"
    )]
    #[test_case(
        Some(ToolOutput::Batch { entries: vec![], text: String::new() }),
        None, "batch", false, false
        ; "batch_always_none"
    )]
    #[test_case(
        Some(ToolOutput::ReadDir { text: "dir listing".into(), instructions: None }),
        None, "read", true, false
        ; "readdir_uses_text_field"
    )]
    #[test_case(
        Some(ToolOutput::GlobResult { files: vec!["a.rs".into()] }),
        None, "glob", true, false
        ; "glob_uses_display_text"
    )]
    #[test_case(
        Some(ToolOutput::ReadCode { path: "a.rs".into(), start_line: 1, lines: vec![], total_lines: 0, instructions: None }),
        None, "read", false, false
        ; "structured_output_resolves_to_none"
    )]
    fn resolve_output_text_presence(
        output: Option<ToolOutput>,
        body: Option<&str>,
        tool: &str,
        expect_text: bool,
        expect_instructions: bool,
    ) {
        let kind = ToolKind::from_name(tool);
        let limits = kind.output_limits();
        let resolved = resolve_output(
            output.as_ref(),
            body,
            0,
            limits.max_lines,
            limits.keep,
            false,
        );
        assert_eq!(resolved.text.is_some(), expect_text);
        assert_eq!(resolved.instructions.is_some(), expect_instructions);
    }

    #[test]
    fn resolve_output_readdir_preserves_instructions() {
        let blocks = vec![InstructionBlock {
            path: "test".into(),
            content: "instructions here".into(),
        }];
        let output = ToolOutput::ReadDir {
            text: "listing".into(),
            instructions: Some(blocks),
        };
        let limits = ToolKind::Read.output_limits();
        let resolved = resolve_output(Some(&output), None, 0, limits.max_lines, limits.keep, false);
        assert!(resolved.instructions.is_some());
    }

    #[test]
    fn resolve_output_body_priority_over_plain() {
        let output = ToolOutput::Plain("from_output".into());
        let limits = ToolKind::Bash.output_limits();
        let resolved = resolve_output(
            Some(&output),
            Some("from_body"),
            0,
            limits.max_lines,
            limits.keep,
            false,
        );
        assert_eq!(resolved.text.as_deref(), Some("from_body"));
    }

    #[test]
    fn resolve_output_pre_truncated_forwarded() {
        let limits = ToolKind::Bash.output_limits();
        let resolved = resolve_output(
            None,
            Some("short"),
            42,
            limits.max_lines,
            limits.keep,
            false,
        );
        assert_eq!(resolved.skipped, 42);
    }

    #[test]
    fn resolve_output_truncation_overrides_pre_truncated() {
        let long = n_lines(200);
        let limits = ToolKind::Bash.output_limits();
        let resolved = resolve_output(None, Some(&long), 5, limits.max_lines, limits.keep, false);
        assert!(resolved.skipped > 5);
    }

    fn long_output_msg(line_count: usize) -> DisplayMessage {
        let full_body = (0..line_count)
            .map(|i| format!("line {i}"))
            .collect::<Vec<_>>()
            .join("\n");
        let limits = ToolKind::Bash.output_limits();
        let tr = truncate_output(&full_body, limits.max_lines, limits.keep);
        let text = if tr.kept.is_empty() {
            "header".into()
        } else {
            format!("header\n{}", tr.kept)
        };
        let truncated_lines = tr.skipped;
        DisplayMessage {
            role: DisplayRole::Tool {
                id: "t1".into(),
                status: ToolStatus::Success,
                name: BASH_TOOL_NAME,
            },
            text,
            tool_input: None,
            tool_output: Some(ToolOutput::Plain(full_body)),
            annotation: None,
            plan_path: None,
            truncated_lines,
            timestamp: None,
            turn_usage: None,
        }
    }

    #[test_case(200, true,  true,  false ; "expanded_shows_all")]
    #[test_case(200, false, true,  true  ; "collapsed_truncates")]
    #[test_case(3,   false, false, false ; "short_no_truncation")]
    fn bash_output_truncation(
        line_count: usize,
        expanded: bool,
        expect_truncation: bool,
        expect_expand_notice: bool,
    ) {
        let msg = long_output_msg(line_count);
        let tl = build_tool_lines(&msg, ToolStatus::Success, Instant::now(), 80, expanded, 10);
        let text = lines_text(&tl);
        assert_eq!(tl.has_truncation, expect_truncation);
        assert_eq!(text.contains("click to expand"), expect_expand_notice);
    }

    fn read_output_msg(line_count: usize) -> DisplayMessage {
        let lines: Vec<String> = (0..line_count).map(|i| format!("line {i}")).collect();
        DisplayMessage {
            role: DisplayRole::Tool {
                id: "t1".into(),
                status: ToolStatus::Success,
                name: READ_TOOL_NAME,
            },
            text: "read /src/main.rs".into(),
            tool_input: None,
            tool_output: Some(ToolOutput::ReadCode {
                path: "main.rs".into(),
                start_line: 1,
                lines,
                total_lines: line_count,
                instructions: None,
            }),
            annotation: None,
            plan_path: None,
            truncated_lines: 0,
            timestamp: None,
            turn_usage: None,
        }
    }

    #[test_case(20, false, true,  true  ; "read_collapsed_truncates")]
    #[test_case(20, true,  true,  false ; "read_expanded_shows_all")]
    #[test_case(3,  false, false, false ; "read_short_no_truncation")]
    fn read_output_truncation(
        line_count: usize,
        expanded: bool,
        expect_truncation: bool,
        expect_expand_notice: bool,
    ) {
        let msg = read_output_msg(line_count);
        let tl = build_tool_lines(&msg, ToolStatus::Success, Instant::now(), 80, expanded, 10);
        assert_eq!(tl.has_truncation, expect_truncation);
        let text = lines_text(&tl);
        assert_eq!(text.contains("click to expand"), expect_expand_notice);
    }
}

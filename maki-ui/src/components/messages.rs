use super::{DisplayMessage, DisplayRole, ToolStatus};

use super::code_view;
use crate::animation::{Typewriter, spinner_frame};
use crate::highlight::{self, CodeHighlighter};
use crate::markdown::{plain_lines, tail_lines, text_to_lines, truncate_lines};
use crate::theme;

use std::time::Instant;

use maki_agent::tools::{
    BASH_TOOL_NAME, EDIT_TOOL_NAME, GLOB_TOOL_NAME, GREP_TOOL_NAME, MULTIEDIT_TOOL_NAME,
    READ_TOOL_NAME, WEBFETCH_TOOL_NAME, WRITE_TOOL_NAME,
};
use maki_providers::{ToolDoneEvent, ToolInput, ToolOutput, ToolStartEvent};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState, Wrap};

const TOOL_INDICATOR: &str = "● ";
const TOOL_OUTPUT_MAX_LINES: usize = 7;
const BASH_OUTPUT_MAX_LINES: usize = 10;
const TOOL_BODY_INDENT: &str = "  ";
const SCROLLBAR_THUMB: &str = "\u{2590}";

fn tool_summary_annotation(tool: &str, text: &str) -> Option<String> {
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

fn path_with_annotation(text: &str, prefix: Option<&str>) -> Vec<Span<'static>> {
    let (path, annotation) = split_trailing_annotation(text);
    let mut spans = match prefix {
        Some(p) => vec![
            Span::styled(format!("{p} in "), theme::TOOL),
            Span::styled(path.to_owned(), theme::TOOL_PATH),
        ],
        None => vec![Span::styled(path.to_owned(), theme::TOOL_PATH)],
    };
    if let Some(ann) = annotation {
        spans.push(Span::styled(ann.to_owned(), theme::TOOL_ANNOTATION));
    }
    spans
}

fn style_tool_header(tool: &str, header: &str) -> Vec<Span<'static>> {
    if PATH_FIRST_TOOLS.contains(&tool) {
        return path_with_annotation(header, None);
    }
    if IN_PATH_TOOLS.contains(&tool)
        && let Some(i) = header.rfind(" in ")
    {
        let (before, after) = header.split_at(i);
        return path_with_annotation(&after[4..], Some(before));
    }
    vec![Span::styled(header.to_owned(), theme::TOOL)]
}

struct RoleStyle {
    prefix: &'static str,
    text_style: Style,
    prefix_style: Style,
    use_markdown: bool,
}

const ASSISTANT_STYLE: RoleStyle = RoleStyle {
    prefix: "maki> ",
    text_style: theme::ASSISTANT,
    prefix_style: theme::ASSISTANT_PREFIX,
    use_markdown: true,
};

const USER_STYLE: RoleStyle = RoleStyle {
    prefix: "you> ",
    text_style: theme::ASSISTANT,
    prefix_style: theme::USER,
    use_markdown: true,
};

const THINKING_STYLE: RoleStyle = RoleStyle {
    prefix: "thinking> ",
    text_style: theme::THINKING,
    prefix_style: theme::THINKING,
    use_markdown: true,
};

const ERROR_STYLE: RoleStyle = RoleStyle {
    prefix: "",
    text_style: theme::ERROR,
    prefix_style: theme::ERROR,
    use_markdown: false,
};

#[derive(Default)]
struct StreamingCache {
    byte_len: usize,
    lines: Vec<Line<'static>>,
    highlighters: Vec<CodeHighlighter>,
    dim: bool,
}

impl StreamingCache {
    fn get_or_update(
        &mut self,
        visible: &str,
        prefix: &str,
        text_style: Style,
        prefix_style: Style,
    ) -> &[Line<'static>] {
        let len = visible.len();
        if len != self.byte_len || self.lines.is_empty() {
            self.lines = text_to_lines(
                visible,
                prefix,
                text_style,
                prefix_style,
                Some(&mut self.highlighters),
            );
            if self.dim {
                theme::dim_lines(&mut self.lines);
            }
            self.byte_len = len;
        }
        &self.lines
    }
}

struct Segment {
    lines: Vec<Line<'static>>,
    tool_id: Option<String>,
    cached_height: Option<(u16, u16)>,
}

pub struct MessagesPanel {
    messages: Vec<DisplayMessage>,
    streaming_thinking: Typewriter,
    streaming_text: Typewriter,
    started_at: Instant,
    in_progress_count: usize,
    scroll_top: u16,
    auto_scroll: bool,
    viewport_height: u16,
    cached_segments: Vec<Segment>,
    cached_msg_count: usize,
    cached_streaming_thinking: StreamingCache,
    cached_streaming_text: StreamingCache,
}

impl MessagesPanel {
    pub fn new() -> Self {
        Self {
            messages: Vec::new(),
            streaming_thinking: Typewriter::new(),
            streaming_text: Typewriter::new(),
            started_at: Instant::now(),
            in_progress_count: 0,
            scroll_top: u16::MAX,
            auto_scroll: true,
            viewport_height: 24,
            cached_segments: Vec::new(),
            cached_msg_count: 0,
            cached_streaming_thinking: StreamingCache {
                dim: true,
                ..StreamingCache::default()
            },
            cached_streaming_text: StreamingCache::default(),
        }
    }

    pub fn push(&mut self, msg: DisplayMessage) {
        self.messages.push(msg);
    }

    pub fn load_messages(&mut self, msgs: Vec<DisplayMessage>) {
        self.in_progress_count = msgs
            .iter()
            .filter(|m| {
                matches!(
                    m.role,
                    DisplayRole::Tool {
                        status: ToolStatus::InProgress,
                        ..
                    }
                )
            })
            .count();
        self.messages = msgs;
    }

    pub fn thinking_delta(&mut self, text: &str) {
        self.streaming_thinking.push(text);
    }

    pub fn text_delta(&mut self, text: &str) {
        self.flush_thinking();
        self.streaming_text.push(text);
    }

    pub fn tool_start(&mut self, event: ToolStartEvent) {
        self.flush();
        self.messages.push(DisplayMessage {
            role: DisplayRole::Tool {
                id: event.id,
                status: ToolStatus::InProgress,
                name: event.tool,
            },
            text: event.summary.clone(),
            tool_input: event.input,
            tool_output: None,
        });
        self.in_progress_count += 1;
    }

    pub fn tool_output(&mut self, tool_id: &str, content: &str) {
        let Some(msg) = self
            .messages
            .iter_mut()
            .rfind(|m| matches!(m.role, DisplayRole::Tool { ref id, .. } if *id == tool_id))
        else {
            return;
        };
        truncate_to_header(&mut msg.text);
        let truncated = tail_lines(content, BASH_OUTPUT_MAX_LINES);
        msg.text.push('\n');
        msg.text.push_str(&truncated);
        self.rebuild_tool_segment(tool_id);
    }

    pub fn tool_done(&mut self, event: ToolDoneEvent) {
        let Some(msg) = self
            .messages
            .iter_mut()
            .rfind(|m| matches!(m.role, DisplayRole::Tool { ref id, .. } if *id == event.id))
        else {
            return;
        };
        if let DisplayRole::Tool { ref mut status, .. } = msg.role {
            *status = if event.is_error {
                ToolStatus::Error
            } else {
                ToolStatus::Success
            };
        }
        truncate_to_header(&mut msg.text);

        match &event.output {
            ToolOutput::Plain(text) => {
                if let Some(annotation) = tool_summary_annotation(event.tool, text) {
                    msg.text = format!("{} ({annotation})", msg.text);
                }
                let hide_body = matches!(event.tool, WEBFETCH_TOOL_NAME);
                if !hide_body {
                    let display = if event.tool == BASH_TOOL_NAME {
                        tail_lines(text, BASH_OUTPUT_MAX_LINES)
                    } else {
                        truncate_lines(text, TOOL_OUTPUT_MAX_LINES)
                    };
                    if !display.is_empty() {
                        msg.text = format!("{}\n{display}", msg.text);
                    }
                }
            }
            ToolOutput::ReadCode { lines, .. } => {
                msg.text = format!("{} ({} lines)", msg.text, lines.len());
            }
            ToolOutput::WriteCode { byte_count, .. } => {
                msg.text = format!("{} ({byte_count} bytes)", msg.text);
            }
            ToolOutput::GrepResult { entries, .. } => {
                msg.text = format!("{} ({} files)", msg.text, entries.len());
            }
            ToolOutput::Batch { entries, .. } => {
                let failed = entries.iter().filter(|e| e.is_error).count();
                if failed > 0 {
                    let total = entries.len();
                    msg.text = format!("{}/{total} tools succeeded", total - failed);
                }
            }
            _ => {}
        }
        msg.tool_output = Some(event.output);
        self.in_progress_count -= 1;
        self.rebuild_tool_segment(&event.id);
    }

    pub fn fail_in_progress(&mut self) {
        for msg in &mut self.messages {
            if let DisplayRole::Tool { ref mut status, .. } = msg.role
                && *status == ToolStatus::InProgress
            {
                *status = ToolStatus::Error;
            }
        }
        self.in_progress_count = 0;
        for seg in &mut self.cached_segments {
            if let Some(ref tool_id) = seg.tool_id
                && let Some(msg) = self
                    .messages
                    .iter()
                    .rfind(|m| matches!(&m.role, DisplayRole::Tool { id, .. } if id == tool_id))
            {
                let DisplayRole::Tool { status, .. } = &msg.role else {
                    continue;
                };
                seg.lines = build_tool_lines(msg, *status, self.started_at);
                seg.cached_height = None;
            }
        }
    }

    #[cfg(test)]
    pub fn in_progress_count(&self) -> usize {
        self.in_progress_count
    }

    pub fn flush(&mut self) {
        self.flush_thinking();
        if !self.streaming_text.is_empty() {
            self.messages.push(DisplayMessage {
                role: DisplayRole::Assistant,
                text: self.streaming_text.take_all(),
                tool_input: None,
                tool_output: None,
            });
            self.cached_streaming_text = StreamingCache::default();
        }
    }

    pub fn scroll(&mut self, delta: i32) {
        if delta > 0 {
            self.scroll_top = self.scroll_top.saturating_sub(delta as u16);
        } else {
            self.scroll_top = self.scroll_top.saturating_add(delta.unsigned_abs() as u16);
        }
        self.auto_scroll = false;
    }

    pub fn auto_scroll(&self) -> bool {
        self.auto_scroll
    }

    pub fn enable_auto_scroll(&mut self) {
        self.auto_scroll = true;
    }

    pub fn half_page(&self) -> i32 {
        self.viewport_height as i32 / 2
    }

    pub fn is_animating(&self) -> bool {
        self.in_progress_count > 0
            || self.streaming_thinking.is_animating()
            || self.streaming_text.is_animating()
    }

    pub fn view(&mut self, frame: &mut Frame, area: Rect) {
        self.viewport_height = area.height;
        self.rebuild_line_cache();
        if self.in_progress_count > 0 {
            self.update_spinners();
        }

        self.streaming_thinking.tick();
        self.streaming_text.tick();

        let width = area.width;

        let mut heights: Vec<u16> = self
            .cached_segments
            .iter_mut()
            .map(|seg| {
                if let Some((w, h)) = seg.cached_height
                    && w == width
                {
                    return h;
                }
                let h = Paragraph::new(seg.lines.clone())
                    .wrap(Wrap { trim: false })
                    .line_count(width) as u16;
                seg.cached_height = Some((width, h));
                h
            })
            .collect();

        let mut segments: Vec<(&[Line<'static>], bool)> = self
            .cached_segments
            .iter()
            .map(|s| (s.lines.as_slice(), s.tool_id.is_some()))
            .collect();

        let spacer_line = vec![Line::default()];
        let streaming_sources: [(&Typewriter, &mut StreamingCache, &str, Style, Style); 2] = [
            (
                &self.streaming_thinking,
                &mut self.cached_streaming_thinking,
                THINKING_STYLE.prefix,
                THINKING_STYLE.text_style,
                THINKING_STYLE.prefix_style,
            ),
            (
                &self.streaming_text,
                &mut self.cached_streaming_text,
                ASSISTANT_STYLE.prefix,
                ASSISTANT_STYLE.text_style,
                ASSISTANT_STYLE.prefix_style,
            ),
        ];
        for (tw, cache, prefix, text_style, prefix_style) in streaming_sources {
            if tw.is_empty() {
                continue;
            }
            let lines = cache.get_or_update(tw.visible(), prefix, text_style, prefix_style);
            if !segments.is_empty() {
                segments.push((&spacer_line, false));
                heights.push(1);
            }
            heights.push(
                Paragraph::new(lines.to_vec())
                    .wrap(Wrap { trim: false })
                    .line_count(width) as u16,
            );
            segments.push((lines, false));
        }

        let total_lines: u16 = heights.iter().sum();
        let max_scroll = total_lines.saturating_sub(area.height);
        self.scroll_top = self.scroll_top.min(max_scroll);
        if self.scroll_top >= max_scroll {
            self.auto_scroll = true;
        }
        if self.auto_scroll {
            self.scroll_top = max_scroll;
        }

        let mut skip = self.scroll_top;
        let mut y = area.y;
        let bottom = area.y + area.height;

        for (i, (lines, is_tool)) in segments.iter().enumerate() {
            if y >= bottom {
                break;
            }
            let h = heights[i];
            if skip >= h {
                skip -= h;
                continue;
            }
            let visible_h = h.saturating_sub(skip).min(bottom - y);
            let seg_area = Rect::new(area.x, y, area.width, visible_h);
            let mut p = Paragraph::new(lines.to_vec()).wrap(Wrap { trim: false });
            if *is_tool {
                p = p.style(theme::TOOL_BG);
            }
            if skip > 0 {
                p = p.scroll((skip, 0));
                skip = 0;
            }
            frame.render_widget(p, seg_area);
            y += visible_h;
        }

        if total_lines > area.height {
            render_vertical_scrollbar(frame, area, total_lines, self.scroll_top);
        }
    }

    fn flush_thinking(&mut self) {
        if !self.streaming_thinking.is_empty() {
            self.messages.push(DisplayMessage {
                role: DisplayRole::Thinking,
                text: self.streaming_thinking.take_all(),
                tool_input: None,
                tool_output: None,
            });
            self.cached_streaming_thinking = StreamingCache {
                dim: true,
                ..StreamingCache::default()
            };
        }
    }

    fn update_spinners(&mut self) {
        let spinner_span = Span::styled(
            format!("{} ", spinner_frame(self.started_at.elapsed().as_millis())),
            theme::TOOL_IN_PROGRESS,
        );
        for seg in &mut self.cached_segments {
            let Some(ref tool_id) = seg.tool_id else {
                continue;
            };
            let is_in_progress = self.messages.iter().any(|m| {
                matches!(&m.role, DisplayRole::Tool { id, status: ToolStatus::InProgress, .. } if id == tool_id)
            });
            if is_in_progress
                && let Some(first_line) = seg.lines.first_mut()
                && !first_line.spans.is_empty()
            {
                first_line.spans[0] = spinner_span.clone();
            }
        }
    }

    fn rebuild_tool_segment(&mut self, tool_id: &str) {
        let Some(msg) = self
            .messages
            .iter()
            .rfind(|m| matches!(&m.role, DisplayRole::Tool { id, .. } if id == tool_id))
        else {
            return;
        };
        let DisplayRole::Tool { status, .. } = &msg.role else {
            unreachable!()
        };
        let lines = build_tool_lines(msg, *status, self.started_at);
        if let Some(seg) = self
            .cached_segments
            .iter_mut()
            .rfind(|s| s.tool_id.as_deref() == Some(tool_id))
        {
            seg.lines = lines;
            seg.cached_height = None;
        }
    }

    fn rebuild_line_cache(&mut self) {
        if self.cached_msg_count == self.messages.len() {
            return;
        }
        for i in self.cached_msg_count..self.messages.len() {
            let msg = &self.messages[i];

            if let DisplayRole::Tool { ref id, status, .. } = msg.role {
                let lines = build_tool_lines(msg, status, self.started_at);
                let id = id.clone();
                self.push_spacer_if_needed();
                self.cached_segments.push(Segment {
                    lines,
                    tool_id: Some(id),
                    cached_height: None,
                });
            } else {
                let style = match &msg.role {
                    DisplayRole::User => &USER_STYLE,
                    DisplayRole::Assistant => &ASSISTANT_STYLE,
                    DisplayRole::Thinking => &THINKING_STYLE,
                    DisplayRole::Error => &ERROR_STYLE,
                    DisplayRole::Tool { .. } => unreachable!(),
                };
                let mut lines = if style.use_markdown {
                    text_to_lines(
                        &msg.text,
                        style.prefix,
                        style.text_style,
                        style.prefix_style,
                        None,
                    )
                } else {
                    plain_lines(
                        &msg.text,
                        style.prefix,
                        style.text_style,
                        style.prefix_style,
                    )
                };
                if msg.role == DisplayRole::Thinking {
                    theme::dim_lines(&mut lines);
                }

                self.push_spacer_if_needed();
                self.cached_segments.push(Segment {
                    lines,
                    tool_id: None,
                    cached_height: None,
                });
            }
        }
        self.cached_msg_count = self.messages.len();
    }

    fn push_spacer_if_needed(&mut self) {
        if !self.cached_segments.is_empty() {
            self.cached_segments.push(Segment {
                lines: vec![Line::default()],
                tool_id: None,
                cached_height: None,
            });
        }
    }
}

fn build_tool_lines(
    msg: &DisplayMessage,
    status: ToolStatus,
    started_at: Instant,
) -> Vec<Line<'static>> {
    let header = msg
        .text
        .split_once('\n')
        .map_or(msg.text.as_str(), |(h, _)| h);
    let tool_name = msg.role.tool_name().unwrap_or("?");
    let prefix = format!("{tool_name}> ");
    let mut header_spans = vec![Span::styled(prefix, theme::TOOL_PREFIX)];
    header_spans.extend(style_tool_header(tool_name, header));
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

    if let Some(ToolInput::Code { language, code }) = &msg.tool_input {
        for mut line in highlight::highlight_code(language, code) {
            line.spans.insert(0, Span::raw(TOOL_BODY_INDENT.to_owned()));
            lines.push(line);
        }
    }

    match msg.tool_output.as_ref() {
        None | Some(ToolOutput::Plain(_)) => {
            if let Some((_, body)) = msg.text.split_once('\n') {
                for line in body.lines() {
                    lines.push(Line::from(vec![
                        Span::styled(TOOL_BODY_INDENT.to_owned(), theme::TOOL),
                        Span::styled(line.to_owned(), theme::TOOL),
                    ]));
                }
            }
        }
        Some(ToolOutput::ReadCode {
            path,
            start_line,
            lines: code_lines,
            ..
        }) => {
            lines.extend(code_view::render_read_code(path, *start_line, code_lines));
        }
        Some(ToolOutput::WriteCode {
            path,
            lines: code_lines,
            ..
        }) => {
            lines.extend(code_view::render_read_code(path, 1, code_lines));
        }
        Some(ToolOutput::GrepResult { entries, .. }) => {
            lines.extend(code_view::render_grep_results(
                entries,
                TOOL_OUTPUT_MAX_LINES,
            ));
        }
        Some(ToolOutput::Diff { path, hunks, .. }) => {
            lines.extend(code_view::render_diff(path, hunks));
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
    }

    lines
}

fn truncate_to_header(text: &mut String) {
    let end = text.find('\n').unwrap_or(text.len());
    text.truncate(end);
}

fn render_vertical_scrollbar(frame: &mut Frame, area: Rect, content_len: u16, position: u16) {
    let max_scroll = content_len.saturating_sub(area.height);
    let mut state = ScrollbarState::default()
        .content_length(max_scroll as usize + 1)
        .position(position as usize);

    let scrollbar = Scrollbar::new(ScrollbarOrientation::VerticalRight)
        .thumb_symbol(SCROLLBAR_THUMB)
        .track_symbol(None)
        .begin_symbol(None)
        .end_symbol(None);

    frame.render_stateful_widget(scrollbar, area, &mut state);
}

#[cfg(test)]
mod tests {
    use super::*;
    use maki_agent::tools::WRITE_TOOL_NAME;
    use maki_providers::{GrepFileEntry, GrepMatch, ToolOutput};
    use ratatui::backend::TestBackend;
    use test_case::test_case;

    fn start(id: &str, tool: &'static str) -> ToolStartEvent {
        ToolStartEvent {
            id: id.into(),
            tool,
            summary: id.into(),
            input: None,
        }
    }

    fn panel_with_tools(ids: &[(&str, &'static str)]) -> MessagesPanel {
        let mut panel = MessagesPanel::new();
        for &(id, tool) in ids {
            panel.tool_start(start(id, tool));
        }
        panel
    }

    #[test_case(false, ToolStatus::Success ; "success_updates_start_to_success")]
    #[test_case(true,  ToolStatus::Error   ; "error_updates_start_to_error")]
    fn tool_done_updates_start_status(is_error: bool, expected: ToolStatus) {
        let mut panel = MessagesPanel::new();
        panel.tool_start(start("t1", "bash"));
        assert!(matches!(
            panel.messages[0].role,
            DisplayRole::Tool {
                status: ToolStatus::InProgress,
                ..
            }
        ));

        panel.tool_done(ToolDoneEvent {
            id: "t1".into(),
            tool: "bash",
            output: ToolOutput::Plain("output".into()),
            is_error,
        });

        assert_eq!(panel.messages.len(), 1);
        assert!(
            matches!(panel.messages[0].role, DisplayRole::Tool { status, .. } if status == expected)
        );
        assert!(panel.messages[0].text.contains("output"));
    }

    #[test_case(GLOB_TOOL_NAME, "src/a.rs\nsrc/b.rs\nsrc/c.rs", Some("3 files") ; "glob_file_count")]
    #[test_case(WEBFETCH_TOOL_NAME, "line1\nline2\nline3", Some("3 lines") ; "webfetch_line_count")]
    #[test_case("bash", "ok", None ; "short_output_no_annotation")]
    #[test_case("bash", &(0..20).map(|i| format!("line {i}")).collect::<Vec<_>>().join("\n"), Some("20 lines") ; "long_output_line_count")]
    fn summary_annotation(tool: &str, output: &str, expected: Option<&str>) {
        assert_eq!(tool_summary_annotation(tool, output).as_deref(), expected,);
    }

    #[test]
    fn webfetch_hides_body() {
        let mut panel = MessagesPanel::new();
        panel.tool_start(start("t1", WEBFETCH_TOOL_NAME));
        panel.tool_done(ToolDoneEvent {
            id: "t1".into(),
            tool: WEBFETCH_TOOL_NAME,
            output: ToolOutput::Plain("fetched content\nmore lines".into()),
            is_error: false,
        });
        assert!(!panel.messages[0].text.contains('\n'));
    }

    #[test]
    fn write_done_shows_bytes_annotation() {
        let mut panel = MessagesPanel::new();
        panel.tool_start(start("t1", WRITE_TOOL_NAME));
        panel.tool_done(ToolDoneEvent {
            id: "t1".into(),
            tool: WRITE_TOOL_NAME,
            output: ToolOutput::WriteCode {
                path: "src/main.rs".into(),
                byte_count: 42,
                lines: vec!["fn main() {}".into()],
            },
            is_error: false,
        });
        assert!(panel.messages[0].text.contains("42 bytes"));
    }

    fn grep_output(n_files: usize) -> ToolOutput {
        ToolOutput::GrepResult {
            entries: (0..n_files)
                .map(|i| GrepFileEntry {
                    path: format!("{i}.rs"),
                    matches: vec![GrepMatch {
                        line_nr: 1,
                        text: String::new(),
                    }],
                })
                .collect(),
        }
    }

    #[test]
    fn tool_done_grep_result_annotation() {
        let mut panel = MessagesPanel::new();
        panel.tool_start(start("t1", "grep"));
        panel.tool_done(ToolDoneEvent {
            id: "t1".into(),
            tool: "grep",
            output: grep_output(2),
            is_error: false,
        });
        assert!(panel.messages[0].text.contains("2 files"));
    }

    #[test]
    fn tool_start_flushes_streaming_text() {
        let mut panel = MessagesPanel::new();
        panel.streaming_text.set_buffer("partial response");

        panel.tool_start(start("t1", "read"));

        assert!(panel.streaming_text.is_empty());
        assert_eq!(panel.messages[0].role, DisplayRole::Assistant);
        assert!(matches!(panel.messages[1].role, DisplayRole::Tool { .. }));
    }

    #[test]
    fn thinking_delta_separate_from_text() {
        let mut panel = MessagesPanel::new();
        panel.thinking_delta("reasoning");
        assert_eq!(panel.streaming_thinking, "reasoning");
        assert!(panel.streaming_text.is_empty());

        panel.text_delta("output");
        assert!(panel.streaming_thinking.is_empty());
        assert_eq!(panel.streaming_text, "output");
        assert_eq!(panel.messages[0].role, DisplayRole::Thinking);
        assert_eq!(panel.messages[0].text, "reasoning");
    }

    #[test_case(10, 10, 0   ; "up_saturates_at_zero")]
    #[test_case(20, 10, 10  ; "half_page_up")]
    #[test_case(5,  -10, 15 ; "half_page_down")]
    #[test_case(5,  1, 4    ; "scroll_up_one")]
    #[test_case(5,  -1, 6   ; "scroll_down_one")]
    fn scroll_by_delta(initial: u16, delta: i32, expected: u16) {
        let mut panel = MessagesPanel::new();
        panel.viewport_height = 20;
        panel.scroll_top = initial;
        panel.scroll(delta);
        assert_eq!(panel.scroll_top, expected);
    }

    #[test]
    fn scroll_top_clamped_to_content() {
        let mut panel = MessagesPanel::new();
        panel.push(DisplayMessage {
            role: DisplayRole::User,
            text: "short".into(),
            tool_input: None,
            tool_output: None,
        });
        panel.scroll_top = 1000;
        panel.auto_scroll = false;
        rebuild(&mut panel);
        assert_eq!(panel.scroll_top, 0);
    }

    #[test]
    fn scroll_up_pins_viewport_during_streaming() {
        let mut panel = MessagesPanel::new();
        panel.streaming_text.set_buffer(&"a\n".repeat(30));
        render(&mut panel, 80, 10);

        panel.scroll(1);
        panel.scroll(1);
        render(&mut panel, 80, 10);
        let pinned = panel.scroll_top;

        panel.text_delta("b\nb\nb\n");
        render(&mut panel, 80, 10);

        assert!(!panel.auto_scroll);
        assert_eq!(panel.scroll_top, pinned);
    }

    fn render(
        panel: &mut MessagesPanel,
        width: u16,
        height: u16,
    ) -> ratatui::Terminal<TestBackend> {
        let backend = TestBackend::new(width, height);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal.draw(|f| panel.view(f, f.area())).unwrap();
        terminal
    }

    fn rebuild(panel: &mut MessagesPanel) {
        render(panel, 80, 24);
    }

    #[test]
    fn ctrl_d_to_bottom_re_enables_auto_scroll() {
        let mut panel = MessagesPanel::new();
        panel.streaming_text.set_buffer(&"a\n".repeat(30));
        render(&mut panel, 80, 10);
        assert!(panel.auto_scroll);

        let half = panel.half_page();
        panel.scroll(half);
        render(&mut panel, 80, 10);
        assert!(!panel.auto_scroll);

        panel.scroll(-half);
        render(&mut panel, 80, 10);
        assert!(panel.auto_scroll);
    }

    #[test]
    fn tool_done_without_matching_start_is_noop() {
        let mut panel = MessagesPanel::new();
        panel.tool_done(ToolDoneEvent {
            id: "orphan".into(),
            tool: "bash",
            output: ToolOutput::Plain("output".into()),
            is_error: false,
        });
        assert!(panel.messages.is_empty());
    }

    #[test]
    fn in_progress_tracking_and_fail() {
        let mut panel = panel_with_tools(&[("t1", "bash"), ("t2", "read")]);
        assert_eq!(panel.in_progress_count, 2);

        panel.tool_done(ToolDoneEvent {
            id: "t1".into(),
            tool: "bash",
            output: ToolOutput::Plain("ok".into()),
            is_error: false,
        });
        assert_eq!(panel.in_progress_count, 1);
        assert!(panel.is_animating());

        panel.tool_done(ToolDoneEvent {
            id: "t2".into(),
            tool: "read",
            output: ToolOutput::Plain("ok".into()),
            is_error: false,
        });
        assert_eq!(panel.in_progress_count, 0);
        assert!(!panel.is_animating());
    }

    #[test]
    fn fail_in_progress_marks_all_as_error() {
        let mut panel = panel_with_tools(&[("t1", "bash"), ("t2", "read")]);

        panel.fail_in_progress();

        assert_eq!(panel.in_progress_count, 0);
        assert!(!panel.is_animating());
        for msg in &panel.messages {
            assert!(matches!(
                msg.role,
                DisplayRole::Tool {
                    status: ToolStatus::Error,
                    ..
                }
            ));
        }
    }

    fn has_scrollbar_thumb(terminal: &ratatui::Terminal<TestBackend>) -> bool {
        let buf = terminal.backend().buffer();
        (0..buf.area.height).any(|y| {
            buf.cell((buf.area.width - 1, y))
                .is_some_and(|c: &ratatui::buffer::Cell| c.symbol() == SCROLLBAR_THUMB)
        })
    }

    #[test_case(40, true  ; "rendered_when_content_overflows")]
    #[test_case(1,  false ; "hidden_when_content_fits")]
    fn scrollbar_visibility(line_count: usize, expected: bool) {
        let mut panel = MessagesPanel::new();
        panel
            .streaming_text
            .set_buffer(&"line\n".repeat(line_count));
        let terminal = render(&mut panel, 80, 10);
        assert_eq!(has_scrollbar_thumb(&terminal), expected);
    }

    fn seg_text(panel: &MessagesPanel, tool_id: &str) -> String {
        panel
            .cached_segments
            .iter()
            .find(|s| s.tool_id.as_deref() == Some(tool_id))
            .unwrap()
            .lines
            .iter()
            .flat_map(|l| l.spans.iter().map(|s| s.content.as_ref()))
            .collect()
    }

    fn msg_status(panel: &MessagesPanel, tool_id: &str) -> ToolStatus {
        panel
            .messages
            .iter()
            .rfind(|m| matches!(&m.role, DisplayRole::Tool { id, .. } if id == tool_id))
            .map(|m| match &m.role {
                DisplayRole::Tool { status, .. } => *status,
                _ => unreachable!(),
            })
            .unwrap()
    }

    fn has_seg(panel: &MessagesPanel, tool_id: &str) -> bool {
        panel
            .cached_segments
            .iter()
            .any(|s| s.tool_id.as_deref() == Some(tool_id))
    }

    #[test]
    fn tool_output_rebuilds_only_target_segment() {
        let mut panel = panel_with_tools(&[("t1", "bash"), ("t2", "bash")]);
        rebuild(&mut panel);
        let seg_count_before = panel.cached_segments.len();

        panel.tool_output("t1", "new output");

        assert_eq!(panel.cached_segments.len(), seg_count_before);
        assert!(seg_text(&panel, "t1").contains("new output"));
    }

    #[test]
    fn tool_output_for_unknown_id_is_noop() {
        let mut panel = panel_with_tools(&[("t1", "bash")]);
        rebuild(&mut panel);
        let seg_count = panel.cached_segments.len();
        panel.tool_output("nonexistent", "data");
        assert_eq!(panel.cached_segments.len(), seg_count);
    }

    #[test]
    fn tool_output_before_cache_built_renders_correctly() {
        let mut panel = panel_with_tools(&[("t1", "bash")]);
        panel.tool_output("t1", "early output");
        rebuild(&mut panel);
        assert!(seg_text(&panel, "t1").contains("early output"));
    }

    #[test]
    fn tool_done_before_cache_built_renders_with_correct_status() {
        let mut panel = panel_with_tools(&[("t1", "bash")]);
        panel.tool_done(ToolDoneEvent {
            id: "t1".into(),
            tool: "bash",
            output: ToolOutput::Plain("result".into()),
            is_error: false,
        });
        rebuild(&mut panel);
        assert_eq!(msg_status(&panel, "t1"), ToolStatus::Success);
        assert!(seg_text(&panel, "t1").contains("result"));
    }

    #[test]
    fn multiple_tool_output_replaces_body() {
        let mut panel = panel_with_tools(&[("t1", "bash")]);
        rebuild(&mut panel);
        panel.tool_output("t1", "first");
        panel.tool_output("t1", "second");
        let text = seg_text(&panel, "t1");
        assert!(text.contains("second"));
        assert!(!text.contains("first"));
    }

    #[test]
    fn fail_in_progress_preserves_completed_tool_status() {
        let mut panel = panel_with_tools(&[("t1", "bash"), ("t2", "read")]);
        panel.tool_done(ToolDoneEvent {
            id: "t1".into(),
            tool: "bash",
            output: ToolOutput::Plain("ok".into()),
            is_error: false,
        });
        rebuild(&mut panel);

        panel.fail_in_progress();

        assert_eq!(msg_status(&panel, "t1"), ToolStatus::Success);
        assert_eq!(msg_status(&panel, "t2"), ToolStatus::Error);
    }

    #[test]
    fn new_tool_after_in_place_update() {
        let mut panel = panel_with_tools(&[("t1", "bash")]);
        rebuild(&mut panel);
        panel.tool_output("t1", "streaming data");

        panel.tool_start(start("t2", "read"));
        rebuild(&mut panel);

        assert!(seg_text(&panel, "t1").contains("streaming data"));
        assert!(has_seg(&panel, "t2"));
    }

    #[test]
    fn tool_done_after_tool_output_transitions_status() {
        let mut panel = panel_with_tools(&[("t1", "bash")]);
        rebuild(&mut panel);
        assert_eq!(msg_status(&panel, "t1"), ToolStatus::InProgress);

        panel.tool_output("t1", "partial");
        assert_eq!(msg_status(&panel, "t1"), ToolStatus::InProgress);

        panel.tool_done(ToolDoneEvent {
            id: "t1".into(),
            tool: "bash",
            output: ToolOutput::Plain("final".into()),
            is_error: false,
        });
        assert_eq!(msg_status(&panel, "t1"), ToolStatus::Success);
    }

    #[test]
    fn fail_in_progress_before_cache_built_no_panic() {
        let mut panel = panel_with_tools(&[("t1", "bash"), ("t2", "read")]);
        panel.fail_in_progress();
        assert_eq!(panel.in_progress_count, 0);
        rebuild(&mut panel);
        assert_eq!(msg_status(&panel, "t1"), ToolStatus::Error);
        assert_eq!(msg_status(&panel, "t2"), ToolStatus::Error);
    }

    fn tool_msg(id: &str, name: &'static str, status: ToolStatus) -> DisplayMessage {
        DisplayMessage {
            role: DisplayRole::Tool {
                id: id.into(),
                status,
                name,
            },
            text: id.into(),
            tool_input: None,
            tool_output: None,
        }
    }

    #[test]
    fn load_messages_counts_in_progress_and_replaces_state() {
        let mut panel = panel_with_tools(&[("old", "bash")]);
        assert_eq!(panel.in_progress_count, 1);

        panel.load_messages(vec![
            tool_msg("t1", "bash", ToolStatus::InProgress),
            tool_msg("t2", "read", ToolStatus::Success),
        ]);
        assert_eq!(panel.in_progress_count, 1);
        assert_eq!(panel.messages.len(), 2);

        panel.load_messages(Vec::new());
        assert_eq!(panel.in_progress_count, 0);
        assert!(panel.messages.is_empty());
    }
}

use super::{DisplayMessage, DisplayRole, ToolStatus};

use super::tool_display::{
    ASSISTANT_STYLE, BASH_OUTPUT_MAX_LINES, ERROR_STYLE, QUESTION_STYLE, THINKING_STYLE,
    TOOL_OUTPUT_MAX_LINES, USER_STYLE, build_tool_lines, tool_summary_annotation,
    truncate_to_header,
};
use crate::animation::{Typewriter, spinner_frame};
use crate::highlight::{CodeHighlighter, HighlightWorker};
use crate::markdown::{plain_lines, tail_lines, text_to_lines, truncate_lines};
use crate::theme;

use std::time::Instant;

use maki_agent::tools::{BASH_TOOL_NAME, QUESTION_TOOL_NAME, WEBFETCH_TOOL_NAME};
use maki_providers::{BatchToolStatus, ToolDoneEvent, ToolOutput, ToolStartEvent};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Wrap};

use super::scrollbar::render_vertical_scrollbar;

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
        width: u16,
    ) -> &[Line<'static>] {
        let len = visible.len();
        if len != self.byte_len || self.lines.is_empty() {
            self.lines = text_to_lines(
                visible,
                prefix,
                text_style,
                prefix_style,
                Some(&mut self.highlighters),
                width,
            );
            if self.dim {
                theme::dim_lines(&mut self.lines);
            }
            self.byte_len = len;
        }
        &self.lines
    }
}

#[derive(Default)]
struct Segment {
    lines: Vec<Line<'static>>,
    tool_id: Option<String>,
    cached_height: Option<(u16, u16)>,
    pending_highlight: Option<u64>,
    highlight_range: Option<(usize, usize)>,
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
    viewport_width: u16,
    cached_segments: Vec<Segment>,
    cached_msg_count: usize,
    cached_streaming_thinking: StreamingCache,
    cached_streaming_text: StreamingCache,
    hl_worker: HighlightWorker,
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
            viewport_width: 80,
            cached_segments: Vec::new(),
            cached_msg_count: 0,
            cached_streaming_thinking: StreamingCache {
                dim: true,
                ..StreamingCache::default()
            },
            cached_streaming_text: StreamingCache::default(),
            hl_worker: HighlightWorker::new(),
        }
    }

    pub fn reset(&mut self) {
        self.messages.clear();
        self.streaming_thinking = Typewriter::new();
        self.streaming_text = Typewriter::new();
        self.started_at = Instant::now();
        self.in_progress_count = 0;
        self.scroll_top = u16::MAX;
        self.auto_scroll = true;
        self.cached_segments.clear();
        self.cached_msg_count = 0;
        self.cached_streaming_thinking = StreamingCache {
            dim: true,
            ..StreamingCache::default()
        };
        self.cached_streaming_text = StreamingCache::default();
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
            text: event.summary,
            tool_input: event.input,
            tool_output: event.output,
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
                if event.tool == QUESTION_TOOL_NAME {
                    msg.text = text.clone();
                } else {
                    if let Some(annotation) = tool_summary_annotation(event.tool, text) {
                        msg.text = format!("{} ({annotation})", msg.text);
                    }
                    if !matches!(event.tool, WEBFETCH_TOOL_NAME) {
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
                let failed = entries
                    .iter()
                    .filter(|e| e.status == BatchToolStatus::Error)
                    .count();
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

    pub fn batch_progress(&mut self, batch_id: &str, index: usize, status: BatchToolStatus) {
        let Some(msg) = self
            .messages
            .iter_mut()
            .rfind(|m| matches!(m.role, DisplayRole::Tool { ref id, .. } if *id == batch_id))
        else {
            return;
        };
        if let Some(ToolOutput::Batch { entries, .. }) = &mut msg.tool_output
            && let Some(entry) = entries.get_mut(index)
        {
            entry.status = status;
        }
        self.rebuild_tool_segment(batch_id);
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
                let tl = build_tool_lines(msg, *status, self.started_at);
                seg.lines = tl.lines;
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
            self.messages.push(DisplayMessage::new(
                DisplayRole::Assistant,
                self.streaming_text.take_all(),
            ));
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
        let width = area.width;
        if self.viewport_width != width {
            self.viewport_width = width;
            self.cached_msg_count = 0;
            self.cached_segments.clear();
        }
        self.drain_highlights();
        self.rebuild_line_cache();
        if self.in_progress_count > 0 {
            self.update_spinners();
        }

        self.streaming_thinking.tick();
        self.streaming_text.tick();

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
            let lines = cache.get_or_update(tw.visible(), prefix, text_style, prefix_style, width);
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
            let seg_area = Rect::new(area.x, y, width, visible_h);
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
            self.messages.push(DisplayMessage::new(
                DisplayRole::Thinking,
                self.streaming_thinking.take_all(),
            ));
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
            let msg = self.messages.iter().find(|m| {
                matches!(&m.role, DisplayRole::Tool { id, status: ToolStatus::InProgress, .. } if id == tool_id)
            });
            let Some(msg) = msg else { continue };
            if let Some(first_line) = seg.lines.first_mut()
                && !first_line.spans.is_empty()
            {
                first_line.spans[0] = spinner_span.clone();
            }
            if let Some(ToolOutput::Batch { entries, .. }) = &msg.tool_output {
                let body_start = 1;
                for (i, entry) in entries.iter().enumerate() {
                    if entry.status != BatchToolStatus::InProgress {
                        continue;
                    }
                    if let Some(line) = seg.lines.get_mut(body_start + i)
                        && line.spans.len() > 1
                    {
                        line.spans[1] = spinner_span.clone();
                    }
                }
            }
        }
    }

    fn drain_highlights(&mut self) {
        while let Some(result) = self.hl_worker.try_recv() {
            if let Some(seg) = self
                .cached_segments
                .iter_mut()
                .find(|s| s.pending_highlight == Some(result.id))
            {
                if let Some((start, end)) = seg.highlight_range {
                    let new_end = start + result.lines.len();
                    seg.lines.splice(start..end, result.lines);
                    seg.highlight_range = Some((start, new_end));
                }
                seg.cached_height = None;
                seg.pending_highlight = None;
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
        let Some(seg_idx) = self
            .cached_segments
            .iter()
            .rposition(|s| s.tool_id.as_deref() == Some(tool_id))
        else {
            return;
        };
        let tl = build_tool_lines(msg, *status, self.started_at);
        let pending = tl.send_highlight(&self.hl_worker);
        let seg = &mut self.cached_segments[seg_idx];
        seg.lines = tl.lines;
        seg.cached_height = None;
        seg.pending_highlight = pending;
        seg.highlight_range = tl.highlight.as_ref().map(|h| h.range);
    }

    fn rebuild_line_cache(&mut self) {
        if self.cached_msg_count == self.messages.len() {
            return;
        }
        for i in self.cached_msg_count..self.messages.len() {
            let msg = &self.messages[i];

            if let DisplayRole::Tool {
                ref id,
                status,
                name,
            } = msg.role
            {
                if name == QUESTION_TOOL_NAME {
                    let lines = text_to_lines(
                        &msg.text,
                        QUESTION_STYLE.prefix,
                        QUESTION_STYLE.text_style,
                        QUESTION_STYLE.prefix_style,
                        None,
                        self.viewport_width,
                    );
                    self.push_spacer_if_needed();
                    self.cached_segments.push(Segment {
                        lines,
                        ..Segment::default()
                    });
                } else {
                    let tl = build_tool_lines(msg, status, self.started_at);
                    let pending = tl.send_highlight(&self.hl_worker);
                    let id = id.clone();
                    self.push_spacer_if_needed();
                    self.cached_segments.push(Segment {
                        lines: tl.lines,
                        tool_id: Some(id),
                        pending_highlight: pending,
                        highlight_range: tl.highlight.as_ref().map(|h| h.range),
                        ..Segment::default()
                    });
                }
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
                        self.viewport_width,
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
                    ..Segment::default()
                });
            }
        }
        self.cached_msg_count = self.messages.len();
    }

    fn push_spacer_if_needed(&mut self) {
        if !self.cached_segments.is_empty() {
            self.cached_segments.push(Segment {
                lines: vec![Line::default()],
                ..Segment::default()
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::components::scrollbar::SCROLLBAR_THUMB;
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
            output: None,
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

    #[test_case(10, 10, 0  ; "up_saturates_at_zero")]
    #[test_case(5,  1, 4    ; "scroll_up")]
    #[test_case(5,  -1, 6   ; "scroll_down")]
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
        panel.push(DisplayMessage::new(DisplayRole::User, "short".into()));
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
    fn in_progress_tracking() {
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
    fn bash_live_output_with_code_input() {
        let mut panel = MessagesPanel::new();
        panel.tool_start(ToolStartEvent {
            id: "t1".into(),
            tool: BASH_TOOL_NAME,
            summary: "echo hello".into(),
            input: Some(maki_providers::ToolInput::Code {
                language: "bash",
                code: "echo hello".into(),
            }),
            output: None,
        });
        rebuild(&mut panel);
        panel.tool_output("t1", "hello\nworld");
        let text = seg_text(&panel, "t1");
        assert!(text.contains("hello"), "live output should be visible");
        assert!(text.contains("world"), "live output should be visible");
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
        DisplayMessage::new(
            DisplayRole::Tool {
                id: id.into(),
                status,
                name,
            },
            id.into(),
        )
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

    #[test]
    fn reset_allows_reuse() {
        let mut panel = panel_with_tools(&[("t1", "bash")]);
        rebuild(&mut panel);

        panel.reset();
        assert!(panel.messages.is_empty());
        assert_eq!(panel.in_progress_count, 0);

        panel.tool_start(start("t2", "bash"));
        rebuild(&mut panel);
        assert!(has_seg(&panel, "t2"));
    }

    #[test]
    fn question_tool_uses_question_style_and_no_truncation() {
        let questions: String = (1..=20)
            .map(|i| format!("Question {i}?"))
            .collect::<Vec<_>>()
            .join("\n");
        let mut panel = MessagesPanel::new();
        panel.tool_start(start("q1", QUESTION_TOOL_NAME));
        panel.tool_done(ToolDoneEvent {
            id: "q1".into(),
            tool: QUESTION_TOOL_NAME,
            output: ToolOutput::Plain(questions.clone()),
            is_error: false,
        });
        rebuild(&mut panel);

        assert!(panel.messages[0].text.contains("Question 1?"));
        assert!(panel.messages[0].text.contains("Question 20?"));
        assert!(!has_seg(&panel, "q1"), "question should not have tool_id");

        let seg = panel
            .cached_segments
            .iter()
            .find(|s| s.tool_id.is_none() && !s.lines.is_empty())
            .expect("should have non-tool segment");
        let text: String = seg
            .lines
            .iter()
            .flat_map(|l| l.spans.iter().map(|s| s.content.as_ref()))
            .collect();
        assert!(
            text.contains("maki asks>"),
            "question should use question prefix"
        );
        assert!(
            text.contains("Question 20?"),
            "full question text should be visible"
        );
    }
}

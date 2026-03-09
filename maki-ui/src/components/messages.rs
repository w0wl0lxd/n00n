use super::{DisplayMessage, DisplayRole, ToolStatus, apply_scroll_delta};

use super::tool_display::{
    ASSISTANT_STYLE, ERROR_STYLE, THINKING_STYLE, ToolLines, USER_STYLE, append_annotation,
    append_timestamp, build_batch_entry_lines, build_tool_lines, format_timestamp_now,
    output_limits, tool_output_annotation, truncate_to_header,
};
use crate::animation::{Typewriter, spinner_frame};
use crate::highlight::CodeHighlighter;
use crate::markdown::{hr_line, plain_lines, text_to_lines, truncate_lines};
use crate::render_worker::RenderWorker;
use crate::theme;

use std::time::Instant;

use maki_agent::tools::{ToolCall, WEBFETCH_TOOL_NAME};
use maki_agent::{
    BatchToolEntry, BatchToolStatus, NO_FILES_FOUND, ToolDoneEvent, ToolOutput, ToolStartEvent,
};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Wrap};
use unicode_width::UnicodeWidthStr;

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

/// `copy_text` holds raw source text for clipboard copy (from
/// `DisplayMessage::copy_text()`). Fully-selected segments use this instead
/// of lossy cell scraping.
#[derive(Default)]
struct Segment {
    lines: Vec<Line<'static>>,
    copy_text: String,
    tool_id: Option<String>,
    msg_index: Option<usize>,
    cached_height: Option<(u16, u16)>,
    pending_highlight: Option<u64>,
    highlight_range: Option<(usize, usize)>,
    highlighted_has_output: bool,
    spinner_lines: Vec<usize>,
    content_indent: &'static str,
}

impl Segment {
    fn reuse_highlight(&self, has_output: bool) -> Option<Vec<Line<'static>>> {
        if self.pending_highlight.is_some() || self.highlighted_has_output != has_output {
            return None;
        }
        let (s, e) = self.highlight_range?;
        if s > e || e > self.lines.len() {
            return None;
        }
        Some(self.lines[s..e].to_vec())
    }

    fn apply_highlight(&mut self, tl: ToolLines, worker: &RenderWorker) {
        self.pending_highlight = tl.send_highlight(worker);
        let hl = tl.highlight.as_ref();
        self.highlight_range = hl.map(|h| h.range);
        self.highlighted_has_output = hl.is_some_and(|h| h.output.is_some());
        self.spinner_lines = tl.spinner_lines;
        self.content_indent = tl.content_indent;
        self.lines = tl.lines;
        self.cached_height = None;
    }

    fn update_with_reuse(&mut self, mut tl: ToolLines, worker: &RenderWorker) {
        let has_output = tl.highlight.as_ref().is_some_and(|h| h.output.is_some());
        let reused = tl.highlight.as_ref().and_then(|req| {
            let hl_lines = self.reuse_highlight(has_output)?;
            let (s, _) = req.range;
            let new_end = s + hl_lines.len();
            tl.lines.splice(s..req.range.1, hl_lines);
            Some((s, new_end))
        });
        self.cached_height = None;
        if let Some((s, e)) = reused {
            self.lines = tl.lines;
            self.highlight_range = Some((s, e));
            self.pending_highlight = None;
            self.spinner_lines = tl.spinner_lines;
            self.content_indent = tl.content_indent;
        } else {
            self.apply_highlight(tl, worker);
        }
    }
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
    hl_worker: RenderWorker,
    visible_regions: Vec<(Rect, usize)>,
    segment_heights: Vec<u16>,
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
            hl_worker: RenderWorker::new(),
            visible_regions: Vec::new(),
            segment_heights: Vec::new(),
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
        self.cached_segments.clear();
        self.cached_msg_count = 0;
    }

    pub fn thinking_delta(&mut self, text: &str) {
        self.streaming_thinking.push(text);
    }

    pub fn text_delta(&mut self, text: &str) {
        self.flush_thinking();
        self.streaming_text.push(text);
    }

    pub fn tool_pending(&mut self, id: String, name: &str) {
        let Some(name) = ToolCall::name_static(name) else {
            return;
        };
        self.flush();
        let role = DisplayRole::Tool {
            id,
            status: ToolStatus::InProgress,
            name,
        };
        let mut msg = DisplayMessage::new(role, String::new());
        msg.timestamp = Some(format_timestamp_now());
        self.messages.push(msg);
        self.in_progress_count += 1;
    }

    pub fn tool_start(&mut self, event: ToolStartEvent) {
        if let Some(msg) = self
            .messages
            .iter_mut()
            .rfind(|m| matches!(m.role, DisplayRole::Tool { ref id, .. } if *id == event.id))
        {
            if let DisplayRole::Tool { ref mut name, .. } = msg.role {
                *name = event.tool;
            }
            msg.text = event.summary;
            msg.tool_input = event.input;
            msg.tool_output = event.output;
            msg.annotation = event.annotation;
            self.rebuild_tool_segment(&event.id);
            return;
        }
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
            annotation: event.annotation,
            plan_path: None,
            timestamp: Some(format_timestamp_now()),
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
        let tool_name = msg.role.tool_name().unwrap_or("");
        let (max_lines, keep) = output_limits(tool_name);
        truncate_to_header(&mut msg.text);
        let truncated = truncate_lines(content, max_lines, keep);
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
        let was_in_progress = matches!(
            msg.role,
            DisplayRole::Tool {
                status: ToolStatus::InProgress,
                ..
            }
        );
        if let DisplayRole::Tool { ref mut status, .. } = msg.role {
            *status = if event.is_error {
                ToolStatus::Error
            } else {
                ToolStatus::Success
            };
        }
        truncate_to_header(&mut msg.text);
        let done_annotation = tool_output_annotation(&event.output, event.tool);
        if let Some(suffix) = &done_annotation {
            append_annotation(&mut msg.annotation, suffix);
        }

        match &event.output {
            ToolOutput::Plain(text) => {
                if !matches!(event.tool, WEBFETCH_TOOL_NAME) {
                    let (max, keep) = output_limits(event.tool);
                    let display = truncate_lines(text, max, keep);
                    if !display.is_empty() {
                        msg.text = format!("{}\n{display}", msg.text);
                    }
                }
            }
            ToolOutput::QuestionAnswers(pairs) => {
                let n = pairs.len();
                msg.text = format!("{n} question{} answered", if n == 1 { "" } else { "s" });
            }
            ToolOutput::GlobResult { files } => {
                if files.is_empty() {
                    msg.text = format!("{}\n{NO_FILES_FOUND}", msg.text);
                } else {
                    let joined = files.join("\n");
                    let (max, keep) = output_limits(event.tool);
                    let display = truncate_lines(&joined, max, keep);
                    msg.text = format!("{}\n{display}", msg.text);
                }
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
        if was_in_progress {
            self.in_progress_count -= 1;
        }
        self.rebuild_tool_segment(&event.id);
    }

    pub fn batch_progress(
        &mut self,
        batch_id: &str,
        index: usize,
        status: BatchToolStatus,
        output: Option<ToolOutput>,
    ) {
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
            if output.is_some() {
                entry.output = output;
            }
        }
        self.rebuild_tool_segment(batch_id);
    }

    pub fn update_tool_summary(&mut self, tool_id: &str, summary: &str) {
        self.update_tool(
            tool_id,
            |msg| msg.text = summary.to_owned(),
            |entry| entry.summary = summary.to_owned(),
        );
    }

    pub fn update_tool_model(&mut self, tool_id: &str, model: &str) {
        self.update_tool(
            tool_id,
            |msg| append_annotation(&mut msg.annotation, model),
            |entry| append_annotation(&mut entry.annotation, model),
        );
    }

    fn update_tool(
        &mut self,
        tool_id: &str,
        update_msg: impl FnOnce(&mut DisplayMessage),
        update_entry: impl FnOnce(&mut BatchToolEntry),
    ) {
        let rebuild_id;
        if let Some((batch_id, idx)) = parse_batch_inner_id(tool_id) {
            let Some(msg) = self
                .messages
                .iter_mut()
                .rfind(|m| matches!(m.role, DisplayRole::Tool { ref id, .. } if *id == batch_id))
            else {
                return;
            };
            if let Some(ToolOutput::Batch { entries, .. }) = &mut msg.tool_output
                && let Some(entry) = entries.get_mut(idx)
            {
                update_entry(entry);
            }
            rebuild_id = batch_id.to_owned();
        } else {
            let Some(msg) = self
                .messages
                .iter_mut()
                .rfind(|m| matches!(m.role, DisplayRole::Tool { ref id, .. } if *id == tool_id))
            else {
                return;
            };
            update_msg(msg);
            rebuild_id = tool_id.to_owned();
        }
        self.rebuild_tool_segment(&rebuild_id);
    }

    pub fn fail_in_progress(&mut self) {
        let mut batch_ids = Vec::new();
        for msg in &mut self.messages {
            if let DisplayRole::Tool {
                ref id,
                ref mut status,
                ..
            } = msg.role
                && *status == ToolStatus::InProgress
            {
                *status = ToolStatus::Error;
                if let Some(ToolOutput::Batch { entries, .. }) = &mut msg.tool_output {
                    for entry in entries.iter_mut() {
                        if entry.status == BatchToolStatus::InProgress
                            || entry.status == BatchToolStatus::Pending
                        {
                            entry.status = BatchToolStatus::Error;
                        }
                    }
                    batch_ids.push(id.clone());
                }
                if let Some(seg) = self
                    .cached_segments
                    .iter_mut()
                    .rfind(|s| s.tool_id.as_deref() == Some(id.as_str()))
                {
                    let mut tl = build_tool_lines(msg, ToolStatus::Error, self.started_at);
                    if let Some(ts) = &msg.timestamp {
                        append_timestamp(&mut tl.lines[0], ts, self.viewport_width);
                    }
                    seg.apply_highlight(tl, &self.hl_worker);
                }
            }
        }
        for batch_id in &batch_ids {
            if let Some(msg) = self
                .messages
                .iter()
                .rfind(|m| matches!(&m.role, DisplayRole::Tool { id, .. } if id == batch_id))
                && let Some(ToolOutput::Batch { entries, .. }) = &msg.tool_output
            {
                let child_prefix = format!("{batch_id}__");
                for (j, entry) in entries.iter().enumerate() {
                    let child_id = format!("{batch_id}__{j}");
                    let tl = build_batch_entry_lines(entry, j, self.started_at);
                    if let Some(idx) = self
                        .cached_segments
                        .iter()
                        .rposition(|s| s.tool_id.as_deref() == Some(&child_id))
                    {
                        self.cached_segments[idx].apply_highlight(tl, &self.hl_worker);
                    } else {
                        let parent_idx = self.cached_segments.iter().rposition(|s| {
                            s.tool_id.as_deref().is_some_and(|id| {
                                id == batch_id.as_str() || id.starts_with(&child_prefix)
                            })
                        });
                        if let Some(pos) = parent_idx {
                            let msg_index = self.cached_segments[pos].msg_index;
                            let mut seg = Segment {
                                tool_id: Some(child_id),
                                msg_index,
                                ..Segment::default()
                            };
                            seg.apply_highlight(tl, &self.hl_worker);
                            self.cached_segments.insert(pos + 1, seg);
                        }
                    }
                }
            }
        }
        self.in_progress_count = 0;
    }

    #[cfg(test)]
    pub fn message_count(&self) -> usize {
        self.messages.len()
    }

    #[cfg(test)]
    pub fn in_progress_count(&self) -> usize {
        self.in_progress_count
    }

    #[cfg(test)]
    pub fn last_message_text(&self) -> &str {
        self.messages.last().map(|m| m.text.as_str()).unwrap_or("")
    }

    #[cfg(test)]
    pub fn last_message_is_plan(&self) -> bool {
        self.messages.last().is_some_and(|m| m.plan_path.is_some())
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
        self.scroll_top = apply_scroll_delta(self.scroll_top, delta).min(self.max_scroll());
        self.auto_scroll = false;
    }

    pub fn auto_scroll(&self) -> bool {
        self.auto_scroll
    }

    pub fn scroll_to_top(&mut self) {
        self.scroll_top = 0;
        self.auto_scroll = false;
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

    /// `has_selection` freezes auto-scroll so the viewport doesn't jump
    /// while the user is dragging a selection during streaming.
    pub fn view(&mut self, frame: &mut Frame, area: Rect, has_selection: bool) {
        self.viewport_height = area.height;
        let width = area.width.saturating_sub(1);
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
                let h = wrapped_line_count(&seg.lines, width);
                seg.cached_height = Some((width, h));
                h
            })
            .collect();

        let mut segments: Vec<(&[Line<'static>], bool, Option<usize>)> = self
            .cached_segments
            .iter()
            .map(|s| (s.lines.as_slice(), s.tool_id.is_some(), s.msg_index))
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
                segments.push((&spacer_line, false, None));
                heights.push(1);
            }
            heights.push(wrapped_line_count(lines, width));
            segments.push((lines, false, None));
        }

        self.segment_heights = heights.clone();
        let total_lines: u16 = self.segment_heights.iter().sum();
        let max_scroll = total_lines.saturating_sub(self.viewport_height);
        self.scroll_top = self.scroll_top.min(max_scroll);
        if !has_selection {
            if self.scroll_top >= max_scroll {
                self.auto_scroll = true;
            }
            if self.auto_scroll {
                self.scroll_top = max_scroll;
            }
        }

        let mut skip = self.scroll_top;
        let mut y = area.y;
        let bottom = area.y + area.height;
        self.visible_regions.clear();

        for (i, (lines, is_tool, msg_idx)) in segments.iter().enumerate() {
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
            if msg_idx.is_some() {
                self.visible_regions
                    .push((Rect::new(area.x, y, width, visible_h), i));
            }
            y += visible_h;
        }

        if total_lines > area.height {
            render_vertical_scrollbar(frame, area, total_lines, self.scroll_top);
        }
    }

    fn max_scroll(&self) -> u16 {
        let total: u16 = self.segment_heights.iter().sum();
        total.saturating_sub(self.viewport_height)
    }

    pub fn scroll_top(&self) -> u16 {
        self.scroll_top
    }

    /// Used by `extract_doc_range` to map doc-space rows to segments.
    pub fn segment_heights(&self) -> &[u16] {
        &self.segment_heights
    }

    pub fn segment_copy_texts(&self) -> Vec<&str> {
        self.cached_segments
            .iter()
            .map(|s| s.copy_text.as_str())
            .collect()
    }

    #[cfg(test)]
    pub fn push_content_regions<'a>(&'a self, out: &mut Vec<crate::selection::ContentRegion<'a>>) {
        out.extend(self.visible_regions.iter().map(|&(area, seg_idx)| {
            crate::selection::ContentRegion {
                area,
                raw_text: &self.cached_segments[seg_idx].copy_text,
            }
        }));
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
            let is_child = seg.tool_id.as_deref().is_some_and(|id| id.contains("__"));
            for &line_idx in &seg.spinner_lines {
                if let Some(line) = seg.lines.get_mut(line_idx)
                    && !line.spans.is_empty()
                {
                    let span_idx = if line_idx == 0 && !is_child { 0 } else { 1 };
                    if line.spans.len() > span_idx {
                        line.spans[span_idx] = spinner_span.clone();
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
                    let indent = seg.content_indent;
                    let indented: Vec<Line<'static>> = result
                        .lines
                        .into_iter()
                        .map(|mut line| {
                            if !indent.is_empty() {
                                line.spans.insert(0, Span::raw(indent));
                            }
                            line
                        })
                        .collect();
                    let new_end = start + indented.len();
                    seg.lines.splice(start..end, indented);
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

        let mut tl = build_tool_lines(msg, *status, self.started_at);
        if let Some(ts) = &msg.timestamp {
            append_timestamp(&mut tl.lines[0], ts, self.viewport_width);
        }

        let seg = &mut self.cached_segments[seg_idx];
        seg.copy_text = msg.copy_text();
        seg.update_with_reuse(tl, &self.hl_worker);

        let batch_children: Option<Vec<(String, ToolLines)>> =
            if let Some(ToolOutput::Batch { entries, .. }) = &msg.tool_output {
                Some(
                    entries
                        .iter()
                        .enumerate()
                        .map(|(j, entry)| {
                            let child_id = format!("{tool_id}__{j}");
                            let tl = build_batch_entry_lines(entry, j, self.started_at);
                            (child_id, tl)
                        })
                        .collect(),
                )
            } else {
                None
            };

        if let Some(children) = batch_children {
            let child_prefix = format!("{tool_id}__");
            let msg_index = self.cached_segments[seg_idx].msg_index;
            for (child_id, tl) in children {
                if let Some(cseg_idx) = self
                    .cached_segments
                    .iter()
                    .rposition(|s| s.tool_id.as_deref() == Some(&child_id))
                {
                    self.cached_segments[cseg_idx].update_with_reuse(tl, &self.hl_worker);
                } else {
                    let mut seg = Segment {
                        tool_id: Some(child_id),
                        msg_index,
                        ..Segment::default()
                    };
                    seg.apply_highlight(tl, &self.hl_worker);
                    let insert_pos = self
                        .cached_segments
                        .iter()
                        .rposition(|s| {
                            s.tool_id
                                .as_deref()
                                .is_some_and(|id| id == tool_id || id.starts_with(&child_prefix))
                        })
                        .map_or(seg_idx + 1, |p| p + 1);
                    self.cached_segments.insert(insert_pos, seg);
                }
            }
        }
    }

    fn rebuild_line_cache(&mut self) {
        if self.cached_msg_count == self.messages.len() {
            return;
        }
        for i in self.cached_msg_count..self.messages.len() {
            let msg = &self.messages[i];

            if let DisplayRole::Tool { ref id, status, .. } = msg.role {
                let mut tl = build_tool_lines(msg, status, self.started_at);
                if let Some(ts) = &msg.timestamp {
                    append_timestamp(&mut tl.lines[0], ts, self.viewport_width);
                }
                let id = id.clone();
                let copy_text = msg.copy_text();
                push_spacer_if_needed(&mut self.cached_segments);
                let mut seg = Segment {
                    copy_text,
                    tool_id: Some(id.clone()),
                    msg_index: Some(i),
                    ..Segment::default()
                };
                seg.apply_highlight(tl, &self.hl_worker);
                self.cached_segments.push(seg);

                if let Some(ToolOutput::Batch { entries, .. }) = &msg.tool_output {
                    for (j, entry) in entries.iter().enumerate() {
                        let child_id = format!("{id}__{j}");
                        let tl = build_batch_entry_lines(entry, j, self.started_at);
                        let mut seg = Segment {
                            tool_id: Some(child_id),
                            msg_index: Some(i),
                            ..Segment::default()
                        };
                        seg.apply_highlight(tl, &self.hl_worker);
                        self.cached_segments.push(seg);
                    }
                }
            } else {
                let style = match &msg.role {
                    DisplayRole::User => &USER_STYLE,
                    DisplayRole::Assistant => &ASSISTANT_STYLE,
                    DisplayRole::Thinking => &THINKING_STYLE,
                    DisplayRole::Error => &ERROR_STYLE,
                    DisplayRole::Tool { .. } => unreachable!(),
                };
                let prefix = if msg.plan_path.is_some() {
                    ""
                } else {
                    style.prefix
                };
                let mut lines = if style.use_markdown {
                    text_to_lines(
                        &msg.text,
                        prefix,
                        style.text_style,
                        style.prefix_style,
                        None,
                        self.viewport_width,
                    )
                } else {
                    plain_lines(&msg.text, prefix, style.text_style, style.prefix_style)
                };
                if msg.role == DisplayRole::Thinking {
                    theme::dim_lines(&mut lines);
                }
                if let Some(pp) = &msg.plan_path {
                    let rule = hr_line(self.viewport_width, theme::PLAN_RULE);
                    lines.insert(0, rule.clone());
                    lines.push(rule);
                    lines.push(Line::from(""));
                    lines.push(Line::from(Span::styled(
                        pp.to_owned(),
                        theme::TOOL_PATH.add_modifier(Modifier::BOLD),
                    )));
                }

                let copy_text = msg.text.clone();
                push_spacer_if_needed(&mut self.cached_segments);
                self.cached_segments.push(Segment {
                    lines,
                    copy_text,
                    msg_index: Some(i),
                    ..Segment::default()
                });
            }
        }
        self.cached_msg_count = self.messages.len();
    }
}

fn parse_batch_inner_id(tool_id: &str) -> Option<(&str, usize)> {
    let (batch_id, idx_str) = tool_id.rsplit_once("__")?;
    let idx = idx_str.parse().ok()?;
    Some((batch_id, idx))
}

fn wrapped_line_count(lines: &[Line<'_>], width: u16) -> u16 {
    let w = width as usize;
    if w == 0 {
        return lines.len() as u16;
    }
    lines
        .iter()
        .map(|line| {
            let line_w: usize = line.spans.iter().map(|s| s.content.width()).sum();
            if line_w == 0 {
                1
            } else {
                line_w.div_ceil(w) as u16
            }
        })
        .sum()
}

fn push_spacer_if_needed(segments: &mut Vec<Segment>) {
    if !segments.is_empty() {
        segments.push(Segment {
            lines: vec![Line::default()],
            ..Segment::default()
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::components::scrollbar::SCROLLBAR_THUMB;
    use maki_agent::tools::{BASH_TOOL_NAME, GLOB_TOOL_NAME, QUESTION_TOOL_NAME, WRITE_TOOL_NAME};
    use maki_agent::{
        BatchToolEntry, DiffHunk, DiffLine, DiffSpan, GrepFileEntry, GrepMatch, QuestionAnswer,
        ToolInput, ToolOutput,
    };
    use ratatui::backend::TestBackend;
    use test_case::test_case;

    fn start(id: &str, tool: &'static str) -> ToolStartEvent {
        ToolStartEvent {
            id: id.into(),
            tool,
            summary: id.into(),
            annotation: None,
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

    #[test_case(
        WRITE_TOOL_NAME,
        ToolOutput::WriteCode { path: "src/main.rs".into(), byte_count: 42, lines: vec!["fn main() {}".into()] },
        Some("42 bytes")
        ; "write_bytes"
    )]
    #[test_case(
        "grep",
        grep_output(2),
        Some("2 files")
        ; "grep_files"
    )]
    fn tool_done_sets_annotation(tool: &'static str, output: ToolOutput, expected: Option<&str>) {
        let mut panel = MessagesPanel::new();
        panel.tool_start(start("t1", tool));
        panel.tool_done(ToolDoneEvent {
            id: "t1".into(),
            tool,
            output,
            is_error: false,
        });
        assert_eq!(panel.messages[0].annotation.as_deref(), expected);
    }

    #[test]
    fn tool_done_merges_start_and_output_annotations() {
        let mut panel = MessagesPanel::new();
        let mut event = start("t1", BASH_TOOL_NAME);
        event.annotation = Some("2m timeout".into());
        panel.tool_start(event);
        panel.tool_done(ToolDoneEvent {
            id: "t1".into(),
            tool: BASH_TOOL_NAME,
            output: ToolOutput::Plain("line\n".repeat(200)),
            is_error: false,
        });
        assert_eq!(
            panel.messages[0].annotation.as_deref(),
            Some("2m timeout · 200 lines")
        );
    }

    #[test]
    fn tool_done_keeps_start_annotation_when_output_has_none() {
        let mut panel = MessagesPanel::new();
        let mut event = start("t1", BASH_TOOL_NAME);
        event.annotation = Some("2m timeout".into());
        panel.tool_start(event);
        panel.tool_done(ToolDoneEvent {
            id: "t1".into(),
            tool: BASH_TOOL_NAME,
            output: ToolOutput::Plain("ok".into()),
            is_error: false,
        });
        assert_eq!(panel.messages[0].annotation.as_deref(), Some("2m timeout"));
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

    #[test_case(
        ToolOutput::GlobResult { files: vec!["a.rs".into(), "b.rs".into()] },
        true, false
        ; "glob_with_files_shows_count"
    )]
    #[test_case(
        ToolOutput::GlobResult { files: vec![] },
        false, true
        ; "glob_empty_shows_no_files_found"
    )]
    fn tool_done_glob_result(output: ToolOutput, has_file_count: bool, has_no_files_msg: bool) {
        let mut panel = MessagesPanel::new();
        panel.tool_start(start("t1", GLOB_TOOL_NAME));
        panel.tool_done(ToolDoneEvent {
            id: "t1".into(),
            tool: GLOB_TOOL_NAME,
            output,
            is_error: false,
        });
        let has_annotation = panel.messages[0].annotation.is_some();
        assert_eq!(has_annotation, has_file_count);
        assert_eq!(
            panel.messages[0].text.contains(NO_FILES_FOUND),
            has_no_files_msg
        );
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

    fn render_sel(
        panel: &mut MessagesPanel,
        width: u16,
        height: u16,
        has_selection: bool,
    ) -> ratatui::Terminal<TestBackend> {
        let backend = TestBackend::new(width, height);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal
            .draw(|f| {
                panel.view(f, f.area(), has_selection);
            })
            .unwrap();
        terminal
    }

    fn render(
        panel: &mut MessagesPanel,
        width: u16,
        height: u16,
    ) -> ratatui::Terminal<TestBackend> {
        render_sel(panel, width, height, false)
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
    fn unknown_tool_id_is_noop() {
        let mut panel = MessagesPanel::new();
        panel.tool_output("ghost", "data");
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
        rebuild(&mut panel);
        assert_eq!(msg_status(&panel, "t1"), ToolStatus::InProgress);

        panel.tool_output("t1", "partial");
        assert_eq!(msg_status(&panel, "t1"), ToolStatus::InProgress);

        panel.tool_done(ToolDoneEvent {
            id: "t1".into(),
            tool: "bash",
            output: ToolOutput::Plain("ok".into()),
            is_error: false,
        });
        assert_eq!(panel.in_progress_count, 1);
        assert_eq!(msg_status(&panel, "t1"), ToolStatus::Success);
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
    fn tool_output_updates_target_segment() {
        let mut panel = panel_with_tools(&[("t1", "bash"), ("t2", "bash")]);
        rebuild(&mut panel);
        panel.tool_output("t1", "new output");
        assert!(seg_text(&panel, "t1").contains("new output"));
    }

    #[test]
    fn events_before_cache_built_render_correctly() {
        let mut panel = panel_with_tools(&[("t1", "bash"), ("t2", "bash")]);
        panel.tool_output("t1", "early output");
        panel.tool_done(ToolDoneEvent {
            id: "t2".into(),
            tool: "bash",
            output: ToolOutput::Plain("result".into()),
            is_error: false,
        });
        rebuild(&mut panel);
        assert!(seg_text(&panel, "t1").contains("early output"));
        assert_eq!(msg_status(&panel, "t2"), ToolStatus::Success);
        assert!(seg_text(&panel, "t2").contains("result"));
    }

    fn bash_code_start(panel: &mut MessagesPanel, id: &str, code: &str) {
        panel.tool_start(ToolStartEvent {
            id: id.into(),
            tool: BASH_TOOL_NAME,
            summary: code.into(),
            annotation: None,
            input: Some(ToolInput::Code {
                language: "bash".into(),
                code: code.into(),
            }),
            output: None,
        });
    }

    #[test]
    fn bash_live_output_with_code_input() {
        let mut panel = MessagesPanel::new();
        bash_code_start(&mut panel, "t1", "echo hello");
        rebuild(&mut panel);

        panel.tool_output("t1", "hello");
        let text = seg_text(&panel, "t1");
        assert!(text.contains("echo hello"), "code input preserved");
        assert!(text.contains("hello"), "live output visible");

        panel.tool_output("t1", "hello\nworld");
        let text = seg_text(&panel, "t1");
        assert!(text.contains("echo hello"), "code input still preserved");
        assert!(text.contains("world"), "updated output visible");

        panel.tool_done(ToolDoneEvent {
            id: "t1".into(),
            tool: BASH_TOOL_NAME,
            output: ToolOutput::Plain("done".into()),
            is_error: false,
        });
        let text = seg_text(&panel, "t1");
        assert!(text.contains("echo hello"), "code input after done");
        assert!(text.contains("done"), "final output visible");
        assert_eq!(msg_status(&panel, "t1"), ToolStatus::Success);
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
    fn fail_in_progress_marks_pending_as_error_preserves_completed() {
        let mut panel = panel_with_tools(&[("t1", "bash"), ("t2", "read")]);
        panel.tool_done(ToolDoneEvent {
            id: "t1".into(),
            tool: "bash",
            output: ToolOutput::Plain("ok".into()),
            is_error: false,
        });
        rebuild(&mut panel);

        panel.fail_in_progress();

        assert_eq!(panel.in_progress_count, 0);
        assert!(!panel.is_animating());
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
    fn fail_in_progress_before_cache_built_no_panic() {
        let mut panel = panel_with_tools(&[("t1", "bash"), ("t2", "read")]);
        panel.fail_in_progress();
        assert_eq!(panel.in_progress_count, 0);
        rebuild(&mut panel);
        assert_eq!(msg_status(&panel, "t1"), ToolStatus::Error);
        assert_eq!(msg_status(&panel, "t2"), ToolStatus::Error);
    }

    #[test]
    fn tool_done_after_fail_in_progress_does_not_underflow() {
        let mut panel = panel_with_tools(&[("t1", "bash"), ("t2", "read")]);
        panel.fail_in_progress();
        assert_eq!(panel.in_progress_count, 0);

        panel.tool_done(ToolDoneEvent {
            id: "t1".into(),
            tool: "bash",
            output: ToolOutput::Plain("late".into()),
            is_error: false,
        });
        assert_eq!(panel.in_progress_count, 0);
        assert_eq!(msg_status(&panel, "t1"), ToolStatus::Success);
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
    fn question_tool_renders_with_tool_chrome() {
        let mut panel = MessagesPanel::new();
        panel.tool_start(start("q1", QUESTION_TOOL_NAME));
        panel.tool_done(ToolDoneEvent {
            id: "q1".into(),
            tool: QUESTION_TOOL_NAME,
            output: ToolOutput::QuestionAnswers(vec![
                QuestionAnswer {
                    question: "DB?".into(),
                    answer: "PostgreSQL".into(),
                },
                QuestionAnswer {
                    question: "Framework?".into(),
                    answer: "Axum".into(),
                },
            ]),
            is_error: false,
        });
        rebuild(&mut panel);

        assert_eq!(panel.messages[0].text, "2 questions answered");
        assert!(has_seg(&panel, "q1"));
    }

    #[test]
    fn selection_freezes_viewport_during_auto_scroll() {
        let mut panel = MessagesPanel::new();
        panel.streaming_text.set_buffer(&"a\n".repeat(30));
        render(&mut panel, 80, 10);
        assert!(panel.auto_scroll);
        let scroll_before = panel.scroll_top;
        assert!(scroll_before > 0);

        panel.streaming_text.set_buffer(&"a\n".repeat(35));
        render_sel(&mut panel, 80, 10, true);
        assert_eq!(panel.scroll_top, scroll_before);
        assert!(panel.auto_scroll);

        render_sel(&mut panel, 80, 10, false);
        assert!(panel.scroll_top > scroll_before);
        assert!(panel.auto_scroll);
    }

    fn content_regions(panel: &MessagesPanel) -> Vec<String> {
        let mut regions = Vec::new();
        panel.push_content_regions(&mut regions);
        regions.iter().map(|r| r.raw_text.to_owned()).collect()
    }

    #[test]
    fn copy_text_grep_result_includes_structured_output() {
        let mut panel = MessagesPanel::new();
        panel.tool_start(start("t1", "grep"));
        panel.tool_done(ToolDoneEvent {
            id: "t1".into(),
            tool: "grep",
            output: grep_output(2),
            is_error: false,
        });
        render(&mut panel, 80, 24);
        let regions = content_regions(&panel);
        assert!(
            regions[0].contains("0.rs:"),
            "should contain grep file path"
        );
        assert!(
            regions[0].contains("1.rs:"),
            "should contain all grep files"
        );
    }

    #[test]
    fn copy_text_diff_output_includes_hunks() {
        let mut panel = MessagesPanel::new();
        panel.tool_start(start("t1", "edit"));
        panel.tool_done(ToolDoneEvent {
            id: "t1".into(),
            tool: "edit",
            output: ToolOutput::Diff {
                path: "src/main.rs".into(),
                hunks: vec![DiffHunk {
                    start_line: 1,
                    lines: vec![
                        DiffLine::Removed(vec![DiffSpan::plain("old".into())]),
                        DiffLine::Added(vec![DiffSpan::plain("new".into())]),
                    ],
                }],
                summary: "1 edit".into(),
            },
            is_error: false,
        });
        render(&mut panel, 80, 24);
        let regions = content_regions(&panel);
        assert!(regions[0].contains("- old"), "should contain removed line");
        assert!(regions[0].contains("+ new"), "should contain added line");
    }

    #[test]
    fn copy_text_bash_with_code_input() {
        let mut panel = MessagesPanel::new();
        bash_code_start(&mut panel, "t1", "echo hello");
        panel.tool_done(ToolDoneEvent {
            id: "t1".into(),
            tool: BASH_TOOL_NAME,
            output: ToolOutput::Plain("hello".into()),
            is_error: false,
        });
        render(&mut panel, 80, 24);
        let regions = content_regions(&panel);
        assert!(
            regions[0].contains("echo hello"),
            "should include bash command"
        );
        assert!(regions[0].contains("hello"), "should include output");
    }

    #[test]
    fn copy_text_assistant_preserves_source() {
        let mut panel = MessagesPanel::new();
        let md = "# Heading\n\nSome **bold** text";
        panel.push(DisplayMessage::new(DisplayRole::Assistant, md.into()));
        render(&mut panel, 80, 24);
        let regions = content_regions(&panel);
        assert_eq!(regions[0], md);
    }

    #[test_case(&["short", &"x".repeat(200)], 80, 4 ; "long_line_wraps")]
    #[test_case(&["", "a", ""],                 40, 3 ; "empty_lines_count_as_one")]
    #[test_case(&[&"a".repeat(80)],              80, 1 ; "exactly_width_no_wrap")]
    #[test_case(&[&"a".repeat(81)],              80, 2 ; "one_over_width_wraps")]
    #[test_case(&["hello", "world"],              0, 2 ; "zero_width_returns_line_count")]
    fn wrapped_line_count_cases(input: &[&str], width: u16, expected: u16) {
        let lines: Vec<Line<'static>> = input
            .iter()
            .map(|s| Line::from(Span::raw(s.to_string())))
            .collect();
        assert_eq!(wrapped_line_count(&lines, width), expected);
    }

    #[test]
    fn update_tool_model_sets_annotation() {
        let mut panel = panel_with_tools(&[("t1", "task"), ("t2", "bash")]);
        rebuild(&mut panel);

        panel.update_tool_model("t1", "anthropic/claude-sonnet-4-20250514");

        let msg = &panel.messages[0];
        assert_eq!(
            msg.annotation.as_deref(),
            Some("anthropic/claude-sonnet-4-20250514")
        );
    }

    #[test]
    fn update_tool_model_batch_inner_id() {
        let mut panel = MessagesPanel::new();
        panel.tool_start(ToolStartEvent {
            id: "b1".into(),
            tool: "batch",
            summary: "2 tools".into(),
            annotation: None,
            input: None,
            output: Some(ToolOutput::Batch {
                entries: vec![
                    BatchToolEntry {
                        tool: "task".into(),
                        summary: "research".into(),
                        status: BatchToolStatus::InProgress,
                        input: None,
                        output: None,
                        annotation: None,
                    },
                    BatchToolEntry {
                        tool: "read".into(),
                        summary: "file.rs".into(),
                        status: BatchToolStatus::Pending,
                        input: None,
                        output: None,
                        annotation: None,
                    },
                ],
                text: String::new(),
            }),
        });
        rebuild(&mut panel);

        panel.update_tool_model("b1__0", "anthropic/claude-haiku-4-20250414");

        let batch_output = panel.messages[0].tool_output.as_ref().unwrap();
        let ToolOutput::Batch { entries, .. } = batch_output else {
            panic!("expected Batch");
        };
        assert_eq!(
            entries[0].annotation.as_deref(),
            Some("anthropic/claude-haiku-4-20250414")
        );
        assert!(entries[1].annotation.is_none());
    }

    #[test]
    fn update_tool_summary_batch_inner_id() {
        let mut panel = MessagesPanel::new();
        panel.tool_start(ToolStartEvent {
            id: "b1".into(),
            tool: "batch",
            summary: "2 tools".into(),
            annotation: None,
            input: None,
            output: Some(ToolOutput::Batch {
                entries: vec![BatchToolEntry {
                    tool: "task".into(),
                    summary: "old".into(),
                    status: BatchToolStatus::InProgress,
                    input: None,
                    output: None,
                    annotation: None,
                }],
                text: String::new(),
            }),
        });
        rebuild(&mut panel);

        panel.update_tool_summary("b1__0", "new name");

        let ToolOutput::Batch { entries, .. } = panel.messages[0].tool_output.as_ref().unwrap()
        else {
            panic!("expected Batch");
        };
        assert_eq!(entries[0].summary, "new name");
    }

    #[test]
    fn scroll_clamps_to_max_scroll() {
        let mut panel = MessagesPanel::new();
        panel.streaming_text.set_buffer(&"a\n".repeat(15));
        render(&mut panel, 80, 10);
        let max = panel.max_scroll();

        panel.scroll(-3);
        assert_eq!(panel.scroll_top, max);
    }

    #[test]
    fn parse_batch_inner_id_cases() {
        assert_eq!(parse_batch_inner_id("b1__0"), Some(("b1", 0)));
        assert_eq!(parse_batch_inner_id("b1__2"), Some(("b1", 2)));
        assert_eq!(parse_batch_inner_id("a__b__1"), Some(("a__b", 1)));
        assert_eq!(parse_batch_inner_id("no_separator"), None);
        assert_eq!(parse_batch_inner_id("b1__notnum"), None);
    }

    #[test]
    fn tool_pending_shows_in_progress() {
        let mut panel = MessagesPanel::new();
        panel.tool_pending("t1".into(), "bash");
        assert_eq!(panel.messages.len(), 1);
        assert_eq!(msg_status(&panel, "t1"), ToolStatus::InProgress);
        assert_eq!(panel.in_progress_count, 1);
        assert!(panel.messages[0].text.is_empty());
    }

    #[test]
    fn tool_pending_unknown_name_ignored() {
        let mut panel = MessagesPanel::new();
        panel.tool_pending("t1".into(), "nonexistent_tool");
        assert_eq!(panel.messages.len(), 0);
        assert_eq!(panel.in_progress_count, 0);
    }

    #[test]
    fn tool_start_upgrades_pending_in_place() {
        let mut panel = MessagesPanel::new();
        panel.tool_pending("t1".into(), "bash");
        assert_eq!(panel.messages.len(), 1);
        assert_eq!(panel.in_progress_count, 1);

        let mut event = start("t1", BASH_TOOL_NAME);
        event.annotation = Some("note".into());
        panel.tool_start(event);

        assert_eq!(panel.messages.len(), 1);
        assert_eq!(panel.in_progress_count, 1);
        assert_eq!(panel.messages[0].text, "t1");
        assert_eq!(panel.messages[0].annotation.as_deref(), Some("note"));
    }
}

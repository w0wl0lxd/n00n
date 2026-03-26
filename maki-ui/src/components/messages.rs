use super::streaming_content::StreamingContent;
use super::tool_display::{
    HighlightRequest, ToolKind, ToolLines, append_annotation, append_right_info, assistant_style,
    build_batch_entry_lines, build_tool_lines, done_style, error_style, format_timestamp_now,
    thinking_style, tool_output_annotation, truncate_to_header, user_style,
};
use super::{DisplayMessage, DisplayRole, ToolRole, ToolStatus, apply_scroll_delta};
use crate::animation::spinner_str;
use crate::components::keybindings::key;
use crate::markdown::{
    CodeBlockRange, hr_line, plain_lines, text_to_lines_with_ranges, truncate_output,
};
use crate::render_worker::RenderWorker;
use crate::selection::{self, LineBreaks, ScreenSelection, Selection};
use crate::splash::{ColorTransition, Splash};
use crate::theme;
use maki_config::{ToolOutputLines, UiConfig};

use std::collections::HashSet;
use std::sync::Arc;
use std::time::Instant;

use maki_agent::tools::{ToolCall, WEBFETCH_TOOL_NAME};
use maki_agent::{
    BatchToolEntry, BatchToolStatus, NO_FILES_FOUND, ToolDoneEvent, ToolOutput, ToolStartEvent,
};
use ratatui::Frame;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Widget, Wrap};

use super::scrollbar::render_vertical_scrollbar;

#[derive(Default, PartialEq, Eq)]
struct HighlightKey {
    has_output: bool,
    expanded: bool,
}

impl HighlightKey {
    fn from_request(hl: Option<&HighlightRequest>) -> Self {
        Self {
            has_output: hl.is_some_and(|h| h.output.is_some()),
            expanded: hl.is_some_and(|h| h.expanded),
        }
    }
}

/// Clipboard copy strategy:
/// - Fully-selected segments → `copy_text` (raw markdown/structured output, lossless)
/// - Partially-selected segments → cell scraping from a temp buffer
/// - Partial + code blocks → `extract_with_fences` reconstructs ``` fences
///   for fully-covered blocks; partially-covered blocks fall back to scraping
#[derive(Default)]
struct Segment {
    lines: Vec<Line<'static>>,
    copy_text: String,
    /// Populated only for markdown segments (assistant/user). Empty for tool segments
    /// because their code blocks are rendered differently (│-prefixed, no fences).
    code_block_ranges: Vec<CodeBlockRange>,
    tool_id: Option<String>,
    msg_index: Option<usize>,
    cached_height: Option<(u16, u16)>,
    pending_highlight: Option<u64>,
    highlight_range: Option<(usize, usize)>,
    highlight_key: HighlightKey,
    spinner_lines: Vec<usize>,
    content_indent: &'static str,
    has_truncation: bool,
}

impl Segment {
    fn reuse_highlight(
        &self,
        key: &HighlightKey,
        new_range: (usize, usize),
    ) -> Option<Vec<Line<'static>>> {
        if self.pending_highlight.is_some() || self.highlight_key != *key {
            return None;
        }
        let (s, e) = self.highlight_range?;
        if s > e || e > self.lines.len() {
            return None;
        }
        if (e - s) != (new_range.1 - new_range.0) {
            return None;
        }
        Some(self.lines[s..e].to_vec())
    }

    fn apply_highlight(&mut self, tl: ToolLines, worker: &RenderWorker) {
        self.pending_highlight = tl.send_highlight(worker);
        self.highlight_range = tl.highlight.as_ref().map(|h| h.range);
        self.highlight_key = HighlightKey::from_request(tl.highlight.as_ref());
        self.spinner_lines = tl.spinner_lines;
        self.content_indent = tl.content_indent;
        self.has_truncation = tl.has_truncation;
        self.lines = tl.lines;
        self.cached_height = None;
    }

    fn update_with_reuse(&mut self, mut tl: ToolLines, worker: &RenderWorker) {
        let key = HighlightKey::from_request(tl.highlight.as_ref());
        let reused = tl.highlight.as_ref().and_then(|req| {
            let hl_lines = self.reuse_highlight(&key, req.range)?;
            let (s, _) = req.range;
            let new_end = s + hl_lines.len();
            tl.lines.splice(s..req.range.1, hl_lines);
            Some((s, new_end))
        });
        self.cached_height = None;
        self.has_truncation = tl.has_truncation;
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
    streaming_thinking: StreamingContent,
    streaming_text: StreamingContent,
    started_at: Instant,
    in_progress_count: usize,
    scroll_top: u16,
    auto_scroll: bool,
    viewport_height: u16,
    viewport_width: u16,
    cached_segments: Vec<Segment>,
    cached_msg_count: usize,
    hl_worker: RenderWorker,
    segment_heights: Vec<u16>,
    theme_generation: u64,
    highlight_segment: Option<usize>,
    idle_splash: Splash,
    accent: ColorTransition,
    expanded_tools: HashSet<String>,
    tool_output_lines: ToolOutputLines,
}

impl MessagesPanel {
    pub fn new(ui_config: UiConfig) -> Self {
        let thinking = thinking_style();
        let assistant = assistant_style();
        let ms = ui_config.typewriter_ms_per_char;
        Self {
            messages: Vec::new(),
            streaming_thinking: StreamingContent::new(
                thinking.prefix,
                thinking.text_style,
                thinking.prefix_style,
                ms,
            ),
            streaming_text: StreamingContent::new(
                assistant.prefix,
                assistant.text_style,
                assistant.prefix_style,
                ms,
            ),
            started_at: Instant::now(),
            in_progress_count: 0,
            scroll_top: u16::MAX,
            auto_scroll: true,
            viewport_height: 24,
            viewport_width: 80,
            cached_segments: Vec::new(),
            cached_msg_count: 0,
            hl_worker: RenderWorker::new(),
            segment_heights: Vec::new(),
            theme_generation: theme::generation(),
            highlight_segment: None,
            idle_splash: Splash::new(ui_config.splash_animation),
            accent: ColorTransition::new(theme::current().mode_build),
            expanded_tools: HashSet::new(),
            tool_output_lines: ui_config.tool_output_lines,
        }
    }

    pub fn push(&mut self, msg: DisplayMessage) {
        self.messages.push(msg);
    }

    pub fn load_messages(&mut self, msgs: Vec<DisplayMessage>) {
        self.in_progress_count = msgs
            .iter()
            .filter(
                |m| matches!(&m.role, DisplayRole::Tool(t) if t.status == ToolStatus::InProgress),
            )
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
        let role = DisplayRole::Tool(Box::new(ToolRole {
            id,
            status: ToolStatus::InProgress,
            name,
        }));
        let mut msg = DisplayMessage::new(role, String::new());
        msg.timestamp = Some(format_timestamp_now());
        self.messages.push(msg);
        self.in_progress_count += 1;
    }

    pub fn tool_start(&mut self, event: ToolStartEvent) {
        if let Some(msg) = self
            .messages
            .iter_mut()
            .rfind(|m| matches!(&m.role, DisplayRole::Tool(t) if t.id == event.id))
        {
            if let DisplayRole::Tool(t) = &mut msg.role {
                t.name = event.tool;
            }
            msg.text = event.summary;
            msg.tool_input = event.input.map(Arc::new);
            msg.tool_output = event.output.map(Arc::new);
            msg.annotation = event.annotation;
            self.rebuild_tool_segment(&event.id);
            return;
        }
        self.flush();
        self.messages.push(DisplayMessage {
            role: DisplayRole::Tool(Box::new(ToolRole {
                id: event.id,
                status: ToolStatus::InProgress,
                name: event.tool,
            })),
            text: event.summary,
            tool_input: event.input.map(Arc::new),
            tool_output: event.output.map(Arc::new),
            live_output: None,
            annotation: event.annotation,
            plan_path: None,
            timestamp: Some(format_timestamp_now()),
            turn_usage: None,
            truncated_lines: 0,
        });
        self.in_progress_count += 1;
    }

    pub fn tool_output(&mut self, tool_id: &str, content: &str) {
        let Some(msg) = self
            .messages
            .iter_mut()
            .rfind(|m| matches!(&m.role, DisplayRole::Tool(t) if t.id == tool_id))
        else {
            return;
        };
        let tool_name = msg.role.tool_name().unwrap_or("");
        let limits = ToolKind::from_name(tool_name).output_limits(&self.tool_output_lines);
        truncate_to_header(&mut msg.text);
        let truncated = truncate_output(content, limits.max_lines, limits.keep);
        msg.truncated_lines = truncated.skipped;
        msg.text.push('\n');
        msg.text.push_str(&truncated.kept);
        msg.live_output = Some(content.to_owned());
        self.rebuild_tool_segment(tool_id);
    }

    pub fn tool_done(&mut self, event: ToolDoneEvent) {
        let Some(msg) = self
            .messages
            .iter_mut()
            .rfind(|m| matches!(&m.role, DisplayRole::Tool(t) if t.id == event.id))
        else {
            return;
        };
        let was_in_progress =
            matches!(&msg.role, DisplayRole::Tool(t) if t.status == ToolStatus::InProgress);
        if let DisplayRole::Tool(t) = &mut msg.role {
            t.status = if event.is_error {
                ToolStatus::Error
            } else {
                ToolStatus::Success
            };
        }
        truncate_to_header(&mut msg.text);
        let done_annotation =
            tool_output_annotation(&event.output, ToolKind::from_name(event.tool));
        if let Some(suffix) = &done_annotation {
            append_annotation(&mut msg.annotation, suffix);
        }

        match &event.output {
            ToolOutput::Plain(text) | ToolOutput::ReadDir { text, .. } => {
                if !matches!(event.tool, WEBFETCH_TOOL_NAME) {
                    let limits =
                        ToolKind::from_name(event.tool).output_limits(&self.tool_output_lines);
                    let tr = truncate_output(text, limits.max_lines, limits.keep);
                    msg.truncated_lines = tr.skipped;
                    if !tr.kept.is_empty() {
                        msg.text = format!("{}\n{}", msg.text, tr.kept);
                    }
                }
            }
            ToolOutput::QuestionAnswers(pairs) => {
                let n = pairs.len();
                msg.text = format!("{n} question{} answered", if n == 1 { "" } else { "s" });
            }
            output @ ToolOutput::GlobResult { .. } => {
                if output.is_empty_result() {
                    msg.text = format!("{}\n{NO_FILES_FOUND}", msg.text);
                } else {
                    let display = output.as_display_text();
                    let limits =
                        ToolKind::from_name(event.tool).output_limits(&self.tool_output_lines);
                    let tr = truncate_output(&display, limits.max_lines, limits.keep);
                    msg.truncated_lines = tr.skipped;
                    msg.text = format!("{}\n{}", msg.text, tr.kept);
                }
            }
            ToolOutput::GrepResult { entries } => {
                if entries.is_empty() {
                    msg.text = format!("{}\n{NO_FILES_FOUND}", msg.text);
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
        if let ToolOutput::Batch {
            entries: new_entries,
            text,
        } = &event.output
            && let Some(arc) = &mut msg.tool_output
            && let ToolOutput::Batch {
                entries: existing,
                text: existing_text,
            } = Arc::make_mut(arc)
        {
            for (existing, new) in existing.iter_mut().zip(new_entries) {
                existing.status = new.status;
                existing.output = new.output.clone();
            }
            *existing_text = text.clone();
        } else {
            msg.tool_output = Some(Arc::new(event.output));
        }
        msg.live_output = None;
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
            .rfind(|m| matches!(&m.role, DisplayRole::Tool(t) if t.id == batch_id))
        else {
            return;
        };
        if let Some(arc) = &mut msg.tool_output
            && let ToolOutput::Batch { entries, .. } = Arc::make_mut(arc)
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

    pub fn set_turn_usage_on_last_tool(&mut self, usage: String) {
        let Some(idx) = self
            .messages
            .iter()
            .rposition(|m| matches!(m.role, DisplayRole::Tool(_)))
        else {
            return;
        };
        self.messages[idx].turn_usage = Some(usage);
        let DisplayRole::Tool(t) = &self.messages[idx].role else {
            unreachable!()
        };
        let id = t.id.clone();
        self.rebuild_tool_segment(&id);
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
                .rfind(|m| matches!(&m.role, DisplayRole::Tool(t) if t.id == batch_id))
            else {
                return;
            };
            if let Some(arc) = &mut msg.tool_output
                && let ToolOutput::Batch { entries, .. } = Arc::make_mut(arc)
                && let Some(entry) = entries.get_mut(idx)
            {
                update_entry(entry);
            }
            rebuild_id = batch_id.to_owned();
        } else {
            let Some(msg) = self
                .messages
                .iter_mut()
                .rfind(|m| matches!(&m.role, DisplayRole::Tool(t) if t.id == tool_id))
            else {
                return;
            };
            update_msg(msg);
            rebuild_id = tool_id.to_owned();
        }
        self.rebuild_tool_segment(&rebuild_id);
    }

    pub fn stream_reset(&mut self) {
        self.streaming_thinking.clear();
        self.streaming_text.clear();
        self.fail_in_progress();
    }

    pub fn fail_in_progress(&mut self) {
        let mut batch_ids = Vec::new();
        for msg in &mut self.messages {
            if let DisplayRole::Tool(t) = &mut msg.role
                && t.status == ToolStatus::InProgress
            {
                t.status = ToolStatus::Error;
                let id = t.id.clone();
                if let Some(arc) = &mut msg.tool_output
                    && let ToolOutput::Batch { entries, .. } = Arc::make_mut(arc)
                {
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
                    let mut tl = build_tool_lines(
                        msg,
                        ToolStatus::Error,
                        self.started_at,
                        self.viewport_width,
                        false,
                        &self.tool_output_lines,
                    );
                    if let Some(ts) = &msg.timestamp
                        && !tl.lines.is_empty()
                    {
                        append_right_info(
                            &mut tl.lines[0],
                            msg.turn_usage.as_deref(),
                            Some(ts),
                            self.viewport_width,
                        );
                    }
                    seg.apply_highlight(tl, &self.hl_worker);
                }
            }
        }
        for batch_id in &batch_ids {
            if let Some(msg) = self
                .messages
                .iter()
                .rfind(|m| matches!(&m.role, DisplayRole::Tool(t) if t.id == *batch_id))
                && let Some(ToolOutput::Batch { entries, .. }) = msg.tool_output.as_deref()
            {
                let child_prefix = format!("{batch_id}__");
                for (j, entry) in entries.iter().enumerate() {
                    let child_id = format!("{batch_id}__{j}");
                    let tl = build_batch_entry_lines(
                        entry,
                        j,
                        self.started_at,
                        self.viewport_width,
                        false,
                        &self.tool_output_lines,
                    );
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

    #[cfg(test)]
    pub fn last_message_role(&self) -> Option<&DisplayRole> {
        self.messages.last().map(|m| &m.role)
    }

    pub fn flush(&mut self) {
        self.flush_thinking();
        if !self.streaming_text.is_empty() {
            self.messages.push(DisplayMessage::new(
                DisplayRole::Assistant,
                self.streaming_text.take_all(),
            ));
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

    pub fn scroll_to_segment(&mut self, segment_index: usize) {
        let offset = self
            .segment_heights
            .iter()
            .take(segment_index)
            .map(|&h| h as u32)
            .sum::<u32>()
            .min(u16::MAX as u32) as u16;
        self.scroll_top = offset.min(self.max_scroll());
        self.auto_scroll = false;
    }

    pub fn restore_scroll(&mut self, scroll_top: u16, auto_scroll: bool) {
        self.scroll_top = scroll_top;
        self.auto_scroll = auto_scroll;
    }

    pub fn set_highlight_segment(&mut self, idx: Option<usize>) {
        self.highlight_segment = idx;
    }

    pub fn half_page(&self) -> i32 {
        self.viewport_height as i32 / 2
    }

    pub fn set_accent(&mut self, color: ratatui::style::Color) {
        self.accent.set(color);
    }

    pub fn toggle_expansion_at(&mut self, row: u16, area: Rect) -> bool {
        if area.height == 0 {
            return false;
        }
        let doc_row = (row.saturating_sub(area.y)) as u32 + self.scroll_top as u32;
        let mut cumulative: u32 = 0;
        let idx = self.segment_heights.iter().position(|&h| {
            cumulative += h as u32;
            doc_row < cumulative
        });
        let Some(idx) = idx else { return false };
        if idx >= self.cached_segments.len() {
            return false;
        }
        let seg = &self.cached_segments[idx];
        let Some(tool_id) = seg.tool_id.as_deref() else {
            return false;
        };
        let is_expanded = self.expanded_tools.contains(tool_id);
        if !seg.has_truncation && !is_expanded {
            return false;
        }
        let tool_id = tool_id.to_owned();
        if !self.expanded_tools.remove(&tool_id) {
            self.expanded_tools.insert(tool_id.clone());
        }
        let rebuild_id = parse_batch_inner_id(&tool_id).map_or(&*tool_id, |(batch_id, _)| batch_id);
        self.rebuild_tool_segment(rebuild_id);
        true
    }

    pub fn is_animating(&self) -> bool {
        self.in_progress_count > 0
            || self.streaming_thinking.is_animating()
            || self.streaming_text.is_animating()
            || self.show_idle_splash()
            || self.accent.is_animating()
    }

    fn show_idle_splash(&self) -> bool {
        self.messages.is_empty()
            && self.streaming_thinking.is_empty()
            && self.streaming_text.is_empty()
    }

    /// `has_selection` freezes auto-scroll so the viewport doesn't jump
    /// while the user is dragging a selection during streaming.
    pub fn view(&mut self, frame: &mut Frame, area: Rect, has_selection: bool) {
        self.viewport_height = area.height;

        if self.show_idle_splash() {
            let accent = self.accent.resolve();
            self.idle_splash.render(area, frame.buffer_mut(), accent);
            return;
        }

        let width = area.width.saturating_sub(1);
        let theme_gen = theme::generation();
        if self.viewport_width != width || self.theme_generation != theme_gen {
            self.viewport_width = width;
            self.theme_generation = theme_gen;
            self.cached_msg_count = 0;
            self.cached_segments.clear();
            let thinking = thinking_style();
            let assistant = assistant_style();
            self.streaming_thinking.set_style(
                thinking.prefix,
                thinking.text_style,
                thinking.prefix_style,
            );
            self.streaming_text.set_style(
                assistant.prefix,
                assistant.text_style,
                assistant.prefix_style,
            );
        }
        self.drain_highlights();
        self.rebuild_line_cache();
        if self.in_progress_count > 0 {
            self.update_spinners();
        }

        self.segment_heights.clear();
        for seg in &mut self.cached_segments {
            if let Some((w, h)) = seg.cached_height
                && w == width
            {
                self.segment_heights.push(h);
            } else {
                let h = wrapped_line_count(&seg.lines, width);
                seg.cached_height = Some((width, h));
                self.segment_heights.push(h);
            }
        }

        let cached_count = self.segment_heights.len();
        let spacer_lines: [Line<'static>; 1] = [Line::default()];
        for sc in [&mut self.streaming_thinking, &mut self.streaming_text] {
            if sc.is_empty() {
                continue;
            }
            let lines = sc.render_lines(width);
            if cached_count > 0 || self.segment_heights.len() > cached_count {
                self.segment_heights.push(1);
            }
            self.segment_heights.push(wrapped_line_count(lines, width));
        }

        let total_lines: u16 = self
            .segment_heights
            .iter()
            .map(|&h| h as u32)
            .sum::<u32>()
            .min(u16::MAX as u32) as u16;
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

        let viewport = Rect::new(area.x, area.y, width, area.height);
        let mut cursor = RenderCursor::new(self.scroll_top, viewport);

        for (i, seg) in self.cached_segments.iter().enumerate() {
            if cursor.past_bottom() {
                break;
            }
            let h = self.segment_heights[i];
            let highlight = self.highlight_segment == Some(i);
            let style = seg.tool_id.as_ref().map(|_| theme::current().tool_bg);
            cursor.render(&seg.lines, h, style, highlight, frame);
        }

        let mut height_idx = self.cached_segments.len();
        for sc in [&self.streaming_thinking, &self.streaming_text] {
            if sc.is_empty() || height_idx >= self.segment_heights.len() || cursor.past_bottom() {
                continue;
            }
            if cached_count > 0 || height_idx > cached_count {
                let h = self.segment_heights[height_idx];
                height_idx += 1;
                cursor.render(&spacer_lines, h, None, false, frame);
            }
            if height_idx < self.segment_heights.len() {
                let h = self.segment_heights[height_idx];
                height_idx += 1;
                cursor.render(sc.cached_lines(), h, None, false, frame);
            }
        }

        if total_lines > area.height {
            render_vertical_scrollbar(frame, area, total_lines, self.scroll_top);
        }
    }

    fn max_scroll(&self) -> u16 {
        let total = self
            .segment_heights
            .iter()
            .map(|&h| h as u32)
            .sum::<u32>()
            .min(u16::MAX as u32) as u16;
        total.saturating_sub(self.viewport_height)
    }

    pub fn scroll_top(&self) -> u16 {
        self.scroll_top
    }

    pub fn segment_heights(&self) -> &[u16] {
        &self.segment_heights
    }

    pub fn segment_copy_texts(&self) -> Vec<&str> {
        self.cached_segments
            .iter()
            .map(|s| s.copy_text.as_str())
            .collect()
    }

    pub fn extract_selection_text(&self, sel: &Selection, msg_area: Rect) -> String {
        let (doc_start, doc_end) = sel.normalized();
        let width = self.viewport_width;
        let mut out = String::new();
        let mut doc_row: u32 = 0;

        for (i, &h) in self.segment_heights.iter().enumerate() {
            let seg_start = doc_row;
            let seg_end = doc_row + h as u32;
            doc_row = seg_end;

            if seg_end <= doc_start.row || seg_start > doc_end.row {
                continue;
            }

            let fully_enclosed = selection::range_covers(
                doc_start,
                doc_end,
                seg_start,
                seg_end.saturating_sub(1),
                msg_area.x,
                msg_area.x + msg_area.width.saturating_sub(1),
            );

            if !out.is_empty() {
                out.push('\n');
            }

            let seg = match self.cached_segments.get(i) {
                Some(s) => s,
                None => continue,
            };

            if fully_enclosed && !seg.copy_text.is_empty() {
                out.push_str(&seg.copy_text);
                continue;
            }

            if seg.lines.is_empty() {
                continue;
            }

            let tmp_area = Rect::new(0, 0, width, h);
            let mut tmp = Buffer::empty(tmp_area);
            Paragraph::new(seg.lines.to_vec())
                .wrap(Wrap { trim: false })
                .render(tmp_area, &mut tmp);

            let rel_start = doc_start.row.saturating_sub(seg_start) as u16;
            let rel_end = ((doc_end.row + 1).saturating_sub(seg_start) as u16).min(h);

            let start_col = if seg_start > doc_start.row {
                0
            } else {
                doc_start.col.saturating_sub(msg_area.x)
            };
            let end_col = if seg_end < doc_end.row + 1 {
                width.saturating_sub(1)
            } else {
                doc_end.col.saturating_sub(msg_area.x)
            };

            let ss = ScreenSelection {
                start_row: rel_start,
                start_col,
                end_row: rel_end.saturating_sub(1),
                end_col,
            };

            let breaks = LineBreaks::from_lines(&seg.lines, width);

            // Tool segments never populate code_block_ranges, so is_empty() covers them
            if seg.code_block_ranges.is_empty() {
                selection::append_rows(&tmp, tmp_area, &ss, rel_start, rel_end, &mut out, &breaks);
                continue;
            }

            extract_with_fences(
                &tmp,
                tmp_area,
                &ss,
                rel_start,
                rel_end,
                &breaks,
                &seg.code_block_ranges,
                &mut out,
            );
        }
        out
    }

    fn flush_thinking(&mut self) {
        if !self.streaming_thinking.is_empty() {
            self.messages.push(DisplayMessage::new(
                DisplayRole::Thinking,
                self.streaming_thinking.take_all(),
            ));
        }
    }

    fn update_spinners(&mut self) {
        let spinner_span = Span::styled(
            spinner_str(self.started_at.elapsed().as_millis()),
            theme::current().spinner,
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
            .rfind(|m| matches!(&m.role, DisplayRole::Tool(t) if t.id == tool_id))
        else {
            return;
        };
        let DisplayRole::Tool(t) = &msg.role else {
            unreachable!()
        };
        let status = t.status;
        let Some(seg_idx) = self
            .cached_segments
            .iter()
            .rposition(|s| s.tool_id.as_deref() == Some(tool_id))
        else {
            return;
        };

        let expanded = self.expanded_tools.contains(tool_id);
        let mut tl = build_tool_lines(
            msg,
            status,
            self.started_at,
            self.viewport_width,
            expanded,
            &self.tool_output_lines,
        );
        if let Some(ts) = &msg.timestamp
            && !tl.lines.is_empty()
        {
            append_right_info(
                &mut tl.lines[0],
                msg.turn_usage.as_deref(),
                Some(ts),
                self.viewport_width,
            );
        }

        let seg = &mut self.cached_segments[seg_idx];
        seg.copy_text = tl.copy_text.clone();
        seg.update_with_reuse(tl, &self.hl_worker);

        if let Some(ToolOutput::Batch { entries, .. }) = msg.tool_output.as_deref() {
            let children: Vec<_> = entries
                .iter()
                .enumerate()
                .map(|(j, entry)| {
                    let child_id = format!("{tool_id}__{j}");
                    let child_expanded = self.expanded_tools.contains(&child_id);
                    let tl = build_batch_entry_lines(
                        entry,
                        j,
                        self.started_at,
                        self.viewport_width,
                        child_expanded,
                        &self.tool_output_lines,
                    );
                    let copy = tl.copy_text.clone();
                    (child_id, copy, tl)
                })
                .collect();
            let child_prefix = format!("{tool_id}__");
            let msg_index = self.cached_segments[seg_idx].msg_index;
            for (child_id, copy, tl) in children {
                if let Some(cseg_idx) = self
                    .cached_segments
                    .iter()
                    .rposition(|s| s.tool_id.as_deref() == Some(&child_id))
                {
                    self.cached_segments[cseg_idx].copy_text = copy;
                    self.cached_segments[cseg_idx].update_with_reuse(tl, &self.hl_worker);
                } else {
                    let mut seg = Segment {
                        copy_text: copy,
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

            if let DisplayRole::Tool(t) = &msg.role {
                let expanded = self.expanded_tools.contains(&t.id);
                let status = t.status;
                let mut tl = build_tool_lines(
                    msg,
                    status,
                    self.started_at,
                    self.viewport_width,
                    expanded,
                    &self.tool_output_lines,
                );
                if let Some(ts) = &msg.timestamp
                    && !tl.lines.is_empty()
                {
                    append_right_info(
                        &mut tl.lines[0],
                        msg.turn_usage.as_deref(),
                        Some(ts),
                        self.viewport_width,
                    );
                }
                let id = t.id.clone();
                let copy_text = tl.copy_text.clone();
                push_spacer_if_needed(&mut self.cached_segments);
                let mut seg = Segment {
                    copy_text,
                    tool_id: Some(id.clone()),
                    msg_index: Some(i),
                    ..Segment::default()
                };
                seg.apply_highlight(tl, &self.hl_worker);
                self.cached_segments.push(seg);

                if let Some(ToolOutput::Batch { entries, .. }) = msg.tool_output.as_deref() {
                    for (j, entry) in entries.iter().enumerate() {
                        let child_id = format!("{id}__{j}");
                        let child_expanded = self.expanded_tools.contains(&child_id);
                        let tl = build_batch_entry_lines(
                            entry,
                            j,
                            self.started_at,
                            self.viewport_width,
                            child_expanded,
                            &self.tool_output_lines,
                        );
                        let mut seg = Segment {
                            copy_text: tl.copy_text.clone(),
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
                    DisplayRole::User => user_style(),
                    DisplayRole::Assistant => assistant_style(),
                    DisplayRole::Thinking => thinking_style(),
                    DisplayRole::Error => error_style(),
                    DisplayRole::Done => done_style(),
                    DisplayRole::Tool(_) => unreachable!(),
                };
                let prefix = if msg.plan_path.is_some() {
                    ""
                } else {
                    style.prefix
                };
                let mut code_block_ranges = Vec::new();
                let mut lines = if style.use_markdown {
                    text_to_lines_with_ranges(
                        &msg.text,
                        prefix,
                        style.text_style,
                        style.prefix_style,
                        None,
                        self.viewport_width,
                        Some(&mut code_block_ranges),
                    )
                } else {
                    plain_lines(&msg.text, prefix, style.text_style, style.prefix_style)
                };
                if let Some(pp) = &msg.plan_path {
                    if !msg.text.is_empty() {
                        let rule = hr_line(self.viewport_width, theme::current().plan_rule);
                        lines.insert(0, rule.clone());
                        lines.push(rule);
                    } else {
                        lines.clear();
                    }
                    if !msg.text.is_empty() {
                        lines.push(Line::from(""));
                    }
                    lines.push(Line::from(Span::styled(
                        pp.to_owned(),
                        theme::current().plan_path,
                    )));
                    lines.push(Line::from(Span::styled(
                        format!(
                            "{} to open in editor ($VISUAL / $EDITOR)",
                            key::OPEN_EDITOR.label
                        ),
                        theme::current().tool_dim,
                    )));
                }

                let copy_text = format!("{prefix}{}", msg.text);
                push_spacer_if_needed(&mut self.cached_segments);
                self.cached_segments.push(Segment {
                    lines,
                    copy_text,
                    code_block_ranges,
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

#[allow(clippy::too_many_arguments)]
fn extract_with_fences(
    buf: &Buffer,
    area: Rect,
    ss: &ScreenSelection,
    rel_start: u16,
    rel_end: u16,
    breaks: &LineBreaks,
    ranges: &[CodeBlockRange],
    out: &mut String,
) {
    let mut cursor = rel_start;
    let full_width = area.width.saturating_sub(1);

    let scrape_to = |out: &mut String, start: u16, end: u16, sc: u16, ec: u16| {
        let chunk_ss = ScreenSelection {
            start_row: start,
            start_col: sc,
            end_row: end.saturating_sub(1),
            end_col: ec,
        };
        selection::append_rows(buf, area, &chunk_ss, start, end, out, breaks);
    };

    let start_col_for = |row: u16| -> u16 { if row == rel_start { ss.start_col } else { 0 } };

    for cb in ranges {
        if cb.end_line < rel_start || cb.start_line >= rel_end {
            continue;
        }
        let fully_covered = rel_start <= cb.start_line && cb.end_line < rel_end;

        if cursor < cb.start_line {
            let chunk_end = if fully_covered {
                cb.start_line
            } else {
                cb.start_line.min(rel_end)
            };
            scrape_to(out, cursor, chunk_end, start_col_for(cursor), full_width);
        }

        if fully_covered {
            let trimmed = out.trim_end_matches('\n').len();
            out.truncate(trimmed);
            out.push_str("\n\n```");
            out.push_str(&cb.lang);
            out.push('\n');
            scrape_to(out, cb.start_line, cb.end_line + 1, 0, full_width);
            if !out.ends_with('\n') {
                out.push('\n');
            }
            out.push_str("```\n\n");
            cursor = cb.end_line + 1;
        } else {
            let block_start = cb.start_line.max(rel_start);
            let block_end = (cb.end_line + 1).min(rel_end);
            let ec = if block_end == rel_end {
                ss.end_col
            } else {
                full_width
            };
            if !out.is_empty() && !out.ends_with('\n') {
                out.push_str("\n\n");
            }
            scrape_to(out, block_start, block_end, start_col_for(block_start), ec);
            cursor = block_end;
        }
    }

    if cursor < rel_end {
        if !out.is_empty() && !out.ends_with('\n') {
            out.push_str("\n\n");
        }
        scrape_to(out, cursor, rel_end, start_col_for(cursor), ss.end_col);
    }
}

struct RenderCursor {
    skip: u16,
    y: u16,
    bottom: u16,
    viewport: Rect,
}

impl RenderCursor {
    fn new(scroll_top: u16, viewport: Rect) -> Self {
        Self {
            skip: scroll_top,
            y: viewport.y,
            bottom: viewport.y + viewport.height,
            viewport,
        }
    }

    fn past_bottom(&self) -> bool {
        self.y >= self.bottom
    }

    fn render(
        &mut self,
        lines: &[Line<'static>],
        h: u16,
        style: Option<ratatui::style::Style>,
        highlight: bool,
        frame: &mut Frame,
    ) {
        if self.skip >= h {
            self.skip -= h;
            return;
        }
        if self.y >= self.bottom {
            return;
        }
        let visible_h = h
            .saturating_sub(self.skip)
            .min(self.bottom.saturating_sub(self.y));
        let seg_area = Rect::new(self.viewport.x, self.y, self.viewport.width, visible_h);
        let mut p = Paragraph::new(lines.to_vec()).wrap(Wrap { trim: false });
        if let Some(s) = style {
            p = p.style(s);
        }
        if self.skip > 0 {
            p = p.scroll((self.skip, 0));
            self.skip = 0;
        }
        frame.render_widget(p, seg_area);
        if highlight {
            for row in seg_area.y..seg_area.y + seg_area.height {
                for col in seg_area.x..seg_area.x + seg_area.width {
                    if let Some(cell) = frame.buffer_mut().cell_mut((col, row)) {
                        let fg = cell.fg;
                        let bg = cell.bg;
                        cell.set_fg(bg);
                        cell.set_bg(fg);
                    }
                }
            }
        }
        self.y += visible_h;
    }
}

fn wrapped_line_count(lines: &[Line<'_>], width: u16) -> u16 {
    if width == 0 {
        return lines.len() as u16;
    }
    Paragraph::new(lines.to_vec())
        .wrap(Wrap { trim: false })
        .line_count(width) as u16
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
    use maki_agent::tools::{
        BASH_TOOL_NAME, GLOB_TOOL_NAME, GREP_TOOL_NAME, QUESTION_TOOL_NAME, READ_TOOL_NAME,
        WRITE_TOOL_NAME,
    };
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
        let mut panel = MessagesPanel::new(UiConfig::default());
        for &(id, tool) in ids {
            panel.tool_start(start(id, tool));
        }
        panel
    }

    #[test_case(false, ToolStatus::Success ; "success_updates_start_to_success")]
    #[test_case(true,  ToolStatus::Error   ; "error_updates_start_to_error")]
    fn tool_done_updates_start_status(is_error: bool, expected: ToolStatus) {
        let mut panel = MessagesPanel::new(UiConfig::default());
        panel.tool_start(start("t1", "bash"));
        panel.tool_done(ToolDoneEvent {
            id: "t1".into(),
            tool: "bash",
            output: ToolOutput::Plain("output".into()),
            is_error,
        });

        assert_eq!(panel.messages.len(), 1);
        assert!(matches!(&panel.messages[0].role, DisplayRole::Tool(t) if t.status == expected));
        assert!(panel.messages[0].text.contains("output"));
    }

    #[test]
    fn webfetch_hides_body() {
        let mut panel = MessagesPanel::new(UiConfig::default());
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
        let mut panel = MessagesPanel::new(UiConfig::default());
        panel.tool_start(start("t1", tool));
        panel.tool_done(ToolDoneEvent {
            id: "t1".into(),
            tool,
            output,
            is_error: false,
        });
        assert_eq!(panel.messages[0].annotation.as_deref(), expected);
    }

    #[test_case("line\n".repeat(200).as_str(), Some("2m timeout · 200 lines") ; "merges_start_and_output_annotations")]
    #[test_case("ok",                           Some("2m timeout")           ; "keeps_start_when_output_has_none")]
    fn tool_done_annotation_merge(output: &str, expected: Option<&str>) {
        let mut panel = MessagesPanel::new(UiConfig::default());
        let mut event = start("t1", BASH_TOOL_NAME);
        event.annotation = Some("2m timeout".into());
        panel.tool_start(event);
        panel.tool_done(ToolDoneEvent {
            id: "t1".into(),
            tool: BASH_TOOL_NAME,
            output: ToolOutput::Plain(output.into()),
            is_error: false,
        });
        assert_eq!(panel.messages[0].annotation.as_deref(), expected);
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
        let mut panel = MessagesPanel::new(UiConfig::default());
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
    fn tool_done_grep_shows_matches() {
        let mut panel = MessagesPanel::new(UiConfig::default());
        panel.tool_start(start("t1", GREP_TOOL_NAME));
        panel.tool_done(ToolDoneEvent {
            id: "t1".into(),
            tool: GREP_TOOL_NAME,
            output: grep_output(2),
            is_error: false,
        });
        let text = &panel.messages[0].text;
        assert!(!text.contains('\n'), "grep body should not be in msg.text");
        assert!(panel.messages[0].tool_output.is_some());
    }

    #[test]
    fn tool_start_flushes_streaming_text() {
        let mut panel = MessagesPanel::new(UiConfig::default());
        panel.streaming_text.set_buffer("partial response");

        panel.tool_start(start("t1", "read"));

        assert!(panel.streaming_text.is_empty());
        assert_eq!(panel.messages[0].role, DisplayRole::Assistant);
        assert!(matches!(panel.messages[1].role, DisplayRole::Tool(_)));
    }

    #[test]
    fn thinking_delta_separate_from_text() {
        let mut panel = MessagesPanel::new(UiConfig::default());
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
        let mut panel = MessagesPanel::new(UiConfig::default());
        panel.push(DisplayMessage::new(DisplayRole::User, "short".into()));
        panel.scroll_top = 1000;
        panel.auto_scroll = false;
        rebuild(&mut panel);
        assert_eq!(panel.scroll_top, 0);
    }

    #[test]
    fn scroll_up_pins_viewport_during_streaming() {
        let mut panel = MessagesPanel::new(UiConfig::default());
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
        let mut panel = MessagesPanel::new(UiConfig::default());
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
        let mut panel = MessagesPanel::new(UiConfig::default());
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
        let mut panel = MessagesPanel::new(UiConfig::default());
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
            .rfind(|m| matches!(&m.role, DisplayRole::Tool(t) if t.id == tool_id))
            .map(|m| match &m.role {
                DisplayRole::Tool(t) => t.status,
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
        let mut panel = MessagesPanel::new(UiConfig::default());
        bash_code_start(&mut panel, "t1", "echo hello");
        rebuild(&mut panel);

        panel.tool_output("t1", "hello");
        let text = seg_text(&panel, "t1");
        assert!(text.contains("echo hello"));
        assert!(text.contains("hello"));

        panel.tool_output("t1", "hello\nworld");
        let text = seg_text(&panel, "t1");
        assert!(text.contains("echo hello"));
        assert!(text.contains("world"));

        panel.tool_done(ToolDoneEvent {
            id: "t1".into(),
            tool: BASH_TOOL_NAME,
            output: ToolOutput::Plain("done".into()),
            is_error: false,
        });
        let text = seg_text(&panel, "t1");
        assert!(text.contains("echo hello"));
        assert!(text.contains("done"));
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

    #[test_case(true  ; "after_cache_built")]
    #[test_case(false ; "before_cache_built")]
    fn fail_in_progress_marks_pending_as_error(cache_built: bool) {
        let mut panel = panel_with_tools(&[("t1", "bash"), ("t2", "read")]);
        panel.tool_done(ToolDoneEvent {
            id: "t1".into(),
            tool: "bash",
            output: ToolOutput::Plain("ok".into()),
            is_error: false,
        });
        if cache_built {
            rebuild(&mut panel);
        }

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
            DisplayRole::Tool(Box::new(ToolRole {
                id: id.into(),
                status,
                name,
            })),
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
        let mut panel = MessagesPanel::new(UiConfig::default());
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
        let mut panel = MessagesPanel::new(UiConfig::default());
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

    fn seg_copy(panel: &MessagesPanel, tool_id: &str) -> String {
        panel
            .cached_segments
            .iter()
            .find(|s| s.tool_id.as_deref() == Some(tool_id))
            .unwrap()
            .copy_text
            .clone()
    }

    #[test]
    fn copy_text_grep_result_includes_structured_output() {
        let mut panel = MessagesPanel::new(UiConfig::default());
        panel.tool_start(start("t1", "grep"));
        panel.tool_done(ToolDoneEvent {
            id: "t1".into(),
            tool: "grep",
            output: grep_output(2),
            is_error: false,
        });
        rebuild(&mut panel);
        let text = seg_copy(&panel, "t1");
        assert!(text.contains("0.rs:") && text.contains("1.rs:"));
    }

    #[test]
    fn copy_text_diff_output_includes_hunks() {
        let mut panel = MessagesPanel::new(UiConfig::default());
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
        rebuild(&mut panel);
        let text = seg_copy(&panel, "t1");
        assert!(text.contains("- old") && text.contains("+ new"));
    }

    #[test]
    fn copy_text_bash_with_code_input() {
        let mut panel = MessagesPanel::new(UiConfig::default());
        bash_code_start(&mut panel, "t1", "echo hello");
        panel.tool_done(ToolDoneEvent {
            id: "t1".into(),
            tool: BASH_TOOL_NAME,
            output: ToolOutput::Plain("hello".into()),
            is_error: false,
        });
        rebuild(&mut panel);
        let text = seg_copy(&panel, "t1");
        assert!(text.contains("echo hello") && text.contains("hello"));
    }

    #[test]
    fn copy_text_includes_role_prefix() {
        let md = "# Heading\n\nSome **bold** text";
        let mut panel = MessagesPanel::new(UiConfig::default());
        panel.push(DisplayMessage::new(DisplayRole::User, "hello".into()));
        panel.push(DisplayMessage::new(DisplayRole::Assistant, md.into()));
        panel.push(DisplayMessage::new(DisplayRole::Thinking, "hmm".into()));
        rebuild(&mut panel);
        let texts = panel.segment_copy_texts();
        assert_eq!(texts[0], "you> hello");
        assert_eq!(texts[2], format!("maki> {md}"));
        assert_eq!(texts[4], "thinking> hmm");
    }

    #[test_case(&["short", &"x".repeat(200)], 80, 4 ; "long_line_wraps")]
    #[test_case(&["", "a", ""],                 40, 3 ; "empty_lines_count_as_one")]
    #[test_case(&[&"a".repeat(80)],              80, 1 ; "exactly_width_no_wrap")]
    #[test_case(&[&"a".repeat(81)],              80, 2 ; "one_over_width_wraps")]
    #[test_case(&["hello", "world"],              0, 2 ; "zero_width_returns_line_count")]
    #[test_case(&["aaaa bbbb cccc dddd"],         10, 2 ; "word_boundary_wrap")]
    #[test_case(&["aaaaaa bbbbbbbbb"],            10, 2 ; "word_straddles_boundary")]
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

    fn batch_entry(tool: &str, summary: &str, status: BatchToolStatus) -> BatchToolEntry {
        BatchToolEntry {
            tool: tool.into(),
            summary: summary.into(),
            status,
            input: None,
            output: None,
            annotation: None,
        }
    }

    fn batch_start(panel: &mut MessagesPanel, entries: Vec<BatchToolEntry>) {
        panel.tool_start(ToolStartEvent {
            id: "b1".into(),
            tool: "batch",
            summary: format!("{} tools", entries.len()),
            annotation: None,
            input: None,
            output: Some(ToolOutput::Batch {
                entries,
                text: String::new(),
            }),
        });
    }

    fn batch_done(panel: &mut MessagesPanel, entries: Vec<BatchToolEntry>) {
        panel.tool_done(ToolDoneEvent {
            id: "b1".into(),
            tool: "batch",
            output: ToolOutput::Batch {
                entries,
                text: String::new(),
            },
            is_error: false,
        });
    }

    fn batch_entries(panel: &MessagesPanel) -> &[BatchToolEntry] {
        let ToolOutput::Batch { entries, .. } = panel.messages[0].tool_output.as_deref().unwrap()
        else {
            panic!("expected Batch");
        };
        entries
    }

    #[test]
    fn tool_done_batch_preserves_entry_annotations() {
        let mut panel = MessagesPanel::new(UiConfig::default());
        batch_start(
            &mut panel,
            vec![
                batch_entry("task", "research", BatchToolStatus::InProgress),
                batch_entry("read", "file.rs", BatchToolStatus::Pending),
            ],
        );

        let model = "anthropic/claude-haiku-4-20250414";
        panel.update_tool_model("b1__0", model);

        let mut done_entries = vec![
            batch_entry("task", "research", BatchToolStatus::Success),
            batch_entry("read", "file.rs", BatchToolStatus::Success),
        ];
        done_entries[0].output = Some(ToolOutput::Plain("result".into()));
        done_entries[1].output = Some(ToolOutput::Plain("contents".into()));
        batch_done(&mut panel, done_entries);

        let entries = batch_entries(&panel);
        assert_eq!(entries[0].annotation.as_deref(), Some(model));
        assert!(entries[1].annotation.is_none());
    }

    #[test]
    fn tool_done_batch_preserves_entry_summaries() {
        let mut panel = MessagesPanel::new(UiConfig::default());
        batch_start(
            &mut panel,
            vec![batch_entry("task", "original", BatchToolStatus::InProgress)],
        );

        panel.update_tool_summary("b1__0", "renamed by ui");

        let mut done_entries = vec![batch_entry("task", "original", BatchToolStatus::Success)];
        done_entries[0].output = Some(ToolOutput::Plain("done".into()));
        batch_done(&mut panel, done_entries);

        assert_eq!(batch_entries(&panel)[0].summary, "renamed by ui");
    }

    #[test]
    fn scroll_clamps_to_max_scroll() {
        let mut panel = MessagesPanel::new(UiConfig::default());
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

    #[test_case("bash", 1, 1 ; "known_tool_creates_message")]
    #[test_case("nonexistent_tool", 0, 0 ; "unknown_tool_ignored")]
    fn tool_pending(tool: &str, expected_msgs: usize, expected_in_progress: usize) {
        let mut panel = MessagesPanel::new(UiConfig::default());
        panel.tool_pending("t1".into(), tool);
        assert_eq!(panel.messages.len(), expected_msgs);
        assert_eq!(panel.in_progress_count, expected_in_progress);
    }

    #[test]
    fn tool_start_upgrades_pending_in_place() {
        let mut panel = MessagesPanel::new(UiConfig::default());
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

    #[test]
    fn stream_reset_clears_streaming_and_fails_tools() {
        let mut panel = panel_with_tools(&[("t1", "bash")]);
        panel.streaming_thinking.set_buffer("partial thinking");
        panel.streaming_text.set_buffer("partial text");
        rebuild(&mut panel);

        panel.stream_reset();

        assert!(panel.streaming_thinking.is_empty());
        assert!(panel.streaming_text.is_empty());
        assert_eq!(panel.in_progress_count, 0);
        assert_eq!(msg_status(&panel, "t1"), ToolStatus::Error);
    }

    use crate::selection::{Selection, SelectionZone};

    const MAKI_PREFIX_LEN: u16 = 6;

    fn make_sel(area: Rect, anchor: (u32, u16), cursor: (u32, u16)) -> Selection {
        let mut sel = Selection::start(
            area.y + anchor.0 as u16,
            anchor.1,
            area,
            SelectionZone::Messages,
            0,
        );
        sel.update(area.y + cursor.0 as u16, cursor.1, 0);
        sel
    }

    fn panel_with_msgs(texts: &[&str], width: u16, height: u16) -> MessagesPanel {
        let mut panel = MessagesPanel::new(UiConfig::default());
        for &text in texts {
            panel.push(DisplayMessage::new(DisplayRole::Assistant, text.into()));
        }
        render(&mut panel, width, height);
        panel
    }

    #[test]
    fn extract_fully_enclosed_segments_use_copy_text() {
        let panel = panel_with_msgs(&["Hello world", "Second message"], 80, 24);
        let total: u16 = panel.segment_heights().iter().sum();
        let area = Rect::new(0, 0, 80, 24);
        let sel = make_sel(area, (0, 0), (total.saturating_sub(1) as u32, 79));
        let text = panel.extract_selection_text(&sel, area);
        assert!(text.contains("Hello world"));
        assert!(text.contains("Second message"));
    }

    #[test]
    fn extract_partial_column_selection() {
        let panel = panel_with_msgs(&["Hello world"], 80, 24);
        let area = Rect::new(0, 0, 80, 24);
        let world_start = MAKI_PREFIX_LEN + "Hello ".len() as u16;
        let sel = make_sel(area, (0, world_start), (0, world_start + 4));
        let text = panel.extract_selection_text(&sel, area);
        assert_eq!(text, "world");
    }

    #[test]
    fn extract_skips_out_of_range_segments() {
        let panel = panel_with_msgs(&["seg0", "seg1", "seg2"], 80, 24);
        let heights = panel.segment_heights();
        let total: u16 = heights.iter().sum();
        let mid = total / 2;
        let area = Rect::new(0, 0, 80, 24);
        let sel = make_sel(area, (mid as u32, 0), (mid as u32, 79));
        let text = panel.extract_selection_text(&sel, area);
        assert!(text.contains("seg1"));
        assert!(!text.contains("seg0"));
        assert!(!text.contains("seg2"));
    }

    #[test]
    fn extract_off_screen_rows_via_temp_buffer() {
        let mut panel = MessagesPanel::new(UiConfig::default());
        let lines: Vec<String> = (0..20).map(|i| format!("line {i}")).collect();
        let long_text = lines.join("\n");
        panel.push(DisplayMessage::new(
            DisplayRole::Assistant,
            long_text.clone(),
        ));
        render(&mut panel, 80, 5);

        let total: u16 = panel.segment_heights().iter().sum();
        assert!(total > 5, "content must exceed viewport");
        // Partial selection forces temp buffer path (not copy_text)
        let sel_area = Rect::new(0, 0, 80, total);
        let sel = make_sel(sel_area, (1, 0), ((total - 1) as u32, 79));

        let text = panel.extract_selection_text(&sel, sel_area);
        assert!(
            !text.contains("line 0"),
            "first line excluded by partial select"
        );
        assert!(text.contains("line 1"));
        assert!(text.contains("line 19"));
    }

    #[test]
    fn extract_mixed_fully_enclosed_and_partial() {
        let panel = panel_with_msgs(&["full segment", "partial here"], 80, 24);
        let heights = panel.segment_heights().to_vec();
        let area = Rect::new(0, 0, 80, 24);
        let seg1_start = heights[0] + heights[1];
        let sel = make_sel(area, (0, 0), (seg1_start as u32, MAKI_PREFIX_LEN + 6));
        let text = panel.extract_selection_text(&sel, area);
        assert!(text.contains("full segment"));
        assert!(text.contains("partial"));
    }

    #[test_case(&["line-0\nline-1\nline-2\nline-3"], "line-0", "line-3" ; "single_segment")]
    #[test_case(&["seg-A-text", "seg-B-text"],      "seg-A-text", "seg-B-text" ; "across_segments")]
    fn extract_partial_col_symmetric(msgs: &[&str], expect_start: &str, expect_end: &str) {
        let mut panel = MessagesPanel::new(UiConfig::default());
        for &text in msgs {
            panel.push(DisplayMessage::new(DisplayRole::Assistant, text.into()));
        }
        render(&mut panel, 80, 24);
        let total: u16 = panel.segment_heights().iter().sum();
        let area = Rect::new(0, 0, 80, 24);
        let down = make_sel(area, (0, MAKI_PREFIX_LEN), ((total - 1) as u32, 79));
        let up = make_sel(area, ((total - 1) as u32, 79), (0, MAKI_PREFIX_LEN));
        let text_down = panel.extract_selection_text(&down, area);
        let text_up = panel.extract_selection_text(&up, area);
        assert!(text_down.contains(expect_start));
        assert!(text_down.contains(expect_end));
        assert_eq!(text_down, text_up, "direction should not affect result");
    }

    #[test_case("```\n{L}\n```", (0, 1)  ; "wrapped_code_block")]
    #[test_case("short\n{L}",   (0, 0)  ; "wrapped_long_line")]
    fn extract_wrapped_no_soft_breaks(template: &str, anchor: (u32, u16)) {
        let long = "x".repeat(200);
        let msg = template.replace("{L}", &long);
        let mut panel = MessagesPanel::new(UiConfig::default());
        panel.push(DisplayMessage::new(DisplayRole::Assistant, msg));
        render(&mut panel, 40, 30);
        let total: u16 = panel.segment_heights().iter().sum();
        let area = Rect::new(0, 0, 40, 30);
        let sel = make_sel(area, anchor, ((total - 1) as u32, 39));
        let text = panel.extract_selection_text(&sel, area);
        assert!(
            text.contains(&long),
            "wrapped line must be copied without newlines: {text:?}"
        );
    }

    #[test]
    fn extract_partial_last_line_truncated() {
        let mut panel = MessagesPanel::new(UiConfig::default());
        panel.push(DisplayMessage::new(
            DisplayRole::Assistant,
            "first\nABCDEFGHIJKLMNOP".into(),
        ));
        render(&mut panel, 80, 24);
        let total: u16 = panel.segment_heights().iter().sum();
        let area = Rect::new(0, 0, 80, 24);
        let last_row = (total - 1) as u32;
        let sel = make_sel(area, (0, 0), (last_row, 3));
        let text = panel.extract_selection_text(&sel, area);
        assert_eq!(text.lines().last().unwrap(), "ABCD");
    }

    fn panel_with_long_tool(line_count: usize) -> MessagesPanel {
        let body = (0..line_count)
            .map(|i| format!("line {i}"))
            .collect::<Vec<_>>()
            .join("\n");
        let mut panel = MessagesPanel::new(UiConfig::default());
        panel.tool_start(ToolStartEvent {
            id: "t1".into(),
            tool: BASH_TOOL_NAME,
            summary: "cmd".into(),
            annotation: None,
            input: None,
            output: None,
        });
        panel.tool_done(ToolDoneEvent {
            id: "t1".into(),
            tool: BASH_TOOL_NAME,
            output: ToolOutput::Plain(body),
            is_error: false,
        });
        render(&mut panel, 80, 24);
        panel
    }

    #[test]
    fn toggle_expands_and_collapses_truncated_tool() {
        let mut panel = panel_with_long_tool(200);
        let area = Rect::new(0, 0, 80, 24);
        let text_before = seg_text(&panel, "t1");
        assert!(text_before.contains("click to expand"));
        assert!(!text_before.contains("line 50"));

        assert!(panel.toggle_expansion_at(area.y, area));
        render(&mut panel, 80, 24);
        let text_after = seg_text(&panel, "t1");
        assert!(text_after.contains("line 50"));
        assert!(!text_after.contains("click to expand"));

        assert!(panel.toggle_expansion_at(area.y, area));
        render(&mut panel, 80, 24);
        let text_restored = seg_text(&panel, "t1");
        assert!(text_restored.contains("click to expand"));
        assert!(!text_restored.contains("line 50"));
    }

    #[test_case(false ; "non_tool_segment")]
    #[test_case(true  ; "short_tool_segment")]
    fn toggle_returns_false_for_non_expandable(is_tool: bool) {
        let mut panel = if is_tool {
            panel_with_long_tool(3)
        } else {
            let mut p = MessagesPanel::new(UiConfig::default());
            p.push(DisplayMessage::new(DisplayRole::Assistant, "hello".into()));
            render(&mut p, 80, 24);
            p
        };
        let area = Rect::new(0, 0, 80, 24);
        assert!(!panel.toggle_expansion_at(area.y, area));
    }

    fn panel_with_grep_tool(match_count: usize) -> MessagesPanel {
        let entries = vec![GrepFileEntry {
            path: "src/main.rs".into(),
            matches: (1..=match_count)
                .map(|i| GrepMatch {
                    line_nr: i,
                    text: format!("match_{i}"),
                })
                .collect(),
        }];
        let mut panel = MessagesPanel::new(UiConfig::default());
        panel.tool_start(ToolStartEvent {
            id: "t1".into(),
            tool: GREP_TOOL_NAME,
            summary: "grep pattern".into(),
            annotation: None,
            input: None,
            output: None,
        });
        panel.tool_done(ToolDoneEvent {
            id: "t1".into(),
            tool: GREP_TOOL_NAME,
            output: ToolOutput::GrepResult { entries },
            is_error: false,
        });
        render(&mut panel, 80, 24);
        panel
    }

    #[test_case(4  ; "max_plus_one")]
    #[test_case(8  ; "max_plus_five")]
    fn toggle_grep_expand_and_collapse(match_count: usize) {
        let mut panel = panel_with_grep_tool(match_count);
        let area = Rect::new(0, 0, 80, 24);
        assert!(seg_text(&panel, "t1").contains("click to expand"));

        assert!(panel.toggle_expansion_at(area.y, area));
        render(&mut panel, 80, 24);
        let text = seg_text(&panel, "t1");
        let last = format!("match_{match_count}");
        assert!(text.contains(&last), "last match should be visible");
        assert!(!text.contains("click to expand"));

        assert!(panel.toggle_expansion_at(area.y, area));
        render(&mut panel, 80, 24);
        assert!(seg_text(&panel, "t1").contains("click to expand"));
    }

    fn panel_with_read_tool(line_count: usize) -> MessagesPanel {
        let lines: Vec<String> = (0..line_count).map(|i| format!("line {i}")).collect();
        let mut panel = MessagesPanel::new(UiConfig::default());
        panel.tool_start(ToolStartEvent {
            id: "t1".into(),
            tool: READ_TOOL_NAME,
            summary: "read /src/main.rs".into(),
            annotation: None,
            input: None,
            output: None,
        });
        panel.tool_done(ToolDoneEvent {
            id: "t1".into(),
            tool: READ_TOOL_NAME,
            output: ToolOutput::ReadCode {
                path: "main.rs".into(),
                start_line: 1,
                lines,
                total_lines: line_count,
                instructions: None,
            },
            is_error: false,
        });
        render(&mut panel, 80, 24);
        panel
    }

    #[test]
    fn toggle_read_tool_expands_and_collapses() {
        let mut panel = panel_with_read_tool(20);
        let area = Rect::new(0, 0, 80, 24);
        assert!(seg_text(&panel, "t1").contains("click to expand"));

        assert!(panel.toggle_expansion_at(area.y, area));
        render(&mut panel, 80, 24);
        assert!(seg_text(&panel, "t1").contains("line 19"));

        assert!(panel.toggle_expansion_at(area.y, area));
        render(&mut panel, 80, 24);
        assert!(seg_text(&panel, "t1").contains("click to expand"));
    }

    fn buffer_text(terminal: &ratatui::Terminal<TestBackend>) -> String {
        let buf = terminal.backend().buffer();
        let mut text = String::new();
        for y in 0..buf.area.height {
            for x in 0..buf.area.width {
                if let Some(cell) = buf.cell((x, y)) {
                    text.push_str(cell.symbol());
                }
            }
            text.push('\n');
        }
        text
    }

    #[test]
    fn streaming_with_cached_segments_shows_end_on_auto_scroll() {
        let mut panel = MessagesPanel::new(UiConfig::default());
        panel.push(DisplayMessage::new(
            DisplayRole::User,
            "a\n".repeat(20).trim().into(),
        ));

        let streaming_lines: Vec<String> = (0..50).map(|i| format!("stream_{i}")).collect();
        panel.streaming_text.set_buffer(&streaming_lines.join("\n"));

        let terminal = render(&mut panel, 80, 10);
        assert!(panel.auto_scroll);

        let screen = buffer_text(&terminal);
        assert!(
            screen.contains("stream_49"),
            "auto-scroll should show end of streaming text, got:\n{screen}"
        );
        assert!(
            !screen.contains("stream_0 "),
            "auto-scroll should not show beginning of streaming text"
        );
    }

    #[test]
    fn batch_parent_copy_text_excludes_children() {
        let mut panel = MessagesPanel::new(UiConfig::default());
        let mut entry = batch_entry("read", "file.rs", BatchToolStatus::Success);
        entry.output = Some(ToolOutput::Plain("file contents".into()));
        let entries = vec![entry];
        batch_start(&mut panel, entries.clone());
        batch_done(&mut panel, entries);
        rebuild(&mut panel);
        let parent = seg_copy(&panel, "b1");
        assert!(!parent.contains("file contents"));
        let child = seg_copy(&panel, "b1__0");
        assert!(child.contains("file contents"));
    }

    #[test]
    fn extract_partial_full_code_block_has_fences() {
        let md = "hello\n\n```python\nprint('hi')\n```\n\ngoodbye";
        let mut panel = MessagesPanel::new(UiConfig::default());
        panel.push(DisplayMessage::new(DisplayRole::Assistant, md.into()));
        render(&mut panel, 80, 24);
        let total: u16 = panel.segment_heights().iter().sum();
        let area = Rect::new(0, 0, 80, 24);
        let sel = make_sel(area, (0, 0), ((total - 1) as u32, 79));
        let text = panel.extract_selection_text(&sel, area);
        assert!(text.contains("hello"), "text before: {text:?}");
        assert!(text.contains("goodbye"), "text after: {text:?}");
        assert!(text.contains("```python"), "code fence: {text:?}");
        assert!(text.contains("print('hi')"), "code content: {text:?}");
    }

    #[test]
    fn extract_partial_code_block_not_fully_selected_has_no_fences() {
        let md = "before\n\n```rust\nline1\nline2\nline3\n```\n\nafter";
        let mut panel = MessagesPanel::new(UiConfig::default());
        panel.push(DisplayMessage::new(DisplayRole::Assistant, md.into()));
        render(&mut panel, 80, 24);
        let total: u16 = panel.segment_heights().iter().sum();
        let area = Rect::new(0, 0, 80, 24);
        let mid = total / 2;
        let sel = make_sel(area, (mid as u32, 0), ((total - 1) as u32, 79));
        let text = panel.extract_selection_text(&sel, area);
        assert!(
            !text.contains("```"),
            "partially-covered code block should not have fences: {text:?}"
        );
    }

    #[test]
    fn extract_partial_boundary_has_newline_separation() {
        let md = "before\n\n```rust\nline1\nline2\nline3\n```\n\nafter";
        let mut panel = MessagesPanel::new(UiConfig::default());
        panel.push(DisplayMessage::new(DisplayRole::Assistant, md.into()));
        render(&mut panel, 80, 24);
        let total: u16 = panel.segment_heights().iter().sum();
        let cb = &panel.cached_segments[0].code_block_ranges[0];
        let area = Rect::new(0, 0, 80, 24);

        let sel_start = make_sel(area, (0, 0), (cb.start_line as u32 + 1, 79));
        let text = panel.extract_selection_text(&sel_start, area);
        assert!(text.contains("before"), "text before code: {text:?}");
        assert!(text.contains("line1"), "code start: {text:?}");
        assert!(
            !text.contains("beforel"),
            "must be newline-separated: {text:?}"
        );

        let sel_end = make_sel(area, (cb.end_line as u32, 0), ((total - 1) as u32, 79));
        let text = panel.extract_selection_text(&sel_end, area);
        assert!(text.contains("line3"), "code end: {text:?}");
        assert!(text.contains("after"), "text after code: {text:?}");
        assert!(
            !text.contains("line3after"),
            "must be newline-separated: {text:?}"
        );
    }
}

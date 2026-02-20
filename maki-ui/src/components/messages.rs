use super::{DisplayMessage, DisplayRole, ToolStatus};

use crate::animation::{Typewriter, spinner_frame};
use crate::highlight::CodeHighlighter;
use crate::markdown::{text_to_lines, truncate_lines};
use crate::theme;

use std::time::Instant;

use maki_agent::tools::{GLOB_TOOL_NAME, GREP_TOOL_NAME, WEBFETCH_TOOL_NAME};
use maki_providers::{DiffLine, ToolDoneEvent, ToolOutput, ToolStartEvent};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Wrap};

const TOOL_INDICATOR: &str = "● ";
const TOOL_OUTPUT_MAX_DISPLAY_LINES: usize = 7;
const TOOL_BODY_INDENT: &str = "  ";

#[derive(Default)]
struct StreamingCache {
    byte_len: usize,
    lines: Vec<Line<'static>>,
    highlighters: Vec<CodeHighlighter>,
}

impl StreamingCache {
    fn get_or_update(&mut self, visible: &str, prefix: &str, style: Style) -> Vec<Line<'static>> {
        let len = visible.len();
        if len == self.byte_len && !self.lines.is_empty() {
            return self.lines.clone();
        }
        self.lines = text_to_lines(visible, prefix, style, Some(&mut self.highlighters));
        self.byte_len = len;
        self.lines.clone()
    }
}

struct Segment {
    lines: Vec<Line<'static>>,
    tool_id: Option<String>,
    cached_height: Option<(u16, u16)>,
}

impl Segment {
    fn is_tool(&self) -> bool {
        self.tool_id.is_some()
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
            cached_streaming_thinking: StreamingCache::default(),
            cached_streaming_text: StreamingCache::default(),
        }
    }

    pub fn push(&mut self, msg: DisplayMessage) {
        self.messages.push(msg);
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
            tool_output: None,
        });
        self.in_progress_count += 1;
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
        if let ToolOutput::Plain(ref text) = event.output {
            if event.tool == GLOB_TOOL_NAME {
                let n = text.lines().count();
                msg.text = format!("{} ({n} files)", msg.text);
            } else if event.tool == GREP_TOOL_NAME {
                let n = text.lines().filter(|l| !l.starts_with(' ')).count();
                msg.text = format!("{} ({n} files)", msg.text);
            }
            let display = if event.tool == WEBFETCH_TOOL_NAME {
                let n = text.lines().count();
                format!("({n} lines)")
            } else {
                truncate_lines(text, TOOL_OUTPUT_MAX_DISPLAY_LINES).into_owned()
            };
            if !display.is_empty() {
                msg.text = format!("{}\n{display}", msg.text);
            }
        }
        msg.tool_output = Some(event.output);
        self.in_progress_count -= 1;
        self.invalidate_line_cache();
    }

    pub fn flush(&mut self) {
        self.flush_thinking();
        if !self.streaming_text.is_empty() {
            self.messages.push(DisplayMessage {
                role: DisplayRole::Assistant,
                text: self.streaming_text.take_all(),
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

        let last_cached_is_tool = self.cached_segments.last().is_some_and(|s| s.is_tool());

        let mut segments: Vec<(&[Line<'static>], bool)> = self
            .cached_segments
            .iter()
            .map(|s| (s.lines.as_slice(), s.is_tool()))
            .collect();

        let spacer_line = vec![Line::default()];
        let mut streaming_lines = Vec::new();
        for (tw, cache, prefix, style) in [
            (
                &self.streaming_thinking,
                &mut self.cached_streaming_thinking,
                "thinking> ",
                theme::THINKING,
            ),
            (
                &self.streaming_text,
                &mut self.cached_streaming_text,
                "maki> ",
                theme::ASSISTANT,
            ),
        ] {
            if !tw.is_empty() {
                let mut lines = cache.get_or_update(tw.visible(), prefix, style);
                if let Some(last) = lines.last_mut() {
                    last.spans.push(Span::styled("_", theme::CURSOR));
                }
                streaming_lines.extend(lines);
            }
        }
        if !streaming_lines.is_empty() {
            if last_cached_is_tool {
                segments.push((&spacer_line, false));
                heights.push(1);
            }
            heights.push(
                Paragraph::new(streaming_lines.clone())
                    .wrap(Wrap { trim: false })
                    .line_count(width) as u16,
            );
            segments.push((&streaming_lines, false));
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
    }

    fn flush_thinking(&mut self) {
        if !self.streaming_thinking.is_empty() {
            self.messages.push(DisplayMessage {
                role: DisplayRole::Thinking,
                text: self.streaming_thinking.take_all(),
                tool_output: None,
            });
            self.cached_streaming_thinking = StreamingCache::default();
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

    fn invalidate_line_cache(&mut self) {
        self.cached_msg_count = 0;
        self.cached_segments.clear();
    }

    fn rebuild_line_cache(&mut self) {
        if self.cached_msg_count == self.messages.len() {
            return;
        }
        for i in self.cached_msg_count..self.messages.len() {
            let msg = &self.messages[i];

            if let DisplayRole::Tool { ref id, status, .. } = msg.role {
                let lines = self.build_tool_lines(msg, status);
                let id = id.clone();
                self.push_spacer_if_needed();
                self.cached_segments.push(Segment {
                    lines,
                    tool_id: Some(id),
                    cached_height: None,
                });
            } else {
                let (prefix, base_style) = match &msg.role {
                    DisplayRole::User => ("you> ", theme::USER),
                    DisplayRole::Assistant => ("maki> ", theme::ASSISTANT),
                    DisplayRole::Thinking => ("thinking> ", theme::THINKING),
                    DisplayRole::Error => ("", theme::ERROR),
                    DisplayRole::Tool { .. } => unreachable!(),
                };
                let lines = text_to_lines(&msg.text, prefix, base_style, None);

                let last_was_tool = self.cached_segments.last().is_some_and(|s| s.is_tool());
                if self.cached_segments.is_empty() || last_was_tool {
                    self.push_spacer_if_needed();
                    self.cached_segments.push(Segment {
                        lines,
                        tool_id: None,
                        cached_height: None,
                    });
                } else {
                    let last = self.cached_segments.last_mut().unwrap();
                    last.lines.extend(lines);
                    last.cached_height = None;
                }
            }
        }
        self.cached_msg_count = self.messages.len();
    }

    fn build_tool_lines(&self, msg: &DisplayMessage, status: ToolStatus) -> Vec<Line<'static>> {
        let header = msg
            .text
            .split_once('\n')
            .map_or(msg.text.as_str(), |(h, _)| h);
        let tool_name = msg.role.tool_name().unwrap_or("?");
        let mut lines = text_to_lines(header, &format!("{tool_name}> "), theme::TOOL, None);

        let (indicator, indicator_style) = match status {
            ToolStatus::InProgress => {
                let ch = spinner_frame(self.started_at.elapsed().as_millis());
                (format!("{ch} "), theme::TOOL_IN_PROGRESS)
            }
            ToolStatus::Success => (TOOL_INDICATOR.into(), theme::TOOL_SUCCESS),
            ToolStatus::Error => (TOOL_INDICATOR.into(), theme::TOOL_ERROR),
        };
        if let Some(first) = lines.first_mut() {
            first
                .spans
                .insert(0, Span::styled(indicator, indicator_style));
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
            Some(ToolOutput::Diff { hunks, .. }) => {
                let max_line_nr = hunks
                    .iter()
                    .map(|h| {
                        let numbered = h
                            .lines
                            .iter()
                            .filter(|l| !matches!(l, DiffLine::Added(_)))
                            .count();
                        h.start_line + numbered.saturating_sub(1)
                    })
                    .max()
                    .unwrap_or(1);
                let nr_width = max_line_nr.ilog10() as usize + 1;

                for (i, hunk) in hunks.iter().enumerate() {
                    if i > 0 {
                        lines.push(Line::from(Span::styled(
                            format!("{TOOL_BODY_INDENT}{:>nr_width$}  ...", ""),
                            theme::DIFF_LINE_NR,
                        )));
                    }
                    let mut line_nr = hunk.start_line;
                    for dl in &hunk.lines {
                        let (prefix, text, style, show_nr) = match dl {
                            DiffLine::Unchanged(t) => {
                                ("  ", t.as_str(), theme::DIFF_UNCHANGED, true)
                            }
                            DiffLine::Removed(t) => ("- ", t.as_str(), theme::DIFF_OLD, true),
                            DiffLine::Added(t) => ("+ ", t.as_str(), theme::DIFF_NEW, false),
                        };
                        let nr_str = if show_nr {
                            let s = format!("{line_nr:>nr_width$}");
                            line_nr += 1;
                            s
                        } else {
                            " ".repeat(nr_width)
                        };
                        lines.push(Line::from(vec![
                            Span::styled(
                                format!("{TOOL_BODY_INDENT}{nr_str} "),
                                theme::DIFF_LINE_NR,
                            ),
                            Span::styled(format!("{prefix}{text}"), style),
                        ]));
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
        }

        lines
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

#[cfg(test)]
mod tests {
    use super::*;
    use maki_providers::ToolOutput;
    use ratatui::backend::TestBackend;
    use test_case::test_case;

    #[test_case(false, ToolStatus::Success ; "success_updates_start_to_success")]
    #[test_case(true,  ToolStatus::Error   ; "error_updates_start_to_error")]
    fn tool_done_updates_start_status(is_error: bool, expected: ToolStatus) {
        let mut panel = MessagesPanel::new();
        panel.tool_start(ToolStartEvent {
            id: "t1".into(),
            tool: "bash",
            summary: "cmd".into(),
        });
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

    #[test_case(WEBFETCH_TOOL_NAME, "line1\nline2\nline3", "(3 lines)" ; "webfetch_shows_line_count")]
    #[test_case(GLOB_TOOL_NAME, "src/a.rs\nsrc/b.rs\nsrc/c.rs", "(3 files)" ; "glob_shows_file_count")]
    #[test_case(GREP_TOOL_NAME, "src/a.rs:\n  1: match\nsrc/b.rs:\n  2: match", "(2 files)" ; "grep_shows_file_count")]
    fn tool_done_summary_annotation(tool: &'static str, output: &str, expected: &str) {
        let mut panel = MessagesPanel::new();
        panel.tool_start(ToolStartEvent {
            id: "t1".into(),
            tool,
            summary: "s".into(),
        });
        panel.tool_done(ToolDoneEvent {
            id: "t1".into(),
            tool,
            output: ToolOutput::Plain(output.into()),
            is_error: false,
        });
        assert!(panel.messages[0].text.contains(expected));
    }

    #[test]
    fn tool_start_flushes_streaming_text() {
        let mut panel = MessagesPanel::new();
        panel.streaming_text.set_buffer("partial response");

        panel.tool_start(ToolStartEvent {
            id: "t1".into(),
            tool: "read",
            summary: "/tmp/file".into(),
        });

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

    #[test_case(10, 'u', 0  ; "ctrl_u_saturates_at_zero")]
    #[test_case(20, 'u', 10 ; "ctrl_u_scrolls_up")]
    #[test_case(5,  'd', 15 ; "ctrl_d_scrolls_down")]
    #[test_case(0,  'd', 10 ; "ctrl_d_from_top")]
    fn half_page_scroll(initial: u16, key_char: char, expected: u16) {
        let mut panel = MessagesPanel::new();
        panel.viewport_height = 20;
        panel.scroll_top = initial;
        let half = panel.half_page();
        let delta = if key_char == 'u' { half } else { -half };
        panel.scroll(delta);
        assert_eq!(panel.scroll_top, expected);
    }

    #[test]
    fn scroll_top_clamped_to_content() {
        let mut panel = MessagesPanel::new();
        panel.push(DisplayMessage {
            role: DisplayRole::User,
            text: "short".into(),
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

    fn render(panel: &mut MessagesPanel, width: u16, height: u16) {
        let backend = TestBackend::new(width, height);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal.draw(|f| panel.view(f, f.area())).unwrap();
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
    fn tool_done_appends_truncated_output() {
        let mut panel = MessagesPanel::new();
        panel.tool_start(ToolStartEvent {
            id: "t1".into(),
            tool: "bash",
            summary: "long output".into(),
        });
        let long_output = (1..=10)
            .map(|i| format!("line{i}"))
            .collect::<Vec<_>>()
            .join("\n");
        panel.tool_done(ToolDoneEvent {
            id: "t1".into(),
            tool: "bash",
            output: ToolOutput::Plain(long_output),
            is_error: false,
        });
        assert_eq!(panel.messages.len(), 1);
        assert!(
            panel.messages[0].text.contains('\n'),
            "body should be appended"
        );
        assert!(
            panel.messages[0].text.contains("..."),
            "long output should be truncated"
        );
    }

    #[test]
    fn tool_done_empty_output_no_body() {
        let mut panel = MessagesPanel::new();
        panel.tool_start(ToolStartEvent {
            id: "t1".into(),
            tool: "edit",
            summary: "/tmp/f.rs".into(),
        });
        panel.tool_done(ToolDoneEvent {
            id: "t1".into(),
            tool: "edit",
            output: ToolOutput::Plain(String::new()),
            is_error: false,
        });
        assert_eq!(panel.messages.len(), 1);
        assert!(
            !panel.messages[0].text.contains('\n'),
            "empty output should not append body"
        );
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
    fn multiple_in_progress_tools_tracked() {
        let mut panel = MessagesPanel::new();
        panel.tool_start(ToolStartEvent {
            id: "t1".into(),
            tool: "bash",
            summary: "a".into(),
        });
        panel.tool_start(ToolStartEvent {
            id: "t2".into(),
            tool: "read",
            summary: "b".into(),
        });
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
}

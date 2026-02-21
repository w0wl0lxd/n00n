use super::{DisplayMessage, DisplayRole, ToolStatus};

use crate::animation::{Typewriter, spinner_frame};
use crate::highlight::{self, CodeHighlighter};
use crate::markdown::{text_to_lines, truncate_lines};
use crate::theme;

use std::time::Instant;

use maki_agent::tools::{GLOB_TOOL_NAME, GREP_TOOL_NAME, WEBFETCH_TOOL_NAME};
use maki_providers::{DiffLine, DiffSpan, ToolDoneEvent, ToolInput, ToolOutput, ToolStartEvent};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState, Wrap};

const TOOL_INDICATOR: &str = "● ";
const TOOL_OUTPUT_MAX_DISPLAY_LINES: usize = 7;
const TOOL_BODY_INDENT: &str = "  ";
const SCROLLBAR_THUMB: &str = "\u{2590}";

#[derive(Default)]
struct StreamingCache {
    byte_len: usize,
    lines: Vec<Line<'static>>,
    highlighters: Vec<CodeHighlighter>,
    dim: bool,
}

impl StreamingCache {
    fn get_or_update(&mut self, visible: &str, prefix: &str, style: Style) -> &[Line<'static>] {
        let len = visible.len();
        if len != self.byte_len || self.lines.is_empty() {
            self.lines = text_to_lines(visible, prefix, style, Some(&mut self.highlighters));
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

impl Segment {
    fn is_tool(&self) -> bool {
        self.tool_id.is_some()
    }
}

fn merge_syntax_with_diff(
    syntax_spans: &[(Style, String)],
    diff_spans: &[DiffSpan],
    base: Style,
    emphasis: Style,
) -> Vec<Span<'static>> {
    let mut result = Vec::new();
    let mut syn_off = 0;
    let mut syn_idx = 0;
    let mut diff_off = 0;
    let mut diff_idx = 0;

    while syn_idx < syntax_spans.len() {
        let (ref syn_style, ref syn_text) = syntax_spans[syn_idx];
        let syn_rem = &syn_text[syn_off..];
        if syn_rem.is_empty() {
            syn_idx += 1;
            syn_off = 0;
            continue;
        }

        let (bg, diff_rem) = if diff_idx < diff_spans.len() {
            let ds = &diff_spans[diff_idx];
            let rem = &ds.text[diff_off..];
            if rem.is_empty() {
                diff_idx += 1;
                diff_off = 0;
                continue;
            }
            let bg = if ds.emphasized { emphasis } else { base };
            (bg, rem.len())
        } else {
            (base, syn_rem.len())
        };

        let take = syn_rem.len().min(diff_rem);
        result.push(Span::styled(
            syn_rem[..take].to_owned(),
            syn_style.patch(bg),
        ));
        syn_off += take;
        diff_off += take;

        if syn_off >= syn_text.len() {
            syn_idx += 1;
            syn_off = 0;
        }
        if diff_idx < diff_spans.len() && diff_off >= diff_spans[diff_idx].text.len() {
            diff_idx += 1;
            diff_off = 0;
        }
    }

    result
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
            .map(|s| (s.lines.as_slice(), s.is_tool()))
            .collect();

        let spacer_line = vec![Line::default()];
        let streaming_sources: [(&Typewriter, &mut StreamingCache, &str, Style); 2] = [
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
        ];
        for (tw, cache, prefix, style) in streaming_sources {
            if tw.is_empty() {
                continue;
            }
            let lines = cache.get_or_update(tw.visible(), prefix, style);
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
                let mut lines = text_to_lines(&msg.text, prefix, base_style, None);
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

    fn build_tool_lines(&self, msg: &DisplayMessage, status: ToolStatus) -> Vec<Line<'static>> {
        let header = msg
            .text
            .split_once('\n')
            .map_or(msg.text.as_str(), |(h, _)| h);
        let tool_name = msg.role.tool_name().unwrap_or("?");
        let prefix = format!("{tool_name}> ");
        let mut lines = vec![Line::from(vec![
            Span::styled(prefix, theme::TOOL_PREFIX),
            Span::styled(header.to_owned(), theme::TOOL),
        ])];

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
            Some(ToolOutput::Diff { path, hunks, .. }) => {
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
                    let mut hl = highlight::highlighter_for_path(path);
                    let mut line_nr = hunk.start_line;
                    for dl in &hunk.lines {
                        let show_nr = !matches!(dl, DiffLine::Added(_));
                        let nr_str = if show_nr {
                            let s = format!("{line_nr:>nr_width$}");
                            line_nr += 1;
                            s
                        } else {
                            " ".repeat(nr_width)
                        };
                        let mut spans = vec![Span::styled(
                            format!("{TOOL_BODY_INDENT}{nr_str} "),
                            theme::DIFF_LINE_NR,
                        )];
                        match dl {
                            DiffLine::Unchanged(t) => {
                                spans.push(Span::raw("  "));
                                let syn = highlight::highlight_line(&mut hl, t);
                                for (style, text) in syn {
                                    spans.push(Span::styled(text, style));
                                }
                            }
                            DiffLine::Removed(ds) | DiffLine::Added(ds) => {
                                let is_add = matches!(dl, DiffLine::Added(_));
                                let (prefix, base, emph) = if is_add {
                                    ("+ ", theme::DIFF_NEW, theme::DIFF_NEW_EMPHASIS)
                                } else {
                                    ("- ", theme::DIFF_OLD, theme::DIFF_OLD_EMPHASIS)
                                };
                                spans.push(Span::styled(prefix, base.fg(theme::FOREGROUND)));
                                let full: String = ds.iter().map(|s| s.text.as_str()).collect();
                                let syn = highlight::highlight_line(&mut hl, &full);
                                spans.extend(merge_syntax_with_diff(&syn, ds, base, emph));
                            }
                        }
                        lines.push(Line::from(spans));
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
            input: None,
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
            input: None,
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
            input: None,
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
    #[test_case(5,  'y', 4  ; "ctrl_y_scrolls_up_one")]
    #[test_case(0,  'y', 0  ; "ctrl_y_saturates_at_zero")]
    #[test_case(5,  'e', 6  ; "ctrl_e_scrolls_down_one")]
    #[test_case(0,  'e', 1  ; "ctrl_e_from_top")]
    fn half_page_scroll(initial: u16, key_char: char, expected: u16) {
        let mut panel = MessagesPanel::new();
        panel.viewport_height = 20;
        panel.scroll_top = initial;
        let half = panel.half_page();
        let delta = match key_char {
            'u' => half,
            'd' => -half,
            'y' => 1,
            'e' => -1,
            _ => unreachable!(),
        };
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

    #[test_case(
        &(1..=10).map(|i| format!("line{i}")).collect::<Vec<_>>().join("\n"),
        true, true
        ; "long_output_truncated"
    )]
    #[test_case("", false, false ; "empty_output_no_body")]
    fn tool_done_output_display(output: &str, expect_newline: bool, expect_ellipsis: bool) {
        let mut panel = MessagesPanel::new();
        panel.tool_start(ToolStartEvent {
            id: "t1".into(),
            tool: "bash",
            summary: "cmd".into(),
            input: None,
        });
        panel.tool_done(ToolDoneEvent {
            id: "t1".into(),
            tool: "bash",
            output: ToolOutput::Plain(output.into()),
            is_error: false,
        });
        assert_eq!(panel.messages.len(), 1);
        assert_eq!(panel.messages[0].text.contains('\n'), expect_newline);
        assert_eq!(panel.messages[0].text.contains("..."), expect_ellipsis);
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
            input: None,
        });
        panel.tool_start(ToolStartEvent {
            id: "t2".into(),
            tool: "read",
            summary: "b".into(),
            input: None,
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

    #[test]
    fn merge_syntax_with_diff_emphasis_split() {
        let base = Style::new().bg(ratatui::style::Color::Red);
        let emph = Style::new().bg(ratatui::style::Color::Green);
        let syn = vec![(
            Style::new().fg(ratatui::style::Color::White),
            "abcde".into(),
        )];
        let diff = vec![
            DiffSpan::plain("abc".into()),
            DiffSpan {
                text: "de".into(),
                emphasized: true,
            },
        ];
        let result = merge_syntax_with_diff(&syn, &diff, base, emph);
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].content.as_ref(), "abc");
        assert_eq!(result[0].style.fg, Some(ratatui::style::Color::White));
        assert_eq!(result[0].style.bg, Some(ratatui::style::Color::Red));
        assert_eq!(result[1].content.as_ref(), "de");
        assert_eq!(result[1].style.bg, Some(ratatui::style::Color::Green));
    }

    #[test]
    fn merge_syntax_with_diff_syntax_longer_than_diff() {
        let base = Style::new().bg(ratatui::style::Color::Red);
        let emph = Style::default();
        let syn = vec![
            (Style::new().fg(ratatui::style::Color::Blue), "ab".into()),
            (Style::new().fg(ratatui::style::Color::Cyan), "cd".into()),
        ];
        let diff = vec![DiffSpan::plain("ab".into())];
        let result = merge_syntax_with_diff(&syn, &diff, base, emph);
        let text: String = result.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(text, "abcd");
    }

    #[test]
    fn merge_syntax_with_diff_interleaved_boundaries() {
        let base = Style::default();
        let emph = Style::new().bg(ratatui::style::Color::Green);
        let syn = vec![
            (Style::new().fg(ratatui::style::Color::Red), "ab".into()),
            (Style::new().fg(ratatui::style::Color::Blue), "cd".into()),
        ];
        let diff = vec![
            DiffSpan::plain("a".into()),
            DiffSpan {
                text: "bcd".into(),
                emphasized: true,
            },
        ];
        let result = merge_syntax_with_diff(&syn, &diff, base, emph);
        let text: String = result.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(text, "abcd");
        assert_eq!(result[0].content.as_ref(), "a");
        assert_eq!(result[0].style.fg, Some(ratatui::style::Color::Red));
        assert_eq!(result[1].content.as_ref(), "b");
        assert_eq!(result[1].style.bg, Some(ratatui::style::Color::Green));
        assert_eq!(result[2].content.as_ref(), "cd");
        assert_eq!(result[2].style.bg, Some(ratatui::style::Color::Green));
    }

    #[test_case("**/*.rs"   ; "double_star_glob")]
    #[test_case("*dir*"     ; "single_star_glob")]
    #[test_case("`backtick`"; "backtick_pattern")]
    fn tool_header_not_markdown_parsed(summary: &str) {
        let mut panel = MessagesPanel::new();
        panel.tool_start(ToolStartEvent {
            id: "t1".into(),
            tool: GLOB_TOOL_NAME,
            summary: summary.into(),
            input: None,
        });
        panel.tool_done(ToolDoneEvent {
            id: "t1".into(),
            tool: GLOB_TOOL_NAME,
            output: ToolOutput::Plain(String::new()),
            is_error: false,
        });
        rebuild(&mut panel);
        let seg = panel.cached_segments.last().unwrap();
        let header = &seg.lines[0];
        let text: String = header.spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(
            text.contains(summary),
            "header should contain raw summary {summary:?}, got {text:?}"
        );
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

    #[test]
    fn segments_cached_across_renders() {
        let mut panel = MessagesPanel::new();
        panel.push(DisplayMessage {
            role: DisplayRole::User,
            text: "hello".into(),
            tool_input: None,
            tool_output: None,
        });

        rebuild(&mut panel);
        let seg_count = panel.cached_segments.len();

        rebuild(&mut panel);
        assert_eq!(panel.cached_segments.len(), seg_count);
        assert_eq!(panel.cached_msg_count, 1);
    }
}

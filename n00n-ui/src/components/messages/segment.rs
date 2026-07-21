use crate::markdown::TRUNCATION_PREFIX;
use crate::render_worker::RenderWorker;
use crate::terminal_image::TerminalImage;
use crate::theme;

use super::super::code_view::SectionFlags;
use super::super::tool_display::{HighlightRequest, ToolLines};
use n00n_agent::{ImageMediaType, ImageSource};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Wrap};
use ratatui_image::picker::Picker;
use std::cell::Cell;

const INST_SUFFIX: &str = "__inst";

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum SegmentIdentity {
    Tool(String),
    Instructions(String),
}

fn media_type_label(media_type: ImageMediaType) -> &'static str {
    match media_type {
        ImageMediaType::Png => "png",
        ImageMediaType::Jpeg => "jpeg",
        ImageMediaType::Gif => "gif",
        ImageMediaType::Webp => "webp",
    }
}

pub fn is_instruction_segment(id: &str) -> bool {
    id.ends_with(INST_SUFFIX)
}

pub fn instruction_id(parent_id: &str) -> String {
    format!("{parent_id}{INST_SUFFIX}")
}

pub fn instruction_parent(id: &str) -> Option<&str> {
    id.strip_suffix(INST_SUFFIX)
}

#[derive(Clone, Copy, Default)]
struct CachedHeight {
    at_width: u16,
    height: u16,
}

#[derive(Default, PartialEq, Eq)]
struct HighlightKey {
    has_output: bool,
}

impl HighlightKey {
    fn from_request(hl: Option<&HighlightRequest>) -> Self {
        Self {
            has_output: hl.is_some_and(|h| h.output.is_some()),
        }
    }
}

#[derive(Clone, Copy, Default, PartialEq, Eq)]
pub(super) enum Surface {
    #[default]
    Plain,
    User,
    Assistant,
    Tool,
}

#[derive(Default)]
pub(super) struct Segment {
    lines: Vec<Line<'static>>,
    pub search_text: String,
    pub raw_text: Option<String>,
    /// Visual width of the prefix added to every rendered line (e.g. "n00n> ").
    /// Used when copying a selection back to the original source text.
    pub prefix_width: u16,
    pub tool_id: Option<String>,
    pub identity: Option<SegmentIdentity>,
    /// Backlink to `self.messages`, set only by `with_lines`. A click on a
    /// collapsed thinking indicator has no `tool_id` to route by, so this is
    /// how the click finds its message. It looks unused; delete it and the
    /// `show_thinking` toggle breaks.
    pub msg_index: Option<usize>,
    pub truncation: SectionFlags,
    cached_height: Cell<Option<CachedHeight>>,
    pending_highlight: Option<u64>,
    highlight_range: Option<(usize, usize)>,
    highlight_key: HighlightKey,
    pub spinner_lines: Vec<(usize, usize)>,
    snapshot_base: Option<usize>,
    pub content_indent: &'static str,
    surface: Surface,
    image: Option<TerminalImage>,
}

impl Segment {
    pub fn with_tool(tool_id: String) -> Self {
        Self {
            identity: Some(SegmentIdentity::Tool(tool_id.clone())),
            tool_id: Some(tool_id),
            surface: Surface::Tool,
            ..Self::default()
        }
    }

    pub fn with_instructions(parent_id: String) -> Self {
        let tool_id = format!("{parent_id}{INST_SUFFIX}");
        Self {
            identity: Some(SegmentIdentity::Instructions(parent_id)),
            tool_id: Some(tool_id),
            surface: Surface::Tool,
            ..Self::default()
        }
    }

    pub fn spacer() -> Self {
        Self {
            lines: vec![Line::default()],
            ..Self::default()
        }
    }

    pub fn with_lines(
        lines: Vec<Line<'static>>,
        search_text: String,
        raw_text: Option<String>,
        prefix_width: u16,
        msg_index: Option<usize>,
    ) -> Self {
        Self {
            lines,
            search_text,
            raw_text,
            prefix_width,
            msg_index,
            ..Self::default()
        }
    }

    pub fn with_image(
        source: &ImageSource,
        picker: &Picker,
        width: u16,
        surface: Surface,
        msg_index: Option<usize>,
    ) -> Self {
        let mut seg = match TerminalImage::from_source(source, picker, width) {
            Ok(term_img) => {
                let search_text = format!(
                    "[image: {} {}x{}]",
                    media_type_label(source.media_type),
                    term_img.size.width,
                    term_img.size.height
                );
                Self {
                    search_text: search_text.clone(),
                    raw_text: Some(search_text),
                    msg_index,
                    image: Some(term_img),
                    ..Self::default()
                }
            }
            Err(err) => {
                let search_text = format!("[image: {err}]");
                let lines = vec![Line::from(vec![Span::styled(
                    search_text.clone(),
                    theme::current().error,
                )])];
                Self::with_lines(lines, search_text.clone(), Some(search_text), 0, msg_index)
            }
        };
        seg.set_surface(surface);
        seg
    }

    pub fn lines(&self) -> &[Line<'static>] {
        &self.lines
    }

    pub fn image(&self) -> Option<&TerminalImage> {
        self.image.as_ref()
    }

    pub fn set_surface(&mut self, surface: Surface) {
        self.surface = surface;
        self.invalidate_height();
    }

    pub fn surface(&self) -> Surface {
        self.surface
    }

    pub fn has_copy_label_room(&self, width: u16, label_width: u16) -> bool {
        width >= label_width
            && self
                .lines
                .first()
                .is_none_or(|line| line.width() <= usize::from(width - label_width))
    }
}

impl Surface {
    pub fn is_framed(self) -> bool {
        matches!(self, Self::User | Self::Tool)
    }

    pub fn content_width(self, width: u16) -> u16 {
        width
            .saturating_sub(if self.is_framed() { 4 } else { 0 })
            .max(1)
    }
}

impl Segment {
    pub fn copy_payload(&self) -> Option<(&str, &'static str)> {
        let raw = self.raw_text.as_deref()?;
        if let Some(code) = n00n_markdown::single_code_block(raw) {
            Some((code, "code"))
        } else {
            Some((raw, "markdown"))
        }
    }

    pub fn content_inset(&self) -> u16 {
        0
    }

    fn content_width(&self, width: u16) -> u16 {
        self.surface.content_width(width)
    }

    pub fn set_lines(&mut self, lines: Vec<Line<'static>>) {
        self.lines = lines;
        self.invalidate_height();
    }

    pub fn height(&self, width: u16) -> u16 {
        if let Some(img) = &self.image {
            return img.size.height;
        }
        if let Some(c) = self.cached_height.get()
            && c.at_width == width
        {
            return c.height;
        }
        let h = wrapped_line_count(&self.lines, self.content_width(width));
        self.cached_height.set(Some(CachedHeight {
            at_width: width,
            height: h,
        }));
        h
    }

    /// Maps a display row (after wrapping) back to the source line index.
    pub fn source_line_at(&self, rel_row: u16, width: u16) -> Option<usize> {
        if self.image.is_some() {
            return None;
        }
        let rel_row = rel_row.saturating_sub(self.content_inset() / 2);
        let width = self.content_width(width);
        let mut acc = 0u16;
        for (i, line) in self.lines.iter().enumerate() {
            acc = acc.saturating_add(wrapped_line_count(std::slice::from_ref(line), width));
            if rel_row < acc {
                return Some(i);
            }
        }
        None
    }

    /// Maps a source line to a 1-based row in the tool's live buffer, or 0
    /// for lines outside it (header etc.). The Lua click-row contract is
    /// computed here and nowhere else, from the base recorded when the
    /// buffer snapshot was laid out.
    pub fn buf_row(&self, source_line: usize) -> usize {
        match self.snapshot_base {
            Some(base) if source_line >= base => source_line - base + 1,
            _ => 0,
        }
    }

    pub fn is_snapshot_line(&self, source_line: usize) -> bool {
        self.snapshot_base.is_some_and(|base| source_line >= base)
    }

    pub fn is_truncation_line(&self, source_line: usize) -> bool {
        let Some(line) = self.lines.get(source_line) else {
            return false;
        };
        let text: String = line
            .spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect();
        text.contains(TRUNCATION_PREFIX) && text.contains('›')
    }

    fn invalidate_height(&self) {
        self.cached_height.set(None);
    }

    pub fn update_spinners(&mut self, span: &Span<'static>) {
        for &(line_idx, span_idx) in &self.spinner_lines {
            if let Some(line) = self.lines.get_mut(line_idx)
                && line.spans.len() > span_idx
            {
                line.spans[span_idx] = span.clone();
            }
        }
    }

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

    pub fn apply_highlight(&mut self, tl: ToolLines, worker: &RenderWorker) {
        self.pending_highlight = tl.send_highlight(worker);
        self.highlight_range = tl.highlight.as_ref().map(|h| h.range);
        self.highlight_key = HighlightKey::from_request(tl.highlight.as_ref());
        self.spinner_lines = tl.spinner_lines;
        self.snapshot_base = tl.snapshot_base;
        self.content_indent = tl.content_indent;
        self.truncation = tl.truncation;
        self.set_lines(tl.lines);
    }

    pub fn update_with_reuse(&mut self, mut tl: ToolLines, worker: &RenderWorker) {
        let key = HighlightKey::from_request(tl.highlight.as_ref());
        let reused = tl.highlight.as_ref().and_then(|req| {
            let hl_lines = self.reuse_highlight(&key, req.range)?;
            let (s, _) = req.range;
            let new_end = s + hl_lines.len();
            tl.lines.splice(s..req.range.1, hl_lines);
            Some((s, new_end))
        });
        self.truncation = tl.truncation;
        if let Some((s, e)) = reused {
            self.set_lines(tl.lines);
            self.highlight_range = Some((s, e));
            self.pending_highlight = None;
            self.spinner_lines = tl.spinner_lines;
            self.snapshot_base = tl.snapshot_base;
            self.content_indent = tl.content_indent;
        } else {
            self.apply_highlight(tl, worker);
        }
    }

    pub fn matches_pending_highlight(&self, id: u64) -> bool {
        self.pending_highlight == Some(id)
    }

    pub fn apply_highlight_result(&mut self, lines: Vec<Line<'static>>) {
        if let Some((start, end)) = self.highlight_range {
            let indent = self.content_indent;
            let indented: Vec<Line<'static>> = lines
                .into_iter()
                .map(|mut line| {
                    if !indent.is_empty() {
                        line.spans.insert(0, Span::raw(indent));
                    }
                    line
                })
                .collect();
            let new_end = start + indented.len();
            self.lines.splice(start..end, indented);
            self.highlight_range = Some((start, new_end));
            self.shift_after(end, new_end as isize - end as isize);
            self.invalidate_height();
        }
        self.pending_highlight = None;
    }

    /// Keeps recorded line positions (spinners, buffer base) in step when
    /// a splice changes the number of lines before them.
    fn shift_after(&mut self, from: usize, delta: isize) {
        if delta == 0 {
            return;
        }
        let shift = |v: &mut usize| {
            if *v >= from {
                *v = v.saturating_add_signed(delta);
            }
        };
        for (line, _) in &mut self.spinner_lines {
            shift(line);
        }
        if let Some(base) = &mut self.snapshot_base {
            shift(base);
        }
    }
}

pub(super) struct SegmentCache {
    segments: Vec<Segment>,
    msg_count: usize,
}

impl SegmentCache {
    pub fn new() -> Self {
        Self {
            segments: Vec::new(),
            msg_count: 0,
        }
    }

    pub fn clear(&mut self) {
        self.segments.clear();
        self.msg_count = 0;
    }

    pub fn push(&mut self, seg: Segment) {
        self.segments.push(seg);
    }

    pub fn push_spacer_if_needed(&mut self) {
        if !self.segments.is_empty() {
            self.segments.push(Segment::spacer());
        }
    }

    pub fn insert(&mut self, pos: usize, seg: Segment) {
        self.segments.insert(pos, seg);
    }

    /// Inserts `segs` before the existing segments, shifting every
    /// `msg_index` backlink by `shift` so it still points at the right
    /// message after older messages are prepended at the front.
    pub fn prepend(&mut self, mut segs: Vec<Segment>, shift: usize) {
        if shift > 0 {
            for seg in &mut self.segments {
                if let Some(ref mut idx) = seg.msg_index {
                    *idx += shift;
                }
            }
        }
        segs.append(&mut self.segments);
        self.segments = segs;
        self.msg_count += shift;
    }

    pub fn needs_rebuild(&self, msg_len: usize) -> bool {
        self.msg_count != msg_len
    }

    pub fn mark_built(&mut self, count: usize) {
        self.msg_count = count;
    }

    pub fn msg_count(&self) -> usize {
        self.msg_count
    }

    pub fn total_height(&self, width: u16) -> u32 {
        self.segments
            .iter()
            .map(|s| u32::from(s.height(width)))
            .sum()
    }

    pub fn segment_at_row(&self, doc_row: u32, width: u16) -> Option<(usize, &Segment, u32)> {
        let mut cumulative: u32 = 0;
        for (i, seg) in self.segments.iter().enumerate() {
            let seg_start = cumulative;
            cumulative += u32::from(seg.height(width));
            if doc_row < cumulative {
                return Some((i, seg, seg_start));
            }
        }
        None
    }

    pub fn segments(&self) -> &[Segment] {
        &self.segments
    }

    pub fn segments_mut(&mut self) -> &mut [Segment] {
        &mut self.segments
    }

    pub fn get(&self, idx: usize) -> Option<&Segment> {
        self.segments.get(idx)
    }

    pub fn get_mut(&mut self, idx: usize) -> Option<&mut Segment> {
        self.segments.get_mut(idx)
    }

    pub fn find_by_tool_id(&self, id: &str) -> Option<usize> {
        self.segments.iter().rposition(
            |s| matches!(&s.identity, Some(SegmentIdentity::Tool(tool_id)) if tool_id == id),
        )
    }

    pub fn find_instructions(&self, parent_id: &str) -> Option<usize> {
        self.segments.iter().rposition(
            |s| matches!(&s.identity, Some(SegmentIdentity::Instructions(id)) if id == parent_id),
        )
    }

    pub fn len(&self) -> usize {
        self.segments.len()
    }

    pub fn search_texts(&self) -> Vec<&str> {
        self.segments
            .iter()
            .map(|s| s.search_text.as_str())
            .collect()
    }

    pub fn invalidate_from_msg_count(&mut self) {
        self.msg_count = 0;
        self.segments.clear();
    }
}

pub(crate) fn wrapped_line_count(lines: &[Line<'_>], width: u16) -> u16 {
    if width == 0 {
        return lines.len() as u16;
    }
    Paragraph::new(lines.to_vec())
        .wrap(Wrap { trim: false })
        .line_count(width) as u16
}

#[cfg(test)]
mod tests {
    use super::*;
    use test_case::test_case;

    fn seg_with_base(line_count: usize, base: Option<usize>) -> Segment {
        Segment {
            lines: (0..line_count)
                .map(|i| Line::raw(format!("l{i}")))
                .collect(),
            snapshot_base: base,
            ..Segment::default()
        }
    }

    #[test_case(0, 0 ; "header_maps_to_zero")]
    #[test_case(1, 1 ; "first_snapshot_line_is_row_one")]
    #[test_case(4, 4 ; "later_line_offsets_from_base")]
    fn buf_row_maps_source_lines_through_snapshot_base(source_line: usize, expected: usize) {
        let seg = seg_with_base(5, Some(1));
        assert_eq!(seg.buf_row(source_line), expected);
    }

    #[test]
    fn buf_row_is_zero_without_snapshot() {
        let seg = seg_with_base(3, None);
        assert_eq!(seg.buf_row(2), 0);
    }

    #[test]
    fn buf_row_tracks_base_when_lines_precede_snapshot() {
        let seg = seg_with_base(6, Some(3));
        assert_eq!(seg.buf_row(2), 0, "pre-snapshot lines map outside the buf");
        assert_eq!(seg.buf_row(3), 1);
        assert_eq!(seg.buf_row(5), 3);
    }

    #[test_case(4, 6 ; "splice_grows")]
    #[test_case(1, 3 ; "splice_shrinks")]
    fn highlight_splice_shifts_spinners_and_base(replacement_lines: usize, expected_base: usize) {
        let mut seg = seg_with_base(8, Some(4));
        seg.highlight_range = Some((1, 3));
        seg.spinner_lines = vec![(0, 0), (5, 1)];
        seg.apply_highlight_result((0..replacement_lines).map(|_| Line::raw("hl")).collect());
        let delta = expected_base as isize - 4;
        assert_eq!(seg.snapshot_base, Some(expected_base));
        assert_eq!(
            seg.spinner_lines,
            vec![(0, 0), (5usize.saturating_add_signed(delta), 1)],
            "positions before the splice stay, after it shift by the delta"
        );
    }
}

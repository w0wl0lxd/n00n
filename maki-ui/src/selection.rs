//! Mouse selection + clipboard copy.
//!
//! We call `EnableMouseCapture` for scroll events, which kills the terminal's
//! native text selection. This module reimplements it.
//!
//! Key design decisions:
//!
//! - Selection stores positions in doc space (`DocPos`), not screen space.
//!   Screen positions go stale on scroll; doc positions don't.
//!
//! - Copy happens inside `view()`, not on mouse-up. The terminal buffer only
//!   has valid cell data during rendering.
//!
//! - Fully-selected segments use `copy_text` (raw markdown/structured output)
//!   instead of scraping cells. Partial selections fall back to cell scraping.
//!   This preserves headings, blank lines, diffs, etc. that rendering strips.
//!
//! - `has_selection` freezes auto-scroll in `MessagesPanel::view()` so the
//!   viewport doesn't jump while the user is dragging.
//!
//! - Content is rendered 1 column narrower than the area to reserve space for
//!   the scrollbar. `highlight_area` and `msg_area()` reflect this content
//!   width. `apply_highlight` and `append_rows` use `area.width - 1` for the
//!   rightmost content column index.

use std::cmp::Ordering;
use std::time::Instant;

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Modifier;
use ratatui::text::Line;
use ratatui::widgets::{Paragraph, Wrap};

use crate::markdown::{CODE_BAR, CODE_BAR_WRAP};
use crate::theme;

/// Position in doc space (full logical document, not just visible window).
/// Stored as (row, col) where col is a screen x coordinate.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DocPos {
    pub row: u32,
    pub col: u16,
}

impl DocPos {
    fn new(row: u32, col: u16) -> Self {
        Self { row, col }
    }
}

impl PartialOrd for DocPos {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for DocPos {
    fn cmp(&self, other: &Self) -> Ordering {
        (self.row, self.col).cmp(&(other.row, other.col))
    }
}

/// Selection is locked to one zone for its entire lifetime.
///
/// Variant order matters: higher index = higher z-order priority in `zone_at`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SelectionZone {
    Messages,
    Input,
    StatusBar,
    Overlay,
}

impl SelectionZone {
    pub const COUNT: usize = 4;

    pub const fn idx(self) -> usize {
        self as usize
    }
}

#[derive(Clone, Copy, Debug)]
pub struct SelectableZone {
    pub area: Rect,
    pub highlight_area: Rect,
    pub zone: SelectionZone,
}

pub type ZoneRegistry = [Option<SelectableZone>; SelectionZone::COUNT];

/// Returns the zone at `(row, col)`, preferring higher-index (higher z-order) zones.
pub fn zone_at(zones: &ZoneRegistry, row: u16, col: u16) -> Option<SelectableZone> {
    let pos = ratatui::layout::Position::new(col, row);
    zones
        .iter()
        .rev()
        .flatten()
        .find(|z| z.area.contains(pos))
        .copied()
}

/// Anchor + cursor in doc space. `area` and `zone` are captured at mouse-down
/// and stay fixed so layout changes mid-drag don't break the selection.
#[derive(Clone, Copy, Debug)]
pub struct Selection {
    anchor: DocPos,
    cursor: DocPos,
    pub area: Rect,
    pub zone: SelectionZone,
}

fn screen_to_doc(screen_row: u16, area: Rect, scroll_offset: u32) -> u32 {
    let clamped = screen_row.clamp(area.y, area.y + area.height.saturating_sub(1));
    scroll_offset + (clamped - area.y) as u32
}

fn clamp_col(col: u16, area: Rect) -> u16 {
    col.clamp(area.x, area.x + area.width.saturating_sub(1))
}

impl Selection {
    pub fn start(row: u16, col: u16, area: Rect, zone: SelectionZone, scroll_offset: u32) -> Self {
        let doc_row = screen_to_doc(row, area, scroll_offset);
        let doc_col = clamp_col(col, area);
        let pos = DocPos::new(doc_row, doc_col);
        Self {
            anchor: pos,
            cursor: pos,
            area,
            zone,
        }
    }

    pub fn update(&mut self, row: u16, col: u16, scroll_offset: u32) {
        self.cursor = DocPos::new(
            screen_to_doc(row, self.area, scroll_offset),
            clamp_col(col, self.area),
        );
    }

    pub fn is_empty(&self) -> bool {
        self.anchor == self.cursor
    }

    pub fn normalized(&self) -> (DocPos, DocPos) {
        if self.anchor <= self.cursor {
            (self.anchor, self.cursor)
        } else {
            (self.cursor, self.anchor)
        }
    }

    pub fn to_screen(self, scroll_offset: u32) -> Option<ScreenSelection> {
        let (start, end) = self.normalized();
        if start == end {
            return None;
        }

        let view_top = scroll_offset;
        let view_bottom = scroll_offset + self.area.height as u32;

        if end.row < view_top || start.row >= view_bottom {
            return None;
        }

        let project_row = |doc_row: u32| -> u16 {
            if doc_row < view_top {
                self.area.y
            } else if doc_row >= view_bottom {
                self.area.y + self.area.height.saturating_sub(1)
            } else {
                self.area.y + (doc_row - view_top) as u16
            }
        };

        let start_row = project_row(start.row);
        let start_col = if start.row < view_top {
            self.area.x
        } else {
            start.col
        };
        let end_row = project_row(end.row);
        let end_col = if end.row >= view_bottom {
            self.area.x + self.area.width.saturating_sub(1)
        } else {
            end.col
        };

        Some(ScreenSelection {
            start_row,
            start_col,
            end_row,
            end_col,
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ScreenSelection {
    pub start_row: u16,
    pub start_col: u16,
    pub end_row: u16,
    pub end_col: u16,
}

pub struct EdgeScroll {
    pub dir: i32,
    pub last_tick: Instant,
}

/// `copy_on_release`: set on mouse-up, consumed in next `view()`. We can't
/// copy on mouse-up because the terminal buffer is only valid during rendering.
/// `last_drag_col`: remembered for edge-scroll ticks that lack mouse coords.
pub struct SelectionState {
    pub sel: Selection,
    pub copy_on_release: bool,
    pub edge_scroll: Option<EdgeScroll>,
    pub last_drag_col: u16,
}

#[derive(Clone, Debug, Default)]
pub enum LineBreaks {
    #[default]
    EveryRow,
    Bitmap(Vec<u64>),
}

impl LineBreaks {
    pub fn from_heights(heights: impl Iterator<Item = u16>) -> Self {
        let mut bits = Vec::new();
        let mut row: u16 = 0;
        for h in heights {
            if h == 0 {
                continue;
            }
            let idx = (row / 64) as usize;
            if idx >= bits.len() {
                bits.resize(idx + 1, 0u64);
            }
            bits[idx] |= 1 << (row % 64);
            row = row.saturating_add(h);
        }
        Self::Bitmap(bits)
    }

    pub fn from_lines(lines: &[Line<'_>], width: u16) -> Self {
        if width == 0 {
            return Self::EveryRow;
        }
        let mut heights = Vec::with_capacity(lines.len());
        for line in lines {
            if is_code_wrap_continuation(line)
                && let Some(last) = heights.last_mut()
            {
                *last += 1;
                continue;
            }
            let h = Paragraph::new(vec![line.clone()])
                .wrap(Wrap { trim: false })
                .line_count(width) as u16;
            heights.push(h);
        }
        Self::from_heights(heights.into_iter())
    }

    pub fn is_line_start(&self, row: u16) -> bool {
        match self {
            Self::EveryRow => true,
            Self::Bitmap(bits) => bits
                .get((row / 64) as usize)
                .is_some_and(|word| word & (1 << (row % 64)) != 0),
        }
    }
}

fn is_code_wrap_continuation(line: &Line<'_>) -> bool {
    line.spans
        .first()
        .is_some_and(|s| s.content.as_ref() == CODE_BAR_WRAP)
}

/// Screen region + optional raw source text for copy. If `raw_text` is
/// non-empty and the region is fully selected, raw text is used as-is.
#[derive(Default)]
pub struct ContentRegion<'a> {
    pub area: Rect,
    pub raw_text: &'a str,
    pub line_breaks: LineBreaks,
}

pub fn inset_border(area: Rect) -> Rect {
    Rect::new(
        area.x + 1,
        area.y + 1,
        area.width.saturating_sub(2),
        area.height.saturating_sub(2),
    )
}

#[inline]
pub(crate) fn col_range(ss: &ScreenSelection, left: u16, right: u16, row: u16) -> (u16, u16) {
    let col_start = if row == ss.start_row {
        ss.start_col.max(left)
    } else {
        left
    };
    let col_end = if row == ss.end_row {
        ss.end_col.min(right)
    } else {
        right
    };
    (col_start, col_end)
}

/// Flips `REVERSED` on selected cells. Skips last column (scrollbar).
pub fn apply_highlight(buf: &mut Buffer, area: Rect, ss: &ScreenSelection) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let row_start = ss.start_row.max(area.y);
    let row_end = ss.end_row.min(area.bottom().saturating_sub(1));
    let right = area.x + area.width.saturating_sub(1);
    for row in row_start..=row_end {
        let (col_start, col_end) = col_range(ss, area.x, right, row);
        for col in col_start..=col_end {
            if col >= buf.area().right() || row >= buf.area().bottom() {
                continue;
            }
            let cell = &mut buf[(col, row)];
            cell.set_style(cell.style().add_modifier(Modifier::REVERSED));
        }
    }
}

pub(crate) fn strip_code_bar_prefix(
    cell: &ratatui::buffer::Cell,
    out: &mut String,
    line_start: usize,
) {
    if cell.style().fg != theme::current().code_bar.fg || cell.symbol() != "│" {
        return;
    }
    let line = &out[line_start..];
    let prefix_len = if line.starts_with(CODE_BAR) {
        CODE_BAR.len()
    } else if line.starts_with(CODE_BAR_WRAP) {
        CODE_BAR_WRAP.len()
    } else {
        return;
    };
    out.drain(line_start..line_start + prefix_len);
}

/// Trailing whitespace trimmed per line; consecutive trailing blank lines
/// collapsed via `pending_newlines`.
pub(crate) fn append_rows(
    buf: &Buffer,
    area: Rect,
    ss: &ScreenSelection,
    from: u16,
    to: u16,
    out: &mut String,
    breaks: &LineBreaks,
) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let right = area.x + area.width.saturating_sub(1);
    let row_start = from.max(area.y);
    let row_end = to.min(area.bottom());
    let mut pending_newlines = 0u16;
    let anchor = out.len();
    for row in row_start..row_end {
        let (col_start, col_end) = col_range(ss, area.x, right, row);
        let line_start = out.len();
        for col in col_start..=col_end {
            out.push_str(buf[(col, row)].symbol());
        }
        if col_start == area.x {
            strip_code_bar_prefix(&buf[(col_start, row)], out, line_start);
        }
        let trimmed_len = out[line_start..].trim_end().len() + line_start;
        out.truncate(trimmed_len);
        let is_new_line = breaks.is_line_start(row - area.y);
        if out.len() == line_start && out.len() > anchor {
            if is_new_line {
                pending_newlines += 1;
            }
        } else if out.len() > anchor && is_new_line {
            for _ in 0..pending_newlines {
                out.insert(line_start, '\n');
            }
            pending_newlines = 0;
            if line_start > anchor {
                out.insert(line_start, '\n');
            }
        }
    }
}

/// Regions searched in reverse (overlays win). Uncovered rows skipped.
pub fn extract_selected_text(
    buf: &Buffer,
    ss: &ScreenSelection,
    regions: &[ContentRegion<'_>],
) -> String {
    let mut out = String::new();
    let mut row = ss.start_row;

    while row <= ss.end_row {
        let region = regions
            .iter()
            .rev()
            .find(|r| r.area.y <= row && row < r.area.bottom());

        let Some(region) = region else {
            row += 1;
            continue;
        };

        let region_start = region.area.y;
        let region_end = region.area.bottom();
        let fully_selected = region_start >= ss.start_row && region_end <= ss.end_row + 1;

        if !out.is_empty() {
            out.push('\n');
        }
        if fully_selected && !region.raw_text.is_empty() {
            out.push_str(region.raw_text);
        } else {
            let chunk_end = region_end.min(ss.end_row + 1);
            append_rows(
                buf,
                region.area,
                ss,
                row,
                chunk_end,
                &mut out,
                &region.line_breaks,
            );
        }
        row = region_end;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::buffer::Buffer;
    use ratatui::layout::Rect;
    use ratatui::style::{Modifier, Style};
    use test_case::test_case;

    fn doc(row: u32, col: u16) -> DocPos {
        DocPos::new(row, col)
    }

    #[test_case(doc(0, 0), doc(5, 10), (doc(0, 0), doc(5, 10)) ; "forward_selection")]
    #[test_case(doc(5, 10), doc(0, 0), (doc(0, 0), doc(5, 10)) ; "backward_selection")]
    #[test_case(doc(3, 5), doc(3, 5), (doc(3, 5), doc(3, 5))   ; "same_point")]
    fn normalized(a: DocPos, c: DocPos, expected: (DocPos, DocPos)) {
        let sel = Selection {
            anchor: a,
            cursor: c,
            area: Rect::default(),
            zone: SelectionZone::Messages,
        };
        assert_eq!(sel.normalized(), expected);
    }

    fn test_buffer() -> (Buffer, Rect) {
        let area = Rect::new(0, 0, 10, 3);
        let mut buf = Buffer::empty(area);
        buf.set_string(0, 0, "Hello     ", ratatui::style::Style::default());
        buf.set_string(0, 1, "World     ", ratatui::style::Style::default());
        buf.set_string(0, 2, "Test      ", ratatui::style::Style::default());
        (buf, area)
    }

    fn ss(sr: u16, sc: u16, er: u16, ec: u16) -> ScreenSelection {
        ScreenSelection {
            start_row: sr,
            start_col: sc,
            end_row: er,
            end_col: ec,
        }
    }

    #[test]
    fn extract_single_region_partial() {
        let (buf, area) = test_buffer();
        let region = ContentRegion {
            area,
            raw_text: "# Hello\n\nWorld\nTest",
            ..Default::default()
        };
        let text = extract_selected_text(&buf, &ss(0, 0, 0, 4), &[region]);
        assert_eq!(text, "Hello");
    }

    #[test]
    fn extract_single_region_fully_selected_uses_raw() {
        let (buf, area) = test_buffer();
        let raw = "# Hello\n\nWorld\nTest";
        let region = ContentRegion {
            area,
            raw_text: raw,
            ..Default::default()
        };
        let text = extract_selected_text(&buf, &ss(0, 0, 2, 9), &[region]);
        assert_eq!(text, raw);
    }

    #[test]
    fn extract_multi_row_partial() {
        let (buf, area) = test_buffer();
        let region = ContentRegion {
            area,
            raw_text: "raw",
            ..Default::default()
        };
        let text = extract_selected_text(&buf, &ss(0, 0, 1, 4), &[region]);
        assert_eq!(text, "Hello\nWorld");
    }

    #[test]
    fn extract_skips_uncovered_rows() {
        let area = Rect::new(0, 0, 10, 5);
        let mut buf = Buffer::empty(area);
        buf.set_string(0, 0, "Line 0    ", ratatui::style::Style::default());
        buf.set_string(0, 1, "──────────", ratatui::style::Style::default());
        buf.set_string(0, 2, "Line 2    ", ratatui::style::Style::default());
        buf.set_string(0, 3, "──────────", ratatui::style::Style::default());
        buf.set_string(0, 4, "Line 4    ", ratatui::style::Style::default());

        let regions = vec![
            ContentRegion {
                area: Rect::new(0, 0, 10, 1),
                raw_text: "Line 0",
                ..Default::default()
            },
            ContentRegion {
                area: Rect::new(0, 2, 10, 1),
                raw_text: "Line 2",
                ..Default::default()
            },
            ContentRegion {
                area: Rect::new(0, 4, 10, 1),
                raw_text: "Line 4",
                ..Default::default()
            },
        ];
        let text = extract_selected_text(&buf, &ss(0, 0, 4, 7), &regions);
        assert_eq!(text, "Line 0\nLine 2\nLine 4");
    }

    #[test]
    fn extract_overlay_wins_over_base() {
        let area = Rect::new(0, 0, 10, 3);
        let mut buf = Buffer::empty(area);
        buf.set_string(0, 0, "base 0    ", ratatui::style::Style::default());
        buf.set_string(0, 1, "overlay 1 ", ratatui::style::Style::default());
        buf.set_string(0, 2, "base 2    ", ratatui::style::Style::default());

        let base = ContentRegion {
            area: Rect::new(0, 0, 10, 3),
            raw_text: "base raw text",
            ..Default::default()
        };
        let overlay = ContentRegion {
            area: Rect::new(0, 0, 10, 3),
            raw_text: "overlay raw text",
            ..Default::default()
        };
        let text = extract_selected_text(&buf, &ss(0, 0, 2, 9), &[base, overlay]);
        assert_eq!(text, "overlay raw text");
    }

    #[test]
    fn extract_multi_region_mixed_full_and_partial() {
        let area = Rect::new(0, 0, 20, 4);
        let mut buf = Buffer::empty(area);
        buf.set_string(
            0,
            0,
            "msg0 rendered       ",
            ratatui::style::Style::default(),
        );
        buf.set_string(
            0,
            1,
            "msg0 line2          ",
            ratatui::style::Style::default(),
        );
        buf.set_string(
            0,
            2,
            "msg1 rendered       ",
            ratatui::style::Style::default(),
        );
        buf.set_string(
            0,
            3,
            "msg1 line2          ",
            ratatui::style::Style::default(),
        );

        let regions = vec![
            ContentRegion {
                area: Rect::new(0, 0, 20, 2),
                raw_text: "# msg0 raw",
                ..Default::default()
            },
            ContentRegion {
                area: Rect::new(0, 2, 20, 2),
                raw_text: "# msg1 raw",
                ..Default::default()
            },
        ];
        let text = extract_selected_text(&buf, &ss(1, 0, 2, 18), &regions);
        assert_eq!(text, "msg0 line2\nmsg1 rendered");
    }

    #[test]
    fn apply_highlight_sets_reversed() {
        let (mut buf, area) = test_buffer();
        let s = ss(0, 0, 0, 2);
        apply_highlight(&mut buf, area, &s);
        for col in 0..=2 {
            assert!(buf[(col, 0u16)].modifier.contains(Modifier::REVERSED));
        }
        assert!(!buf[(3u16, 0u16)].modifier.contains(Modifier::REVERSED));
    }

    #[test]
    fn extract_no_matching_region_returns_empty() {
        let (buf, _) = test_buffer();
        assert_eq!(
            extract_selected_text(&buf, &ss(0, 0, 2, 7), &[]),
            "",
            "no regions at all"
        );

        let region = ContentRegion {
            area: Rect::new(0, 5, 10, 1),
            raw_text: "far away",
            ..Default::default()
        };
        assert_eq!(
            extract_selected_text(&buf, &ss(0, 0, 2, 7), &[region]),
            "",
            "region outside selection range"
        );
    }

    #[test]
    fn fully_selected_empty_raw_text_extracts_from_buffer() {
        let area = Rect::new(0, 0, 10, 1);
        let mut buf = Buffer::empty(area);
        buf.set_string(0, 0, "Status    ", ratatui::style::Style::default());
        let region = ContentRegion {
            area,
            ..Default::default()
        };
        let text = extract_selected_text(&buf, &ss(0, 0, 0, 9), &[region]);
        assert_eq!(text, "Status");
    }

    #[test]
    fn extract_clips_scrollbar_column() {
        let area = Rect::new(0, 0, 10, 1);
        let mut buf = Buffer::empty(area);
        buf.set_string(0, 0, "ABCDEFGHI@", ratatui::style::Style::default());
        let region = ContentRegion {
            area,
            raw_text: "ABCDEFGHI",
            ..Default::default()
        };
        let text = extract_selected_text(&buf, &ss(0, 0, 0, 9), &[region]);
        assert_eq!(text, "ABCDEFGHI");
    }

    #[test]
    fn doc_space_start_computes_doc_row() {
        let msg_area = Rect::new(0, 3, 80, 20);
        let sel = Selection::start(15, 5, msg_area, SelectionZone::Messages, 10);
        let (start, _) = sel.normalized();
        assert_eq!(start.row, 22);
    }

    #[test]
    fn doc_space_update_computes_cursor_doc_row() {
        let msg_area = Rect::new(0, 3, 80, 20);
        let mut sel = Selection::start(15, 5, msg_area, SelectionZone::Messages, 10);
        sel.update(20, 8, 10);
        let (start, end) = sel.normalized();
        assert_eq!(start.row, 22);
        assert_eq!(end.row, 27);
    }

    #[test]
    fn is_empty_uses_doc_rows() {
        let msg_area = Rect::new(0, 0, 80, 20);
        let mut sel = Selection::start(5, 3, msg_area, SelectionZone::Messages, 0);
        assert!(sel.is_empty());
        sel.update(5, 4, 0);
        assert!(!sel.is_empty());
    }

    #[test]
    fn to_screen_fully_visible() {
        let area = Rect::new(0, 0, 80, 20);
        let sel = Selection {
            anchor: doc(5, 2),
            cursor: doc(8, 10),
            area,
            zone: SelectionZone::Messages,
        };
        let screen = sel.to_screen(0).unwrap();
        assert_eq!(screen, ss(5, 2, 8, 10));
    }

    #[test]
    fn to_screen_partially_off_top() {
        let area = Rect::new(0, 0, 80, 20);
        let sel = Selection {
            anchor: doc(2, 5),
            cursor: doc(12, 8),
            area,
            zone: SelectionZone::Messages,
        };
        let screen = sel.to_screen(5).unwrap();
        assert_eq!(screen.start_row, 0);
        assert_eq!(screen.start_col, 0);
        assert_eq!(screen.end_row, 7);
        assert_eq!(screen.end_col, 8);
    }

    #[test]
    fn to_screen_entirely_off_screen() {
        let area = Rect::new(0, 0, 80, 20);
        let sel = Selection {
            anchor: doc(0, 0),
            cursor: doc(3, 5),
            area,
            zone: SelectionZone::Messages,
        };
        assert!(sel.to_screen(10).is_none());
    }

    #[test]
    fn to_screen_empty_selection_returns_none() {
        let area = Rect::new(0, 0, 80, 20);
        let sel = Selection {
            anchor: doc(5, 5),
            cursor: doc(5, 5),
            area,
            zone: SelectionZone::Messages,
        };
        assert!(sel.to_screen(0).is_none());
    }

    #[test]
    fn clamped_doc_row_below_msg_area() {
        let msg_area = Rect::new(0, 2, 80, 10);
        let sel = Selection::start(15, 5, msg_area, SelectionZone::Messages, 0);
        let (start, _) = sel.normalized();
        assert_eq!(start.row, 9, "clamped to last visible doc row");
    }

    #[test]
    fn clamped_doc_row_above_msg_area() {
        let msg_area = Rect::new(0, 5, 80, 10);
        let sel = Selection::start(2, 5, msg_area, SelectionZone::Messages, 7);
        let (start, _) = sel.normalized();
        assert_eq!(start.row, 7, "clamped to scroll_top");
    }

    #[test]
    fn to_screen_anchor_in_area_cursor_below() {
        let msg_area = Rect::new(0, 0, 80, 10);
        let sel = Selection {
            anchor: doc(5, 3),
            cursor: doc(12, 8),
            area: msg_area,
            zone: SelectionZone::Messages,
        };
        let screen = sel.to_screen(0).unwrap();
        assert_eq!(screen.start_row, 5);
        assert_eq!(screen.start_col, 3);
        assert_eq!(screen.end_row, 9);
        assert_eq!(screen.end_col, 79);
    }

    #[test]
    fn to_screen_backward_from_below() {
        let msg_area = Rect::new(0, 0, 80, 10);
        let sel = Selection {
            anchor: doc(12, 5),
            cursor: doc(3, 2),
            area: msg_area,
            zone: SelectionZone::Messages,
        };
        let screen = sel.to_screen(0).unwrap();
        assert_eq!(screen.start_row, 3);
        assert_eq!(screen.start_col, 2);
        assert_eq!(screen.end_row, 9);
        assert_eq!(screen.end_col, 79);
    }

    #[test]
    fn to_screen_highlight_consistent_after_edge_scroll_reversal() {
        let msg_area = Rect::new(0, 2, 80, 20);
        let sel = Selection {
            anchor: doc(58, 5),
            cursor: doc(55, 3),
            area: msg_area,
            zone: SelectionZone::Messages,
        };
        let screen = sel.to_screen(50).unwrap();
        assert!(
            (screen.start_row, screen.start_col) < (screen.end_row, screen.end_col),
            "projected highlight must be ordered"
        );
        assert_eq!(screen.start_row, 2 + (55 - 50) as u16);
        assert_eq!(screen.start_col, 3);
        assert_eq!(screen.end_row, 2 + (58 - 50) as u16);
        assert_eq!(screen.end_col, 5);
    }

    #[test]
    fn update_clamps_cursor_row_to_area_bottom() {
        let msg_area = Rect::new(0, 2, 80, 20);
        let mut sel = Selection::start(10, 5, msg_area, SelectionZone::Messages, 0);
        sel.update(25, 5, 0);
        let (_, end) = sel.normalized();
        assert_eq!(end.row, 19, "clamped to area bottom doc row");
    }

    #[test]
    fn update_clamps_cursor_col_to_area() {
        let msg_area = Rect::new(5, 0, 40, 20);
        let mut sel = Selection::start(10, 10, msg_area, SelectionZone::Messages, 0);
        sel.update(10, 50, 0);
        assert_eq!(sel.cursor.col, 44, "clamped to area right");
        sel.update(10, 2, 0);
        assert_eq!(sel.cursor.col, 5, "clamped to area left");
    }

    #[test]
    fn input_zone_with_scroll() {
        let area = Rect::new(0, 22, 80, 5);
        let sel = Selection::start(23, 5, area, SelectionZone::Input, 3);
        let (start, _) = sel.normalized();
        assert_eq!(start.row, 4);
    }

    fn code_bar_buffer() -> (Buffer, Rect) {
        let area = Rect::new(0, 0, 20, 2);
        let mut buf = Buffer::empty(area);
        let code_bar_style = theme::current().code_bar;
        buf.set_string(0, 0, "│", code_bar_style);
        buf.set_string(2, 0, "fn main() {}        ", Style::default());
        buf.set_string(0, 1, "│", code_bar_style);
        buf.set_string(2, 1, "let x = 1;          ", Style::default());
        (buf, area)
    }

    #[test]
    fn strips_code_bar_prefix_from_partial_selection() {
        let (buf, area) = code_bar_buffer();
        let region = ContentRegion {
            area,
            ..Default::default()
        };
        let text = extract_selected_text(&buf, &ss(0, 0, 1, 18), &[region]);
        assert_eq!(text, "fn main() {}\nlet x = 1;");
    }

    #[test]
    fn does_not_strip_table_border_prefix() {
        let area = Rect::new(0, 0, 20, 1);
        let mut buf = Buffer::empty(area);
        let table_style = theme::current().table_border;
        buf.set_string(0, 0, "│", table_style);
        buf.set_string(2, 0, "cell content        ", Style::default());
        let region = ContentRegion {
            area,
            ..Default::default()
        };
        let text = extract_selected_text(&buf, &ss(0, 0, 0, 18), &[region]);
        assert_eq!(text, "│ cell content");
    }

    #[test]
    fn no_strip_when_selection_starts_mid_line() {
        let (buf, area) = code_bar_buffer();
        let region = ContentRegion {
            area,
            ..Default::default()
        };
        let text = extract_selected_text(&buf, &ss(0, 5, 0, 13), &[region]);
        assert_eq!(text, "main() {}");
    }

    #[test]
    fn strips_code_bar_wrap_prefix() {
        let area = Rect::new(0, 0, 12, 1);
        let mut buf = Buffer::empty(area);
        let code_bar_style = theme::current().code_bar;
        buf.set_string(0, 0, "│", code_bar_style);
        buf.set_string(1, 0, "continued  ", Style::default());
        let region = ContentRegion {
            area,
            ..Default::default()
        };
        let text = extract_selected_text(&buf, &ss(0, 0, 0, 10), &[region]);
        assert_eq!(text, "continued");
    }

    #[test_case(&[1, 1, 1], &[0, 1, 2]    ; "no_wrapping")]
    #[test_case(&[1, 3, 1], &[0, 1, 4]    ; "middle_line_wraps")]
    #[test_case(&[3, 3],    &[0, 3]        ; "all_lines_wrap")]
    fn line_breaks_from_heights(heights: &[u16], expected_starts: &[u16]) {
        let lb = LineBreaks::from_heights(heights.iter().copied());
        for row in 0..heights.iter().sum::<u16>().max(1) {
            let should_be_start = expected_starts.contains(&row);
            assert_eq!(
                lb.is_line_start(row),
                should_be_start,
                "row {row}: expected is_line_start={should_be_start}"
            );
        }
    }

    #[test]
    fn line_breaks_beyond_64_rows() {
        let lb = LineBreaks::from_heights([65, 1].iter().copied());
        assert!(lb.is_line_start(0));
        assert!(!lb.is_line_start(64));
        assert!(lb.is_line_start(65));
    }

    #[test]
    fn zone_at_overlay_wins_over_messages() {
        let msg_area = Rect::new(0, 0, 80, 20);
        let overlay_area = Rect::new(10, 5, 60, 10);
        let mut zones: ZoneRegistry = [None; SelectionZone::COUNT];
        zones[SelectionZone::Messages.idx()] = Some(SelectableZone {
            area: msg_area,
            highlight_area: msg_area,
            zone: SelectionZone::Messages,
        });
        zones[SelectionZone::Overlay.idx()] = Some(SelectableZone {
            area: overlay_area,
            highlight_area: overlay_area,
            zone: SelectionZone::Overlay,
        });

        assert_eq!(zone_at(&zones, 7, 20).unwrap().zone, SelectionZone::Overlay);
        assert_eq!(
            zone_at(&zones, 2, 20).unwrap().zone,
            SelectionZone::Messages
        );
        assert_eq!(zone_at(&zones, 7, 5).unwrap().zone, SelectionZone::Messages);
    }

    #[test]
    fn apply_highlight_zero_area_no_panic() {
        let area = Rect::new(0, 0, 0, 0);
        let mut buf = Buffer::empty(Rect::new(0, 0, 1, 1));
        let ss = ScreenSelection {
            start_row: 0,
            end_row: 0,
            start_col: 0,
            end_col: 0,
        };
        apply_highlight(&mut buf, area, &ss);
    }
}

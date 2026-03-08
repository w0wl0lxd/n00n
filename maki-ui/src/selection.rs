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

use std::time::Instant;

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Modifier;

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
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for DocPos {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        (self.row, self.col).cmp(&(other.row, other.col))
    }
}

/// Selection is locked to one zone for its entire lifetime.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SelectionZone {
    Messages,
    Input,
    StatusBar,
}

impl SelectionZone {
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

pub type ZoneRegistry = [Option<SelectableZone>; 3];

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

/// Screen region + optional raw source text for copy. If `raw_text` is
/// non-empty and the region is fully selected, raw text is used as-is.
pub struct ContentRegion<'a> {
    pub area: Rect,
    pub raw_text: &'a str,
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
fn col_range(ss: &ScreenSelection, left: u16, right: u16, row: u16) -> (u16, u16) {
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
    let row_start = ss.start_row.max(area.y);
    let row_end = ss.end_row.min(area.bottom().saturating_sub(1));
    let right = area.x + area.width.saturating_sub(1);
    for row in row_start..=row_end {
        let (col_start, col_end) = col_range(ss, area.x, right, row);
        for col in col_start..=col_end {
            let cell = &mut buf[(col, row)];
            cell.set_style(cell.style().add_modifier(Modifier::REVERSED));
        }
    }
}

/// Trailing whitespace trimmed per line; consecutive trailing blank lines
/// collapsed via `pending_newlines`.
fn append_rows(
    buf: &Buffer,
    area: Rect,
    ss: &ScreenSelection,
    from: u16,
    to: u16,
    out: &mut String,
) {
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
        let trimmed_len = out[line_start..].trim_end().len() + line_start;
        out.truncate(trimmed_len);
        if out.len() == line_start && out.len() > anchor {
            pending_newlines += 1;
        } else if out.len() > anchor {
            for _ in 0..pending_newlines {
                out.insert(line_start, '\n');
            }
            pending_newlines = 0;
            if line_start > anchor {
                out.insert(line_start + pending_newlines as usize, '\n');
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
            append_rows(buf, region.area, ss, row, chunk_end, &mut out);
        }
        row = region_end;
    }
    out
}

/// Messages zone extraction. Fully-enclosed segments use `copy_text`;
/// partial on-screen segments fall back to cell scraping; partial off-screen
/// segments use `copy_text` as best-effort.
pub fn extract_doc_range(
    buf: &Buffer,
    sel: &Selection,
    msg_area: Rect,
    scroll_top: u16,
    segment_heights: &[u16],
    segments_copy_text: &[&str],
) -> String {
    let (doc_start, doc_end) = sel.normalized();
    let mut out = String::new();
    let mut doc_row: u32 = 0;

    for (i, &h) in segment_heights.iter().enumerate() {
        let seg_start = doc_row;
        let seg_end = doc_row + h as u32;
        doc_row = seg_end;

        if seg_end <= doc_start.row || seg_start > doc_end.row {
            continue;
        }

        let fully_enclosed = seg_start >= doc_start.row
            && seg_end <= doc_end.row + 1
            && (seg_start != doc_start.row || doc_start.col <= msg_area.x)
            && (seg_end != doc_end.row + 1
                || doc_end.col >= msg_area.x + msg_area.width.saturating_sub(1));

        if !out.is_empty() {
            out.push('\n');
        }

        let copy_text = segments_copy_text.get(i).copied().unwrap_or("");

        if fully_enclosed && !copy_text.is_empty() {
            out.push_str(copy_text);
        } else {
            let view_top = scroll_top as u32;
            let view_bottom = view_top + msg_area.height as u32;
            let is_on_screen = seg_start < view_bottom && seg_end > view_top;

            if is_on_screen {
                let screen_start = msg_area.y + seg_start.saturating_sub(view_top) as u16;
                let screen_end =
                    msg_area.y + (seg_end.saturating_sub(view_top) as u16).min(msg_area.height);

                let sel_screen_start = msg_area.y + doc_start.row.saturating_sub(view_top) as u16;
                let sel_screen_end = msg_area.y
                    + ((doc_end.row + 1).saturating_sub(view_top) as u16).min(msg_area.height);
                let from = screen_start.max(sel_screen_start);
                let to = screen_end.min(sel_screen_end);

                let fake_start_row = if seg_start >= doc_start.row {
                    screen_start
                } else {
                    msg_area.y + doc_start.row.saturating_sub(view_top) as u16
                };
                let fake_start_col = if seg_start > doc_start.row {
                    msg_area.x
                } else {
                    doc_start.col
                };
                let fake_end_row = if seg_end <= doc_end.row + 1 {
                    screen_end.saturating_sub(1)
                } else {
                    msg_area.y + doc_end.row.saturating_sub(view_top) as u16
                };
                let fake_end_col = if seg_end < doc_end.row + 1 {
                    msg_area.x + msg_area.width.saturating_sub(1)
                } else {
                    doc_end.col
                };

                let ss = ScreenSelection {
                    start_row: fake_start_row,
                    start_col: fake_start_col,
                    end_row: fake_end_row,
                    end_col: fake_end_col,
                };

                let area = Rect::new(msg_area.x, from, msg_area.width, to.saturating_sub(from));
                append_rows(buf, area, &ss, from, to, &mut out);
            } else if !copy_text.is_empty() {
                out.push_str(copy_text);
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::buffer::Buffer;
    use ratatui::layout::Rect;
    use ratatui::style::Modifier;
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
        };
        let text = extract_selected_text(&buf, &ss(0, 0, 2, 9), &[region]);
        assert_eq!(text, raw);
    }

    #[test]
    fn extract_backward_selection() {
        let (buf, area) = test_buffer();
        let region = ContentRegion {
            area,
            raw_text: "raw",
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
            },
            ContentRegion {
                area: Rect::new(0, 2, 10, 1),
                raw_text: "Line 2",
            },
            ContentRegion {
                area: Rect::new(0, 4, 10, 1),
                raw_text: "Line 4",
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
        };
        let overlay = ContentRegion {
            area: Rect::new(0, 0, 10, 3),
            raw_text: "overlay raw text",
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
            },
            ContentRegion {
                area: Rect::new(0, 2, 20, 2),
                raw_text: "# msg1 raw",
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
        let region = ContentRegion { area, raw_text: "" };
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
    fn normalized_doc_forward_and_backward() {
        let sel = Selection {
            anchor: doc(20, 5),
            cursor: doc(15, 2),
            area: Rect::default(),
            zone: SelectionZone::Messages,
        };
        assert_eq!(sel.normalized(), (doc(15, 2), doc(20, 5)));

        let sel2 = Selection {
            anchor: doc(15, 2),
            cursor: doc(20, 5),
            area: Rect::default(),
            zone: SelectionZone::Messages,
        };
        assert_eq!(sel2.normalized(), (doc(15, 2), doc(20, 5)));
    }

    #[test]
    fn extract_doc_range_fully_enclosed_segments() {
        let area = Rect::new(0, 0, 20, 6);
        let buf = Buffer::empty(area);
        let sel = Selection {
            anchor: doc(0, 0),
            cursor: doc(5, 19),
            area,
            zone: SelectionZone::Messages,
        };
        let heights = [3, 3];
        let copy_texts = ["segment one", "segment two"];
        let text = extract_doc_range(&buf, &sel, area, 0, &heights, &copy_texts);
        assert_eq!(text, "segment one\nsegment two");
    }

    #[test]
    fn extract_doc_range_skips_out_of_range_segments() {
        let area = Rect::new(0, 0, 20, 10);
        let buf = Buffer::empty(area);
        let sel = Selection {
            anchor: doc(3, 0),
            cursor: doc(5, 19),
            area,
            zone: SelectionZone::Messages,
        };
        let heights = [3, 3, 3];
        let copy_texts = ["seg0", "seg1", "seg2"];
        let text = extract_doc_range(&buf, &sel, area, 0, &heights, &copy_texts);
        assert_eq!(text, "seg1");
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

    #[test]
    fn extract_doc_range_partial_first_segment() {
        let area = Rect::new(0, 0, 20, 6);
        let mut buf = Buffer::empty(area);
        let style = ratatui::style::Style::default();
        buf.set_string(0, 0, "seg0 line0          ", style);
        buf.set_string(0, 1, "seg0 line1          ", style);
        buf.set_string(0, 2, "seg0 line2          ", style);
        buf.set_string(0, 3, "seg1 line0          ", style);
        buf.set_string(0, 4, "seg1 line1          ", style);
        buf.set_string(0, 5, "seg1 line2          ", style);

        let sel = Selection {
            anchor: doc(2, 0),
            cursor: doc(5, 19),
            area,
            zone: SelectionZone::Messages,
        };
        let heights = [3, 3];
        let copy_texts = ["", "seg1 full"];
        let text = extract_doc_range(&buf, &sel, area, 0, &heights, &copy_texts);
        assert_eq!(text, "seg0 line2\nseg1 full");
    }

    #[test]
    fn extract_doc_range_partial_last_segment() {
        let area = Rect::new(0, 0, 20, 6);
        let mut buf = Buffer::empty(area);
        let style = ratatui::style::Style::default();
        buf.set_string(0, 0, "seg0 line0          ", style);
        buf.set_string(0, 1, "seg0 line1          ", style);
        buf.set_string(0, 2, "seg0 line2          ", style);
        buf.set_string(0, 3, "seg1 line0          ", style);
        buf.set_string(0, 4, "seg1 line1          ", style);
        buf.set_string(0, 5, "seg1 line2          ", style);

        let sel = Selection {
            anchor: doc(0, 0),
            cursor: doc(3, 19),
            area,
            zone: SelectionZone::Messages,
        };
        let heights = [3, 3];
        let copy_texts = ["seg0 full", ""];
        let text = extract_doc_range(&buf, &sel, area, 0, &heights, &copy_texts);
        assert_eq!(text, "seg0 full\nseg1 line0");
    }

    #[test]
    fn extract_doc_range_partial_col_single_row() {
        let area = Rect::new(0, 0, 20, 1);
        let mut buf = Buffer::empty(area);
        let style = ratatui::style::Style::default();
        buf.set_string(0, 0, "maki> hello world   ", style);

        let sel = Selection {
            anchor: doc(0, 12),
            cursor: doc(0, 16),
            area,
            zone: SelectionZone::Messages,
        };
        let heights = [1];
        let copy_texts = ["hello world"];
        let text = extract_doc_range(&buf, &sel, area, 0, &heights, &copy_texts);
        assert_eq!(text, "world");
    }

    #[test]
    fn extract_doc_range_full_width_single_row_uses_copy_text() {
        let content_area = Rect::new(0, 0, 19, 1);
        let buf = Buffer::empty(Rect::new(0, 0, 20, 1));

        let sel = Selection {
            anchor: doc(0, 0),
            cursor: doc(0, 18),
            area: content_area,
            zone: SelectionZone::Messages,
        };
        let heights = [1];
        let copy_texts = ["hello world"];
        let text = extract_doc_range(&buf, &sel, content_area, 0, &heights, &copy_texts);
        assert_eq!(text, "hello world");
    }

    #[test]
    fn selection_zone_equality() {
        assert_eq!(SelectionZone::Messages, SelectionZone::Messages);
        assert_ne!(SelectionZone::Messages, SelectionZone::Input);
    }

    #[test]
    fn selection_state_atomic_reset() {
        let area = Rect::new(0, 0, 80, 20);
        let state = Some(SelectionState {
            sel: Selection::start(5, 5, area, SelectionZone::Messages, 0),
            copy_on_release: true,
            edge_scroll: Some(EdgeScroll {
                dir: 1,
                last_tick: Instant::now(),
            }),
            last_drag_col: 42,
        });
        let cleared: Option<SelectionState> = None;
        assert!(state.is_some());
        assert!(cleared.is_none());
    }
}

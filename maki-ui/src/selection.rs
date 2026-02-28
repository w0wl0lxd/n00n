//! Mouse selection + copy.
//!
//! Because we EnableMouseCapture (for scroll), the terminal can no longer do
//! its own text selection. So we reimplement it here.
//!
//! Selection (the struct) tracks anchor + cursor positions. App owns an
//! Option<Selection>: mouse-down creates it, drag updates it, mouse-up
//! copies, any key or scroll clears it.
//!
//! apply_highlight runs after all widgets rendered in App::view(). It just
//! walks the selected rows/cols in the raw terminal buffer and flips REVERSED
//! on each cell. No widget awareness needed.
//!
//! extract_selected_text is where the interesting stuff happens. Components
//! register ContentRegions (screen area + the original raw_text before
//! rendering). On copy:
//! - If a region is fully inside the selection and has raw_text, we grab that
//!   directly. This preserves markdown headings, blank lines, etc. that the
//!   rendered output loses.
//! - If partially selected, we read cells from the terminal buffer instead.
//! - Regions are checked in reverse order so overlays (popups, pickers) win
//!   over whatever is behind them.
//! - Rows that don't belong to any region (borders, separators) are skipped.
//!
//! Adding selection to a new component:
//!
//! 1. In view(), remember the Rect where your text lands. For bordered
//!    widgets, use inset_border(area) to get the inner area.
//! 2. Add a push_content_regions method. Push a ContentRegion per block of
//!    selectable text. raw_text can be "" if cell-level extraction is fine
//!    (usually is for short/single-line stuff).
//! 3. Wire it into App::copy_selection. Base content goes first, overlays
//!    last (last one wins for any given row).
//!
//! MessagesPanel::push_content_regions is the main example. App::copy_selection
//! has the overlay ones (popup, picker, input box).

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Modifier;

#[derive(Clone, Copy)]
pub struct Selection {
    pub anchor_row: u16,
    pub anchor_col: u16,
    pub cursor_row: u16,
    pub cursor_col: u16,
}

impl Selection {
    pub fn start(row: u16, col: u16) -> Self {
        Self {
            anchor_row: row,
            anchor_col: col,
            cursor_row: row,
            cursor_col: col,
        }
    }

    pub fn update(&mut self, row: u16, col: u16) {
        self.cursor_row = row;
        self.cursor_col = col;
    }

    pub fn is_empty(&self) -> bool {
        self.anchor_row == self.cursor_row && self.anchor_col == self.cursor_col
    }

    pub fn normalized(&self) -> (u16, u16, u16, u16) {
        if (self.anchor_row, self.anchor_col) <= (self.cursor_row, self.cursor_col) {
            (
                self.anchor_row,
                self.anchor_col,
                self.cursor_row,
                self.cursor_col,
            )
        } else {
            (
                self.cursor_row,
                self.cursor_col,
                self.anchor_row,
                self.anchor_col,
            )
        }
    }
}

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
fn col_range(sr: u16, sc: u16, er: u16, ec: u16, left: u16, right: u16, row: u16) -> (u16, u16) {
    let col_start = if row == sr { sc.max(left) } else { left };
    let col_end = if row == er { ec.min(right) } else { right };
    (col_start, col_end)
}

pub fn apply_highlight(buf: &mut Buffer, area: Rect, sel: &Selection) {
    let (sr, sc, er, ec) = sel.normalized();
    let row_start = sr.max(area.y);
    let row_end = er.min(area.bottom().saturating_sub(1));
    let right = area.x + area.width.saturating_sub(2);
    for row in row_start..=row_end {
        let (col_start, col_end) = col_range(sr, sc, er, ec, area.x, right, row);
        for col in col_start..=col_end {
            let cell = &mut buf[(col, row)];
            cell.set_style(cell.style().add_modifier(Modifier::REVERSED));
        }
    }
}

fn append_rows(buf: &Buffer, area: Rect, sel: &Selection, from: u16, to: u16, out: &mut String) {
    let (sr, sc, er, ec) = sel.normalized();
    let right = area.x + area.width.saturating_sub(2);
    let row_start = from.max(area.y);
    let row_end = to.min(area.bottom());
    let mut pending_newlines = 0u16;
    let anchor = out.len();
    for row in row_start..row_end {
        let (col_start, col_end) = col_range(sr, sc, er, ec, area.x, right, row);
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

pub fn extract_selected_text(
    buf: &Buffer,
    sel: &Selection,
    regions: &[ContentRegion<'_>],
) -> String {
    let (sr, _, er, _) = sel.normalized();
    let mut out = String::new();
    let mut row = sr;

    while row <= er {
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
        let fully_selected = region_start >= sr && region_end <= er + 1;

        if !out.is_empty() {
            out.push('\n');
        }
        if fully_selected && !region.raw_text.is_empty() {
            out.push_str(region.raw_text);
        } else {
            let chunk_end = region_end.min(er + 1);
            append_rows(buf, region.area, sel, row, chunk_end, &mut out);
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
    use ratatui::style::Modifier;
    use test_case::test_case;

    #[test_case(0, 0, 5, 10, (0, 0, 5, 10) ; "forward_selection")]
    #[test_case(5, 10, 0, 0, (0, 0, 5, 10) ; "backward_selection")]
    #[test_case(3, 5, 3, 5, (3, 5, 3, 5) ; "same_point")]
    fn normalized(ar: u16, ac: u16, cr: u16, cc: u16, expected: (u16, u16, u16, u16)) {
        let sel = Selection {
            anchor_row: ar,
            anchor_col: ac,
            cursor_row: cr,
            cursor_col: cc,
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

    fn sel(sr: u16, sc: u16, er: u16, ec: u16) -> Selection {
        Selection {
            anchor_row: sr,
            anchor_col: sc,
            cursor_row: er,
            cursor_col: ec,
        }
    }

    #[test]
    fn extract_single_region_partial() {
        let (buf, area) = test_buffer();
        let region = ContentRegion {
            area,
            raw_text: "# Hello\n\nWorld\nTest",
        };
        let text = extract_selected_text(&buf, &sel(0, 0, 0, 4), &[region]);
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
        let text = extract_selected_text(&buf, &sel(0, 0, 2, 9), &[region]);
        assert_eq!(text, raw);
    }

    #[test]
    fn extract_backward_selection() {
        let (buf, area) = test_buffer();
        let region = ContentRegion {
            area,
            raw_text: "raw",
        };
        let s = Selection {
            anchor_row: 1,
            anchor_col: 4,
            cursor_row: 0,
            cursor_col: 0,
        };
        let text = extract_selected_text(&buf, &s, &[region]);
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
        let text = extract_selected_text(&buf, &sel(0, 0, 4, 7), &regions);
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
        let text = extract_selected_text(&buf, &sel(0, 0, 2, 9), &[base, overlay]);
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
        // Select from row 1 of msg0 to row 0 of msg1 — both partially selected
        let text = extract_selected_text(&buf, &sel(1, 0, 2, 18), &regions);
        assert_eq!(text, "msg0 line2\nmsg1 rendered");
    }

    #[test]
    fn apply_highlight_sets_reversed() {
        let (mut buf, area) = test_buffer();
        let s = sel(0, 0, 0, 2);
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
            extract_selected_text(&buf, &sel(0, 0, 2, 7), &[]),
            "",
            "no regions at all"
        );

        let region = ContentRegion {
            area: Rect::new(0, 5, 10, 1),
            raw_text: "far away",
        };
        assert_eq!(
            extract_selected_text(&buf, &sel(0, 0, 2, 7), &[region]),
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
        let text = extract_selected_text(&buf, &sel(0, 0, 0, 9), &[region]);
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
        let text = extract_selected_text(&buf, &sel(0, 0, 0, 9), &[region]);
        assert_eq!(text, "ABCDEFGHI");
    }
}

use super::segment::SegmentCache;
use crate::markdown::CodeBlockRange;
use crate::selection::{self, LineBreaks, ScreenSelection, Selection};

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::widgets::{Paragraph, Widget, Wrap};

pub(super) fn parse_batch_inner_id(tool_id: &str) -> Option<(&str, usize)> {
    let (batch_id, idx_str) = tool_id.rsplit_once("__")?;
    let idx = idx_str.parse().ok()?;
    Some((batch_id, idx))
}

pub(super) fn extract_selection_text(
    cache: &SegmentCache,
    viewport_width: u16,
    sel: &Selection,
    msg_area: Rect,
) -> String {
    let (doc_start, doc_end) = sel.normalized();
    let width = viewport_width;
    let mut out = String::new();
    let mut doc_row: u32 = 0;

    for (i, &h) in cache.heights().iter().enumerate() {
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

        let seg = match cache.get(i) {
            Some(s) => s,
            None => continue,
        };

        if fully_enclosed
            && !seg.copy_text.is_empty()
            && has_non_code_rows(0, h, &seg.code_block_ranges)
        {
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

fn has_non_code_rows(rel_start: u16, rel_end: u16, ranges: &[CodeBlockRange]) -> bool {
    let mut covered = rel_start;
    for cb in ranges {
        if cb.start_line >= rel_end || cb.end_line < rel_start {
            continue;
        }
        let cb_start = cb.start_line.max(rel_start);
        if covered < cb_start {
            return true;
        }
        covered = covered.max(cb.end_line + 1);
    }
    covered < rel_end
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

    let emit_fences = has_non_code_rows(rel_start, rel_end, ranges);

    for cb in ranges {
        if cb.end_line < rel_start || cb.start_line >= rel_end {
            continue;
        }
        let fully_covered = emit_fences && rel_start <= cb.start_line && cb.end_line < rel_end;

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

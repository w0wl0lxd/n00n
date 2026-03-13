use std::borrow::Cow;
use std::iter;
use std::mem;

use crate::highlight;
use crate::highlight::CodeHighlighter;
use crate::theme;

use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use unicode_width::UnicodeWidthStr;

pub(crate) const CODE_BAR: &str = "│ ";
pub(crate) const CODE_BAR_WRAP: &str = "│";
pub const TRUNCATION_PREFIX: &str = "...";
const BULLET: &str = "• ";
const HR_CHAR: char = '─';
const LIST_INDENT: &str = "  ";
const MIN_COL_WIDTH: usize = 5;

fn fit_width(text: &str, max_width: usize) -> usize {
    let mut width = 0;
    for (i, ch) in text.char_indices() {
        let cw = unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0);
        if width + cw > max_width {
            return i;
        }
        width += cw;
    }
    text.len()
}

fn prepend_code_bar(line: &mut Line<'static>) {
    line.spans
        .insert(0, Span::styled(CODE_BAR, theme::current().code_bar));
}

pub(crate) fn wrap_code_lines(lines: &mut Vec<Line<'static>>, start: usize, width: u16) {
    let width = width as usize;
    if width == 0 {
        return;
    }
    let tail = lines.split_off(start);
    for line in tail {
        let line_width: usize = line.spans.iter().map(|s| s.content.width()).sum();
        if line_width <= width {
            lines.push(line);
        } else {
            lines.extend(split_line_with_bar(line, width));
        }
    }
}

fn split_line_with_bar(line: Line<'static>, width: usize) -> Vec<Line<'static>> {
    debug_assert!(
        line.spans
            .first()
            .is_some_and(|s| s.content.as_ref() == CODE_BAR)
    );

    let bar_span = line.spans[0].clone();
    let content_spans = &line.spans[1..];
    let first_avail = width.saturating_sub(CODE_BAR.width());
    let cont_avail = width.saturating_sub(CODE_BAR_WRAP.width());

    let mut result: Vec<Line<'static>> = Vec::new();
    let mut current_spans: Vec<Span<'static>> = vec![bar_span];
    let mut remaining = first_avail;

    for span in content_spans {
        let mut text = span.content.as_ref();
        let style = span.style;

        while !text.is_empty() {
            let fits = fit_width(text, remaining);
            if fits == 0 && remaining == 0 {
                break;
            }
            if fits > 0 {
                current_spans.push(Span::styled(text[..fits].to_owned(), style));
                remaining -= text[..fits].width();
                text = &text[fits..];
            }
            if !text.is_empty() {
                result.push(Line::from(current_spans));
                current_spans = vec![Span::styled(CODE_BAR_WRAP, theme::current().code_bar)];
                remaining = cont_avail;
            }
        }
    }

    if current_spans.len() > 1 || result.is_empty() {
        result.push(Line::from(current_spans));
    }

    result
}

fn highlight_code(lang: &str, code: &str, width: u16) -> Vec<Line<'static>> {
    let mut lines = highlight::highlight_code_plain(lang, code);
    for line in &mut lines {
        prepend_code_bar(line);
    }
    wrap_code_lines(&mut lines, 0, width);
    lines
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Keep {
    Head,
    Tail,
}

pub fn truncation_notice(count: usize) -> String {
    let label = if count == 1 { "line" } else { "lines" };
    format!("{TRUNCATION_PREFIX} ({count} {label})")
}

fn code_style(base: Style) -> Style {
    theme::current().inline_code.add_modifier(base.add_modifier)
}

pub(crate) fn hr_line(width: u16, style: Style) -> Line<'static> {
    let hr: String = iter::repeat_n(HR_CHAR, width as usize).collect();
    Line::from(Span::styled(hr, style))
}

fn bold_style_fn(base: Style) -> Style {
    theme::current().bold.add_modifier(base.add_modifier)
}

fn italic_style(base: Style) -> Style {
    base.add_modifier(Modifier::ITALIC)
}

fn strikethrough_style_fn(base: Style) -> Style {
    theme::current()
        .strikethrough
        .add_modifier(base.add_modifier)
}

fn count_run(bytes: &[u8], pos: usize, ch: u8) -> usize {
    bytes[pos..].iter().take_while(|&&b| b == ch).count()
}

fn count_backtick_run(bytes: &[u8], pos: usize) -> usize {
    count_run(bytes, pos, b'`')
}

fn find_code_span_close(bytes: &[u8], pos: usize, run_len: usize) -> Option<(usize, usize, usize)> {
    let content_start = pos + run_len;
    let mut i = content_start;
    while i < bytes.len() {
        if bytes[i] == b'`' {
            let close_run = count_backtick_run(bytes, i);
            if close_run == run_len {
                return Some((content_start, i, i + run_len));
            }
            i += close_run;
        } else {
            i += 1;
        }
    }
    None
}

fn find_emphasis_close(bytes: &[u8], start: usize, delim: &[u8]) -> Option<usize> {
    let mut pos = start;
    while pos + delim.len() <= bytes.len() {
        if bytes[pos] == b'`' {
            let run = count_backtick_run(bytes, pos);
            if let Some((_, _, close_end)) = find_code_span_close(bytes, pos, run) {
                pos = close_end;
            } else {
                pos += run;
            }
            continue;
        }
        if bytes[pos..].starts_with(delim) {
            return Some(pos);
        }
        pos += 1;
    }
    None
}

fn is_word_char(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

fn find_italic_close(bytes: &[u8], start: usize, ch: u8) -> Option<usize> {
    let mut pos = start;
    while pos < bytes.len() {
        if bytes[pos] == b'`' {
            let run = count_backtick_run(bytes, pos);
            if let Some((_, _, close_end)) = find_code_span_close(bytes, pos, run) {
                pos = close_end;
            } else {
                pos += run;
            }
            continue;
        }
        if bytes[pos] == ch {
            if (ch == b'*' && pos + 1 < bytes.len() && bytes[pos + 1] == b'*')
                || (pos > 0 && bytes[pos - 1] == ch)
            {
                pos += 1;
                continue;
            }
            if pos > start && !bytes[pos - 1].is_ascii_whitespace() {
                if ch == b'_' && pos + 1 < bytes.len() && is_word_char(bytes[pos + 1]) {
                    pos += 1;
                    continue;
                }
                return Some(pos);
            }
        }
        pos += 1;
    }
    None
}

fn is_valid_italic_open(bytes: &[u8], pos: usize) -> bool {
    if pos + 1 >= bytes.len() || bytes[pos + 1].is_ascii_whitespace() {
        return false;
    }
    let ch = bytes[pos];
    if ch == b'*' {
        if bytes[pos + 1] == b'*' {
            return false;
        }
        if pos > 0 && bytes[pos - 1] == b'*' {
            return false;
        }
        if pos > 0 && is_word_char(bytes[pos - 1]) {
            return false;
        }
    }
    if ch == b'_' && pos > 0 && is_word_char(bytes[pos - 1]) {
        return false;
    }
    true
}

fn is_valid_strike_open(bytes: &[u8], pos: usize) -> bool {
    if pos + 2 >= bytes.len() {
        return false;
    }
    if bytes[pos + 2] == b'~' {
        return false;
    }
    if pos > 0 && bytes[pos - 1] == b'~' {
        return false;
    }
    !bytes[pos + 2].is_ascii_whitespace()
}

fn find_strike_close(bytes: &[u8], start: usize) -> Option<usize> {
    let mut pos = start;
    while pos + 1 < bytes.len() {
        if bytes[pos] == b'`' {
            let run = count_backtick_run(bytes, pos);
            if let Some((_, _, close_end)) = find_code_span_close(bytes, pos, run) {
                pos = close_end;
            } else {
                pos += run;
            }
            continue;
        }
        if bytes[pos] == b'~' && bytes[pos + 1] == b'~' {
            if pos + 2 < bytes.len() && bytes[pos + 2] == b'~' {
                pos += 1;
                continue;
            }
            if pos > start && bytes[pos - 1] == b'~' {
                pos += 1;
                continue;
            }
            if pos > start && !bytes[pos - 1].is_ascii_whitespace() {
                return Some(pos);
            }
        }
        pos += 1;
    }
    None
}

struct EmphasisMatch {
    style_fn: fn(Style) -> Style,
    content_start: usize,
    close: usize,
    delim_len: usize,
    skip_on_fail: usize,
}

fn try_star_emphasis(bytes: &[u8], pos: usize) -> Option<EmphasisMatch> {
    let run = count_run(bytes, pos, b'*');

    if run >= 3
        && let Some(close) = find_emphasis_close(bytes, pos + 3, b"***")
        && close > pos + 3
    {
        return Some(EmphasisMatch {
            style_fn: |s| italic_style(bold_style_fn(s)),
            content_start: pos + 3,
            close,
            delim_len: 3,
            skip_on_fail: 0,
        });
    }

    if run >= 2 {
        if let Some(close) = find_emphasis_close(bytes, pos + 2, b"**")
            && close > pos + 2
        {
            return Some(EmphasisMatch {
                style_fn: bold_style_fn,
                content_start: pos + 2,
                close,
                delim_len: 2,
                skip_on_fail: 0,
            });
        }
        return Some(EmphasisMatch {
            style_fn: |s| s,
            content_start: pos,
            close: pos,
            delim_len: 0,
            skip_on_fail: 2,
        });
    }

    if is_valid_italic_open(bytes, pos)
        && let Some(close) = find_italic_close(bytes, pos + 1, b'*')
        && close > pos + 1
    {
        return Some(EmphasisMatch {
            style_fn: italic_style,
            content_start: pos + 1,
            close,
            delim_len: 1,
            skip_on_fail: 0,
        });
    }
    Some(EmphasisMatch {
        style_fn: |s| s,
        content_start: pos,
        close: pos,
        delim_len: 0,
        skip_on_fail: 1,
    })
}

fn try_strike_emphasis(bytes: &[u8], pos: usize) -> Option<EmphasisMatch> {
    if pos + 1 >= bytes.len() || bytes[pos + 1] != b'~' {
        return None;
    }
    if is_valid_strike_open(bytes, pos)
        && let Some(close) = find_strike_close(bytes, pos + 2)
        && close > pos + 2
    {
        return Some(EmphasisMatch {
            style_fn: strikethrough_style_fn,
            content_start: pos + 2,
            close,
            delim_len: 2,
            skip_on_fail: 0,
        });
    }
    Some(EmphasisMatch {
        style_fn: |s| s,
        content_start: pos,
        close: pos,
        delim_len: 0,
        skip_on_fail: 2,
    })
}

fn try_underscore_emphasis(bytes: &[u8], pos: usize) -> Option<EmphasisMatch> {
    if !is_valid_italic_open(bytes, pos) {
        return None;
    }
    if let Some(close) = find_italic_close(bytes, pos + 1, b'_')
        && close > pos + 1
    {
        return Some(EmphasisMatch {
            style_fn: italic_style,
            content_start: pos + 1,
            close,
            delim_len: 1,
            skip_on_fail: 0,
        });
    }
    Some(EmphasisMatch {
        style_fn: |s| s,
        content_start: pos,
        close: pos,
        delim_len: 0,
        skip_on_fail: 1,
    })
}

pub fn parse_inline_markdown<'a>(text: &'a str, base_style: Style) -> Vec<Span<'a>> {
    parse_inline(text, base_style, true)
}

fn parse_inline<'a>(text: &'a str, base_style: Style, code_spans: bool) -> Vec<Span<'a>> {
    let bytes = text.as_bytes();
    let mut spans = Vec::new();
    let mut pos = 0;
    let mut plain_start = 0;

    macro_rules! flush_before {
        () => {
            if plain_start < pos {
                let before = &text[plain_start..pos];
                if code_spans {
                    spans.extend(parse_inline(before, base_style, false));
                } else {
                    spans.push(Span::styled(before, base_style));
                }
            }
        };
    }

    while pos < bytes.len() {
        if code_spans && bytes[pos] == b'`' {
            let run_len = count_backtick_run(bytes, pos);
            if let Some((cs, ce, close_end)) = find_code_span_close(bytes, pos, run_len)
                && ce > cs
            {
                flush_before!();
                spans.push(Span::styled(&text[cs..ce], code_style(base_style)));
                pos = close_end;
                plain_start = pos;
                continue;
            }
            pos += run_len;
            continue;
        }

        let em = match bytes[pos] {
            b'*' => try_star_emphasis(bytes, pos),
            b'~' => try_strike_emphasis(bytes, pos),
            b'_' => try_underscore_emphasis(bytes, pos),
            _ => None,
        };

        if let Some(em) = em {
            if em.delim_len > 0 {
                flush_before!();
                let content = &text[em.content_start..em.close];
                let inner = (em.style_fn)(base_style);
                spans.extend(parse_inline(content, inner, code_spans));
                pos = em.close + em.delim_len;
                plain_start = pos;
            } else {
                pos += em.skip_on_fail;
            }
            continue;
        }

        pos += 1;
    }

    if plain_start < bytes.len() {
        spans.push(Span::styled(&text[plain_start..], base_style));
    }

    spans
}

fn parse_heading(line: &str) -> Option<&str> {
    let hashes = line.bytes().take_while(|&b| b == b'#').count();
    if hashes == 0 || hashes > 6 {
        return None;
    }
    let rest = &line[hashes..];
    if let Some(stripped) = rest.strip_prefix(' ') {
        Some(stripped.trim_end())
    } else if rest.is_empty() {
        Some("")
    } else {
        None
    }
}

fn parse_unordered_marker(line: &str) -> Option<(usize, &str)> {
    let indent = line.bytes().take_while(|&b| b == b' ').count();
    let rest = &line[indent..];
    let marker = rest.as_bytes().first()?;
    if !matches!(marker, b'-' | b'*' | b'+') {
        return None;
    }
    let after = &rest[1..];
    if let Some(stripped) = after.strip_prefix(' ') {
        Some((indent, stripped))
    } else {
        None
    }
}

fn parse_ordered_marker(line: &str) -> Option<(usize, &str, &str)> {
    let indent = line.bytes().take_while(|&b| b == b' ').count();
    let rest = &line[indent..];
    let digits_end = rest.bytes().take_while(|b| b.is_ascii_digit()).count();
    if digits_end == 0 {
        return None;
    }
    let after_digits = &rest[digits_end..];
    if !after_digits.starts_with(". ") {
        return None;
    }
    Some((indent, &rest[..digits_end + 1], &after_digits[2..]))
}

fn is_horizontal_rule(line: &str) -> bool {
    let trimmed = line.trim();
    let first = match trimmed.as_bytes().first() {
        Some(b'-' | b'*' | b'_') => trimmed.as_bytes()[0],
        _ => return false,
    };
    trimmed.bytes().all(|b| b == first || b == b' ')
        && trimmed.bytes().filter(|&b| b == first).count() >= 3
}

fn parse_line_prefix(line: &str, base_style: Style) -> (Option<String>, &str, Style) {
    if let Some(heading_text) = parse_heading(line) {
        return (None, heading_text, theme::current().heading);
    }
    if let Some((indent, content)) = parse_unordered_marker(line) {
        let depth = indent / 2;
        let prefix = format!("{}{}", LIST_INDENT.repeat(depth), BULLET);
        return (Some(prefix), content, base_style);
    }
    if let Some((indent, marker, content)) = parse_ordered_marker(line) {
        let depth = indent / 2;
        let prefix = format!("{}{} ", LIST_INDENT.repeat(depth), marker);
        return (Some(prefix), content, base_style);
    }
    (None, line, base_style)
}

pub(crate) enum TextBlock<'a> {
    Normal(&'a str),
    Code {
        lang: &'a str,
        code: &'a str,
    },
    Table {
        rows: Vec<Vec<String>>,
        header_end: usize,
    },
}

pub(crate) struct ParsedBlock<'a> {
    pub block: TextBlock<'a>,
    pub byte_offset: usize,
}

fn is_table_row(line: &str) -> bool {
    let t = line.trim();
    t.starts_with('|') && t.ends_with('|') && t.matches('|').count() >= 2
}

fn is_separator_row(line: &str) -> bool {
    if !is_table_row(line) {
        return false;
    }
    parse_table_cells(line)
        .iter()
        .all(|cell| cell.bytes().all(|b| matches!(b, b'-' | b':')) && cell.contains('-'))
}

fn parse_table_cells(line: &str) -> Vec<String> {
    let t = line.trim();
    let inner = t.strip_prefix('|').unwrap_or(t);
    let inner = inner.strip_suffix('|').unwrap_or(inner);

    let bytes = inner.as_bytes();
    let mut cells = Vec::new();
    let mut current = String::new();
    let mut i = 0;

    while i < bytes.len() {
        if bytes[i] == b'`' {
            let run_len = count_backtick_run(bytes, i);
            if let Some((_, _, close_end)) = find_code_span_close(bytes, i, run_len) {
                current.push_str(&inner[i..close_end]);
                i = close_end;
            } else {
                current.push_str(&inner[i..]);
                i = bytes.len();
            }
        } else if bytes[i] == b'\\' && i + 1 < bytes.len() && bytes[i + 1] == b'|' {
            current.push('|');
            i += 2;
        } else if bytes[i] == b'|' {
            cells.push(current.trim().to_owned());
            current = String::new();
            i += 1;
        } else {
            let ch = inner[i..].chars().next().unwrap();
            current.push(ch);
            i += ch.len_utf8();
        }
    }

    cells.push(current.trim().to_owned());
    cells
}

fn split_normal_blocks<'a>(text: &'a str) -> Vec<TextBlock<'a>> {
    let mut lines_with_offsets: Vec<(usize, &str)> = Vec::new();
    let mut offset = 0;
    for line in text.split('\n') {
        lines_with_offsets.push((offset, line));
        offset += line.len() + 1;
    }

    let mut blocks: Vec<TextBlock<'a>> = Vec::new();
    let mut normal_start: Option<usize> = None;
    let mut i = 0;

    while i < lines_with_offsets.len() {
        let (_, line) = lines_with_offsets[i];
        if is_table_row(line) {
            let table_start = i;
            let mut sep_idx = None;
            let mut j = i;
            while j < lines_with_offsets.len() && is_table_row(lines_with_offsets[j].1) {
                if sep_idx.is_none() && is_separator_row(lines_with_offsets[j].1) {
                    sep_idx = Some(j - table_start);
                }
                j += 1;
            }

            if let Some(si) = sep_idx
                && j - table_start >= 2
            {
                if let Some(ns) = normal_start.take() {
                    let start = lines_with_offsets[ns].0;
                    let end = lines_with_offsets[table_start].0;
                    let slice = text[start..end].trim_matches('\n');
                    if !slice.is_empty() {
                        blocks.push(TextBlock::Normal(slice));
                    }
                }

                let mut rows = Vec::new();
                for (k, &(_, line)) in lines_with_offsets[table_start..j].iter().enumerate() {
                    if k != si {
                        rows.push(parse_table_cells(line));
                    }
                }
                blocks.push(TextBlock::Table {
                    rows,
                    header_end: si,
                });
                i = j;
                continue;
            }
        }

        if normal_start.is_none() {
            normal_start = Some(i);
        }
        i += 1;
    }

    if let Some(ns) = normal_start {
        let start = lines_with_offsets[ns].0;
        let content = text[start..].trim_start_matches('\n');
        if !content.is_empty() {
            blocks.push(TextBlock::Normal(content));
        }
    }

    if blocks.is_empty() {
        blocks.push(TextBlock::Normal(text));
    }

    blocks
}

fn cell_display_width(cell: &str) -> usize {
    parse_inline_markdown(cell, Style::default())
        .iter()
        .map(|s| s.content.width())
        .sum()
}

fn constrain_col_widths(col_widths: &mut [usize], available: usize) {
    let total: usize = col_widths.iter().sum();
    if total <= available {
        return;
    }
    for w in col_widths.iter_mut() {
        *w = (*w * available / total).max(MIN_COL_WIDTH).min(*w);
    }
    let mut excess = col_widths.iter().sum::<usize>().saturating_sub(available);
    while excess > 0 {
        let max_w = col_widths.iter().copied().max().unwrap_or(0);
        if max_w <= MIN_COL_WIDTH {
            break;
        }
        for w in col_widths.iter_mut() {
            if excess == 0 {
                break;
            }
            if *w == max_w && *w > MIN_COL_WIDTH {
                *w -= 1;
                excess -= 1;
            }
        }
    }
}

fn wrap_cell_spans(spans: Vec<Span<'static>>, max_width: usize) -> Vec<Vec<Span<'static>>> {
    if max_width == 0 {
        return vec![spans];
    }
    let mut result: Vec<Vec<Span<'static>>> = Vec::new();
    let mut current: Vec<Span<'static>> = Vec::new();
    let mut remaining = max_width;

    for span in spans {
        let mut text = span.content.as_ref();
        let style = span.style;

        while !text.is_empty() {
            let fits = fit_width(text, remaining);
            if fits == 0 {
                if current.is_empty() {
                    let ch_len = text.chars().next().map_or(1, char::len_utf8);
                    current.push(Span::styled(text[..ch_len].to_owned(), style));
                    text = &text[ch_len..];
                }
                result.push(mem::take(&mut current));
                remaining = max_width;
                text = text.strip_prefix(' ').unwrap_or(text);
                continue;
            }
            let (take, skip) = if fits < text.len() {
                match text[..fits].rfind(' ') {
                    Some(sp) if sp > 0 => (sp, sp + 1),
                    _ => (fits, fits),
                }
            } else {
                (fits, fits)
            };
            current.push(Span::styled(text[..take].to_owned(), style));
            remaining -= text[..take].width();
            text = &text[skip..];
            if take < fits && !text.is_empty() {
                result.push(mem::take(&mut current));
                remaining = max_width;
            }
        }
    }
    if !current.is_empty() || result.is_empty() {
        result.push(current);
    }
    result
}

fn spans_width(spans: &[Span<'_>]) -> usize {
    spans.iter().map(|s| s.content.width()).sum()
}

fn render_table(
    rows: &[Vec<String>],
    header_end: usize,
    text_style: Style,
    width: u16,
) -> Vec<Line<'static>> {
    let col_count = rows.iter().map(|r| r.len()).max().unwrap_or(0);
    if col_count == 0 {
        return Vec::new();
    }

    let mut col_widths = vec![0usize; col_count];
    for row in rows {
        for (c, cell) in row.iter().enumerate() {
            col_widths[c] = col_widths[c].max(cell_display_width(cell));
        }
    }

    let overhead = col_count * 3 + 1;
    let available = (width as usize).saturating_sub(overhead);
    constrain_col_widths(&mut col_widths, available);

    let mut lines = Vec::new();

    let border = |left: &str, mid: &str, right: &str, fill: &str| -> Line<'static> {
        let tbs = theme::current().table_border;
        let mut spans = vec![Span::styled(left.to_owned(), tbs)];
        for (i, &w) in col_widths.iter().enumerate() {
            spans.push(Span::styled(fill.repeat(w + 2), tbs));
            if i < col_count - 1 {
                spans.push(Span::styled(mid.to_owned(), tbs));
            }
        }
        spans.push(Span::styled(right.to_owned(), tbs));
        Line::from(spans)
    };

    lines.push(border("╭", "┬", "╮", "─"));

    for (ri, row) in rows.iter().enumerate() {
        let base = if ri < header_end {
            bold_style_fn(text_style)
        } else {
            text_style
        };

        let wrapped_cells: Vec<Vec<Vec<Span<'static>>>> = (0..col_count)
            .map(|c| {
                let cell = row.get(c).map(String::as_str).unwrap_or("");
                let cell_spans: Vec<Span<'static>> = parse_inline_markdown(cell, base)
                    .into_iter()
                    .map(|s| Span::styled(s.content.into_owned(), s.style))
                    .collect();
                wrap_cell_spans(cell_spans, col_widths[c])
            })
            .collect();

        let row_height = wrapped_cells.iter().map(|c| c.len()).max().unwrap_or(1);

        for line_idx in 0..row_height {
            let mut spans = vec![Span::styled("│ ".to_owned(), theme::current().table_border)];
            for (c, &w) in col_widths.iter().enumerate() {
                let sub_line = wrapped_cells[c].get(line_idx);
                let content_width = sub_line.map_or(0, |sl| spans_width(sl));
                let pad = w.saturating_sub(content_width);

                if let Some(sl) = sub_line {
                    spans.extend(sl.iter().cloned());
                }
                spans.push(Span::styled(" ".repeat(pad + 1), base));
                if c < col_count - 1 {
                    spans.push(Span::styled("│ ".to_owned(), theme::current().table_border));
                } else {
                    spans.push(Span::styled("│".to_owned(), theme::current().table_border));
                }
            }
            lines.push(Line::from(spans));
        }

        if ri + 1 < rows.len() {
            lines.push(border("├", "┼", "┤", "─"));
        }
    }

    lines.push(border("╰", "┴", "╯", "─"));

    lines
}

struct CodeFence<'a> {
    before_end: usize,
    lang: &'a str,
    code: &'a str,
    block_end: usize,
}

fn find_code_fence(text: &str) -> Option<CodeFence<'_>> {
    let bytes = text.as_bytes();
    let mut search_from = 0;

    while search_from < bytes.len() {
        let pos = text[search_from..].find("```")?;
        let abs = search_from + pos;

        if abs != 0 && bytes[abs - 1] != b'\n' {
            search_from = abs + 3;
            continue;
        }

        let fence_len = 3 + bytes[abs + 3..].iter().take_while(|&&b| b == b'`').count();
        let after_ticks = abs + fence_len;

        let Some(nl) = text[after_ticks..].find('\n') else {
            search_from = abs + fence_len;
            continue;
        };
        let info = &text[after_ticks..after_ticks + nl];
        if info.contains('`') {
            search_from = abs + fence_len;
            continue;
        }

        let lang = info.trim();
        let code_start = after_ticks + nl + 1;

        let fence_str = "`".repeat(fence_len);
        let mut offset = 0;
        let mut close: Option<(usize, usize)> = None;
        for line in text[code_start..].split('\n') {
            let trimmed = line.trim_end();
            if trimmed.len() >= fence_len
                && trimmed.starts_with(&fence_str)
                && !trimmed[fence_len..].starts_with('`')
            {
                close = Some((offset, line.len()));
                break;
            }
            offset += line.len() + 1;
        }

        let (code, block_end) = if let Some((close_off, close_line_len)) = close {
            let raw_end = code_start + close_off;
            let code_end = if raw_end > code_start && bytes[raw_end - 1] == b'\n' {
                raw_end - 1
            } else {
                raw_end
            };
            let trailing_start = code_start + close_off + fence_len;
            let trailing_end = code_start + close_off + close_line_len;
            let block_end = if text[trailing_start..trailing_end].trim().is_empty() {
                trailing_end
            } else {
                trailing_start
            };
            (&text[code_start..code_end], block_end)
        } else {
            (&text[code_start..], text.len())
        };

        return Some(CodeFence {
            before_end: abs,
            lang,
            code,
            block_end,
        });
    }
    None
}

fn parse_blocks(text: &str) -> Vec<TextBlock<'_>> {
    parse_blocks_with_offsets(text)
        .into_iter()
        .map(|pb| pb.block)
        .collect()
}

fn push_normal_blocks<'a>(
    blocks: &mut Vec<ParsedBlock<'a>>,
    text: &'a str,
    context: &str,
    base_offset: usize,
) {
    for nb in split_normal_blocks(text) {
        let off = match &nb {
            TextBlock::Normal(s) => base_offset + byte_offset_in(context, s),
            _ => base_offset,
        };
        blocks.push(ParsedBlock {
            block: nb,
            byte_offset: off,
        });
    }
}

pub(crate) fn parse_blocks_with_offsets(text: &str) -> Vec<ParsedBlock<'_>> {
    let mut blocks = Vec::new();
    let mut rest = text;
    let mut consumed = 0;

    while let Some(fence) = find_code_fence(rest) {
        let before = rest[..fence.before_end].trim_end_matches('\n');
        if !before.is_empty() {
            push_normal_blocks(&mut blocks, before, rest, consumed);
        }

        blocks.push(ParsedBlock {
            block: TextBlock::Code {
                lang: fence.lang,
                code: fence.code,
            },
            byte_offset: consumed + fence.before_end,
        });
        let skip = fence.block_end + rest[fence.block_end..].len()
            - rest[fence.block_end..].trim_start_matches('\n').len();
        consumed += skip;
        rest = &rest[skip..];
    }

    if !rest.is_empty() {
        push_normal_blocks(&mut blocks, rest, rest, consumed);
    }

    blocks
}

fn byte_offset_in(haystack: &str, needle: &str) -> usize {
    let h = haystack.as_ptr() as usize;
    let n = needle.as_ptr() as usize;
    n.saturating_sub(h)
}

fn is_blank_line(line: &Line<'_>) -> bool {
    line.spans.is_empty() || line.spans.iter().all(|s| s.content.is_empty())
}

fn ensure_blank_line(lines: &mut Vec<Line<'static>>) {
    if !lines.last().is_some_and(is_blank_line) {
        lines.push(Line::default());
    }
}

fn prefix_span(prefix: &str, style: Style) -> Span<'static> {
    Span::styled(prefix.to_owned(), style.add_modifier(Modifier::BOLD))
}

pub fn plain_lines(
    text: &str,
    prefix: &str,
    text_style: Style,
    prefix_style: Style,
) -> Vec<Line<'static>> {
    let text = text.trim_start_matches('\n');
    let mut lines: Vec<Line<'static>> = Vec::new();
    let mut first_line = true;

    for line in text.split('\n') {
        let mut spans: Vec<Span<'static>> = Vec::new();
        if first_line {
            spans.push(prefix_span(prefix, prefix_style));
            first_line = false;
        }
        spans.push(Span::styled(line.to_owned(), text_style));
        lines.push(Line::from(spans));
    }

    if lines.is_empty() {
        lines.push(Line::from(prefix_span(prefix, prefix_style)));
    }

    lines
}

#[derive(Clone)]
pub(crate) struct RenderState {
    pub first_line: bool,
    pub code_idx: usize,
}

impl RenderState {
    pub fn new() -> Self {
        Self {
            first_line: true,
            code_idx: 0,
        }
    }
}

pub(crate) struct RenderCtx<'a, 'b> {
    pub prefix: &'a str,
    pub text_style: Style,
    pub prefix_style: Style,
    pub highlighters: &'b mut Option<&'a mut Vec<CodeHighlighter>>,
    pub width: u16,
}

pub(crate) fn render_block(
    block: &TextBlock<'_>,
    lines: &mut Vec<Line<'static>>,
    state: &mut RenderState,
    ctx: &mut RenderCtx<'_, '_>,
) {
    match block {
        TextBlock::Normal(content) => {
            for line in content.split('\n') {
                if is_horizontal_rule(line) {
                    if state.first_line {
                        if !ctx.prefix.is_empty() {
                            lines.push(Line::from(prefix_span(ctx.prefix, ctx.prefix_style)));
                        }
                        state.first_line = false;
                    }
                    lines.push(hr_line(ctx.width, theme::current().horizontal_rule));
                    continue;
                }
                let mut spans: Vec<Span<'static>> = Vec::new();
                if state.first_line {
                    spans.push(prefix_span(ctx.prefix, ctx.prefix_style));
                    state.first_line = false;
                }
                let (line_prefix, rest, style) = parse_line_prefix(line, ctx.text_style);
                if let Some(lp) = line_prefix {
                    spans.push(Span::styled(lp, theme::current().list_marker));
                }
                spans.extend(
                    parse_inline_markdown(rest, style)
                        .into_iter()
                        .map(|s| Span::styled(s.content.into_owned(), s.style)),
                );
                lines.push(Line::from(spans));
            }
        }
        TextBlock::Code { lang, code } => {
            if state.first_line {
                lines.push(Line::from(prefix_span(ctx.prefix, ctx.prefix_style)));
                state.first_line = false;
            }
            ensure_blank_line(lines);
            if let Some(hl) = ctx.highlighters {
                if state.code_idx >= hl.len() {
                    hl.push(CodeHighlighter::new(lang));
                }
                let unwrapped = hl[state.code_idx].update(code);
                let start = lines.len();
                for src_line in unwrapped {
                    let mut line = src_line.clone();
                    prepend_code_bar(&mut line);
                    lines.push(line);
                }
                wrap_code_lines(lines, start, ctx.width);
            } else {
                lines.extend(highlight_code(lang, code, ctx.width));
            }
            ensure_blank_line(lines);
            state.code_idx += 1;
        }
        TextBlock::Table { rows, header_end } => {
            if state.first_line {
                if !ctx.prefix.is_empty() {
                    lines.push(Line::from(prefix_span(ctx.prefix, ctx.prefix_style)));
                }
                state.first_line = false;
            }
            ensure_blank_line(lines);
            lines.extend(render_table(rows, *header_end, ctx.text_style, ctx.width));
            ensure_blank_line(lines);
        }
    }
}

pub(crate) fn finalize_lines(lines: &mut Vec<Line<'static>>, prefix: &str, prefix_style: Style) {
    while lines.last().is_some_and(is_blank_line) {
        lines.pop();
    }
    if lines.is_empty() {
        lines.push(Line::from(prefix_span(prefix, prefix_style)));
    }
}

pub fn text_to_lines<'a>(
    text: &str,
    prefix: &'a str,
    text_style: Style,
    prefix_style: Style,
    mut highlighters: Option<&'a mut Vec<CodeHighlighter>>,
    width: u16,
) -> Vec<Line<'static>> {
    let text = text.trim_start_matches('\n');
    let blocks = parse_blocks(text);
    let mut lines: Vec<Line<'static>> = Vec::new();
    let mut state = RenderState::new();
    let mut ctx = RenderCtx {
        prefix,
        text_style,
        prefix_style,
        highlighters: &mut highlighters,
        width,
    };

    for block in &blocks {
        render_block(block, &mut lines, &mut state, &mut ctx);
    }

    if let Some(hl) = ctx.highlighters {
        hl.truncate(state.code_idx);
    }

    finalize_lines(&mut lines, prefix, prefix_style);
    lines
}

pub fn truncate_lines(s: &str, max: usize, keep: Keep) -> Cow<'_, str> {
    let split = match keep {
        Keep::Head => s.match_indices('\n').nth(max.saturating_sub(1)),
        Keep::Tail => s.rmatch_indices('\n').nth(max.saturating_sub(1)),
    };
    let Some((i, _)) = split else {
        return Cow::Borrowed(s);
    };
    Cow::Owned(match keep {
        Keep::Head => {
            let skipped = s[i..].matches('\n').count();
            format!("{}\n{}", &s[..i], truncation_notice(skipped))
        }
        Keep::Tail => {
            let skipped = s[..i].matches('\n').count() + 1;
            format!("{}\n{}", truncation_notice(skipped), &s[i + 1..])
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use test_case::test_case;

    fn bs() -> Style {
        theme::current().bold
    }
    fn cs() -> Style {
        theme::current().inline_code
    }
    fn ss() -> Style {
        theme::current().strikethrough
    }
    fn bcs() -> Style {
        cs().add_modifier(Modifier::BOLD)
    }
    fn bis() -> Style {
        bs().add_modifier(Modifier::ITALIC)
    }
    const IS: Style = Style::new().add_modifier(Modifier::ITALIC);
    const TEST_WIDTH: u16 = 80;

    fn assert_inline(input: &str, expected: &[(&str, Option<Style>)]) {
        let base = Style::default();
        let spans = parse_inline_markdown(input, base);
        assert_eq!(
            spans.len(),
            expected.len(),
            "span count mismatch for {input:?}: got {spans:?}"
        );
        for (span, (text, style)) in spans.iter().zip(expected) {
            assert_eq!(span.content, *text);
            assert_eq!(span.style, style.unwrap_or(base));
        }
    }

    #[test]
    fn inline_bold() {
        assert_inline(
            "a **bold** b",
            &[("a ", None), ("bold", Some(bs())), (" b", None)],
        );
    }
    #[test]
    fn inline_code() {
        assert_inline(
            "use `foo` here",
            &[("use ", None), ("foo", Some(cs())), (" here", None)],
        );
    }
    #[test]
    fn code_before_bold() {
        assert_inline(
            "a `code` then **bold**",
            &[
                ("a ", None),
                ("code", Some(cs())),
                (" then ", None),
                ("bold", Some(bs())),
            ],
        );
    }
    #[test]
    fn unclosed_bold() {
        assert_inline("a **unclosed", &[("a **unclosed", None)]);
    }
    #[test]
    fn unclosed_backtick() {
        assert_inline("a `unclosed", &[("a `unclosed", None)]);
    }
    #[test]
    fn code_inside_bold() {
        assert_inline(
            "**bold `code` bold**",
            &[
                ("bold ", Some(bs())),
                ("code", Some(bcs())),
                (" bold", Some(bs())),
            ],
        );
    }
    #[test]
    fn bold_inside_code() {
        assert_inline(
            "`code **bold** code`",
            &[("code **bold** code", Some(cs()))],
        );
    }
    #[test]
    fn entire_bold_is_code() {
        assert_inline("**`all`**", &[("all", Some(bcs()))]);
    }
    #[test]
    fn entire_code_is_bold() {
        assert_inline("`**all**`", &[("**all**", Some(cs()))]);
    }
    #[test]
    fn unclosed_nested_code_in_bold() {
        assert_inline("**bold `unclosed**", &[("bold `unclosed", Some(bs()))]);
    }
    #[test]
    fn unclosed_nested_bold_in_code() {
        assert_inline("`code **unclosed`", &[("code **unclosed", Some(cs()))]);
    }
    #[test]
    fn no_delimiters() {
        assert_inline("plain text", &[("plain text", None)]);
    }
    #[test]
    fn empty_code_span() {
        assert_inline("``", &[("``", None)]);
    }
    #[test]
    fn empty_bold_span() {
        assert_inline("****", &[("****", None)]);
    }
    #[test]
    fn star_with_spaces_not_italic() {
        assert_inline("a * b", &[("a * b", None)]);
    }
    #[test]
    fn intraword_stars_not_italic() {
        assert_inline("a*b*c", &[("a*b*c", None)]);
    }
    #[test]
    fn two_code_spans_with_text() {
        assert_inline(
            "`a` middle `b`",
            &[("a", Some(cs())), (" middle ", None), ("b", Some(cs()))],
        );
    }
    #[test]
    fn code_then_bold() {
        assert_inline(
            "`a` **b**",
            &[("a", Some(cs())), (" ", None), ("b", Some(bs()))],
        );
    }
    #[test]
    fn bold_then_code() {
        assert_inline(
            "**a** `b`",
            &[("a", Some(bs())), (" ", None), ("b", Some(cs()))],
        );
    }
    #[test]
    fn code_then_unclosed_backtick() {
        assert_inline(
            "a `b` c `unclosed",
            &[("a ", None), ("b", Some(cs())), (" c `unclosed", None)],
        );
    }
    #[test]
    fn bold_then_unclosed_bold() {
        assert_inline(
            "a **b** c **unclosed",
            &[("a ", None), ("b", Some(bs())), (" c **unclosed", None)],
        );
    }
    #[test]
    fn interleaved_bold_code() {
        assert_inline("**a `b** c`", &[("**a ", None), ("b** c", Some(cs()))]);
    }
    #[test]
    fn interleaved_code_bold() {
        assert_inline("`a **b` c**", &[("a **b", Some(cs())), (" c**", None)]);
    }
    #[test]
    fn triple_star_bold_italic() {
        assert_inline("***bold italic***", &[("bold italic", Some(bis()))]);
    }
    #[test]
    fn code_span_captures_bold_delim() {
        assert_inline("**`**`", &[("**", None), ("**", Some(cs()))]);
    }
    #[test]
    fn italic_star() {
        assert_inline(
            "some *emphasized* word",
            &[("some ", None), ("emphasized", Some(IS)), (" word", None)],
        );
    }
    #[test]
    fn italic_underscore() {
        assert_inline("_italic_", &[("italic", Some(IS))]);
    }
    #[test]
    fn intraword_underscores_not_italic() {
        assert_inline("file_name_here", &[("file_name_here", None)]);
    }
    #[test]
    fn double_underscore_not_italic() {
        assert_inline("__dunder__", &[("__dunder__", None)]);
    }
    #[test]
    fn strikethrough_test() {
        assert_inline(
            "a ~~struck~~ b",
            &[("a ", None), ("struck", Some(ss())), (" b", None)],
        );
    }
    #[test]
    fn empty_strikethrough() {
        assert_inline("~~~~", &[("~~~~", None)]);
    }
    #[test]
    fn double_backtick_code_span() {
        assert_inline(
            "``code with ` inside``",
            &[("code with ` inside", Some(cs()))],
        );
    }
    #[test]
    fn triple_backtick_inline_code() {
        assert_inline("```code```", &[("code", Some(cs()))]);
    }
    #[test]
    fn italic_inside_bold() {
        assert_inline(
            "**bold *italic* bold**",
            &[
                ("bold ", Some(bs())),
                ("italic", Some(bis())),
                (" bold", Some(bs())),
            ],
        );
    }
    #[test]
    fn bold_closer_inside_code_ignored() {
        assert_inline(
            "**bold `code**` bold**",
            &[
                ("bold ", Some(bs())),
                ("code**", Some(bcs())),
                (" bold", Some(bs())),
            ],
        );
    }

    #[test_case("here is `/home/tony/file.rs` path" ; "path_in_backticks")]
    #[test_case("use `fn main()` and **important**" ; "code_and_bold_real_content")]
    #[test_case("**`/home/tony/c/maki/src/tools/read.rs:23-38`**" ; "bold_code_path")]
    #[test_case("### 1. Data ` Types` — How Output" ; "heading_with_stray_backtick")]
    #[test_case("**/ Diffs Are Structured" ; "unclosed_bold_with_slash")]
    #[test_case("text `code` more **bold** end `code2` fin" ; "mixed_inline")]
    fn inline_parse_invariants(input: &str) {
        let base = Style::default();
        let spans = parse_inline_markdown(input, base);
        let reconstructed: String = spans.iter().map(|s| s.content.as_ref()).collect();

        let mut input_chars = input.chars().peekable();
        for ch in reconstructed.chars() {
            loop {
                match input_chars.next() {
                    Some(c) if c == ch => break,
                    Some(_) => continue,
                    None => panic!(
                        "output not a subsequence of input\n  input: {input:?}\n  output: {reconstructed:?}"
                    ),
                }
            }
        }

        let strip = |s: &str| -> String {
            s.chars()
                .filter(|c| !matches!(c, '`' | '*' | '~' | '_'))
                .collect()
        };
        assert_eq!(
            strip(&reconstructed),
            strip(input),
            "non-delimiter content lost or reordered\n  input: {input:?}\n  output: {reconstructed:?}"
        );
    }

    #[test_case("line1\nline2\nline3", 3, "p> line1" ; "splits_newlines")]
    #[test_case("\n\nfirst line\nsecond", 2, "p> first line" ; "strips_leading_newlines")]
    fn text_to_lines_cases(input: &str, expected_lines: usize, first_line: &str) {
        let style = Style::default();
        let lines = text_to_lines(input, "p> ", style, style, None, TEST_WIDTH);
        assert_eq!(lines.len(), expected_lines);
        assert_eq!(lines_text(&lines)[0], first_line);
    }

    #[test_case("a\nb\nc", 5, Keep::Head, "a\nb\nc" ; "under_limit_returns_input")]
    #[test_case("a\nb\nc\nd", 2, Keep::Head, "a\nb\n... (2 lines)" ; "head_over_limit")]
    #[test_case("a\nb\nc", 2, Keep::Head, "a\nb\n... (1 line)" ; "head_singular_notice")]
    #[test_case("a\nb\nc\nd", 2, Keep::Tail, "... (2 lines)\nc\nd" ; "tail_over_limit")]
    #[test_case("a\nb\nc\nd\ne", 3, Keep::Tail, "... (2 lines)\nc\nd\ne" ; "tail_keeps_last_n")]
    #[test_case("a\nb\nc", 2, Keep::Tail, "... (1 line)\nb\nc" ; "tail_singular_notice")]
    fn truncate_lines_cases(input: &str, max: usize, keep: Keep, expected: &str) {
        assert_eq!(truncate_lines(input, max, keep), expected);
    }

    fn block_summary<'a>(blocks: &'a [TextBlock<'a>]) -> Vec<(&'a str, Option<&'a str>)> {
        blocks
            .iter()
            .filter_map(|b| match b {
                TextBlock::Normal(t) => Some((*t, None)),
                TextBlock::Code { lang, code } => Some((*code, Some(*lang))),
                TextBlock::Table { .. } => None,
            })
            .collect()
    }

    #[test_case(
        "hello world\nsecond line",
        &[("hello world\nsecond line", None)]
        ; "no_fences"
    )]
    #[test_case(
        "before\n```rust\nfn main() {}\n```\nafter",
        &[("before", None), ("fn main() {}", Some("rust")), ("after", None)]
        ; "single_code_block"
    )]
    #[test_case(
        "a\n```py\nx=1\n```\nb\n```js\ny=2\n```\nc",
        &[("a", None), ("x=1", Some("py")), ("b", None), ("y=2", Some("js")), ("c", None)]
        ; "multiple_code_blocks"
    )]
    #[test_case(
        "before\n```rust\nfn main() {}",
        &[("before", None), ("fn main() {}", Some("rust"))]
        ; "unclosed_fence"
    )]
    #[test_case(
        "a\n```rs\n```\nb",
        &[("a", None), ("", Some("rs")), ("b", None)]
        ; "empty_code_block"
    )]
    #[test_case(
        "```\ncode\n```",
        &[("code", Some(""))]
        ; "no_language_tag"
    )]
    #[test_case(
        "inline ```code``` here\ntext with ``` inside\nand more",
        &[("inline ```code``` here\ntext with ``` inside\nand more", None)]
        ; "mid_line_backticks_not_a_fence"
    )]
    #[test_case(
        "before\n````markdown\n```rust\nfn main() {}\n```\n````\nafter",
        &[("before", None), ("```rust\nfn main() {}\n```", Some("markdown")), ("after", None)]
        ; "four_backtick_fence_nests_three"
    )]
    #[test_case(
        "before\n```md\nuse ``` in code\n```\nafter",
        &[("before", None), ("use ``` in code", Some("md")), ("after", None)]
        ; "backticks_inside_code_block_not_closing_fence"
    )]
    #[test_case(
        "before\n```rs\ncode\n```trailing\nmore",
        &[("before", None), ("code", Some("rs")), ("trailing\nmore", None)]
        ; "closing_fence_with_trailing_text"
    )]
    #[test_case(
        "```\nhello\n```  \nafter",
        &[("hello", Some("")), ("after", None)]
        ; "closing_fence_trailing_whitespace_dropped"
    )]
    #[test_case(
        "before\n```rust",
        &[("before\n```rust", None)]
        ; "partial_fence_no_newline_after_lang"
    )]
    #[test_case(
        "```",
        &[("```", None)]
        ; "only_backticks_no_newline"
    )]
    #[test_case(
        "a\n```\n",
        &[("a", None), ("", Some(""))]
        ; "fence_with_newline_then_eof"
    )]
    #[test_case(
        "```lang`ish\ncode\n```",
        &[("```lang`ish\ncode\n```", None)]
        ; "backtick_in_info_string_not_a_fence"
    )]
    fn parse_blocks_cases(input: &str, expected: &[(&str, Option<&str>)]) {
        let blocks = parse_blocks(input);
        assert_eq!(block_summary(&blocks), expected);
    }

    fn lines_text(lines: &[Line<'_>]) -> Vec<String> {
        lines
            .iter()
            .map(|l| {
                l.spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>()
            })
            .collect()
    }

    fn strip_md(s: &str) -> String {
        s.chars()
            .filter(|c| {
                !matches!(
                    c,
                    '`' | '*'
                        | '#'
                        | '•'
                        | '-'
                        | '+'
                        | '~'
                        | '_'
                        | '─'
                        | '│'
                        | '╭'
                        | '╮'
                        | '├'
                        | '┤'
                        | '╰'
                        | '╯'
                        | '┬'
                        | '┴'
                        | '┼'
                        | '|'
                )
            })
            .collect()
    }

    fn normalize_ws(s: &str) -> String {
        s.split_whitespace().collect::<Vec<_>>().join(" ")
    }

    #[test]
    fn highlighter_reuse_matches_fresh_render() {
        let style = Style::default();
        let text = "hello\n```rust\nfn main() {}\n```\nbye";
        let full = text_to_lines(text, "p> ", style, style, None, TEST_WIDTH);
        let mut hl = Vec::new();
        let inc = text_to_lines(text, "p> ", style, style, Some(&mut hl), TEST_WIDTH);
        assert_eq!(lines_text(&full), lines_text(&inc));
    }

    #[test_case(
        "Here is **bold** and `code` text.\nLine2 has `more` stuff."
        ; "streaming_mixed_markdown"
    )]
    #[test_case(
        "### 1. Data Types\n\nHere is `/home/file.rs` path\n**bold** end"
        ; "streaming_heading_with_code"
    )]
    #[test_case(
        "**`/home/tony/c/maki/src/tools/read.rs:23-38`**\n\nSome text after"
        ; "streaming_bold_code_path"
    )]
    #[test_case(
        "Before\n```rust\nfn main() {}\n```\nAfter with **bold**"
        ; "streaming_code_block_then_inline"
    )]
    #[test_case(
        "a `b` c **d** e\n`f` **g**\nh"
        ; "streaming_multiline_inline"
    )]
    #[test_case(
        "- **bold item**\n- `code item`\n  - nested"
        ; "streaming_list_with_inline"
    )]
    #[test_case(
        "Here is *italic* and ~~struck~~ text with _underscores_"
        ; "streaming_italic_strike_underscore"
    )]
    #[test_case(
        concat!(
            "## Refactoring `parse_inline` for ***extensibility***\n",
            "\n",
            "The old approach used a ~~naive~~ **greedy** scan:\n",
            "\n",
            "```rust\n",
            "fn find_earliest_delim(text: &str) -> Option<(usize, &str)> {\n",
            "    [(\"**\", BOLD), (\"`\", CODE)]\n",
            "        .into_iter()\n",
            "        .filter_map(|(d, s)| text.find(d).map(|p| (p, d, s)))\n",
            "        .min_by_key(|(p, _, _)| *p)\n",
            "}\n",
            "```\n",
            "\n",
            "Key changes:\n",
            "\n",
            "1. **Priority**: backtick runs are matched *first*, so `code **ignores** bold`\n",
            "2. **Nesting**: ``code with ` inside`` uses double-backtick fencing\n",
            "3. **Emphasis stack**:\n",
            "   - Single `*` or `_` for *italic*\n",
            "   - Double `**` for **bold**\n",
            "   - Triple `***` for ***bold italic***\n",
            "   - `~~tildes~~` for ~~strikethrough~~\n",
            "\n",
            "Run `cargo test -p maki-ui -- markdown` to verify.\n",
            "\n",
            "````markdown\n",
            "```rust\n",
            "fn nested_fence() {}\n",
            "```\n",
            "````\n",
            "\n",
            "- **`/home/tony/c/maki/src/tools/read.rs:23-38`** was the _root cause_\n",
            "- Items with `inline_code` and **bold** and *italic* and ~~struck~~ in one line",
        )
        ; "streaming_realistic_llm_response"
    )]
    #[test_case(
        "Before table\n\n| Name | Value |\n| --- | --- |\n| foo | 42 |\n| bar | 99 |\n\nAfter table"
        ; "streaming_table_between_paragraphs"
    )]
    fn streaming_never_garbles(input: &str) {
        let style = Style::default();
        let step = if input.len() > 200 { 31 } else { 1 };
        let mut end = step;
        while end <= input.len() {
            if !input.is_char_boundary(end) {
                end += 1;
                continue;
            }
            let prefix = &input[..end];
            let lines = text_to_lines(prefix, "", style, style, None, TEST_WIDTH);
            let rendered: String = lines
                .iter()
                .map(|l| {
                    l.spans
                        .iter()
                        .map(|s| s.content.as_ref())
                        .collect::<String>()
                })
                .collect::<Vec<_>>()
                .join("\n");

            for line in rendered.split('\n') {
                if line.is_empty() {
                    continue;
                }
                let trimmed = line.trim_end();
                let without_bar = trimmed
                    .strip_prefix(CODE_BAR)
                    .or_else(|| trimmed.strip_prefix(CODE_BAR.trim_end()))
                    .unwrap_or(trimmed);
                let line_stripped = normalize_ws(&strip_md(without_bar));
                if line_stripped.is_empty() {
                    continue;
                }
                let input_stripped = normalize_ws(&strip_md(prefix));
                assert!(
                    input_stripped.contains(&line_stripped),
                    "rendered line not found in input at prefix len={end}\n  prefix: {prefix:?}\n  rendered line: {line:?}\n  full rendered: {rendered:?}"
                );
            }
            end += step;
        }
    }

    fn hr_line() -> String {
        iter::repeat_n(HR_CHAR, TEST_WIDTH as usize).collect()
    }

    #[test_case("---", true ; "three_dashes")]
    #[test_case("***", true ; "three_stars")]
    #[test_case("___", true ; "three_underscores")]
    #[test_case("-----", true ; "five_dashes")]
    #[test_case("- - -", true ; "spaced_dashes")]
    #[test_case("  ---  ", true ; "indented_dashes")]
    #[test_case("--", false ; "two_dashes_too_short")]
    #[test_case("- item", false ; "list_item_not_hr")]
    #[test_case("---text", false ; "text_after_dashes")]
    fn horizontal_rule_detection(input: &str, expected: bool) {
        assert_eq!(is_horizontal_rule(input), expected);
    }

    #[test_case(
        "before\n---\nafter",
        &["before", &hr_line(), "after"]
        ; "hr_between_paragraphs"
    )]
    #[test_case(
        "---",
        &[&hr_line()]
        ; "hr_only"
    )]
    fn horizontal_rule_rendering(input: &str, expected: &[&str]) {
        let style = Style::default();
        let lines = text_to_lines(input, "", style, style, None, TEST_WIDTH);
        assert_eq!(lines_text(&lines), expected);
    }

    fn prefixed(code: &str) -> String {
        format!("{}{code}", CODE_BAR)
    }

    #[test_case(
        "before\n```rust\nfn main() {}\n```\nafter",
        vec!["before".into(), "".into(), prefixed("fn main() {}"), "".into(), "after".into()]
        ; "margin_around_code_block"
    )]
    #[test_case(
        "before\n\n```rust\ncode\n```\n\nafter",
        vec!["before".into(), "".into(), prefixed("code"), "".into(), "after".into()]
        ; "extra_blanks_collapsed"
    )]
    #[test_case(
        "hello\n```rust\ncode\n```",
        vec!["hello".into(), "".into(), prefixed("code")]
        ; "no_trailing_blank_after_final_code_block"
    )]
    fn code_block_margins(input: &str, expected: Vec<String>) {
        let style = Style::default();
        let lines = text_to_lines(input, "", style, style, None, TEST_WIDTH);
        assert_eq!(lines_text(&lines), expected);
    }

    #[test_case("# heading", "heading" ; "h1")]
    #[test_case("###### heading", "heading" ; "h6")]
    #[test_case("# ", "" ; "h1_empty")]
    fn heading_parsed(input: &str, expected: &str) {
        assert_eq!(parse_heading(input), Some(expected));
    }

    #[test]
    fn heading_with_inline_markdown() {
        let style = Style::default();
        let lines = text_to_lines("## **bold** and `code`", "", style, style, None, TEST_WIDTH);
        assert_eq!(lines.len(), 1);
        let text: String = lines[0].spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(text, "bold and code");
        let styles: Vec<_> = lines[0].spans.iter().map(|s| s.style).collect();
        assert!(styles.contains(&theme::current().bold));
        assert!(styles.contains(&theme::current().heading));
        assert!(styles.contains(&code_style(theme::current().heading)));
    }

    #[test_case("##nospace" ; "no_space_not_heading")]
    #[test_case("####### seven" ; "seven_hashes_not_heading")]
    fn not_a_heading(input: &str) {
        assert_eq!(parse_heading(input), None);
    }

    #[test_case(
        "- first\n- second\n- third",
        &["• first", "• second", "• third"]
        ; "simple_unordered_list"
    )]
    #[test_case(
        "- item\n  - nested\n    - deep",
        &["• item", "  • nested", "    • deep"]
        ; "nested_unordered_list"
    )]
    #[test_case(
        "* star item\n+ plus item",
        &["• star item", "• plus item"]
        ; "star_and_plus_markers"
    )]
    #[test_case(
        "1. first\n2. second\n3. third",
        &["1. first", "2. second", "3. third"]
        ; "simple_ordered_list"
    )]
    #[test_case(
        "1. item\n   - nested bullet",
        &["1. item", "  • nested bullet"]
        ; "ordered_then_nested_unordered"
    )]
    #[test_case(
        "10. double digits\n100. triple digits",
        &["10. double digits", "100. triple digits"]
        ; "multi_digit_numbers"
    )]
    fn list_rendering(input: &str, expected: &[&str]) {
        let style = Style::default();
        let lines = text_to_lines(input, "", style, style, None, TEST_WIDTH);
        assert_eq!(lines_text(&lines), expected);
    }

    #[test_case("- item", "• " ; "unordered_bullet")]
    #[test_case("1. item", "1. " ; "ordered_number")]
    fn list_marker_styled(input: &str, expected_marker: &str) {
        let style = Style::default();
        let lines = text_to_lines(input, "", style, style, None, TEST_WIDTH);
        let marker = lines[0]
            .spans
            .iter()
            .find(|s| s.style == theme::current().list_marker);
        assert_eq!(marker.unwrap().content, expected_marker);
    }

    #[test]
    fn list_item_with_inline_markdown() {
        let style = Style::default();
        let lines = text_to_lines("- **bold** and `code`", "", style, style, None, TEST_WIDTH);
        let text: String = lines[0].spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(text, "• bold and code");
    }

    #[test_case(
        "**bold** `code` ```fences```",
        &["p> **bold** `code` ```fences```"]
        ; "plain_ignores_all_markdown"
    )]
    #[test_case(
        "before\n```rust\nfn main() {}\n```\nafter",
        &["p> before", "```rust", "fn main() {}", "```", "after"]
        ; "plain_preserves_code_fences_literally"
    )]
    #[test_case(
        "line1\nline2",
        &["p> line1", "line2"]
        ; "plain_splits_lines"
    )]
    fn plain_content(input: &str, expected: &[&str]) {
        let base = Style::new().fg(ratatui::style::Color::Cyan);
        let lines = plain_lines(input, "p> ", base, base);
        assert_eq!(lines_text(&lines), expected);
        for line in &lines {
            for span in &line.spans {
                assert!(
                    span.style == base || span.style == base.add_modifier(Modifier::BOLD),
                    "unexpected style on {:?}",
                    span.content
                );
            }
        }
    }

    #[test_case("| a | b |", true ; "valid_row")]
    #[test_case("| a | b | c |", true ; "three_cols")]
    #[test_case("no pipes", false ; "no_pipes")]
    #[test_case("| single |", true ; "single_col")]
    #[test_case("| a | b", false ; "no_trailing_pipe")]
    #[test_case("a | b |", false ; "no_leading_pipe")]
    fn is_table_row_cases(input: &str, expected: bool) {
        assert_eq!(is_table_row(input), expected);
    }

    #[test_case("| --- | --- |", true ; "simple_sep")]
    #[test_case("| :---: | ---: |", true ; "aligned_sep")]
    #[test_case("| - | -- |", true ; "short_dashes")]
    #[test_case("| abc | def |", false ; "not_sep")]
    #[test_case("| --- |", true ; "single_col_sep")]
    fn is_separator_row_cases(input: &str, expected: bool) {
        assert_eq!(is_separator_row(input), expected);
    }

    #[test_case("| a | b |", &["a", "b"] ; "basic_cells")]
    #[test_case("|  x  |  y  |  z  |", &["x", "y", "z"] ; "trimmed_cells")]
    #[test_case("| a |", &["a"] ; "single_cell")]
    #[test_case("| a \\| b | c |", &["a | b", "c"] ; "escaped_pipe")]
    #[test_case("| `a | b` | c |", &["`a | b`", "c"] ; "pipe_in_backtick_code")]
    #[test_case("| `a \\| b` | c |", &["`a \\| b`", "c"] ; "escaped_pipe_in_code_preserved")]
    #[test_case("| ``a | b`` | c |", &["``a | b``", "c"] ; "pipe_in_double_backtick_code")]
    fn parse_table_cells_cases(input: &str, expected: &[&str]) {
        let result = parse_table_cells(input);
        let result: Vec<&str> = result.iter().map(String::as_str).collect();
        assert_eq!(result, expected);
    }

    #[test]
    fn split_normal_extracts_table() {
        let blocks = split_normal_blocks("before\n| a | b |\n| --- | --- |\n| 1 | 2 |\nafter");
        assert_eq!(blocks.len(), 3);
        assert!(matches!(blocks[0], TextBlock::Normal(_)));
        assert!(matches!(blocks[2], TextBlock::Normal(_)));
        let TextBlock::Table {
            ref rows,
            header_end,
        } = blocks[1]
        else {
            panic!("expected Table block at index 1");
        };
        assert_eq!(header_end, 1);
        assert_eq!(rows, &[vec!["a", "b"], vec!["1", "2"]]);
    }

    #[test_case("| h |\n| --- |\n| d |\nafter", 0 ; "table_at_start")]
    #[test_case("before\n| h |\n| --- |\n| d |", -1 ; "table_at_end")]
    fn split_normal_table_position(input: &str, idx: isize) {
        let blocks = split_normal_blocks(input);
        let i = if idx < 0 {
            blocks.len() - 1
        } else {
            idx as usize
        };
        assert!(matches!(blocks[i], TextBlock::Table { .. }));
    }

    #[test]
    fn render_table_structure() {
        let style = Style::default();
        let input = "| Name | Value |\n| --- | --- |\n| foo | 42 |";
        let lines = text_to_lines(input, "", style, style, None, TEST_WIDTH);
        let joined = lines_text(&lines).join("\n");
        for expected in ["Name", "foo", "42"] {
            assert!(joined.contains(expected), "missing {expected:?} in table");
        }
        assert!(
            lines.len() >= 5,
            "table should have border+header+sep+data+border"
        );
        let sep_lines: Vec<_> = lines
            .iter()
            .filter(|l| l.spans.iter().any(|s| s.content.contains('├')))
            .collect();
        for sep in &sep_lines {
            assert_eq!(
                sep.spans.first().unwrap().style,
                theme::current().table_border,
                "all separators should use table_border_style"
            );
        }
    }

    #[test_case("| H |\n| --- |\n| a |\n| b |\n| c |", 3 ; "multi_row_separators")]
    #[test_case("| H |\n| --- |\n| only |", 1 ; "single_row_header_only")]
    fn table_separator_count(input: &str, expected: usize) {
        let style = Style::default();
        let lines = text_to_lines(input, "", style, style, None, TEST_WIDTH);
        let sep_count = lines
            .iter()
            .filter(|l| l.spans.iter().any(|s| s.content.contains('├')))
            .count();
        assert_eq!(sep_count, expected);
    }

    #[test]
    fn render_table_escaped_pipe_stays_in_cell() {
        let style = Style::default();
        let input = "| Query | Result |\n| --- | --- |\n| `cmd \\| filter` | ok |";
        let lines = text_to_lines(input, "", style, style, None, 80);
        let joined = lines_text(&lines).join("\n");
        assert!(
            joined.contains("cmd \\| filter"),
            "escaped pipe content missing"
        );
        assert!(joined.contains("ok"), "adjacent cell missing");
    }

    #[test]
    fn table_with_prefix() {
        let style = Style::default();
        let input = "| a | b |\n| --- | --- |\n| 1 | 2 |";
        let lines = text_to_lines(input, "p> ", style, style, None, TEST_WIDTH);
        assert_eq!(lines[0].spans[0].content, "p> ");
    }

    #[test]
    fn table_no_double_blank_before_hr() {
        let style = Style::default();
        let input = "| a | b |\n| --- | --- |\n| 1 | 2 |\n\n---\n\nafter";
        let lines = text_to_lines(input, "", style, style, None, TEST_WIDTH);
        let text = lines_text(&lines);
        let consecutive_blanks = text
            .windows(2)
            .filter(|w| w[0].is_empty() && w[1].is_empty())
            .count();
        assert_eq!(
            consecutive_blanks, 0,
            "should never have two consecutive blank lines"
        );
    }

    #[test]
    fn mismatched_cell_counts_does_not_panic() {
        let style = Style::default();
        let input = "| a | b | c |\n| --- | --- | --- |\n| 1 | 2 |";
        let _ = text_to_lines(input, "", style, style, None, TEST_WIDTH);
    }

    #[test]
    fn header_row_is_bold() {
        let style = Style::default();
        let input = "| Header |\n| --- |\n| Data |";
        let lines = text_to_lines(input, "", style, style, None, TEST_WIDTH);
        let header_span = lines
            .iter()
            .flat_map(|l| &l.spans)
            .find(|s| s.content.trim() == "Header")
            .expect("Header span");
        assert!(header_span.style.add_modifier.contains(Modifier::BOLD));
    }

    fn assert_table_within_width(input: &str, width: u16) {
        let style = Style::default();
        let lines = text_to_lines(input, "", style, style, None, width);
        for line in lines_text(&lines) {
            assert!(
                line.width() <= width as usize,
                "line exceeds width {width}: ({}) {line:?}",
                line.width()
            );
        }
    }

    #[test]
    fn table_wraps_and_preserves_content() {
        let style = Style::default();
        let long = "x".repeat(60);
        let input = format!("| Col1 | Col2 |\n| --- | --- |\n| short | {long} |");
        let width: u16 = 40;
        let lines = text_to_lines(&input, "", style, style, None, width);
        let rendered: String = lines_text(&lines).join("");
        let x_count = rendered.chars().filter(|c| *c == 'x').count();
        assert_eq!(x_count, 60, "wrapped table must preserve all content");
        assert_table_within_width(&input, width);
    }

    #[test]
    fn table_narrow_prose_within_width() {
        let input = "| Test | Rationale |\n| --- | --- |\n| name | This is a very long rationale that should definitely be wrapped when the terminal width is narrow enough to require it |";
        assert_table_within_width(input, 50);
    }

    fn wrap_texts(spans: Vec<Span<'static>>, width: usize) -> Vec<String> {
        wrap_cell_spans(spans, width)
            .iter()
            .map(|l| l.iter().map(|s| s.content.as_ref()).collect())
            .collect()
    }

    #[test_case("hello world foo bar", 10, &["hello", "world foo", "bar"] ; "word_boundary")]
    #[test_case("abcdefghij", 6, &["abcdef", "ghij"] ; "char_boundary_fallback")]
    fn wrap_cell_spans_cases(input: &str, width: usize, expected: &[&str]) {
        assert_eq!(
            wrap_texts(vec![Span::raw(input.to_owned())], width),
            expected
        );
    }

    #[test_case(&[10, 90], 50, true  ; "shrinks_proportionally")]
    #[test_case(&[10, 20], 50, false ; "noop_when_fits")]
    fn constrain_col_widths_cases(input: &[usize], available: usize, should_shrink: bool) {
        let mut widths = input.to_vec();
        let original = widths.clone();
        constrain_col_widths(&mut widths, available);
        assert!(widths.iter().sum::<usize>() <= available);
        for w in &widths {
            assert!(*w >= MIN_COL_WIDTH);
        }
        if !should_shrink {
            assert_eq!(widths, original);
        }
    }

    fn content_text(lines: &[Line<'_>]) -> String {
        lines
            .iter()
            .flat_map(|l| &l.spans)
            .filter(|s| s.content.as_ref() != CODE_BAR && s.content.as_ref() != CODE_BAR_WRAP)
            .map(|s| s.content.as_ref())
            .collect()
    }

    #[test]
    fn wrap_preserves_content() {
        let code = "a".repeat(20);
        let lines = highlight_code("txt", &code, 12);
        assert!(lines.len() >= 2);
        assert_eq!(content_text(&lines), code);
    }

    #[test]
    fn wrap_zero_width_does_not_panic() {
        let lines = highlight_code("txt", "hello", 0);
        assert!(!lines.is_empty());
    }

    #[test]
    fn wrap_code_lines_preserves_prefix() {
        let style = Style::default();
        let input = format!("```\n{}\n```", "a".repeat(30));
        let lines = text_to_lines(&input, "", style, style, None, 15);
        for line in &lines {
            if is_blank_line(line) {
                continue;
            }
            let first = &line.spans[0].content;
            assert!(
                first.as_ref() == CODE_BAR || first.as_ref() == CODE_BAR_WRAP,
                "code line missing bar prefix: {first:?}"
            );
        }
    }

    #[test]
    fn wrap_code_lines_preserves_lines_before_start() {
        let mut lines = vec![
            Line::from("header"),
            Line::from(vec![
                Span::styled(CODE_BAR, theme::current().code_bar),
                Span::raw("short"),
            ]),
        ];
        wrap_code_lines(&mut lines, 1, 80);
        assert_eq!(lines[0].spans[0].content, "header");
    }
}

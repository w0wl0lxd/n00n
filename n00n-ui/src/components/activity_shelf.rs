use std::time::Duration;

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::theme;

const MAX_ROWS: usize = 4;
pub(crate) const STATUS: &str = "RUNNING";

pub struct Activity<'a> {
    pub name: &'a str,
    pub phase: &'a str,
    pub elapsed: Duration,
    pub detail: &'a str,
    pub status: &'a str,
}

fn truncate_end(text: &str, max_width: usize) -> String {
    if text.width() <= max_width {
        return text.to_owned();
    }
    if max_width == 0 {
        return String::new();
    }
    let mut result = String::new();
    let target = max_width.saturating_sub(1);
    let mut width = 0;
    for ch in text.chars() {
        let char_width = ch.width().map_or(0, |width| width);
        if width + char_width > target {
            break;
        }
        result.push(ch);
        width += char_width;
    }
    result.push('\u{2026}');
    result
}

fn truncate_tail(text: &str, max_width: usize) -> String {
    if text.width() <= max_width {
        return text.to_owned();
    }
    if max_width == 0 {
        return String::new();
    }
    let target = max_width.saturating_sub(1);
    let mut width = 0;
    let mut start = text.len();
    for (index, ch) in text.char_indices().rev() {
        let char_width = ch.width().map_or(0, |width| width);
        if width + char_width > target {
            break;
        }
        start = index;
        width += char_width;
    }
    format!("…{}", &text[start..])
}

fn elapsed_text(elapsed: Duration) -> String {
    let seconds = elapsed.as_secs();
    if seconds < 60 {
        format!("{seconds}s")
    } else {
        format!("{}m {:02}s", seconds / 60, seconds % 60)
    }
}

fn activity_lines<'a>(
    activities: &[Activity<'a>],
    available_width: u16,
    available_rows: usize,
) -> Vec<Line<'a>> {
    let width = usize::from(available_width);
    activities
        .iter()
        .take(available_rows.min(MAX_ROWS))
        .map(|activity| {
            let status = truncate_end(activity.status, width);
            let status_width = status.width();
            let left_width = width.saturating_sub(status_width.saturating_add(1));
            let prefix = format!(
                "◈ {} · {} · {}",
                activity.name,
                activity.phase,
                elapsed_text(activity.elapsed)
            );
            let left = if prefix.width() >= left_width {
                truncate_end(&prefix, left_width)
            } else {
                let detail_width = left_width - prefix.width() - 3;
                let detail = truncate_tail(activity.detail, detail_width);
                if detail.is_empty() {
                    prefix
                } else {
                    format!("{prefix} · {detail}")
                }
            };
            let padding = " ".repeat(width.saturating_sub(left.width() + status_width));
            Line::from(vec![
                Span::styled(left, theme::semantic_style(theme::SemanticRole::Text)),
                Span::raw(padding),
                Span::styled(status, theme::semantic_style(theme::SemanticRole::Activity)),
            ])
        })
        .collect()
}

pub fn view(frame: &mut Frame, area: Rect, activities: &[Activity<'_>]) {
    if area.is_empty() || activities.is_empty() {
        return;
    }
    frame.render_widget(
        Paragraph::new(activity_lines(
            activities,
            area.width,
            usize::from(area.height),
        )),
        area,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn activity(detail: &'static str) -> Activity<'static> {
        Activity {
            name: "task",
            phase: "tools",
            elapsed: Duration::from_secs(12),
            detail,
            status: STATUS,
        }
    }

    fn line_text(line: &Line<'_>) -> String {
        line.spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect()
    }

    #[test]
    fn shelf_renders_phase_elapsed_detail_and_four_rows() {
        let activities = vec![
            activity("tail"),
            activity("tail"),
            activity("tail"),
            activity("tail"),
            activity("tail"),
        ];
        let lines = activity_lines(&activities, 80, 6);

        assert_eq!(lines.len(), MAX_ROWS);
        let text = line_text(&lines[0]);
        assert!(text.starts_with("◈ task · tools · 12s · tail"));
        assert!(text.ends_with(STATUS));
    }

    #[test]
    fn narrow_rows_keep_status_on_the_right_and_bound_width() {
        let lines = activity_lines(
            &[activity("a very long output tail that should be clipped")],
            28,
            2,
        );
        let text = line_text(&lines[0]);

        assert!(text.ends_with(STATUS));
        assert!(text.width() <= 28);
        assert!(!text.contains("a very long output tail"));
    }

    #[test]
    fn empty_width_is_safe() {
        let lines = activity_lines(&[activity("tail")], 0, 2);
        assert_eq!(line_text(&lines[0]), "");
    }
}

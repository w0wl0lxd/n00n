use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use crate::theme;

pub struct Activity<'a> {
    pub name: &'a str,
    pub in_progress: usize,
}

fn activity_lines<'a>(activities: &[Activity<'a>], available_rows: usize) -> Vec<Line<'a>> {
    activities
        .iter()
        .take(available_rows.min(4))
        .map(|activity| {
            let detail = match activity.in_progress {
                0 => "responding".to_owned(),
                1 => "1 operation".to_owned(),
                count => format!("{count} operations"),
            };
            Line::from(vec![
                Span::styled(
                    "◈ running ",
                    theme::semantic_style(theme::SemanticRole::Activity),
                ),
                Span::styled(
                    activity.name,
                    theme::semantic_style(theme::SemanticRole::Text),
                ),
                Span::styled(
                    format!(" · {detail}"),
                    theme::semantic_style(theme::SemanticRole::Muted),
                ),
            ])
        })
        .collect()
}

pub fn view(frame: &mut Frame, area: Rect, activities: &[Activity<'_>]) {
    if area.is_empty() || activities.is_empty() {
        return;
    }
    frame.render_widget(
        Paragraph::new(activity_lines(activities, usize::from(area.height))),
        area,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line_text(line: &Line<'_>) -> String {
        line.spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect()
    }

    #[test]
    fn shelf_renders_live_state_and_is_bounded_to_four_rows() {
        let activities = (0..6)
            .map(|index| Activity {
                name: "task",
                in_progress: index,
            })
            .collect::<Vec<_>>();
        let lines = activity_lines(&activities, 6);

        assert_eq!(lines.len(), 4);
        assert_eq!(line_text(&lines[0]), "◈ running task · responding");
        assert_eq!(line_text(&lines[1]), "◈ running task · 1 operation");
        assert_eq!(line_text(&lines[2]), "◈ running task · 2 operations");
    }
}

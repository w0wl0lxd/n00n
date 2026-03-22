use crate::theme;
use maki_agent::{TodoItem, TodoStatus, ToolOutput};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Paragraph};
use std::collections::HashMap;

const PANEL_TITLE: &str = " Todos ";
const HIDE_HINT: &str = " Ctrl+T to hide ";

pub struct TodoPanel {
    visible: bool,
    user_dismissed: bool,
    items: Vec<TodoItem>,
}

impl TodoPanel {
    pub fn new() -> Self {
        Self {
            visible: false,
            user_dismissed: false,
            items: Vec::new(),
        }
    }

    pub fn reset(&mut self) {
        self.visible = false;
        self.user_dismissed = false;
        self.items.clear();
    }

    pub fn on_todowrite(&mut self, items: &[TodoItem]) {
        self.items = items.to_vec();
        if items.is_empty() {
            self.visible = false;
        } else if !self.user_dismissed {
            self.visible = true;
        }
    }

    pub fn restore(&mut self, tool_outputs: &HashMap<String, ToolOutput>) {
        self.items = extract_last_todos(tool_outputs);
        self.visible = !self.items.is_empty();
    }

    pub fn toggle(&mut self) {
        if self.items.is_empty() {
            return;
        }
        self.visible = !self.visible;
        self.user_dismissed = !self.visible;
    }

    pub fn on_done(&mut self) {
        self.visible = false;
    }

    pub fn height(&self) -> u16 {
        if !self.visible || self.items.is_empty() {
            0
        } else {
            self.items.len() as u16 + 2
        }
    }

    pub fn view(&self, frame: &mut Frame, area: Rect) {
        if self.items.is_empty() {
            return;
        }

        let t = theme::current();
        let lines: Vec<Line> = self
            .items
            .iter()
            .map(|item| {
                let style = match item.status {
                    TodoStatus::Completed => t.todo_completed,
                    TodoStatus::InProgress => t.todo_in_progress,
                    TodoStatus::Pending => t.todo_pending,
                    TodoStatus::Cancelled => t.todo_cancelled,
                };
                Line::from(Span::styled(
                    format!("{} {}", item.status.marker(), item.content),
                    style,
                ))
            })
            .collect();

        let block = Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(t.panel_border)
            .title_top(Line::from(PANEL_TITLE).left_aligned())
            .title_bottom(Line::from(Span::styled(HIDE_HINT, t.tool_dim)).right_aligned())
            .title_style(t.panel_title);

        let paragraph = Paragraph::new(lines)
            .style(Style::new().fg(t.foreground))
            .block(block);

        frame.render_widget(paragraph, area);
    }
}

fn extract_last_todos(outputs: &HashMap<String, ToolOutput>) -> Vec<TodoItem> {
    outputs
        .values()
        .find_map(|o| match o {
            ToolOutput::TodoList(items) => Some(items.clone()),
            _ => None,
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use maki_agent::{TodoPriority, TodoStatus};

    fn make_items(n: usize) -> Vec<TodoItem> {
        (0..n)
            .map(|i| TodoItem {
                content: format!("task {i}"),
                status: TodoStatus::Pending,
                priority: TodoPriority::Medium,
            })
            .collect()
    }

    #[test]
    fn on_todowrite_lifecycle() {
        let mut panel = TodoPanel::new();

        panel.on_todowrite(&make_items(2));
        assert!(panel.visible);
        assert_eq!(panel.items.len(), 2);

        panel.on_todowrite(&make_items(1));
        assert_eq!(panel.items.len(), 1);

        panel.on_todowrite(&[]);
        assert!(!panel.visible);
        assert!(panel.items.is_empty());
    }

    #[test]
    fn toggle_requires_items() {
        let mut panel = TodoPanel::new();
        panel.toggle();
        assert!(!panel.visible);

        panel.on_todowrite(&make_items(1));
        assert!(panel.visible);
        panel.toggle();
        assert!(!panel.visible);
        panel.toggle();
        assert!(panel.visible);
    }

    #[test]
    fn dismissed_stays_closed_until_manual_reopen() {
        let mut panel = TodoPanel::new();

        panel.on_todowrite(&make_items(1));
        assert!(panel.visible);

        panel.toggle();
        assert!(!panel.visible);

        panel.on_todowrite(&make_items(2));
        assert!(!panel.visible);
        assert_eq!(panel.items.len(), 2);

        panel.toggle();
        assert!(panel.visible);

        panel.on_todowrite(&make_items(3));
        assert!(panel.visible);
    }

    #[test]
    fn reset_clears_dismissed() {
        let mut panel = TodoPanel::new();
        panel.on_todowrite(&make_items(1));
        panel.toggle();
        panel.reset();
        assert!(!panel.visible);
        assert!(panel.items.is_empty());

        panel.on_todowrite(&make_items(1));
        assert!(panel.visible);
    }

    #[test]
    fn restore_from_tool_outputs() {
        let mut panel = TodoPanel::new();
        let mut map = HashMap::new();
        map.insert("a".into(), ToolOutput::TodoList(make_items(3)));
        map.insert("b".into(), ToolOutput::Plain("noise".into()));
        panel.restore(&map);
        assert!(panel.visible);
        assert_eq!(panel.items.len(), 3);

        panel.restore(&HashMap::new());
        assert!(!panel.visible);
    }

    #[test]
    fn height_scales_with_items() {
        let mut panel = TodoPanel::new();
        assert_eq!(panel.height(), 0);

        panel.on_todowrite(&make_items(3));
        assert_eq!(panel.height(), 5);

        panel.on_todowrite(&make_items(1));
        assert_eq!(panel.height(), 3);

        panel.toggle();
        assert_eq!(panel.height(), 0);
    }
}

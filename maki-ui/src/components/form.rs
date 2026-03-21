use crate::theme::Theme;

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::Line;
use ratatui::widgets::{Block, BorderType, Borders, Paragraph, Wrap};

pub(crate) fn render_form(
    t: &Theme,
    title: &str,
    frame: &mut Frame,
    area: Rect,
    lines: Vec<Line<'static>>,
    scroll: (u16, u16),
) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(t.panel_border)
        .title_top(Line::from(title.to_string()).left_aligned())
        .title_style(t.panel_title);

    let paragraph = Paragraph::new(lines)
        .style(Style::new().fg(t.foreground))
        .wrap(Wrap { trim: false })
        .block(block)
        .scroll(scroll);

    frame.render_widget(paragraph, area);
}

pub(crate) fn selected_prefix(t: &Theme, is_selected: bool) -> (&'static str, Style) {
    if is_selected {
        ("▸ ", t.form_active)
    } else {
        ("  ", Style::new().fg(t.foreground))
    }
}

macro_rules! overlay_impl {
    ($ty:ty) => {
        impl $crate::components::Overlay for $ty {
            fn is_open(&self) -> bool {
                self.visible
            }
            fn is_modal(&self) -> bool {
                false
            }
            fn close(&mut self) {
                // delegates to inherent close(), not trait (would recurse)
                Self::close(self);
            }
        }
    };
}
pub(crate) use overlay_impl;

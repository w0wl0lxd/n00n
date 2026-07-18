use std::sync::atomic::{AtomicBool, Ordering};

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::widgets::{Scrollbar, ScrollbarOrientation, ScrollbarState};

pub const SCROLLBAR_THUMB: &str = "\u{2590}";

static ENABLED: AtomicBool = AtomicBool::new(true);

pub fn set_enabled(enabled: bool) {
    ENABLED.store(enabled, Ordering::Relaxed);
}

pub fn render_vertical_scrollbar(
    frame: &mut Frame,
    area: Rect,
    content_len: u16,
    position: u16,
    style: Option<Style>,
) {
    if !ENABLED.load(Ordering::Relaxed) {
        return;
    }
    let max_scroll = content_len.saturating_sub(area.height);
    let mut state = ScrollbarState::default()
        .content_length(max_scroll as usize + 1)
        .position(position as usize);

    let mut scrollbar = Scrollbar::new(ScrollbarOrientation::VerticalRight)
        .thumb_symbol(SCROLLBAR_THUMB)
        .track_symbol(None)
        .begin_symbol(None)
        .end_symbol(None);
    if let Some(style) = style {
        scrollbar = scrollbar.style(style);
    }

    frame.render_stateful_widget(scrollbar, area, &mut state);
}

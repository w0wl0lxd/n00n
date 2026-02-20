use std::time::{Duration, Instant};

use super::Status;

use crate::animation::spinner_frame;

use maki_agent::AgentMode;
use maki_providers::{ModelPricing, TokenUsage};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

const STATUS_IDLE_STYLE: Style = Style::new().fg(Color::DarkGray);
const STATUS_STREAMING_STYLE: Style = Style::new().fg(Color::Yellow);
const STATUS_ERROR_STYLE: Style = Style::new().fg(Color::Red);
const MODE_BUILD_STYLE: Style = Style::new().fg(Color::Green).add_modifier(Modifier::BOLD);
const MODE_PLAN_STYLE: Style = Style::new().fg(Color::Blue).add_modifier(Modifier::BOLD);
const CANCEL_HINT_STYLE: Style = Style::new().fg(Color::Yellow);

const CANCEL_WINDOW: Duration = Duration::from_secs(3);

pub enum CancelResult {
    FirstPress,
    Confirmed,
}

pub struct StatusBar {
    cancel_hint_since: Option<Instant>,
    started_at: Instant,
}

impl StatusBar {
    pub fn new() -> Self {
        Self {
            cancel_hint_since: None,
            started_at: Instant::now(),
        }
    }

    pub fn handle_cancel_press(&mut self) -> CancelResult {
        if let Some(t) = self.cancel_hint_since
            && t.elapsed() < CANCEL_WINDOW
        {
            self.cancel_hint_since = None;
            return CancelResult::Confirmed;
        }
        self.cancel_hint_since = Some(Instant::now());
        CancelResult::FirstPress
    }

    pub fn clear_cancel_hint(&mut self) {
        self.cancel_hint_since = None;
    }

    pub fn clear_expired_hint(&mut self) {
        if self
            .cancel_hint_since
            .is_some_and(|t| t.elapsed() >= CANCEL_WINDOW)
        {
            self.cancel_hint_since = None;
        }
    }

    pub fn view(
        &self,
        frame: &mut Frame,
        area: Rect,
        status: &Status,
        mode: &AgentMode,
        token_usage: &TokenUsage,
        pricing: &ModelPricing,
    ) {
        let (mode_label, mode_style) = match mode {
            AgentMode::Build => ("[BUILD]", MODE_BUILD_STYLE),
            AgentMode::Plan(_) => ("[PLAN]", MODE_PLAN_STYLE),
        };

        let stats = format!(
            " tokens: {}in / {}out (${:.3})",
            token_usage.input,
            token_usage.output,
            token_usage.cost(pricing)
        );

        let mut spans = Vec::new();

        if *status == Status::Streaming {
            let ch = spinner_frame(self.started_at.elapsed().as_millis());
            spans.push(Span::styled(format!(" {ch}"), STATUS_STREAMING_STYLE));
        }

        spans.push(Span::styled(format!(" {mode_label}"), mode_style));

        match status {
            Status::Error(e) => {
                spans.push(Span::styled(format!(" error: {e}"), STATUS_ERROR_STYLE));
            }
            _ => {
                spans.push(Span::styled(stats, STATUS_IDLE_STYLE));
            }
        }

        if self.cancel_hint_since.is_some() {
            spans.push(Span::styled(" press Esc again to stop", CANCEL_HINT_STYLE));
        }

        frame.render_widget(Paragraph::new(Line::from(spans)), area);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn esc_after_expired_window_resets_hint() {
        let mut bar = StatusBar::new();
        bar.cancel_hint_since = Some(Instant::now() - CANCEL_WINDOW - Duration::from_millis(1));

        let result = bar.handle_cancel_press();
        assert!(matches!(result, CancelResult::FirstPress));
        assert!(bar.cancel_hint_since.is_some());
    }

    #[test]
    fn double_press_within_window_confirms() {
        let mut bar = StatusBar::new();
        let result = bar.handle_cancel_press();
        assert!(matches!(result, CancelResult::FirstPress));

        let result = bar.handle_cancel_press();
        assert!(matches!(result, CancelResult::Confirmed));
        assert!(bar.cancel_hint_since.is_none());
    }

    #[test]
    fn clear_expired_hint_removes_stale() {
        let mut bar = StatusBar::new();
        bar.cancel_hint_since = Some(Instant::now() - CANCEL_WINDOW - Duration::from_millis(1));
        bar.clear_expired_hint();
        assert!(bar.cancel_hint_since.is_none());
    }

    #[test]
    fn clear_expired_hint_keeps_fresh() {
        let mut bar = StatusBar::new();
        bar.cancel_hint_since = Some(Instant::now());
        bar.clear_expired_hint();
        assert!(bar.cancel_hint_since.is_some());
    }
}

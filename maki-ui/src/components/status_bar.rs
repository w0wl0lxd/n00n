use std::time::{Duration, Instant};

use super::Status;

use crate::animation::spinner_frame;
use crate::theme;

use maki_agent::AgentMode;
use maki_providers::{ModelPricing, TokenUsage};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

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
            AgentMode::Build => ("[BUILD]", theme::MODE_BUILD),
            AgentMode::Plan(_) => ("[PLAN]", theme::MODE_PLAN),
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
            spans.push(Span::styled(format!(" {ch}"), theme::STATUS_STREAMING));
        }

        spans.push(Span::styled(format!(" {mode_label}"), mode_style));

        match status {
            Status::Error(e) => {
                spans.push(Span::styled(format!(" error: {e}"), theme::ERROR));
            }
            _ => {
                spans.push(Span::styled(stats, theme::STATUS_IDLE));
            }
        }

        if self.cancel_hint_since.is_some() {
            spans.push(Span::styled(" press Esc again to stop", theme::CANCEL_HINT));
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

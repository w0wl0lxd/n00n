use std::time::{Duration, Instant};

use super::Status;

use crate::animation::spinner_frame;
use crate::theme;

use maki_agent::AgentMode;
use maki_providers::{ModelPricing, TokenUsage};
use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

const CANCEL_WINDOW: Duration = Duration::from_secs(3);

fn format_tokens(n: u32) -> String {
    match n {
        0..1_000 => n.to_string(),
        1_000..1_000_000 => format!("{:.1}k", n as f64 / 1_000.0),
        _ => format!("{:.1}m", n as f64 / 1_000_000.0),
    }
}

pub struct UsageStats<'a> {
    pub usage: &'a TokenUsage,
    pub context_size: u32,
    pub pricing: &'a ModelPricing,
    pub context_window: u32,
}

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
        stats: &UsageStats,
    ) {
        let (mode_label, mode_style) = match mode {
            AgentMode::Build => ("[BUILD]", theme::MODE_BUILD),
            AgentMode::Plan(_) => ("[PLAN]", theme::MODE_PLAN),
        };

        let mut left_spans = Vec::new();

        if *status == Status::Streaming {
            let ch = spinner_frame(self.started_at.elapsed().as_millis());
            left_spans.push(Span::styled(format!(" {ch}"), theme::STATUS_STREAMING));
        }

        left_spans.push(Span::styled(format!(" {mode_label}"), mode_style));

        let mut right_spans = Vec::new();

        match status {
            Status::Error(e) => {
                left_spans.push(Span::styled(format!(" error: {e}"), theme::ERROR));
            }
            _ => {
                let pct = if stats.context_window > 0 {
                    (stats.context_size as f64 / stats.context_window as f64 * 100.0) as u32
                } else {
                    0
                };
                let text = format!(
                    "{} ({}%) ${:.3} ",
                    format_tokens(stats.context_size),
                    pct,
                    stats.usage.cost(stats.pricing),
                );
                right_spans.push(Span::styled(text, theme::STATUS_IDLE));
            }
        }

        if self.cancel_hint_since.is_some() {
            left_spans.push(Span::styled(" press Esc again to stop", theme::CANCEL_HINT));
        }

        let [left_area, right_area] = Layout::horizontal([
            Constraint::Min(0),
            Constraint::Length(right_spans.iter().map(|s| s.width() as u16).sum()),
        ])
        .areas(area);

        frame.render_widget(Paragraph::new(Line::from(left_spans)), left_area);
        frame.render_widget(
            Paragraph::new(Line::from(right_spans)).alignment(Alignment::Right),
            right_area,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use test_case::test_case;

    #[test_case(999, "999")]
    #[test_case(1_000, "1.0k")]
    #[test_case(12_345, "12.3k")]
    #[test_case(999_999, "1000.0k")]
    #[test_case(1_000_000, "1.0m")]
    #[test_case(1_500_000, "1.5m")]
    fn format_tokens_display(input: u32, expected: &str) {
        assert_eq!(format_tokens(input), expected);
    }

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

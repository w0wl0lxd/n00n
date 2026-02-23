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
const ERROR_DISPLAY: Duration = Duration::from_secs(5);

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

pub struct StatusBarContext<'a> {
    pub status: &'a Status,
    pub mode: &'a AgentMode,
    pub model_id: &'a str,
    pub stats: UsageStats<'a>,
    pub auto_scroll: bool,
}

pub enum CancelResult {
    FirstPress,
    Confirmed,
}

pub struct StatusBar {
    cancel_hint_since: Option<Instant>,
    error_since: Option<Instant>,
    started_at: Instant,
}

impl StatusBar {
    pub fn new() -> Self {
        Self {
            cancel_hint_since: None,
            error_since: None,
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

    pub fn mark_error(&mut self) {
        self.error_since = Some(Instant::now());
    }

    pub fn is_error_expired(&self) -> bool {
        self.error_since
            .is_some_and(|t| t.elapsed() >= ERROR_DISPLAY)
    }

    pub fn view(&self, frame: &mut Frame, area: Rect, ctx: &StatusBarContext) {
        let (mode_label, mode_style) = match ctx.mode {
            AgentMode::Build => ("[BUILD]", theme::MODE_BUILD),
            AgentMode::Plan(_) => ("[PLAN]", theme::MODE_PLAN),
        };

        let mut left_spans = Vec::new();

        if *ctx.status == Status::Streaming {
            let ch = spinner_frame(self.started_at.elapsed().as_millis());
            left_spans.push(Span::styled(format!(" {ch}"), theme::STATUS_STREAMING));
        }

        left_spans.push(Span::styled(format!(" {mode_label}"), mode_style));

        if !ctx.auto_scroll {
            left_spans.push(Span::styled(" auto-scroll paused", theme::COMMENT));
        }

        let mut right_spans = Vec::new();

        match ctx.status {
            Status::Error(e) => {
                left_spans.push(Span::styled(format!(" error: {e}"), theme::ERROR));
            }
            _ => {
                let pct = if ctx.stats.context_window > 0 {
                    (ctx.stats.context_size as f64 / ctx.stats.context_window as f64 * 100.0) as u32
                } else {
                    0
                };

                right_spans.push(Span::styled(ctx.model_id.to_string(), theme::STATUS_IDLE));

                let rest_text = format!(
                    "  {} ({}%) ${:.3} ",
                    format_tokens(ctx.stats.context_size),
                    pct,
                    ctx.stats.usage.cost(ctx.stats.pricing),
                );
                right_spans.push(Span::styled(rest_text, theme::STATUS_CONTEXT));
            }
        }

        if self.cancel_hint_since.is_some() {
            left_spans.push(Span::styled(
                " Press esc again to stop...",
                theme::CANCEL_HINT,
            ));
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

    #[test]
    fn error_expiry_lifecycle() {
        let mut bar = StatusBar::new();
        assert!(!bar.is_error_expired(), "no error marked yet");

        bar.mark_error();
        assert!(!bar.is_error_expired(), "fresh error not expired");

        bar.error_since = Some(Instant::now() - ERROR_DISPLAY - Duration::from_millis(1));
        assert!(bar.is_error_expired(), "stale error is expired");

        bar.mark_error();
        assert!(!bar.is_error_expired(), "re-marking resets the timer");
    }
}

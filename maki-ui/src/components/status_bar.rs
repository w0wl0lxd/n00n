use std::env;
use std::path::Path;
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
    pub global_usage: &'a TokenUsage,
    pub context_size: u32,
    pub pricing: &'a ModelPricing,
    pub context_window: u32,
    pub show_global: bool,
}

pub struct StatusBarContext<'a> {
    pub status: &'a Status,
    pub mode: &'a AgentMode,
    pub model_id: &'a str,
    pub stats: UsageStats<'a>,
    pub auto_scroll: bool,
    pub chat_name: Option<&'a str>,
}

pub enum CancelResult {
    FirstPress,
    Confirmed,
}

pub struct StatusBar {
    cancel_hint_since: Option<Instant>,
    error_since: Option<Instant>,
    started_at: Instant,
    cwd_branch: String,
}

impl StatusBar {
    pub fn new() -> Self {
        Self {
            cancel_hint_since: None,
            error_since: None,
            started_at: Instant::now(),
            cwd_branch: cwd_branch_label(),
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

        if let Some(name) = ctx.chat_name {
            left_spans.push(Span::styled(format!(" [{name}]"), theme::COMMENT));
        }

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

                right_spans.push(Span::styled(self.cwd_branch.clone(), theme::COMMENT_STYLE));
                right_spans.push(Span::raw("  "));
                right_spans.push(Span::styled(ctx.model_id.to_string(), theme::STATUS_IDLE));

                let rest_text = format!(
                    "  {} ({}%) ${:.3} ",
                    format_tokens(ctx.stats.context_size),
                    pct,
                    ctx.stats.usage.cost(ctx.stats.pricing),
                );
                right_spans.push(Span::styled(rest_text, theme::STATUS_CONTEXT));

                if ctx.stats.show_global {
                    let global_text = format!(
                        " \u{03a3}${:.3} ",
                        ctx.stats.global_usage.cost(ctx.stats.pricing),
                    );
                    right_spans.push(Span::styled(global_text, theme::STATUS_GLOBAL_COST));
                }
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

fn collapse_home(path: &str) -> String {
    let Some(home) = env::var_os("HOME") else {
        return path.to_string();
    };
    collapse_home_with(path, &home.to_string_lossy())
}

fn collapse_home_with(path: &str, home: &str) -> String {
    path.strip_prefix(home)
        .map(|rest| format!("~{rest}"))
        .unwrap_or_else(|| path.to_string())
}

fn cwd_branch_label() -> String {
    let cwd = env::current_dir()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|_| ".".into());
    let label = collapse_home(&cwd);
    match detect_branch(&cwd) {
        Some(branch) => format!("{label}:{branch}"),
        None => label,
    }
}

fn detect_branch(cwd: &str) -> Option<String> {
    let head = std::fs::read_to_string(Path::new(cwd).join(".git/HEAD")).ok()?;
    let head = head.trim();
    head.strip_prefix("ref: refs/heads/")
        .map(str::to_string)
        .or_else(|| Some(head.get(..7)?.to_string()))
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;
    use tempfile::TempDir;
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

    #[test_case("/home/user/projects/app", "/home/user", "~/projects/app" ; "inside_home")]
    #[test_case("/tmp/other", "/home/user", "/tmp/other"                  ; "outside_home")]
    #[test_case("/home/user", "/home/user", "~"                           ; "exact_home")]
    fn collapse_home_cases(path: &str, home: &str, expected: &str) {
        assert_eq!(collapse_home_with(path, home), expected);
    }

    fn tmp_with_head(content: Option<&str>) -> (TempDir, String) {
        let dir = TempDir::new().unwrap();
        if let Some(head) = content {
            let git = dir.path().join(".git");
            fs::create_dir(&git).unwrap();
            fs::write(git.join("HEAD"), head).unwrap();
        }
        let path = dir.path().to_string_lossy().into_owned();
        (dir, path)
    }

    #[test_case(Some("ref: refs/heads/feature/foo\n"), Some("feature/foo") ; "regular_ref")]
    #[test_case(Some("abc1234deadbeef\n"),            Some("abc1234")      ; "detached_head")]
    #[test_case(None,                                 None                 ; "no_git_dir")]
    fn detect_branch_cases(head: Option<&str>, expected: Option<&str>) {
        let (_dir, path) = tmp_with_head(head);
        assert_eq!(detect_branch(&path), expected.map(String::from));
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

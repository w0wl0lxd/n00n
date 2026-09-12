use std::borrow::Cow;
use std::env;
use std::path::Path;
use std::time::{Duration, Instant};

use super::{RetryInfo, Status};

use crate::animation::{animation_elapsed_ms, spinner_frame};
use crate::cast;
use crate::theme;

use n00n_agent::FusionPhase;
use n00n_providers::{CacheHealth, ModelPricing, TokenUsage};
use quanta::Instant as CacheInstant;
use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

const FAST_LABEL: &str = " [fast]";
const WORKFLOW_LABEL: &str = " [workflow]";
const CACHE_ICON: &str = "⧉";
const SECONDS_PER_MINUTE: u64 = 60;
const SECONDS_PER_HOUR: u64 = 60 * SECONDS_PER_MINUTE;
const SECONDS_PER_DAY: u64 = 24 * SECONDS_PER_HOUR;
const CACHE_GREEN_NUMERATOR: u64 = 2;
const CACHE_YELLOW_NUMERATOR: u64 = 5;

pub(crate) fn format_tokens(n: u32) -> String {
    match n {
        0..1_000 => n.to_string(),
        1_000..1_000_000 => format!("{:.1}k", f64::from(n) / 1_000.0),
        _ => format!("{:.1}m", f64::from(n) / 1_000_000.0),
    }
}

pub struct UsageStats<'a> {
    pub usage: &'a TokenUsage,
    pub context_size: u32,
    pub pricing: &'a ModelPricing,
    pub context_window: u32,
    pub global_costs: Option<(f64, f64)>,
}

pub struct StatusBarContext<'a> {
    pub status: &'a Status,
    pub mode_label: Cow<'static, str>,
    pub mode_style: Style,
    pub model_id: &'a str,
    pub stats: UsageStats<'a>,
    pub auto_scroll: bool,
    pub chat_name: Option<&'a str>,
    pub retry_info: Option<&'a RetryInfo>,
    pub thinking_label: Option<Cow<'static, str>>,
    pub fusion_phase: Option<FusionPhase>,
    pub fast: bool,
    pub workflow: bool,
    pub restoring: bool,
    pub cache_health: Option<&'a CacheHealth>,
    pub cache_valid_until: Option<CacheInstant>,
}

pub struct StatusBar {
    flash: Option<(String, Instant)>,
    cwd_branch: String,
    pub flash_duration: Duration,
    branch_update_rx: Option<flume::Receiver<()>>,
}

impl StatusBar {
    pub fn new(flash_duration: Duration) -> Self {
        Self {
            flash: None,
            cwd_branch: cwd_branch_label(),
            flash_duration,
            branch_update_rx: spawn_branch_watcher(),
        }
    }

    pub fn flash(&mut self, msg: String) {
        self.flash = Some((msg, Instant::now()));
    }

    #[cfg(test)]
    pub fn flash_text(&self) -> Option<&str> {
        self.flash.as_ref().map(|(s, _)| s.as_str())
    }

    pub fn refresh_cwd(&mut self) {
        self.cwd_branch = cwd_branch_label();
    }

    /// Override the cwd/branch label (tests only) so ambient path length cannot
    /// clip other status-bar spans in a fixed-width `TestBackend`.
    #[cfg(test)]
    fn set_cwd_branch_for_test(&mut self, label: impl Into<String>) {
        self.cwd_branch = label.into();
    }

    pub fn poll_branch_update(&mut self) {
        let Some(rx) = &self.branch_update_rx else {
            return;
        };
        if rx.try_iter().next().is_some() {
            self.cwd_branch = cwd_branch_label();
        }
    }

    pub fn clear_flash(&mut self) {
        self.flash = None;
    }

    pub fn clear_expired_hint(&mut self) {
        if self
            .flash
            .as_ref()
            .is_some_and(|(_, t)| t.elapsed() >= self.flash_duration)
        {
            self.flash = None;
        }
    }

    pub fn view(&self, frame: &mut Frame, area: Rect, ctx: &StatusBarContext) {
        let mut left_spans = Vec::new();

        if ctx.restoring || matches!(ctx.status, Status::Streaming) {
            let ch = spinner_frame(animation_elapsed_ms());
            left_spans.push(Span::styled(
                format!(" {ch}"),
                theme::current().status_notice,
            ));
        }

        left_spans.push(Span::styled(format!(" {}", ctx.mode_label), ctx.mode_style));

        if let Some(label) = ctx.fusion_phase.and_then(fusion_phase_label) {
            left_spans.push(Span::styled(
                format!(" · Fusion {label}"),
                theme::current().status_notice,
            ));
        }

        if let Some(name) = ctx.chat_name {
            left_spans.push(Span::styled(
                format!(" [{name}]"),
                theme::current().status_dim,
            ));
        }

        if !ctx.auto_scroll {
            left_spans.push(Span::styled(
                " auto-scroll paused",
                theme::current().status_dim,
            ));
        }

        if let Some(retry) = ctx.retry_info {
            let secs = retry
                .deadline
                .saturating_duration_since(Instant::now())
                .as_secs();
            left_spans.push(Span::styled(
                format!(" {}", retry.message),
                theme::current().status_retry_error,
            ));
            left_spans.push(Span::styled(
                format!(" · retrying in {secs}s (#{})", retry.attempt),
                theme::current().status_retry_info,
            ));
        }

        let mut right_spans = Vec::new();
        let mut usage_parts = Vec::new();

        if let Status::Error { message: e, .. } = ctx.status {
            left_spans.push(Span::styled(format!(" {e}"), theme::current().error));
        } else {
            let pct = if ctx.stats.context_window > 0 {
                cast::f64_to_u32(
                    f64::from(ctx.stats.context_size) / f64::from(ctx.stats.context_window) * 100.0,
                )
            } else {
                0
            };

            right_spans.push(Span::styled(
                self.cwd_branch.clone(),
                theme::current().status_dim,
            ));
            right_spans.push(Span::raw("  "));
            right_spans.push(Span::styled(
                ctx.model_id.to_string(),
                theme::current().status_dim,
            ));

            if let Some(ref label) = ctx.thinking_label {
                right_spans.push(Span::styled(
                    format!(" [{label}]"),
                    theme::current().status_dim,
                ));
            }

            if ctx.fast {
                right_spans.push(Span::styled(FAST_LABEL, theme::current().status_dim));
            }
            if ctx.workflow {
                right_spans.push(Span::styled(WORKFLOW_LABEL, theme::current().status_dim));
            }

            if let (Some(health), Some(valid_until)) = (ctx.cache_health, ctx.cache_valid_until) {
                let remaining = valid_until.saturating_duration_since(CacheInstant::now());
                if !remaining.is_zero() {
                    let label = format_cache_remaining(remaining);
                    let color = cache_color(remaining, health.ttl_seconds);
                    right_spans.push(Span::styled(
                        format!(" {CACHE_ICON} {label}"),
                        Style::new().fg(color),
                    ));
                }
            }

            usage_parts.push(format!(
                "{}/{} ({}%)",
                format_tokens(ctx.stats.context_size),
                format_tokens(ctx.stats.context_window),
                pct,
            ));
            let prompt_tokens = ctx.stats.usage.total_input();
            if prompt_tokens > 0 {
                let cache_hit =
                    f64::from(ctx.stats.usage.cache_read) * 100.0 / f64::from(prompt_tokens);
                usage_parts.push(format!("cache {cache_hit:.1}%"));
            }
            if !ctx.stats.pricing.is_zero() {
                let cost = ctx.stats.usage.cost(ctx.stats.pricing, ctx.fast);
                let savings = ctx.stats.usage.savings_cost(ctx.stats.pricing, ctx.fast);
                usage_parts.push(format!("cost ${cost:.3}"));
                if savings > 0.0 {
                    usage_parts.push(format!("saved ${savings:.3}"));
                }
                if let Some((global_cost, global_savings)) = ctx.stats.global_costs {
                    usage_parts.push(format!("Σ cost ${global_cost:.3}"));
                    if global_savings > 0.0 {
                        usage_parts.push(format!("Σ saved ${global_savings:.3}"));
                    }
                }
            }
        }

        if let Some((ref msg, _)) = self.flash {
            left_spans.push(Span::styled(
                format!(" {msg}"),
                theme::current().status_notice,
            ));
        }

        let spans_width = |spans: &[Span<'_>]| {
            spans.iter().fold(0u16, |width, span| {
                width.saturating_add(cast::usize_to_u16(span.width()))
            })
        };
        let left_width = spans_width(&left_spans).min(area.width);
        let max_right_width = area.width.saturating_sub(left_width);
        let mut right_width = spans_width(&right_spans);
        for (index, part) in usage_parts.into_iter().enumerate() {
            let separator = if index == 0 { "  " } else { " · " };
            let span = Span::styled(
                format!("{separator}{part}"),
                Style::new().fg(theme::current().foreground),
            );
            let candidate_width = right_width.saturating_add(cast::usize_to_u16(span.width()));
            if candidate_width > max_right_width {
                break;
            }
            right_width = candidate_width;
            right_spans.push(span);
        }
        right_width = right_width.min(max_right_width);

        let [left_area, right_area] =
            Layout::horizontal([Constraint::Min(0), Constraint::Length(right_width)]).areas(area);

        frame.render_widget(Paragraph::new(Line::from(left_spans)), left_area);
        frame.render_widget(
            Paragraph::new(Line::from(right_spans)).alignment(Alignment::Right),
            right_area,
        );
    }
}

fn fusion_phase_label(phase: FusionPhase) -> Option<&'static str> {
    match phase {
        FusionPhase::Planning => Some("planning"),
        FusionPhase::Executing => Some("executing"),
        FusionPhase::Reviewing => Some("reviewing"),
        FusionPhase::LeadFallback => Some("lead fallback"),
        FusionPhase::Idle
        | FusionPhase::Complete
        | FusionPhase::Cancelled
        | FusionPhase::Failed => None,
    }
}

fn collapse_home(path: &str) -> String {
    let Some(home) = n00n_storage::paths::home() else {
        return path.to_string();
    };
    collapse_home_with(path, &home.to_string_lossy())
}

fn collapse_home_with(path: &str, home: &str) -> String {
    path.strip_prefix(home)
        .map_or_else(|| path.to_string(), |rest| format!("~{rest}"))
}

fn cwd_branch_label() -> String {
    let cwd = env::current_dir().map_or_else(|_| ".".into(), |p| p.to_string_lossy().into_owned());
    let label = collapse_home(&cwd);
    match detect_branch(&cwd) {
        Some(branch) => format!("{label}:{branch}"),
        None => label,
    }
}

fn detect_branch(cwd: &str) -> Option<String> {
    let head = std::fs::read_to_string(find_git_dir(Path::new(cwd))?.join("HEAD")).ok()?;
    let head = head.trim();
    head.strip_prefix("ref: refs/heads/")
        .map(str::to_string)
        .or_else(|| Some(head.get(..7)?.to_string()))
}

fn format_cache_remaining(remaining: Duration) -> String {
    let secs = remaining.as_secs();
    if secs >= SECONDS_PER_DAY {
        return format!("{}d", secs / SECONDS_PER_DAY);
    }
    if secs >= SECONDS_PER_HOUR {
        return format!("{}h", secs / SECONDS_PER_HOUR);
    }
    if secs >= SECONDS_PER_MINUTE {
        return format!("{}m", secs / SECONDS_PER_MINUTE);
    }
    "<1m".to_string()
}

fn cache_color(remaining: Duration, ttl_seconds: u64) -> Color {
    if ttl_seconds == 0 {
        return Color::Green;
    }
    let secs = remaining.as_secs();
    if secs.saturating_mul(CACHE_GREEN_NUMERATOR) > ttl_seconds {
        return Color::Green;
    }
    if secs.saturating_mul(CACHE_YELLOW_NUMERATOR) > ttl_seconds {
        return Color::Yellow;
    }
    Color::Red
}

fn find_git_dir(cwd: &Path) -> Option<std::path::PathBuf> {
    let mut dir = cwd;
    loop {
        let git = dir.join(".git");
        if git.is_dir() {
            return Some(git);
        }
        dir = dir.parent()?;
    }
}

fn spawn_branch_watcher() -> Option<flume::Receiver<()>> {
    use notify::{RecursiveMode, Watcher};

    let cwd = env::current_dir().ok()?;
    let git_dir = find_git_dir(&cwd)?;
    let (tx, rx) = flume::bounded(1);

    std::thread::spawn(move || {
        let Ok(mut watcher) = notify::recommended_watcher(move |res: Result<notify::Event, _>| {
            if res.is_ok_and(|e| e.paths.iter().any(|p| p.ends_with("HEAD"))) {
                let _ = tx.try_send(());
            }
        }) else {
            return;
        };
        if watcher.watch(&git_dir, RecursiveMode::NonRecursive).is_ok() {
            std::thread::park();
        }
    });

    Some(rx)
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;
    use tempfile::TempDir;
    use test_case::test_case;

    #[test_case(FusionPhase::Planning, Some("planning") ; "planning")]
    #[test_case(FusionPhase::Executing, Some("executing") ; "executing")]
    #[test_case(FusionPhase::Reviewing, Some("reviewing") ; "reviewing")]
    #[test_case(FusionPhase::LeadFallback, Some("lead fallback") ; "fallback")]
    #[test_case(FusionPhase::Idle, None ; "idle")]
    #[test_case(FusionPhase::Complete, None ; "complete")]
    #[test_case(FusionPhase::Cancelled, None ; "cancelled")]
    #[test_case(FusionPhase::Failed, None ; "failed")]
    fn fusion_phase_labels(phase: FusionPhase, expected: Option<&str>) {
        assert_eq!(fusion_phase_label(phase), expected);
    }

    #[test_case(999, "999")]
    #[test_case(1_000, "1.0k")]
    #[test_case(12_345, "12.3k")]
    #[test_case(999_999, "1000.0k")]
    #[test_case(1_000_000, "1.0m")]
    #[test_case(1_500_000, "1.5m")]
    fn format_tokens_display(input: u32, expected: &str) {
        assert_eq!(format_tokens(input), expected);
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
    fn detect_branch_from_subdirectory() {
        let (_dir, path) = tmp_with_head(Some("ref: refs/heads/main\n"));
        let sub = Path::new(&path).join("sub");
        fs::create_dir(&sub).unwrap();
        assert_eq!(
            detect_branch(&sub.to_string_lossy()),
            Some("main".to_string())
        );
    }

    #[test]
    fn streaming_status_does_not_render_thinking_in_status_bar() {
        use n00n_providers::{ModelPricing, TokenUsage};
        use ratatui::{Terminal, backend::TestBackend};

        let backend = TestBackend::new(100, 1);
        let mut terminal = Terminal::new(backend).unwrap();
        let bar = StatusBar::new(Duration::from_secs(1));
        let usage = TokenUsage::default();
        let pricing = ModelPricing::default();
        terminal
            .draw(|frame| {
                bar.view(
                    frame,
                    frame.area(),
                    &StatusBarContext {
                        status: &Status::Streaming,
                        mode_label: Cow::Borrowed("NORMAL"),
                        mode_style: Style::default(),
                        model_id: "test/model",
                        stats: UsageStats {
                            usage: &usage,
                            context_size: 0,
                            pricing: &pricing,
                            context_window: 1,
                            global_costs: None,
                        },
                        auto_scroll: true,
                        chat_name: None,
                        retry_info: None,
                        thinking_label: None,
                        fusion_phase: Some(FusionPhase::Executing),
                        fast: false,
                        workflow: false,
                        restoring: false,
                        cache_health: None,
                        cache_valid_until: None,
                    },
                );
            })
            .unwrap();
        let text: String = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(ratatui::buffer::Cell::symbol)
            .collect();

        assert!(!text.contains("thinking..."), "status bar: {text:?}");
        assert!(text.contains("NORMAL"), "status bar: {text:?}");
        assert!(text.contains("Fusion executing"), "status bar: {text:?}");
        assert!(
            text.chars()
                .any(|ch| ('\u{2800}'..='\u{28ff}').contains(&ch)),
            "status bar: {text:?}"
        );
    }

    #[test]
    fn status_bar_renders_cache_hit_rate() {
        use n00n_providers::{ModelPricing, TokenUsage};
        use ratatui::{Terminal, backend::TestBackend};

        let backend = TestBackend::new(140, 1);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut bar = StatusBar::new(Duration::from_secs(1));
        bar.set_cwd_branch_for_test("repo");
        let usage = TokenUsage {
            input: 70,
            cache_read: 30,
            ..TokenUsage::default()
        };
        terminal
            .draw(|frame| {
                bar.view(
                    frame,
                    frame.area(),
                    &StatusBarContext {
                        status: &Status::Idle,
                        mode_label: Cow::Borrowed("NORMAL"),
                        mode_style: Style::default(),
                        model_id: "openai/gpt-5.6",
                        stats: UsageStats {
                            usage: &usage,
                            context_size: 100,
                            pricing: &ModelPricing::default(),
                            context_window: 1_000,
                            global_costs: None,
                        },
                        auto_scroll: true,
                        chat_name: None,
                        retry_info: None,
                        thinking_label: None,
                        fusion_phase: None,
                        fast: false,
                        workflow: false,
                        restoring: false,
                        cache_health: None,
                        cache_valid_until: None,
                    },
                );
            })
            .unwrap();
        let text: String = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(ratatui::buffer::Cell::symbol)
            .collect();

        assert!(text.contains("cache 30.0%"), "status bar: {text:?}");
    }

    #[test]
    fn narrow_status_bar_preserves_left_status() {
        use n00n_providers::{ModelPricing, TokenUsage};
        use ratatui::{Terminal, backend::TestBackend};

        let backend = TestBackend::new(60, 1);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut bar = StatusBar::new(Duration::from_secs(1));
        bar.flash("Connected".into());
        let usage = TokenUsage {
            input: 999_999,
            output: 999_999,
            cache_creation: 999_999,
            cache_read: 999_999,
        };
        let pricing = ModelPricing {
            input: 3.0,
            output: 15.0,
            cache_write: 3.75,
            cache_read: 0.3,
            fast: None,
        };
        terminal
            .draw(|frame| {
                bar.view(
                    frame,
                    frame.area(),
                    &StatusBarContext {
                        status: &Status::Streaming,
                        mode_label: Cow::Borrowed("NORMAL"),
                        mode_style: Style::default(),
                        model_id: "anthropic/claude-sonnet-with-a-long-name",
                        stats: UsageStats {
                            usage: &usage,
                            context_size: 999_999,
                            pricing: &pricing,
                            context_window: 1_000_000,
                            global_costs: Some((999.999, 999.999)),
                        },
                        auto_scroll: true,
                        chat_name: None,
                        retry_info: None,
                        thinking_label: None,
                        fusion_phase: None,
                        fast: false,
                        workflow: false,
                        restoring: false,
                        cache_health: None,
                        cache_valid_until: None,
                    },
                );
            })
            .unwrap();
        let text: String = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(ratatui::buffer::Cell::symbol)
            .collect();

        assert!(text.contains("NORMAL"), "status bar: {text:?}");
        assert!(text.contains("Connected"), "status bar: {text:?}");
        assert!(
            text.chars()
                .any(|ch| ('\u{2800}'..='\u{28ff}').contains(&ch)),
            "status bar: {text:?}"
        );
    }

    #[test]
    fn clear_expired_hint_removes_stale_flash() {
        let mut bar = StatusBar::new(Duration::ZERO);
        bar.flash("Copied".into());
        bar.clear_expired_hint();
        assert!(bar.flash.is_none());
    }

    #[test]
    fn clear_flash_removes_flash() {
        let mut bar = StatusBar::new(Duration::from_secs(999));
        bar.flash("Copied".into());
        bar.clear_flash();
        assert!(bar.flash.is_none());
    }

    #[test]
    fn cache_health_badge_renders_in_status_bar() {
        use n00n_providers::{CacheHealth, CacheKind};
        use ratatui::{Terminal, backend::TestBackend};

        let original = std::env::current_dir().unwrap();
        let tmpdir = TempDir::new().unwrap();
        std::env::set_current_dir(tmpdir.path()).unwrap();

        let backend = TestBackend::new(80, 1);
        let mut terminal = Terminal::new(backend).unwrap();
        // Pin a short cwd label: ambient / macOS TempDir paths can exceed the
        // 80-col budget and clip the cache badge from the right pane.
        let mut bar = StatusBar::new(Duration::from_secs(1));
        bar.set_cwd_branch_for_test("cwd");
        let usage = TokenUsage::default();
        let pricing = ModelPricing::default();
        let health = CacheHealth {
            kind: CacheKind::ResponseChain,
            valid_until: 0,
            ttl_seconds: 7200,
            hit: false,
        };
        let valid_until = CacheInstant::now() + Duration::from_hours(1);
        terminal
            .draw(|frame| {
                bar.view(
                    frame,
                    frame.area(),
                    &StatusBarContext {
                        status: &Status::Idle,
                        mode_label: Cow::Borrowed("NORMAL"),
                        mode_style: Style::default(),
                        model_id: "test/model",
                        stats: UsageStats {
                            usage: &usage,
                            context_size: 0,
                            pricing: &pricing,
                            context_window: 1,
                            global_costs: None,
                        },
                        auto_scroll: true,
                        chat_name: None,
                        retry_info: None,
                        thinking_label: None,
                        fusion_phase: None,
                        fast: false,
                        workflow: false,
                        restoring: false,
                        cache_health: Some(&health),
                        cache_valid_until: Some(valid_until),
                    },
                );
            })
            .unwrap();
        let text: String = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(ratatui::buffer::Cell::symbol)
            .collect();
        assert!(text.contains('⧉'), "status bar: {text:?}");
        let after_icon = text.split('⧉').nth(1).expect("icon not found");
        assert!(
            after_icon.chars().any(|c| c.is_ascii_digit()),
            "status bar: {text:?}"
        );

        std::env::set_current_dir(original).unwrap();
    }
}

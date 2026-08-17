use std::cmp::Reverse;
use std::collections::{HashMap, HashSet};

use crossterm::event::{KeyCode, KeyEvent};
use jiff::Timestamp;
use jiff::tz::TimeZone;
use n00n_providers::{Model, ModelPricing, ProviderUsage, TokenUsage};
use n00n_storage::sessions::StoredTokenUsage;
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use crate::components::ModalScroll;
use crate::components::keybindings::key;
use crate::components::modal::Modal;
use crate::components::scrollbar::render_vertical_scrollbar;
use crate::components::status_bar::format_tokens;
use crate::theme;

const TITLE: &str = " Token usage ";
const PREFIX: &str = "  ";
const MODEL_COL_MIN: usize = 16;
const NUM_COL: usize = 7;
const HIT_COL: usize = 6;
const COL_GAP: usize = 2;
const NO_USAGE_ENDPOINT: &str = "no usage endpoint for this provider";
const PRICE_HEADING: &str = "Prices per 1M tokens";
const FAST_PRICE_HEADING: &str = "Prices per 1M tokens (fast mode when available)";
const PRICE_DISCLAIMER: &str = "Estimates use current reported rates and selected mode; models without reported rates are excluded. Fast mode uses premium rates where the provider reports them. Coding-plan values are API-equivalent, not subscription charges.";
const MIN_DISPLAY_PRICE: f64 = 0.0001;
const HOUR: i64 = 3600;
const DAY: i64 = 24 * HOUR;
const WEEK: i64 = 7 * DAY;

/// Live provider quota fetch, shared from the event loop. `Loading` is shown
/// until the background fetch completes; the modal reads this each render.
pub enum UsageFetchState {
    Loading,
    Ready(ProviderUsage),
    Unsupported,
    Error(String),
}

pub struct UsageModalContext<'a> {
    pub total: &'a TokenUsage,
    pub by_model: &'a HashMap<String, StoredTokenUsage>,
    pub model: &'a Model,
    pub fast: bool,
    pub quota: Option<&'a UsageFetchState>,
}

pub struct UsageModal {
    open: bool,
    scroll: ModalScroll,
}

impl UsageModal {
    pub fn new() -> Self {
        Self {
            open: false,
            scroll: ModalScroll::new_top(),
        }
    }

    pub fn is_open(&self) -> bool {
        self.open
    }

    pub fn toggle(&mut self) {
        self.open = !self.open;
        self.scroll.reset();
    }

    pub fn close(&mut self) {
        self.open = false;
        self.scroll.reset();
    }

    pub fn scroll(&mut self, delta: i32) {
        self.scroll.scroll(delta);
    }

    pub fn handle_key(&mut self, key_event: KeyEvent) {
        if key_event.code == KeyCode::Esc || key::QUIT.matches(key_event) {
            self.close();
        }
        self.scroll.handle_key(key_event);
    }

    pub fn view(&mut self, frame: &mut Frame, area: Rect, ctx: &UsageModalContext) -> Rect {
        if !self.open {
            return Rect::default();
        }

        let theme = theme::current();
        let lines = build_lines(ctx, &theme);

        let total = u16::try_from(lines.len()).unwrap_or_else(|_| u16::MAX);
        let modal = Modal {
            title: TITLE,
            width_percent: 90,
            max_height_percent: 70,
        };
        let (popup, inner) = modal.render(frame, area, total);
        let viewport_h = inner.height;
        self.scroll.update_dimensions(total, viewport_h);
        let scroll = self.scroll.offset();

        frame.render_widget(Paragraph::new(lines).scroll((scroll, 0)), inner);

        if total > viewport_h {
            render_vertical_scrollbar(frame, inner, total, scroll, None);
        }

        let hint = Line::from(vec![
            Span::raw(" "),
            Span::styled("Ctrl+R", theme.keybind_key),
            Span::styled(" reload ", theme.tool_dim),
        ]);
        let hint_w = u16::try_from(hint.width()).unwrap_or_else(|_| u16::MAX);
        let hint_area = Rect {
            x: popup.x + popup.width.saturating_sub(hint_w + 1),
            y: popup.y + popup.height.saturating_sub(1),
            width: hint_w,
            height: 1,
        };
        frame.render_widget(Paragraph::new(hint), hint_area);

        popup
    }
}

fn model_for(id: &str, current: &Model) -> Option<Model> {
    if id == current.id || id == current.spec() {
        return Some(current.clone());
    }
    match Model::from_spec(id) {
        Ok(model) => Some(model),
        Err(direct_error) => {
            let fallback_spec = format!("{}/{id}", current.provider);
            match Model::from_spec(&fallback_spec) {
                Ok(model) => Some(model),
                Err(fallback_error) => {
                    tracing::warn!(
                        model_id = id,
                        %direct_error,
                        %fallback_error,
                        "unable to resolve usage model"
                    );
                    None
                }
            }
        }
    }
}

fn pricing_for(id: &str, current: &Model) -> Option<ModelPricing> {
    model_for(id, current).map(|model| model.pricing)
}

fn display_model_id(id: &str) -> String {
    id.split_once('/')
        .map_or_else(|| id.to_owned(), |(_, model_id)| model_id.to_owned())
}

fn colliding_model_labels<'a>(ids: impl IntoIterator<Item = &'a str>) -> HashSet<String> {
    let mut seen = HashSet::new();
    ids.into_iter()
        .map(display_model_id)
        .filter(|label| !seen.insert(label.clone()))
        .collect()
}

fn model_label(id: &str, collisions: &HashSet<String>) -> String {
    let display_id = display_model_id(id);
    if collisions.contains(&display_id) {
        id.to_owned()
    } else {
        display_id
    }
}

pub(crate) fn attributed_costs(
    by_model: &HashMap<String, StoredTokenUsage>,
    current: &Model,
    fast: bool,
) -> Option<(f64, f64)> {
    if by_model.is_empty() {
        return None;
    }
    let (cost, savings, any_priced) = by_model.iter().fold(
        (0.0, 0.0, false),
        |(cost, savings, any_priced), (id, usage)| {
            let Some(pricing) = pricing_for(id, current) else {
                return (cost, savings, any_priced);
            };
            if pricing.effective(fast).is_zero() {
                return (cost, savings, any_priced);
            }
            let usage = TokenUsage::from(*usage);
            (
                cost + usage.cost(&pricing, fast),
                savings + usage.savings_cost(&pricing, fast),
                true,
            )
        },
    );
    any_priced.then_some((cost, savings))
}

fn build_lines(ctx: &UsageModalContext, theme: &crate::theme::Theme) -> Vec<Line<'static>> {
    let mut lines: Vec<Line> = Vec::new();
    let fg = Style::new().fg(theme.foreground);

    lines.push(Line::from(Span::styled(
        format!("{PREFIX}Session total"),
        theme.keybind_section,
    )));

    let total_cost = if ctx.by_model.is_empty() {
        (!ctx.model.pricing.effective(ctx.fast).is_zero())
            .then(|| ctx.total.cost(&ctx.model.pricing, ctx.fast))
    } else {
        attributed_costs(ctx.by_model, ctx.model, ctx.fast).map(|(cost, _)| cost)
    };
    lines.push(Line::from(totals_row(ctx.total, total_cost, theme)));
    lines.push(Line::from(Span::styled(
        format!(
            "{PREFIX}Local token counts include cached context; they are not ChatGPT subscription quota."
        ),
        theme.status_dim,
    )));
    lines.extend(pricing_lines(ctx, theme));

    if let Some(state) = ctx.quota {
        lines.push(Line::default());
        lines.push(Line::from(Span::styled(
            format!("{PREFIX}{} quota", ctx.model.provider_display_name()),
            theme.keybind_section,
        )));
        lines.extend(quota_lines(state, theme));
    }

    if ctx.by_model.is_empty() {
        return lines;
    }

    let mut entries: Vec<(&String, &StoredTokenUsage)> = ctx.by_model.iter().collect();
    entries.sort_by_key(|(_, u)| Reverse(u.total()));

    let label_collisions = colliding_model_labels(entries.iter().map(|(id, _)| id.as_str()));
    let model_w = entries
        .iter()
        .map(|(id, _)| model_label(id, &label_collisions).chars().count())
        .max()
        .unwrap_or_else(|| 0)
        .max(MODEL_COL_MIN);

    lines.push(Line::default());
    lines.push(Line::from(Span::styled(
        format!("{PREFIX}Per model"),
        theme.keybind_section,
    )));
    lines.push(Line::from(header_row(model_w, theme)));

    for (id, usage) in entries {
        let pricing = pricing_for(id, ctx.model);
        let token_usage = TokenUsage::from(*usage);
        let (cost, savings) = pricing.as_ref().map_or((None, None), |p| {
            if p.effective(ctx.fast).is_zero() {
                (None, None)
            } else {
                (
                    Some(token_usage.cost(p, ctx.fast)),
                    Some(token_usage.savings_cost(p, ctx.fast)),
                )
            }
        });
        lines.push(Line::from(model_row(
            &model_label(id, &label_collisions),
            usage,
            cost,
            savings,
            model_w,
            fg,
            theme.status_dim,
        )));
    }

    lines
}
fn pricing_lines(ctx: &UsageModalContext, theme: &crate::theme::Theme) -> Vec<Line<'static>> {
    let mut rates = ctx
        .by_model
        .keys()
        .filter_map(|id| model_for(id, ctx.model))
        .map(|model| (model.spec(), model.pricing))
        .collect::<Vec<_>>();
    rates.push((ctx.model.spec(), ctx.model.pricing));
    rates.retain(|(_, pricing)| !pricing.effective(ctx.fast).is_zero());
    if rates.is_empty() {
        return Vec::new();
    }
    rates.sort_by(|(left, _), (right, _)| left.cmp(right));
    rates.dedup_by(|(left, _), (right, _)| left == right);

    let price_heading = if ctx.fast {
        FAST_PRICE_HEADING
    } else {
        PRICE_HEADING
    };
    let mut lines = vec![
        Line::default(),
        Line::from(Span::styled(
            format!("{PREFIX}{price_heading}"),
            theme.keybind_section,
        )),
    ];
    for (id, pricing) in rates {
        let pricing = pricing.effective(ctx.fast);
        lines.push(Line::from(vec![
            Span::raw(PREFIX),
            Span::styled(format!("{id}: "), Style::new().fg(theme.foreground)),
            Span::styled(
                format!(
                    "input {}  output {}  cache read {}  cache write {}",
                    format_currency(pricing.input),
                    format_currency(pricing.output),
                    format_currency(pricing.cache_read),
                    format_currency(pricing.cache_write),
                ),
                theme.status_dim,
            ),
        ]));
    }
    lines.push(Line::from(Span::styled(
        format!("{PREFIX}{PRICE_DISCLAIMER}"),
        theme.status_dim,
    )));
    lines
}

fn format_currency(price: f64) -> String {
    if price > 0.0 && price < MIN_DISPLAY_PRICE {
        return format!("<${MIN_DISPLAY_PRICE:.4}");
    }
    let formatted = format!("{price:.4}");
    let trimmed = formatted.trim_end_matches('0');
    let decimals = trimmed
        .split_once('.')
        .map_or(0, |(_, fraction)| fraction.len());
    if decimals >= 2 {
        format!("${trimmed}")
    } else {
        format!("${price:.2}")
    }
}

fn cache_hit_rate(usage: &TokenUsage) -> Option<f64> {
    let prompt_tokens = usage.total_input();
    (prompt_tokens > 0).then(|| f64::from(usage.cache_read) * 100.0 / f64::from(prompt_tokens))
}

fn format_cache_hit(usage: &TokenUsage) -> String {
    cache_hit_rate(usage).map_or_else(|| "—".into(), |rate| format!("{rate:.1}%"))
}

fn totals_row(
    total: &TokenUsage,
    cost: Option<f64>,
    theme: &crate::theme::Theme,
) -> Vec<Span<'static>> {
    let mut spans = vec![
        Span::raw(PREFIX),
        Span::styled(
            format!(
                "in {:<7} out {:<7} read {:<7} write {:<7} hit {:<6} total {:<7}",
                format_tokens(total.input),
                format_tokens(total.output),
                format_tokens(total.cache_read),
                format_tokens(total.cache_creation),
                format_cache_hit(total),
                format_tokens(total.context_tokens()),
            ),
            Style::new().fg(theme.foreground),
        ),
    ];
    if let Some(c) = cost {
        spans.push(Span::styled(format!("  ${c:.3}"), theme.accent));
    }
    spans
}

fn header_row(model_w: usize, theme: &crate::theme::Theme) -> Vec<Span<'static>> {
    let h = |label: &str| Span::styled(format!("{label:>NUM_COL$}"), theme.status_dim);
    let gap = || Span::raw(" ".repeat(COL_GAP));
    vec![
        Span::raw(PREFIX),
        Span::styled(
            format!("{:width$}", "model", width = model_w),
            theme.status_dim,
        ),
        gap(),
        h("fresh"),
        gap(),
        h("out"),
        gap(),
        h("read"),
        gap(),
        h("write"),
        gap(),
        Span::styled(format!("{:>HIT_COL$}", "hit"), theme.status_dim),
        gap(),
        h("total"),
        gap(),
        h("saved $"),
        gap(),
        Span::styled(format!("{:>6}", "est $"), theme.status_dim),
    ]
}

fn model_row(
    id: &str,
    usage: &StoredTokenUsage,
    cost: Option<f64>,
    savings: Option<f64>,
    model_w: usize,
    fg: Style,
    dim: Style,
) -> Vec<Span<'static>> {
    let num = |v: u32| Span::styled(format!("{:>NUM_COL$}", format_tokens(v)), fg);
    let token_usage = TokenUsage::from(*usage);
    let gap = || Span::raw(" ".repeat(COL_GAP));
    let money = |v: Option<f64>| match v {
        Some(v) if v > 0.0 => {
            let s = format!("${v:.3}");
            Span::styled(format!("{s:>NUM_COL$}"), fg)
        }
        _ => Span::styled(format!("{:>NUM_COL$}", "—"), dim),
    };
    vec![
        Span::raw(PREFIX),
        Span::styled(format!("{id:<model_w$}"), fg),
        gap(),
        num(usage.input),
        gap(),
        num(usage.output),
        gap(),
        num(usage.cache_read),
        gap(),
        num(usage.cache_creation),
        gap(),
        Span::styled(format!("{:>HIT_COL$}", format_cache_hit(&token_usage)), fg),
        gap(),
        num(usage.total()),
        gap(),
        money(savings),
        gap(),
        match cost {
            Some(c) => Span::styled(format!("{c:>6.3}"), fg),
            None => Span::styled(format!("{:>6}", "—"), dim),
        },
    ]
}

impl crate::components::Overlay for UsageModal {
    fn is_open(&self) -> bool {
        self.is_open()
    }

    fn close(&mut self) {
        self.close();
    }
}

fn quota_lines(state: &UsageFetchState, theme: &crate::theme::Theme) -> Vec<Line<'static>> {
    let fg = Style::new().fg(theme.foreground);
    let dim = theme.status_dim;
    match state {
        UsageFetchState::Loading => {
            vec![Line::from(Span::styled(format!("{PREFIX}loading…"), dim))]
        }
        UsageFetchState::Unsupported => vec![Line::from(Span::styled(
            format!("{PREFIX}{NO_USAGE_ENDPOINT}"),
            dim,
        ))],
        UsageFetchState::Error(msg) => {
            vec![Line::from(Span::styled(format!("{PREFIX}{msg}"), dim))]
        }
        UsageFetchState::Ready(usage) => {
            let mut out = Vec::with_capacity(usage.limits.len() + 1);
            if let Some(plan) = &usage.plan {
                out.push(Line::from(Span::styled(
                    format!("{PREFIX}plan: {plan}"),
                    fg,
                )));
            }
            let tz = TimeZone::system();
            let label_w = usage
                .limits
                .iter()
                .map(|l| l.label.chars().count())
                .max()
                .unwrap_or_else(|| 0);
            for limit in &usage.limits {
                let mut spans = vec![Span::styled(
                    format!("{PREFIX}{:<label_w$}", limit.label),
                    fg,
                )];
                if let Some(pct) = limit.percentage {
                    spans.push(Span::styled(format!("{pct:>3}%"), theme.accent));
                    spans.push(Span::styled(" used", dim));
                }
                if let Some(detail) = &limit.detail {
                    spans.push(Span::styled(format!("  {detail}"), dim));
                }
                if let Some(ms) = limit.reset_at {
                    spans.push(Span::styled(
                        format!("  Resets {}", format_reset(ms, &tz)),
                        dim,
                    ));
                }
                out.push(Line::from(spans));
            }
            out
        }
    }
}

fn format_reset(epoch_ms: u64, tz: &TimeZone) -> String {
    let secs = (epoch_ms / 1000).cast_signed();
    let Ok(ts) = Timestamp::from_second(secs) else {
        return epoch_ms.to_string();
    };
    let delta = secs - Timestamp::now().as_second();
    if (1..DAY).contains(&delta) {
        return relative(delta);
    }
    let zoned = ts.to_zoned(tz.clone());
    let fmt = if delta < WEEK {
        "%a %-I:%M %p"
    } else {
        "%b %-d, %-I:%M %p"
    };
    zoned.strftime(fmt).to_string()
}

fn relative(seconds: i64) -> String {
    let hrs = seconds / HOUR;
    let mins = (seconds % HOUR) / 60;
    if hrs > 0 {
        format!("in {hrs} hr {mins} min")
    } else {
        format!("in {mins} min")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::KeyModifiers;
    use n00n_providers::UsageLimit;
    use test_case::test_case;

    fn key(code: KeyCode, mods: KeyModifiers) -> KeyEvent {
        KeyEvent::new(code, mods)
    }

    #[test_case(key(KeyCode::Esc, KeyModifiers::NONE) ; "esc_closes")]
    #[test_case(key(KeyCode::Char('c'), KeyModifiers::CONTROL) ; "ctrl_c_closes")]
    fn handle_key_closes(k: KeyEvent) {
        let mut modal = UsageModal::new();
        modal.toggle();
        assert!(modal.is_open());
        modal.handle_key(k);
        assert!(!modal.is_open());
    }

    #[test]
    fn toggle_open_close() {
        let mut modal = UsageModal::new();
        assert!(!modal.is_open());
        modal.toggle();
        assert!(modal.is_open());
        modal.toggle();
        assert!(!modal.is_open());
    }

    #[test]
    fn handle_key_ignores_arbitrary() {
        let mut modal = UsageModal::new();
        modal.toggle();
        modal.handle_key(key(KeyCode::Char('a'), KeyModifiers::NONE));
        assert!(modal.is_open());
    }

    #[test]
    fn quota_ready_lines_include_labels_and_percentages() {
        let theme = crate::theme::current();
        let usage = ProviderUsage {
            plan: Some("lite".into()),
            limits: vec![
                UsageLimit {
                    label: "Current session".into(),
                    percentage: Some(16),
                    reset_at: Some(0),
                    detail: None,
                },
                UsageLimit {
                    label: "Usage credits".into(),
                    percentage: Some(4),
                    reset_at: None,
                    detail: Some("$2.33 spent".into()),
                },
            ],
        };
        let lines = quota_lines(&UsageFetchState::Ready(usage), &theme);
        assert_eq!(lines.len(), 3);
        assert!(
            lines[0]
                .spans
                .iter()
                .any(|s| s.content.contains("plan: lite"))
        );
        assert!(
            lines[1]
                .spans
                .iter()
                .any(|s| s.content.contains("Current session"))
        );
        assert!(lines[1].spans.iter().any(|s| s.content.contains("16%")));
        assert!(lines[1].spans.iter().any(|s| s.content.contains("used")));
        assert!(
            lines[2]
                .spans
                .iter()
                .any(|s| s.content.contains("Usage credits"))
        );
        assert!(lines[2].spans.iter().any(|s| s.content.contains("4%")));
        assert!(
            lines[2]
                .spans
                .iter()
                .any(|s| s.content.contains("$2.33 spent"))
        );
    }

    #[test]
    fn quota_non_terminal_states_render_single_line() {
        let theme = crate::theme::current();
        assert_eq!(quota_lines(&UsageFetchState::Loading, &theme).len(), 1);
        let unsupported = quota_lines(&UsageFetchState::Unsupported, &theme);
        assert_eq!(unsupported.len(), 1);
        assert!(
            unsupported[0]
                .spans
                .iter()
                .any(|s| s.content.contains(NO_USAGE_ENDPOINT))
        );
        let err = quota_lines(&UsageFetchState::Error("nope".into()), &theme);
        assert_eq!(err.len(), 1);
        assert!(err[0].spans.iter().any(|s| s.content.contains("nope")));
    }

    #[test]
    fn usage_columns_keep_fresh_and_cached_tokens_separate() {
        let theme = crate::theme::current();
        let header = header_row(10, &theme)
            .iter()
            .map(|span| span.content.as_ref())
            .collect::<String>();
        assert!(header.contains("fresh"));
        assert!(header.contains("read"));
        assert!(header.contains("write"));
        assert!(header.contains("hit"));
        assert!(header.contains("saved $"));

        let usage = StoredTokenUsage {
            input: 10,
            output: 20,
            cache_read: 30,
            cache_creation: 40,
        };
        let row = model_row(
            "gpt",
            &usage,
            None,
            Some(0.123),
            10,
            Style::new(),
            Style::new(),
        )
        .iter()
        .map(|span| span.content.as_ref())
        .collect::<String>();
        for value in ["10", "20", "30", "40", "37.5%", "$0.123"] {
            assert!(row.contains(value));
        }
    }

    #[test_case(TokenUsage { input: 70, cache_read: 30, ..TokenUsage::default() }, "30.0%" ; "cache_read_share")]
    #[test_case(TokenUsage { input: 70, cache_creation: 20, cache_read: 30, ..TokenUsage::default() }, "25.0%" ; "cache_write_in_denominator")]
    #[test_case(TokenUsage::default(), "—" ; "no_prompt_tokens")]
    fn cache_hit_percentage_uses_all_prompt_tokens(usage: TokenUsage, expected: &str) {
        assert_eq!(format_cache_hit(&usage), expected);
    }

    #[test]
    fn priced_models_show_effective_token_rates() {
        let theme = crate::theme::current();
        let model = Model::from_spec("codex/gpt-5.6-sol").unwrap();
        let total = TokenUsage {
            input: 1_000_000,
            output: 100_000,
            cache_read: 500_000,
            cache_creation: 200_000,
        };
        let by_model = HashMap::from([(
            model.id.clone(),
            StoredTokenUsage {
                input: total.input,
                output: total.output,
                cache_read: total.cache_read,
                cache_creation: total.cache_creation,
            },
        )]);
        let lines = build_lines(
            &UsageModalContext {
                total: &total,
                by_model: &by_model,
                model: &model,
                fast: false,
                quota: None,
            },
            &theme,
        );
        let text = lines
            .iter()
            .flat_map(|line| line.spans.iter())
            .map(|span| span.content.as_ref())
            .collect::<String>();

        assert!(text.contains(PRICE_HEADING));
        assert!(text.contains("input $5.00"));
        assert!(text.contains("output $30.00"));
        assert!(text.contains("cache read $0.50"));
        assert!(text.contains("cache write $6.25"));
        assert!(text.contains("API-equivalent"));
    }

    #[test]
    fn pricing_includes_current_model_after_switching_models() {
        let theme = crate::theme::current();
        let model = Model::from_spec("codex/gpt-5.6-sol").unwrap();
        let by_model = HashMap::from([(
            "anthropic/claude-haiku-4-5".into(),
            StoredTokenUsage::default(),
        )]);

        let text = pricing_lines(
            &UsageModalContext {
                total: &TokenUsage::default(),
                by_model: &by_model,
                model: &model,
                fast: false,
                quota: None,
            },
            &theme,
        )
        .iter()
        .flat_map(|line| line.spans.iter())
        .map(|span| span.content.as_ref())
        .collect::<String>();

        assert!(text.contains(&model.spec()));
        assert!(text.contains("anthropic/claude-haiku-4-5"));
    }

    #[test]
    fn model_rows_hide_provider_prefixes() {
        let label = model_label("anthropic/claude-haiku-4-5", &HashSet::new());
        let row = model_row(
            &label,
            &StoredTokenUsage::default(),
            None,
            None,
            18,
            Style::new(),
            Style::new(),
        )
        .iter()
        .map(|span| span.content.as_ref())
        .collect::<String>();

        assert!(row.contains("claude-haiku-4-5"));
        assert!(!row.contains("anthropic/"));
    }

    #[test]
    fn model_rows_keep_provider_prefixes_for_colliding_ids() {
        let ids = ["copilot/gpt-5.2", "cursor/gpt-5.2"];
        let collisions = colliding_model_labels(ids);

        assert_eq!(model_label(ids[0], &collisions), ids[0]);
        assert_eq!(model_label(ids[1], &collisions), ids[1]);
    }

    #[test]
    fn zero_priced_models_keep_usage_without_price_metrics() {
        let theme = crate::theme::current();
        let model = Model::from_spec("ollama/test-model").unwrap();
        let total = TokenUsage {
            input: 100,
            output: 20,
            ..TokenUsage::default()
        };
        let lines = build_lines(
            &UsageModalContext {
                total: &total,
                by_model: &HashMap::new(),
                model: &model,
                fast: false,
                quota: None,
            },
            &theme,
        );
        let text = lines
            .iter()
            .flat_map(|line| line.spans.iter())
            .map(|span| span.content.as_ref())
            .collect::<String>();

        assert!(!text.contains(PRICE_HEADING));
        assert!(text.contains("in 100"));
    }

    #[test]
    fn attributed_costs_price_each_model_separately() {
        let current = Model::from_spec("anthropic/claude-sonnet-4-5").unwrap();
        let mut by_model = HashMap::new();
        let usage = StoredTokenUsage {
            input: 1_000_000,
            output: 1_000_000,
            cache_read: 1_000_000,
            cache_creation: 1_000_000,
        };
        by_model.insert(current.id.clone(), usage);
        by_model.insert("claude-haiku-4-5".into(), usage);

        let (cost, savings) = attributed_costs(&by_model, &current, false).unwrap();
        let current_pricing = pricing_for(&current.id, &current).unwrap();
        let other_pricing = pricing_for("claude-haiku-4-5", &current).unwrap();
        let token_usage = TokenUsage::from(usage);
        let expected_cost =
            token_usage.cost(&current_pricing, false) + token_usage.cost(&other_pricing, false);
        let expected_savings = token_usage.savings_cost(&current_pricing, false)
            + token_usage.savings_cost(&other_pricing, false);

        assert!((cost - expected_cost).abs() < f64::EPSILON);
        assert!((savings - expected_savings).abs() < f64::EPSILON);
        assert!(
            (savings - token_usage.savings_cost(&current.pricing, false) * 2.0).abs()
                > f64::EPSILON
        );
    }

    #[test]
    fn attributed_costs_skip_zero_priced_models_without_dropping_the_estimate() {
        let current = Model::from_spec("anthropic/claude-sonnet-4-5").unwrap();
        let usage = StoredTokenUsage {
            input: 1_000_000,
            output: 1_000_000,
            cache_read: 1_000_000,
            cache_creation: 1_000_000,
        };
        let free = Model::from_spec("zai/glm-4.7-flash").unwrap();
        assert!(free.pricing.is_zero(), "test needs a zero-priced model");

        let by_model = HashMap::from([(current.id.clone(), usage), (free.id.clone(), usage)]);

        let (cost, savings) = attributed_costs(&by_model, &current, false)
            .expect("a priced model in the session still yields an estimate");
        let token_usage = TokenUsage::from(usage);

        assert!((cost - token_usage.cost(&current.pricing, false)).abs() < f64::EPSILON);
        assert!((savings - token_usage.savings_cost(&current.pricing, false)).abs() < f64::EPSILON);

        let only_free = HashMap::from([(free.id, usage)]);
        assert!(attributed_costs(&only_free, &current, false).is_none());
    }

    #[test]
    fn attributed_costs_resolve_provider_qualified_models() {
        let current = Model::from_spec("anthropic/claude-sonnet-4-5").unwrap();
        let usage = StoredTokenUsage {
            input: 1_000_000,
            output: 1_000_000,
            cache_read: 1_000_000,
            cache_creation: 1_000_000,
        };
        let by_model = HashMap::from([("openai/gpt-5.6-sol".into(), usage)]);

        let (cost, savings) = attributed_costs(&by_model, &current, false).unwrap();
        let pricing = Model::from_spec("openai/gpt-5.6-sol").unwrap().pricing;
        let token_usage = TokenUsage::from(usage);

        assert!((cost - token_usage.cost(&pricing, false)).abs() < f64::EPSILON);
        assert!((savings - token_usage.savings_cost(&pricing, false)).abs() < f64::EPSILON);
        assert!((cost - token_usage.cost(&current.pricing, false)).abs() > f64::EPSILON);
    }

    #[test]
    fn attributed_costs_skip_unknown_models_without_dropping_known_estimates() {
        let current = Model::from_spec("anthropic/claude-sonnet-4-5").unwrap();
        let usage = StoredTokenUsage {
            input: 1_000_000,
            output: 1_000_000,
            cache_read: 1_000_000,
            cache_creation: 1_000_000,
        };
        let by_model = HashMap::from([
            (current.id.clone(), usage),
            ("unknown-provider/unknown-model".into(), usage),
        ]);

        let (cost, savings) = attributed_costs(&by_model, &current, false).unwrap();
        let token_usage = TokenUsage::from(usage);

        assert!((cost - token_usage.cost(&current.pricing, false)).abs() < f64::EPSILON);
        assert!((savings - token_usage.savings_cost(&current.pricing, false)).abs() < f64::EPSILON);
    }

    #[test_case(0.00001, "<$0.0001" ; "tiny_nonzero")]
    #[test_case(0.0, "$0.00" ; "zero")]
    #[test_case(1.25, "$1.25" ; "regular")]
    fn currency_format_preserves_nonzero_rates(price: f64, expected: &str) {
        assert_eq!(format_currency(price), expected);
    }

    #[test]
    fn relative_formats_future_windows() {
        assert_eq!(relative(30), "in 0 min");
        assert_eq!(relative(120), "in 2 min");
        assert_eq!(relative(3 * HOUR + 36 * 60), "in 3 hr 36 min");
        assert_eq!(relative(5 * HOUR), "in 5 hr 0 min");
    }
}

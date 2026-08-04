//! Renders tool-call argument JSON as styled, truncated display lines so a
//! tool call never shows the user raw JSON.

use ratatui::text::{Line, Span};

use crate::markdown::should_truncate;
use crate::theme;
use n00n_redact::{REDACTED, escape_value, is_secret_key, looks_like_secret_value};

const COMPACT_BUDGET: usize = 40;
const COLLAPSED_LINE_BUDGET: usize = 160;
const EXPANDED_LINE_BUDGET: usize = n00n_markdown::render::TOOL_OUTPUT_MAX_LINE_BYTES;
const INDENT_UNIT: &str = "  ";
const DASH_PREFIX: &str = "- ";

pub struct ArgView {
    pub lines: Vec<Line<'static>>,
    pub hidden: usize,
}

pub(crate) fn render_args(
    input: &serde_json::Value,
    collapsed_limit: usize,
    expanded_limit: usize,
    expanded: bool,
) -> ArgView {
    let (limit, budget) = if expanded {
        (expanded_limit, EXPANDED_LINE_BUDGET)
    } else {
        (collapsed_limit, COLLAPSED_LINE_BUDGET)
    };
    let mut builder = ArgsBuilder {
        lines: Vec::new(),
        budget,
    };
    let redacted = redact_display_value(input);
    if let serde_json::Value::Object(map) = &redacted {
        for (key, value) in map {
            builder.push_entry(key, value, 0);
        }
    } else {
        builder.push_line(0, vec![builder.value_span(&redacted)]);
    }
    let hidden = builder.lines.len().saturating_sub(limit);
    let hidden = if should_truncate(hidden) { hidden } else { 0 };
    let mut lines = builder.lines;
    if hidden > 0 {
        lines.truncate(limit);
    }
    ArgView { lines, hidden }
}

pub(crate) fn arg_search_text(input: &serde_json::Value) -> String {
    let redacted = redact_display_value(input);
    if let serde_json::Value::Object(map) = &redacted {
        map.iter()
            .map(|(key, value)| format!("{key}: {}", search_value(value)))
            .collect::<Vec<_>>()
            .join("\n")
    } else {
        search_value(&redacted)
    }
}

pub(crate) fn redacted_json_text(value: &serde_json::Value) -> String {
    let redacted = redact_display_value(value);
    match serde_json::to_string_pretty(&redacted) {
        Ok(text) => text,
        Err(error) => format!("<invalid JSON: {error}>"),
    }
}

fn redact_display_value(value: &serde_json::Value) -> serde_json::Value {
    match value {
        serde_json::Value::Object(map) => {
            let out: serde_json::Map<String, serde_json::Value> = map
                .iter()
                .map(|(key, item)| {
                    let item = if is_secret_key(key) {
                        serde_json::Value::String(REDACTED.to_owned())
                    } else {
                        redact_display_value(item)
                    };
                    (key.clone(), item)
                })
                .collect();
            serde_json::Value::Object(out)
        }
        serde_json::Value::Array(items) => items.iter().map(redact_display_value).collect(),
        serde_json::Value::String(text) if looks_like_secret_value(text) => {
            serde_json::Value::String(REDACTED.to_owned())
        }
        serde_json::Value::String(text) => match serde_json::from_str::<serde_json::Value>(text) {
            Ok(inner) => {
                let redacted_inner = redact_display_value(&inner);
                match serde_json::to_string(&redacted_inner) {
                    Ok(text) => serde_json::Value::String(text),
                    Err(_) => serde_json::Value::String(REDACTED.to_owned()),
                }
            }
            Err(_) if text.starts_with('{') || text.starts_with('[') => {
                serde_json::Value::String(n00n_redact::redact_json_arg(text))
            }
            Err(_) => value.clone(),
        },
        other => other.clone(),
    }
}

fn search_value(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Object(map) => map
            .iter()
            .map(|(key, item)| format!("{key}: {}", search_value(item)))
            .collect::<Vec<_>>()
            .join(", "),
        serde_json::Value::Array(items) => items
            .iter()
            .map(search_value)
            .collect::<Vec<_>>()
            .join(", "),
        other => other.to_string(),
    }
}

struct ArgsBuilder {
    lines: Vec<Line<'static>>,
    budget: usize,
}

impl ArgsBuilder {
    fn push_line(&mut self, depth: usize, mut spans: Vec<Span<'static>>) {
        if depth > 0 {
            spans.insert(0, Span::raw(INDENT_UNIT.repeat(depth)));
        }
        self.lines.push(Line::from(spans));
    }

    fn push_entry(&mut self, key: &str, value: &serde_json::Value, depth: usize) {
        let key_span = Span::styled(format!("{key}:"), theme::current().tool_annotation);
        if is_secret_key(key) {
            self.push_line(
                depth,
                vec![
                    key_span,
                    Span::raw(" "),
                    Span::styled(REDACTED, theme::current().tool_dim),
                ],
            );
            return;
        }
        match value {
            serde_json::Value::Array(_) | serde_json::Value::Object(_) => {
                let compact = self.compact_text(value);
                if compact.chars().count() <= COMPACT_BUDGET {
                    self.push_line(
                        depth,
                        vec![
                            key_span,
                            Span::raw(" "),
                            Span::styled(compact, theme::current().tool),
                        ],
                    );
                } else {
                    self.push_line(depth, vec![key_span]);
                    self.push_container(value, depth + 1);
                }
            }
            scalar => {
                self.push_line(
                    depth,
                    vec![key_span, Span::raw(" "), self.value_span(scalar)],
                );
            }
        }
    }

    fn push_container(&mut self, value: &serde_json::Value, depth: usize) {
        match value {
            serde_json::Value::Object(map) => {
                for (key, item) in map {
                    self.push_entry(key, item, depth);
                }
            }
            serde_json::Value::Array(items) => {
                for item in items {
                    self.push_item(item, depth);
                }
            }
            _ => {}
        }
    }

    fn push_item(&mut self, value: &serde_json::Value, depth: usize) {
        let dash = Span::styled(DASH_PREFIX, theme::current().tool_dim);
        match value {
            serde_json::Value::Array(_) | serde_json::Value::Object(_) => {
                let compact = self.compact_text(value);
                if compact.chars().count() <= COMPACT_BUDGET {
                    self.push_line(
                        depth,
                        vec![dash, Span::styled(compact, theme::current().tool)],
                    );
                } else {
                    self.push_line(depth, vec![dash]);
                    self.push_container(value, depth + 1);
                }
            }
            scalar => self.push_line(depth, vec![dash, self.value_span(scalar)]),
        }
    }

    fn compact_text(&self, value: &serde_json::Value) -> String {
        match value {
            serde_json::Value::String(s) => self.escaped(s),
            serde_json::Value::Number(n) => n.to_string(),
            serde_json::Value::Bool(b) => b.to_string(),
            serde_json::Value::Null => "null".to_owned(),
            serde_json::Value::Array(items) => {
                let inner = items
                    .iter()
                    .map(|item| self.compact_text(item))
                    .collect::<Vec<_>>()
                    .join(", ");
                format!("[{inner}]")
            }
            serde_json::Value::Object(map) => {
                let inner = map
                    .iter()
                    .map(|(key, item)| {
                        let text = if is_secret_key(key) {
                            REDACTED.to_owned()
                        } else {
                            self.compact_text(item)
                        };
                        format!("{key}: {text}")
                    })
                    .collect::<Vec<_>>()
                    .join(", ");
                format!("{{{inner}}}")
            }
        }
    }

    fn value_span(&self, value: &serde_json::Value) -> Span<'static> {
        match value {
            serde_json::Value::String(s) => Span::styled(self.escaped(s), theme::current().tool),
            serde_json::Value::Number(n) => Span::styled(n.to_string(), theme::current().tool),
            serde_json::Value::Bool(b) => Span::styled(b.to_string(), theme::current().tool),
            serde_json::Value::Null => Span::styled("null", theme::current().tool_dim),
            serde_json::Value::Array(_) | serde_json::Value::Object(_) => {
                Span::styled(self.compact_text(value), theme::current().tool)
            }
        }
    }

    fn escaped(&self, s: &str) -> String {
        let out = escape_value(s);
        if out.len() > self.budget {
            let mut end = self.budget.saturating_sub(1);
            while !out.is_char_boundary(end) {
                end -= 1;
            }
            let mut head: String = out[..end].to_owned();
            head.push('…');
            head
        } else {
            out
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use test_case::test_case;

    fn lines_text(view: &ArgView) -> String {
        view.lines
            .iter()
            .flat_map(|line| line.spans.iter().map(|span| span.content.as_ref()))
            .collect::<Vec<_>>()
            .join("")
    }

    fn render(input: &serde_json::Value) -> ArgView {
        render_args(input, usize::MAX, usize::MAX, false)
    }

    #[test]
    fn flat_object_renders_key_value_lines() {
        let view = render(&json!({ "query": "needle", "path": "src" }));
        let text = lines_text(&view);
        assert!(text.contains("query: needle"));
        assert!(text.contains("path: src"));
        assert!(!text.contains('"'));
        assert_eq!(view.hidden, 0);
    }

    #[test]
    fn empty_object_renders_no_lines() {
        let view = render(&json!({}));
        assert!(view.lines.is_empty());
        assert_eq!(view.hidden, 0);
    }

    #[test]
    fn short_nested_object_stays_compact() {
        let view = render(&json!({ "opts": { "a": 1, "b": 2 } }));
        assert!(lines_text(&view).contains("opts: {a: 1, b: 2}"));
    }

    #[test]
    fn large_nested_object_expands_multiline() {
        let view = render(&json!({
            "opts": {
                "alpha": "x",
                "beta": "y",
                "gamma": "z",
                "delta": "w",
                "epsilon": "v",
                "zeta": "u",
                "eta": "t",
            }
        }));
        let text = lines_text(&view);
        assert!(text.contains("opts:"));
        assert!(text.contains("alpha: x"));
        assert!(text.contains("eta: t"));
        assert_eq!(view.lines.len(), 8);
    }

    #[test]
    fn large_array_expands_multiline() {
        let view = render(&json!({
            "items": ["one", "two", "three", "four", "five", "six", "seven", "eight", "nine", "ten"]
        }));
        let text = lines_text(&view);
        assert!(text.contains("items:"));
        assert!(text.contains("- one"));
        assert!(text.contains("- ten"));
    }

    #[test]
    fn short_array_stays_compact() {
        let view = render(&json!({ "tags": ["a", "b"] }));
        assert!(lines_text(&view).contains("tags: [a, b]"));
    }

    #[test]
    fn collapsed_truncates_lines_and_reports_hidden() {
        let view = render_args(
            &json!({ "a": 1, "b": 2, "c": 3, "d": 4, "e": 5 }),
            2,
            usize::MAX,
            false,
        );
        assert_eq!(view.hidden, 3);
        assert_eq!(view.lines.len(), 2);
    }

    #[test]
    fn single_hidden_line_shows_all() {
        let view = render_args(&json!({ "a": 1, "b": 2, "c": 3 }), 2, usize::MAX, false);
        assert_eq!(view.hidden, 0);
        assert_eq!(view.lines.len(), 3);
    }

    #[test]
    fn expanded_uses_separate_bounded_line_limit() {
        let view = render_args(
            &json!({ "a": 1, "b": 2, "c": 3, "d": 4, "e": 5, "f": 6 }),
            2,
            4,
            true,
        );
        assert_eq!(view.hidden, 2);
        assert_eq!(view.lines.len(), 4);
    }

    #[test]
    fn long_string_truncated_when_collapsed() {
        let long = "x".repeat(400);
        let view = render(&json!({ "content": long }));
        let text = lines_text(&view);
        assert!(text.contains('…'));
        assert!(text.chars().count() < 400);
    }

    #[test]
    fn long_string_full_when_expanded() {
        let long = "x".repeat(400);
        let view = render_args(&json!({ "content": long }), 1, usize::MAX, true);
        let text = lines_text(&view);
        assert!(!text.contains('…'));
        assert_eq!(text.chars().count(), "content: ".chars().count() + 400);
    }

    #[test]
    fn expanded_long_string_uses_existing_line_budget() {
        let long = "x".repeat(EXPANDED_LINE_BUDGET + 100);
        let view = render_args(&json!({ "content": long }), 1, usize::MAX, true);
        let text = lines_text(&view);
        assert!(text.contains('…'));
        assert!(text.chars().count() <= EXPANDED_LINE_BUDGET + "content: ".len());
    }

    #[test]
    fn newlines_escaped_in_string_values() {
        let view = render(&json!({ "script": "a\nb\tc" }));
        let text = lines_text(&view);
        assert!(text.contains("a\\nb\\tc"));
        assert_eq!(view.lines.len(), 1);
    }

    #[test]
    fn secret_keys_redacted() {
        let view = render(&json!({ "api_key": "sk-123", "token": "abc", "path": "/tmp" }));
        let text = lines_text(&view);
        assert!(text.contains("api_key: [redacted]"));
        assert!(text.contains("token: [redacted]"));
        assert!(text.contains("path: /tmp"));
        assert!(!text.contains("sk-123"));
        assert!(!text.contains("abc"));
    }

    #[test_case("client_secret" ; "client_secret")]
    #[test_case("Client-Secret" ; "client_secret_mixed_case_dash")]
    #[test_case("refresh token" ; "refresh_token_space")]
    #[test_case("ID_TOKEN" ; "id_token")]
    #[test_case("session-token" ; "session_token_dash")]
    #[test_case("Secret.Key" ; "secret_key_dot")]
    #[test_case("AWS_SECRET_ACCESS_KEY" ; "aws_secret_access_key")]
    #[test_case("x-api-key" ; "x_api_key")]
    #[test_case("access_key" ; "access_key")]
    #[test_case("auth_key" ; "auth_key")]
    #[test_case("password_hash" ; "password_hash")]
    #[test_case("secret_token" ; "secret_token")]
    #[test_case("private_token" ; "private_token")]
    #[test_case("api_secret" ; "api_secret")]
    fn normalized_secret_keys_redacted(key: &str) {
        let view = render(&json!({ key: "sensitive-value" }));
        let text = lines_text(&view);
        assert!(text.contains(REDACTED));
        assert!(!text.contains("sensitive-value"));
    }

    #[test]
    fn secrets_redacted_nested() {
        let view = render(&json!({ "auth": { "access_token": "abc", "user": "bob" } }));
        let text = lines_text(&view);
        assert!(text.contains("access_token: [redacted]"));
        assert!(text.contains("user: bob"));
    }

    #[test]
    fn innocuous_keys_not_redacted() {
        let view = render(&json!({
            "max_tokens": 2048,
            "max-tokens": 1024,
            "Max Tokens": 512,
            "tokens": 12,
            "token_count": 5,
            "model": "gpt"
        }));
        let text = lines_text(&view);
        assert!(text.contains("max_tokens: 2048"));
        assert!(text.contains("max-tokens: 1024"));
        assert!(text.contains("Max Tokens: 512"));
        assert!(text.contains("tokens: 12"));
        assert!(text.contains("token_count: 5"));
        assert!(text.contains("model: gpt"));
    }

    #[test_case(&json!("hello") => "hello" ; "string_root")]
    #[test_case(&json!(42) => "42" ; "number_root")]
    #[test_case(&json!(["a", "b"]) => "[a, b]" ; "array_root")]
    fn scalar_and_array_roots_render_value_line(input: &serde_json::Value) -> String {
        lines_text(&render(input))
    }

    #[test]
    fn search_text_uses_unescaped_values() {
        let input = json!({ "script": "a\nb", "nested": { "x": "y" } });
        assert_eq!(arg_search_text(&input), "script: a\nb\nnested: x: y");
    }

    #[test]
    fn search_text_hides_secret_values() {
        let text = arg_search_text(&json!({
            "user": "bob",
            "token": "abc",
            "nested": { "api_key": "sk-123" }
        }));
        assert!(text.contains("bob"));
        assert!(text.contains("[redacted]"));
        assert!(!text.contains("abc"));
        assert!(!text.contains("sk-123"));
    }

    #[test]
    fn redacted_json_text_hides_secret_values() {
        let text = redacted_json_text(&json!({ "user": "bob", "token": "abc" }));
        assert!(text.contains("[redacted]"));
        assert!(text.contains("bob"));
        assert!(!text.contains("abc"));
    }

    #[test]
    fn secret_shaped_values_redacted_under_benign_keys() {
        let view = render(&json!({ "note": "sk-live-0123456789abcdef0123456789ab" }));
        let text = lines_text(&view);
        assert!(
            !text.contains("sk-live-0123456789abcdef0123456789ab"),
            "secret value leaked: {text}"
        );
        assert!(text.contains(REDACTED), "value should be redacted: {text}");
    }

    #[test]
    fn secret_keys_redacted_inside_stringified_json() {
        let view = render(&json!({
            "payload": "{\"user\":\"bob\",\"api_key\":\"sk-live-0123456789abcdef0123456789ab\"}"
        }));
        let text = lines_text(&view);
        assert!(
            !text.contains("sk-live-0123456789abcdef0123456789ab"),
            "secret value leaked through stringified JSON: {text}"
        );
        assert!(text.contains("bob"));
        assert!(text.contains(REDACTED));
    }

    #[test]
    fn malformed_stringified_json_is_left_untouched() {
        let view = render(&json!({ "payload": "{not valid json" }));
        let text = lines_text(&view);
        assert!(text.contains("{not valid json"));
    }

    #[test]
    fn malformed_stringified_json_redacts_embedded_credentials() {
        let view = render(&json!({ "payload": r#"{"user":"bob","api_key":"short"}"# }));
        let text = lines_text(&view);
        assert!(text.contains("user"));
        assert!(text.contains("bob"));
        assert!(text.contains(REDACTED));
        assert!(!text.contains("short"));
    }

    #[test]
    fn keys_use_annotation_style() {
        let view = render(&json!({ "query": "x" }));
        let span = view.lines[0]
            .spans
            .iter()
            .find(|span| span.content == "query:")
            .expect("key span");
        assert_eq!(span.style, theme::current().tool_annotation);
    }
}

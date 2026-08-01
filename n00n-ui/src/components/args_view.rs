//! Renders tool-call argument JSON as styled, truncated display lines so a
//! tool call never shows the user raw JSON.

use ratatui::text::{Line, Span};

use crate::markdown::should_truncate;
use crate::theme;

const COMPACT_BUDGET: usize = 40;
const LINE_BUDGET: usize = 160;
const REDACTED: &str = "[redacted]";
const INDENT_UNIT: &str = "  ";
const DASH_PREFIX: &str = "- ";

pub struct ArgView {
    pub lines: Vec<Line<'static>>,
    pub hidden: usize,
}

pub(crate) fn render_args(input: &serde_json::Value, limit: usize, expanded: bool) -> ArgView {
    let budget = if expanded { usize::MAX } else { LINE_BUDGET };
    let mut builder = ArgsBuilder {
        lines: Vec::new(),
        budget,
    };
    if let serde_json::Value::Object(map) = input {
        for (key, value) in map {
            builder.push_entry(key, value, 0);
        }
    } else {
        builder.push_line(0, vec![builder.value_span(input)]);
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
    let redacted = redact_value(input);
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
    let redacted = redact_value(value);
    match serde_json::to_string_pretty(&redacted) {
        Ok(text) => text,
        Err(error) => format!("<invalid JSON: {error}>"),
    }
}

fn is_secret_key(key: &str) -> bool {
    matches!(
        key.to_ascii_lowercase().as_str(),
        "password"
            | "passwd"
            | "passphrase"
            | "pwd"
            | "secret"
            | "token"
            | "access_token"
            | "auth_token"
            | "api_key"
            | "apikey"
            | "api-key"
            | "authorization"
            | "cookie"
            | "credential"
            | "credentials"
            | "private_key"
            | "privatekey"
    )
}

fn redact_value(value: &serde_json::Value) -> serde_json::Value {
    match value {
        serde_json::Value::Object(map) => {
            let out: serde_json::Map<String, serde_json::Value> = map
                .iter()
                .map(|(key, item)| {
                    let item = if is_secret_key(key) {
                        serde_json::Value::String(REDACTED.to_owned())
                    } else {
                        redact_value(item)
                    };
                    (key.clone(), item)
                })
                .collect();
            serde_json::Value::Object(out)
        }
        serde_json::Value::Array(items) => items.iter().map(redact_value).collect(),
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
        let mut out = String::with_capacity(s.len());
        for ch in s.chars() {
            match ch {
                '\n' => out.push_str("\\n"),
                '\t' => out.push_str("\\t"),
                '\r' => out.push_str("\\r"),
                _ => out.push(ch),
            }
        }
        if out.chars().count() > self.budget {
            let head: String = out.chars().take(self.budget.saturating_sub(1)).collect();
            format!("{head}…")
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
        render_args(input, usize::MAX, false)
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
        let view = render_args(&json!({ "a": 1, "b": 2, "c": 3, "d": 4, "e": 5 }), 2, false);
        assert_eq!(view.hidden, 3);
        assert_eq!(view.lines.len(), 2);
    }

    #[test]
    fn single_hidden_line_shows_all() {
        let view = render_args(&json!({ "a": 1, "b": 2, "c": 3 }), 2, false);
        assert_eq!(view.hidden, 0);
        assert_eq!(view.lines.len(), 3);
    }

    #[test]
    fn expanded_keeps_all_lines() {
        let view = render_args(&json!({ "a": 1, "b": 2, "c": 3 }), usize::MAX, true);
        assert_eq!(view.hidden, 0);
        assert_eq!(view.lines.len(), 3);
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
        let view = render_args(&json!({ "content": long }), usize::MAX, true);
        let text = lines_text(&view);
        assert!(!text.contains('…'));
        assert_eq!(text.chars().count(), "content: ".chars().count() + 400);
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
            "tokens": 12,
            "token_count": 5,
            "model": "gpt"
        }));
        let text = lines_text(&view);
        assert!(text.contains("max_tokens: 2048"));
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

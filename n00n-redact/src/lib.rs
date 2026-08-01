//! Shared secret redaction for tool arguments, display, and logs.
//!
//! Every consumer (UI rendering, Lua activity descriptions, provider and
//! agent logs) redacts through this module so the secret-key policy lives in
//! one place instead of three drifting copies.

use serde_json::Value;

pub mod sanitize;
pub use sanitize::sanitize_text;

/// Placeholder for values under secret keys in JSON redaction.
pub const REDACTED: &str = "[redacted]";

/// Max characters kept when a raw argument string must fall back to the
/// free-text sanitizer (malformed JSON) for log output.
const LOG_ARG_MAX_CHARS: usize = 2000;

/// Secret key names, lowercased and stripped of separators. Exact-match only:
/// a JSON entry is redacted when its key name normalizes to one of these.
/// Used for display, search text, and logging.
pub const SECRET_KEYS: &[&str] = &[
    "password",
    "passwd",
    "passphrase",
    "pwd",
    "secret",
    "token",
    "accesstoken",
    "authtoken",
    "apikey",
    "authorization",
    "cookie",
    "credential",
    "credentials",
    "privatekey",
    "clientsecret",
    "refreshtoken",
    "idtoken",
    "sessiontoken",
    "secretkey",
    "awssecretaccesskey",
    "xapikey",
    "accesskey",
    "authkey",
    "passwordhash",
    "secrettoken",
    "privatetoken",
    "apisecret",
];

/// Returns true when the key name matches a known secret key after
/// normalizing away separators and case.
#[must_use]
pub fn is_secret_key(key: &str) -> bool {
    let normalized = normalize_key(key);
    SECRET_KEYS.contains(&normalized.as_str())
}

fn normalize_key(key: &str) -> String {
    key.chars()
        .filter(char::is_ascii_alphanumeric)
        .flat_map(char::to_lowercase)
        .collect()
}

/// Recursively replaces the values under secret keys with `[redacted]`,
/// leaving all other structure intact.
#[must_use]
pub fn redact_json_value(value: &Value) -> Value {
    match value {
        Value::Object(map) => {
            let out: serde_json::Map<String, Value> = map
                .iter()
                .map(|(key, item)| {
                    let item = if is_secret_key(key) {
                        Value::String(REDACTED.to_owned())
                    } else {
                        redact_json_value(item)
                    };
                    (key.clone(), item)
                })
                .collect();
            Value::Object(out)
        }
        Value::Array(items) => items.iter().map(redact_json_value).collect(),
        other => other.clone(),
    }
}

/// Redacts a raw tool-argument JSON string for logging. Valid JSON is
/// redacted structurally; malformed JSON falls back to the free-text
/// sanitizer as a best-effort safety net.
#[must_use]
pub fn redact_json_arg(arg: &str) -> String {
    let redacted = match serde_json::from_str::<Value>(arg) {
        Ok(value) => redact_json_value(&value),
        Err(_) => return sanitize::sanitize_text(arg, LOG_ARG_MAX_CHARS),
    };
    match serde_json::to_string(&redacted) {
        Ok(text) => text,
        Err(_) => sanitize::sanitize_text(arg, LOG_ARG_MAX_CHARS),
    }
}

/// Escapes control and quote characters for single-line display, escaping
/// backslash first so escaped sequences stay unambiguous.
#[must_use]
pub fn escape_value(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        let escaped = match ch {
            '\\' => Some("\\\\"),
            '"' => Some("\\\""),
            '\n' => Some("\\n"),
            '\t' => Some("\\t"),
            '\r' => Some("\\r"),
            _ => None,
        };
        match escaped {
            Some(s) => out.push_str(s),
            None => out.push(ch),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use test_case::test_case;

    #[test_case("api_key"; "snake")]
    #[test_case("API-KEY"; "upper")]
    #[test_case("accessKey"; "camel")]
    #[test_case("x-api-key"; "hyphen")]
    #[test_case("Authorization"; "title")]
    fn secret_key_matches_normalized_forms(raw: &str) {
        assert!(is_secret_key(raw));
    }

    #[test_case("query"; "query")]
    #[test_case("path"; "path")]
    #[test_case("content"; "content")]
    fn secret_key_ignores_benign_names(raw: &str) {
        assert!(!is_secret_key(raw));
    }

    #[test]
    fn redact_json_redacts_nested_and_arrays() {
        let value = json!({
            "user": "bob",
            "token": "abc",
            "nested": { "api_key": "sk-123" },
            "list": [{ "password": "pw" }, "kept"]
        });
        let redacted = redact_json_value(&value);
        assert_eq!(redacted["user"], "bob");
        assert_eq!(redacted["token"], REDACTED);
        assert_eq!(redacted["nested"]["api_key"], REDACTED);
        assert_eq!(redacted["list"][0]["password"], REDACTED);
        assert_eq!(redacted["list"][1], "kept");
    }

    #[test]
    fn redact_json_leaves_scalars_untouched() {
        assert_eq!(redact_json_value(&json!("plain")), json!("plain"));
        assert_eq!(redact_json_value(&json!(42)), json!(42));
        assert_eq!(redact_json_value(&json!([1, 2, 3])), json!([1, 2, 3]));
    }

    #[test]
    fn redact_json_arg_redacts_valid_json() {
        let out = redact_json_arg(r#"{"query":"needle","api_key":"sk-123"}"#);
        let parsed: serde_json::Value =
            serde_json::from_str(&out).expect("redacted arg stays valid JSON");
        assert_eq!(parsed["query"], "needle");
        assert_eq!(parsed["api_key"], REDACTED);
    }

    #[test]
    fn redact_json_arg_sanitizes_malformed_json() {
        let out = redact_json_arg(r#"{"token":"sk-123","user":""#);
        assert!(!out.contains("sk-123"), "token value leaked: {out}");
        assert!(out.contains("token"), "key name should stay: {out}");
    }

    #[test_case("plain", "plain"; "plain")]
    #[test_case("a\nb", "a\\nb"; "newline")]
    #[test_case("a\\nb", "a\\\\nb"; "backslash first")]
    #[test_case("say \"hi\"", "say \\\"hi\\\""; "quote")]
    #[test_case("tab\there", "tab\\there"; "tab")]
    fn escape_value_cases(raw: &str, expected: &str) {
        assert_eq!(escape_value(raw), expected);
    }

    #[test]
    fn escape_value_keeps_distinguishable_backslash_and_newline() {
        assert_ne!(escape_value("a\\nb"), escape_value("a\nb"));
    }
}

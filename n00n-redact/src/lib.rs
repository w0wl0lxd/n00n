//! Shared secret redaction for tool arguments, display, and logs.
//!
//! Every consumer (UI rendering, Lua activity descriptions, provider and
//! agent logs) redacts through this module so the secret-key policy lives in
//! one place instead of three drifting copies.

use std::mem;

use serde_json::Value;
use tracing::debug;

pub mod sanitize;
pub use sanitize::sanitize_text;

/// Demotes a log line to `info!` while making the demotion explicit and
/// greppable. Every expected-noise demotion goes through this macro so the
/// `warn!`/`error!` audit stays meaningful.
#[macro_export]
macro_rules! demoted {
    ($($arg:tt)*) => {
        ::tracing::info!($($arg)*)
    };
}

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
    "awsaccesskeyid",
    "googlesecretkey",
    "googlesecret",
    "googleprivatekey",
    "googleaccesskey",
    "googleaccesstoken",
    "googlerefreshtoken",
    "googleclientsecret",
    "oauthtoken",
    "oauthsecret",
    "oauthkey",
    "jwt",
    "jwttoken",
    "jwtsigningkey",
    "sastoken",
    "sas",
    "connectionstring",
    "slacktoken",
    "slacksecret",
    "bottoken",
    "botsecret",
    "githubtoken",
    "githubsecret",
    "githubkey",
    "ghtoken",
    "ghsecret",
    "ghkey",
    "openaisecret",
    "openaikey",
    "openaitoken",
    "anthropickey",
    "anthropictoken",
    "apollokey",
];

/// Final-word secret markers used by `is_secret_key` when the exact-match
/// list misses a compound key. A key is redacted when its last
/// separator-delimited word (case-insensitive) is one of these.
const SECRET_KEY_LAST_WORDS: &[&str] = &[
    "token",
    "secret",
    "password",
    "passwd",
    "passphrase",
    "credential",
    "cookie",
    "oauth",
    "jwt",
    "signature",
    "authorization",
    "privatekey",
    "apikey",
    "accesskey",
    "secretkey",
];

const SECRET_KEY_QUALIFIERS: &[&str] = &[
    "api",
    "auth",
    "access",
    "secret",
    "private",
    "oauth",
    "signing",
    "encryption",
    "credential",
    "client",
    "github",
    "openai",
    "google",
    "anthropic",
    "ssh",
    "database",
    "db",
];

/// Returns true when the key name matches a known secret key after
/// normalizing away separators and case, or when its final word is a secret
/// marker (covers compound keys like `slack_token` or `db_password`).
#[must_use]
pub fn is_secret_key(key: &str) -> bool {
    let normalized = normalize_key(key);
    if SECRET_KEYS.contains(&normalized.as_str()) {
        return true;
    }
    last_word(key).is_some_and(|word| {
        SECRET_KEY_LAST_WORDS.contains(&word.as_str())
            || (word == "key" && has_secret_key_qualifier(&normalized))
    })
}

fn has_secret_key_qualifier(normalized: &str) -> bool {
    normalized.strip_suffix("key").is_some_and(|prefix| {
        SECRET_KEY_QUALIFIERS
            .iter()
            .any(|qualifier| prefix.ends_with(qualifier))
    })
}

/// Splits `key` into separator- or case-delimited words, lowercased.
pub(crate) fn words(key: &str) -> Vec<String> {
    let characters = key.chars().collect::<Vec<_>>();
    let mut current = String::new();
    let mut result = Vec::new();
    for (index, character) in characters.iter().copied().enumerate() {
        let separator = !character.is_ascii_alphanumeric();
        let previous = index
            .checked_sub(1)
            .and_then(|position| characters.get(position));
        let next = characters.get(index + 1);
        let case_boundary = character.is_ascii_uppercase()
            && (previous.is_some_and(char::is_ascii_lowercase)
                || (previous.is_some_and(char::is_ascii_uppercase)
                    && next.is_some_and(char::is_ascii_lowercase)));
        if separator || case_boundary {
            if !current.is_empty() {
                result.push(mem::take(&mut current));
            }
            if separator {
                continue;
            }
        }
        current.push(character);
    }
    if !current.is_empty() {
        result.push(current);
    }
    result
        .into_iter()
        .map(|word| word.to_ascii_lowercase())
        .collect()
}

/// Returns the last separator- or case-delimited word of `key`, lowercased.
fn last_word(key: &str) -> Option<String> {
    words(key).pop()
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
        Ok(value) => redact_json_value_for_log(&value),
        Err(error) => {
            debug!(%error, "tool argument JSON parse failed; sanitizing text");
            return sanitize::sanitize_text(arg, LOG_ARG_MAX_CHARS);
        }
    };
    match serde_json::to_string(&redacted) {
        Ok(text) => sanitize::truncate(&text, LOG_ARG_MAX_CHARS),
        Err(error) => {
            debug!(%error, "redacted tool argument serialization failed; sanitizing text");
            sanitize::sanitize_text(arg, LOG_ARG_MAX_CHARS)
        }
    }
}

/// Log-path variant of `redact_json_value`: in addition to secret keys, it
/// replaces string values that look like embedded secrets (JWTs, provider
/// tokens, cloud access keys) even under benign key names. Kept separate so
/// display and search redaction stay key-anchored only.
#[must_use]
pub fn redact_json_value_for_log(value: &Value) -> Value {
    match value {
        Value::Object(map) => {
            let out: serde_json::Map<String, Value> = map
                .iter()
                .map(|(key, item)| {
                    let item = if is_secret_key(key) {
                        Value::String(REDACTED.to_owned())
                    } else {
                        redact_json_value_for_log(item)
                    };
                    (key.clone(), item)
                })
                .collect();
            Value::Object(out)
        }
        Value::Array(items) => items.iter().map(redact_json_value_for_log).collect(),
        Value::String(text) => {
            if looks_like_secret_value(text) {
                Value::String(REDACTED.to_owned())
            } else if let Ok(inner) = serde_json::from_str::<Value>(text) {
                // String contains JSON - redact the inner content and re-serialize
                let redacted_inner = redact_json_value_for_log(&inner);
                match serde_json::to_string(&redacted_inner) {
                    Ok(text) => sanitize::truncate(&text, LOG_ARG_MAX_CHARS).into(),
                    Err(error) => {
                        debug!(%error, "redacted nested JSON serialization failed");
                        REDACTED.into()
                    }
                }
            } else {
                sanitize::sanitize_text_preserve_newlines(text, LOG_ARG_MAX_CHARS).into()
            }
        }
        other => other.clone(),
    }
}

/// Minimum total characters for a token-shaped value before it is treated as
/// a secret, so short benign strings like `sk-abc` never match.
const TOKEN_VALUE_MIN_CHARS: usize = 24;

/// JWT-shaped values need all three dot-separated base64url segments, a
/// header short enough to be a claims header, a payload long enough to carry
/// claims, mixed-case alphanumeric payload, and a total length that only real
/// tokens reach. This keeps version strings (`1.2.3`), IPs (`1.2.3.4`), and
/// short dotted identifiers from false-positive.
const JWT_MIN_TOTAL_CHARS: usize = 50;
const JWT_MAX_HEADER_CHARS: usize = 50;
const JWT_MIN_PAYLOAD_CHARS: usize = 16;

/// AWS access key ids are `AKIA`/`ASIA` followed by exactly 16 uppercase
/// alphanumerics.
const AWS_ACCESS_KEY_PREFIX_CHARS: usize = 4;
const AWS_ACCESS_KEY_ID_CHARS: usize = 20;

/// Minimum characters after `Bearer ` before a value is treated as a token.
const BEARER_VALUE_MIN_CHARS: usize = 40;

/// Returns true when `value` is shaped like a secret that has no key to
/// anchor on: a JWT, a provider-prefixed token, an AWS access key id, or a
/// `Bearer` header. Conservative by design: short and low-entropy strings
/// never match, so ordinary content stays visible.
#[must_use]
pub fn looks_like_secret_value(value: &str) -> bool {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return false;
    }
    if let Some((scheme, credential)) = trimmed.split_once(' ')
        && (scheme.eq_ignore_ascii_case("bearer") || scheme.eq_ignore_ascii_case("basic"))
    {
        return credential.trim().len() >= BEARER_VALUE_MIN_CHARS;
    }
    is_private_key(trimmed)
        || is_jwt_like(trimmed)
        || is_prefixed_token(trimmed)
        || is_aws_access_key_id(trimmed)
}

fn is_private_key(value: &str) -> bool {
    value.starts_with("-----BEGIN ") && value.contains(" PRIVATE KEY-----")
}

fn is_jwt_like(value: &str) -> bool {
    if value.len() < JWT_MIN_TOTAL_CHARS {
        return false;
    }
    let segments = value.split('.').collect::<Vec<_>>();
    if segments.len() != 3 {
        return false;
    }
    let (header, payload, signature) = (segments[0], segments[1], segments[2]);
    if header.is_empty() || payload.is_empty() || signature.is_empty() {
        return false;
    }
    if header.len() > JWT_MAX_HEADER_CHARS || payload.len() < JWT_MIN_PAYLOAD_CHARS {
        return false;
    }
    let base64url = |segment: &str| {
        segment
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '-' || ch == '_')
    };
    if !segments.iter().all(|segment| base64url(segment)) {
        return false;
    }
    if segments
        .iter()
        .any(|segment| segment.chars().all(|ch| ch.is_ascii_digit()))
    {
        return false;
    }
    let has_upper = payload.chars().any(char::is_uppercase);
    let has_lower = payload.chars().any(char::is_lowercase);
    has_upper && has_lower
}

fn is_prefixed_token(value: &str) -> bool {
    if value.len() < TOKEN_VALUE_MIN_CHARS {
        return false;
    }
    let lower = value.to_ascii_lowercase();
    sanitize::SECRET_TOKEN_PREFIXES
        .iter()
        .any(|prefix| lower.starts_with(prefix))
        || lower.starts_with("aiza")
}

fn is_aws_access_key_id(value: &str) -> bool {
    if value.len() != AWS_ACCESS_KEY_ID_CHARS || !value.is_ascii() {
        return false;
    }
    let (prefix, rest) = value.split_at(AWS_ACCESS_KEY_PREFIX_CHARS);
    (prefix == "AKIA" || prefix == "ASIA")
        && rest
            .chars()
            .all(|ch| ch.is_ascii_uppercase() || ch.is_ascii_digit())
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
    #[test_case("aws_access_key_id"; "aws snake")]
    #[test_case("AWS_ACCESS_KEY_ID"; "aws upper")]
    #[test_case("x-goog-api-key"; "google hyphen")]
    #[test_case("oauth_token"; "oauth snake")]
    #[test_case("OAuthToken"; "oauth camel")]
    #[test_case("jwt"; "jwt")]
    #[test_case("connection_string"; "connection snake")]
    #[test_case("slack_token"; "slack")]
    #[test_case("bot_token"; "bot")]
    #[test_case("github_token"; "github")]
    #[test_case("sas_token"; "sas")]
    fn secret_key_matches_normalized_forms(raw: &str) {
        assert!(is_secret_key(raw));
    }

    #[test_case("query"; "query")]
    #[test_case("path"; "path")]
    #[test_case("content"; "content")]
    #[test_case("max_tokens"; "plural token")]
    #[test_case("token_count"; "count")]
    #[test_case("tokens"; "tokens")]
    #[test_case("model"; "model")]
    #[test_case("nested"; "nested")]
    #[test_case("x"; "x")]
    #[test_case("keyname"; "keyname merged")]
    #[test_case("publickey"; "publickey merged")]
    #[test_case("object_key"; "object key")]
    #[test_case("cacheKey"; "cache key")]
    fn secret_key_ignores_benign_names(raw: &str) {
        assert!(!is_secret_key(raw));
    }

    #[test]
    fn compound_keys_match_by_final_word() {
        assert!(is_secret_key("db_password"));
        assert!(is_secret_key("slack-token"));
        assert!(is_secret_key("my_api key"));
        assert!(is_secret_key("session_credential"));
        assert!(is_secret_key("vaultSecret"));
        assert!(is_secret_key("myAuthKey"));
        assert!(is_secret_key("DBPassword"));
        let redacted = redact_json_value_for_log(&json!({
            "DBPassword": "correct horse battery staple",
        }));
        assert_eq!(redacted["DBPassword"], REDACTED);
    }

    #[test]
    fn value_patterns_match_jwts_aws_and_prefixed_tokens() {
        let jwt = "eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9.eyJzdWIiOiIxMjM0NTY3ODkwIiwibmFtZSI6IkpvaG4gRG9lIiwiaWF0IjoxNTE2MjM5MDIyfQ.SflKxwRJSMeKKF2QT4fwpMeJf36POk6yJV_adQssw5c";
        assert!(looks_like_secret_value(jwt));
        assert!(looks_like_secret_value(&format!("sk-{}", "a".repeat(48))));
        assert!(looks_like_secret_value(&format!("ghp_{}", "a".repeat(36))));
        assert!(looks_like_secret_value(&format!("gho_{}", "a".repeat(36))));
        assert!(looks_like_secret_value(&format!("ghu_{}", "a".repeat(36))));
        assert!(looks_like_secret_value(&format!("ghs_{}", "a".repeat(36))));
        assert!(looks_like_secret_value(&format!("ghr_{}", "a".repeat(36))));
        assert!(looks_like_secret_value(&format!(
            "github_pat_{}",
            "a".repeat(30)
        )));
        assert!(looks_like_secret_value(&format!(
            "glpat-{}",
            "a".repeat(26)
        )));
        assert!(looks_like_secret_value("AKIA0123456789ABCDEF"));
        assert!(looks_like_secret_value("ASIA0123456789ABCDEF"));
        assert!(looks_like_secret_value(
            "AIzaSyD0123456789abcdefghijklmnopqrstuv"
        ));
        assert!(looks_like_secret_value(&format!(
            "Bearer {}",
            "abc.def.".repeat(10)
        )));
        assert!(looks_like_secret_value(&format!(
            "bearer {}",
            "abc.def.".repeat(10)
        )));
        assert!(looks_like_secret_value(&format!(
            "BEARER {}",
            "abc.def.".repeat(10)
        )));
        assert!(looks_like_secret_value(
            "Bearer eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0NTY3ODkwIiwibmFtZSI6IkpvaG4gRG9lIn0.abc-DEF_ghi",
        ));
        let private_key = format!(
            "-----BEGIN {kind} PRIVATE KEY-----\nplaceholder\n-----END {kind} PRIVATE KEY-----",
            kind = "OPENSSH",
        );
        assert!(looks_like_secret_value(&private_key));
    }

    #[test]
    fn log_redaction_covers_private_keys_under_benign_keys() {
        let private_key = format!(
            "-----BEGIN {kind} PRIVATE KEY-----\nplaceholder\n-----END {kind} PRIVATE KEY-----",
            kind = "RSA",
        );
        let value = json!({ "content": private_key });
        assert_eq!(redact_json_value_for_log(&value)["content"], REDACTED);
    }

    #[test]
    fn jwt_payload_without_digits_is_secret_shaped() {
        let jwt = "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiJTcXhkVnQifQ.abcDEFghiJKLmnopQRSTuv";
        assert!(looks_like_secret_value(jwt));
    }

    #[test]
    fn value_patterns_ignore_short_and_low_entropy_strings() {
        assert!(!looks_like_secret_value("1.2.3"));
        assert!(!looks_like_secret_value("1.2.3.4"));
        assert!(!looks_like_secret_value("a.b.c"));
        assert!(!looks_like_secret_value("sk-abc"));
        assert!(!looks_like_secret_value("ghp_short"));
        assert!(!looks_like_secret_value("AKIA0"));
        assert!(!looks_like_secret_value("AA€€€€€€"));
        assert!(!looks_like_secret_value("Bearer short"));
        assert!(!looks_like_secret_value("plain text"));
        assert!(!looks_like_secret_value(""));
        assert!(!looks_like_secret_value("gpt-4o-mini"));
    }

    #[test]
    fn log_redaction_covers_value_patterns_under_benign_keys() {
        let jwt = "eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9.eyJzdWIiOiIxMjM0NTY3ODkwIiwibmFtZSI6IkpvaG4gRG9lIiwiaWF0IjoxNTE2MjM5MDIyfQ.SflKxwRJSMeKKF2QT4fwpMeJf36POk6yJV_adQssw5c";
        let value = json!({
            "user": "bob",
            "note": jwt,
            "nested": { "data": format!("sk-{}", "a".repeat(48)) },
            "list": [format!("AKIA0123456789ABCDEF")],
        });
        let redacted = redact_json_value_for_log(&value);
        assert_eq!(redacted["user"], "bob");
        assert_eq!(redacted["note"], REDACTED);
        assert_eq!(redacted["nested"]["data"], REDACTED);
        assert_eq!(redacted["list"][0], REDACTED);
    }

    #[test]
    fn display_redaction_stays_key_anchored() {
        let jwt = "eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9.eyJzdWIiOiIxMjM0NTY3ODkwIiwibmFtZSI6IkpvaG4gRG9lIiwiaWF0IjoxNTE2MjM5MDIyfQ.SflKxwRJSMeKKF2QT4fwpMeJf36POk6yJV_adQssw5c";
        let redacted = redact_json_value(&json!({ "note": jwt }));
        assert_eq!(
            redacted["note"], jwt,
            "UI redaction must not use value patterns"
        );
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

    #[test]
    fn redact_json_arg_redacts_stringified_containers() {
        let input = serde_json::to_string(&json!(r#"{"api_key":"sk-live"}"#)).unwrap();
        let out = redact_json_arg(&input);
        assert!(
            !out.contains("sk-live"),
            "secret leaked from stringified container: {out}"
        );
        assert!(out.contains("api_key"), "key name should stay: {out}");
    }

    #[test]
    fn redact_json_value_does_not_descend_into_strings() {
        use serde_json::json;
        let value = json!(r#"{"api_key":"sk-live"}"#);
        let redacted = redact_json_value(&value);
        // redact_json_value does NOT descend into strings, so the secret remains
        assert!(redacted.as_str().unwrap().contains("sk-live"));
    }

    #[test]
    fn redact_json_value_for_log_descends_into_strings() {
        use serde_json::json;
        let value = json!(r#"{"api_key":"sk-live"}"#);
        let redacted = redact_json_value_for_log(&value);
        // redact_json_value_for_log DOES descend into strings via value pattern matching
        assert!(!redacted.as_str().unwrap().contains("sk-live"));
    }

    #[test]
    fn redact_json_value_for_log_redacts_nested_stringified_json() {
        use serde_json::json;
        let value = json!({
            "config": r#"{"api_key":"sk-live","user":"bob"}"#,
            "plain": "text"
        });
        let redacted = redact_json_value_for_log(&value);
        assert!(!redacted["config"].as_str().unwrap().contains("sk-live"));
        assert_eq!(redacted["plain"], "text");
    }

    #[test]
    fn log_redaction_sanitizes_malformed_stringified_json() {
        let value = json!({"config": r#"{"password":"live-secret"#});
        let redacted = redact_json_value_for_log(&value);
        assert!(!redacted["config"].as_str().unwrap().contains("live-secret"));
    }

    #[test]
    fn redact_json_value_for_log_preserves_newlines_in_benign_strings() {
        let value = json!({"note": "line1\nline2"});
        let redacted = redact_json_value_for_log(&value);
        // Newlines are preserved but whitespace is normalized
        assert!(redacted["note"].as_str().unwrap().contains("line1"));
        assert!(redacted["note"].as_str().unwrap().contains("line2"));
    }

    #[test]
    fn redact_json_arg_caps_large_valid_json_payload() {
        use serde_json::json;
        let large_value = json!({
            "data": "x".repeat(3000),
            "nested": {
                "more": "y".repeat(3000)
            }
        });
        let large_json = serde_json::to_string(&large_value).unwrap();
        let redacted = redact_json_arg(&large_json);
        assert!(redacted.chars().count() <= LOG_ARG_MAX_CHARS);
        assert!(redacted.ends_with('…'));
    }

    #[test]
    fn redact_json_value_for_log_caps_nested_stringified_json() {
        use serde_json::json;
        let large_inner = json!({"data": "x".repeat(3000)});
        let value = json!({"config": large_inner.to_string()});
        let redacted = redact_json_value_for_log(&value);
        let redacted_str = redacted["config"].as_str().unwrap();
        assert!(redacted_str.chars().count() <= LOG_ARG_MAX_CHARS);
        assert!(redacted_str.ends_with('…'));
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

    #[test]
    fn demoted_macro_emits_at_info_level() {
        let (sender, receiver) = std::sync::mpsc::channel();
        let subscriber = TestSubscriber { sender };
        tracing::subscriber::with_default(subscriber, || {
            demoted!(target: "n00n-redact", "routine noise");
        });
        let (level, message) = receiver.recv().expect("event was emitted");
        assert_eq!(level, tracing::Level::INFO);
        assert_eq!(message, "routine noise");
    }

    struct TestSubscriber {
        sender: std::sync::mpsc::Sender<(tracing::Level, String)>,
    }

    impl tracing::Subscriber for TestSubscriber {
        fn enabled(&self, _metadata: &tracing::Metadata<'_>) -> bool {
            true
        }

        fn new_span(&self, _span: &tracing::span::Attributes<'_>) -> tracing::Id {
            tracing::Id::from_u64(1)
        }

        fn record(&self, _span: &tracing::Id, _values: &tracing::span::Record<'_>) {}

        fn record_follows_from(&self, _span: &tracing::Id, _follows: &tracing::Id) {}

        fn event(&self, event: &tracing::Event<'_>) {
            let mut visitor = MessageVisitor::default();
            event.record(&mut visitor);
            if let Some(message) = visitor.0 {
                self.sender
                    .send((*event.metadata().level(), message))
                    .expect("receiver is alive");
            }
        }

        fn enter(&self, _span: &tracing::Id) {}

        fn exit(&self, _span: &tracing::Id) {}
    }

    #[derive(Default)]
    struct MessageVisitor(Option<String>);

    impl tracing::field::Visit for MessageVisitor {
        fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
            if field.name() == "message" {
                self.0 = Some(value.to_owned());
            }
        }

        fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
            if field.name() == "message" {
                self.0 = Some(format!("{value:?}"));
            }
        }
    }
}

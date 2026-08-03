//! Word-level sanitizer for free-text activity descriptions and malformed
//! tool-argument JSON. Moved verbatim from the n00n-lua activity sanitizer so
//! its output stays byte-identical; `sanitize_text` just adds the caller's
//! character cap on top.

const REDACTED: &str = "[REDACTED]";

/// Substring-matched secret key fragments for free text, deliberately shorter
/// than the exact-match `SECRET_KEYS` list: free text has no key position to
/// anchor on, so matching stays aggressive to err toward redaction.
const SENSITIVE_KEY_FRAGMENTS: &[&str] = &[
    "apikey",
    "accesstoken",
    "authtoken",
    "token",
    "cookie",
    "credential",
    "refreshtoken",
    "authorization",
    "password",
    "passwd",
    "secret",
    "privatekey",
    "clientsecret",
];

pub(crate) const SECRET_TOKEN_PREFIXES: &[&str] = &[
    "sk-",
    "ghp_",
    "gho_",
    "ghu_",
    "ghs_",
    "ghr_",
    "github_pat_",
    "glpat-",
    "xoxb-",
    "xoxp-",
];

/// Sanitizes a free-text string: `Bearer <token>`, `key=value` / `key:value`
/// with sensitive keys, and token-shaped values are replaced with
/// `[REDACTED]`, then the result is capped at `max_chars` characters.
#[must_use]
pub fn sanitize_text(raw: &str, max_chars: usize) -> String {
    let words = raw.split_whitespace().collect::<Vec<_>>();
    let mut sanitized = Vec::with_capacity(words.len());
    let mut index = 0;
    while index < words.len() {
        let word = words[index];
        if is_authentication_scheme(word) {
            let scheme = if word.eq_ignore_ascii_case("basic") {
                "Basic"
            } else {
                "Bearer"
            };
            sanitized.push(format!("{scheme} {REDACTED}"));
            index = index.saturating_add(2);
            continue;
        }

        let separator = word.find(['=', ':']);
        let key = separator.map_or(word, |position| &word[..position]);
        if is_sensitive_key(key) || is_sensitive_key(word) {
            let separator_char =
                separator.map_or('=', |position| word.as_bytes()[position] as char);
            sanitized.push(format!("{key}{separator_char}{REDACTED}"));
            let inline_value = separator.and_then(|position| word.get(position + 1..));
            index += 1;
            if let Some(quote) = inline_value.and_then(unterminated_opening_quote) {
                while let Some(fragment) = words.get(index) {
                    index += 1;
                    if contains_unescaped_quote(fragment, quote) {
                        break;
                    }
                }
            } else if inline_value.is_some_and(is_authentication_scheme) {
                index = index.saturating_add(1).min(words.len());
            } else if inline_value.is_none_or(str::is_empty) {
                let next_is_separator = words
                    .get(index)
                    .is_some_and(|next| *next == "=" || *next == ":");
                let json_like_key = key.starts_with(['{', '[']);
                if separator.is_some() || next_is_separator || key.starts_with('-') || json_like_key
                {
                    if next_is_separator {
                        index += 1;
                    }
                    if words
                        .get(index)
                        .is_some_and(|next| is_authentication_scheme(next))
                    {
                        index += 1;
                    }
                    if let Some(quote) = words
                        .get(index)
                        .and_then(|value| unterminated_opening_quote(value))
                    {
                        index += 1;
                        while let Some(fragment) = words.get(index) {
                            index += 1;
                            if contains_unescaped_quote(fragment, quote) {
                                break;
                            }
                        }
                    } else if index < words.len() {
                        index += 1;
                    }
                }
            }
            continue;
        }

        let secret_value = separator.map_or(word, |position| &word[position + 1..]);
        let contains_secret_token = word
            .split(|character: char| !character.is_ascii_alphanumeric() && character != '-')
            .any(is_secret_token);
        if is_secret_token(secret_value) || contains_secret_token {
            let prefix = separator.map_or("", |position| &word[..=position]);
            sanitized.push(format!("{prefix}{REDACTED}"));
        } else {
            sanitized.push(word.to_owned());
        }
        index += 1;
    }
    truncate(&sanitized.join(" "), max_chars)
}

fn unterminated_opening_quote(value: &str) -> Option<char> {
    let quote = value
        .chars()
        .next()
        .filter(|character| matches!(character, '\'' | '"'))?;
    (!contains_unescaped_quote(&value[quote.len_utf8()..], quote)).then_some(quote)
}

fn is_authentication_scheme(value: &str) -> bool {
    value.eq_ignore_ascii_case("bearer") || value.eq_ignore_ascii_case("basic")
}

fn contains_unescaped_quote(value: &str, quote: char) -> bool {
    let mut escaped = false;
    for character in value.chars() {
        if character == quote && !escaped {
            return true;
        }
        escaped = character == '\\' && !escaped;
    }
    false
}

fn is_sensitive_key(value: &str) -> bool {
    let normalized = normalize_key(value);
    SENSITIVE_KEY_FRAGMENTS
        .iter()
        .any(|fragment| normalized.contains(fragment))
}

fn is_secret_token(value: &str) -> bool {
    let trimmed = value.trim_matches(|character: char| !character.is_ascii_alphanumeric());
    if super::is_jwt_like(trimmed) {
        return true;
    }
    let lower = trimmed.to_ascii_lowercase();
    SECRET_TOKEN_PREFIXES
        .iter()
        .filter(|prefix| **prefix != "sk-")
        .any(|prefix| lower.contains(prefix))
        || lower.match_indices("sk-").any(|(position, _)| {
            lower[..position]
                .chars()
                .next_back()
                .is_none_or(|character| !character.is_alphabetic())
        })
        || lower.starts_with("akia")
        || lower.starts_with("aiza")
        || super::is_jwt_like(&lower)
}

fn normalize_key(value: &str) -> String {
    value
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .flat_map(char::to_lowercase)
        .collect()
}

fn truncate(message: &str, max_chars: usize) -> String {
    if message.chars().count() <= max_chars {
        return message.to_owned();
    }
    let mut truncated: String = message.chars().take(max_chars.saturating_sub(1)).collect();
    truncated.push('…');
    truncated
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redacts_separated_credentials() {
        let sanitized = sanitize_text(
            "API_KEY = first Authorization: Bearer second --password third foo=sk-secret",
            80,
        );
        assert_eq!(
            sanitized,
            "API_KEY=[REDACTED] Authorization:[REDACTED] --password=[REDACTED] foo=[REDACTED]"
        );
    }

    #[test]
    fn redacts_adjacent_bearer_scheme_and_token() {
        let sanitized = sanitize_text("Authorization:Bearer visible-token trailing", 80);
        assert_eq!(sanitized, "Authorization:[REDACTED] trailing");
    }

    #[test]
    fn redacts_basic_auth_credentials_completely() {
        let sanitized = sanitize_text("Authorization: Basic dXNlcjpwYXNz trailing", 80);
        assert_eq!(sanitized, "Authorization:[REDACTED] trailing");
    }

    #[test]
    fn redacts_standalone_basic_auth_credentials() {
        let sanitized = sanitize_text("value Basic dXNlcjpwYXNz trailing", 80);
        assert_eq!(sanitized, "value Basic [REDACTED] trailing");
    }

    #[test]
    fn redacts_credentials_embedded_in_urls_and_tokens() {
        let sanitized = sanitize_text(
            "https://host.test/path?api_key=visible glpat-visible prefix-ghp_visible",
            80,
        );
        assert_eq!(sanitized, "https:[REDACTED] [REDACTED] [REDACTED]");
    }

    #[test]
    fn redacts_complete_multi_word_quoted_value_in_malformed_json() {
        let sanitized = sanitize_text(r#"{"password":"two words"#, 80);
        assert_eq!(sanitized, r#"{"password":[REDACTED]"#);
        assert!(!sanitized.contains("two"));
        assert!(!sanitized.contains("words"));
    }

    #[test]
    fn redacts_complete_multi_word_quoted_value_after_spaced_separator() {
        let sanitized = sanitize_text(r#"{"password": "two words""#, 80);
        assert_eq!(sanitized, r#"{"password":[REDACTED]"#);
        assert!(!sanitized.contains("two"));
        assert!(!sanitized.contains("words"));
    }

    #[test]
    fn consumes_quoted_value_after_json_like_bare_key() {
        let sanitized = sanitize_text(r#"{"password" "two words"}"#, 80);
        assert!(!sanitized.contains("two"));
        assert!(!sanitized.contains("words"));
    }

    #[test]
    fn preserves_word_after_bare_sensitive_term() {
        let sanitized = sanitize_text("check authorization header format", 80);
        assert_eq!(sanitized, "check authorization=[REDACTED] header format");
    }

    #[test]
    fn short_secret_prefix_requires_word_boundary() {
        let sanitized = sanitize_text("desk-top risk-taking sk-visible", 80);
        assert_eq!(sanitized, "desk-top risk-taking [REDACTED]");
    }

    #[test]
    fn redacts_secret_tokens_in_compact_malformed_json() {
        let sanitized = sanitize_text(r#"{"user":"bob","id":"AKIA0123456789ABCDEF""#, 200);
        assert!(!sanitized.contains("AKIA0123456789ABCDEF"));
    }
    #[test]
    fn redacts_jwt_in_malformed_json_text() {
        let jwt = format!("{}.{}.{}", "e".repeat(20), "aA1".repeat(6), "bB2".repeat(6));
        let sanitized = sanitize_text(&format!(r#"{{"note":"{jwt}"#), 200);
        assert!(!sanitized.contains(&jwt));
    }

    #[test]
    fn truncates_past_cap_with_ellipsis() {
        let long = "token=abc ".repeat(20);
        let sanitized = sanitize_text(&long, 50);
        assert!(sanitized.chars().count() <= 50);
        assert!(sanitized.ends_with('…'));
    }

    #[test]
    fn leaves_benign_text_untouched() {
        let sanitized = sanitize_text("searching for needle in src/main.rs", 80);
        assert_eq!(sanitized, "searching for needle in src/main.rs");
    }
}

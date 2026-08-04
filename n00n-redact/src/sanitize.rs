//! Word-level sanitizer for free-text activity descriptions and malformed
//! tool-argument JSON. Moved verbatim from the n00n-lua activity sanitizer so
//! its output stays byte-identical; `sanitize_text` just adds the caller's
//! character cap on top.

use crate::REDACTED;

/// Minimum length for a bare `Basic` or `Bearer` value before it is treated as
/// an HTTP authentication credential. Shorter or non-base64-ish values are
/// left alone to avoid over-redacting common prose (`the basic idea`,
/// `the bearer of bad news`).
const AUTH_CREDENTIAL_MIN_CHARS: usize = 9;

/// Substring-matched secret key fragments for free text, deliberately shorter
/// than the exact-match `SECRET_KEYS` list: free text has no key position to
/// anchor on, so matching stays aggressive to err toward redaction.
const SENSITIVE_KEY_FRAGMENTS: &[&str] = &[
    "apikey",
    "accesstoken",
    "authtoken",
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
/// `[redacted]`, then the result is capped at `max_chars` characters.
/// Whitespace is collapsed to single spaces, so this is suitable for
/// one-line activity messages and compact log previews.
#[must_use]
pub fn sanitize_text(raw: &str, max_chars: usize) -> String {
    let words = raw.split_whitespace().collect::<Vec<_>>();
    let sanitized = sanitize_words(&words)
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join(" ");
    truncate(&sanitized, max_chars)
}

/// Like `sanitize_text`, but preserves line breaks by sanitizing across
/// line boundaries while preserving each original line separator (handles
/// `\n`, `\r\n`, `\r`). Use this for multiline log output and JSON string
/// values where newlines should stay intact.
#[must_use]
pub(crate) fn sanitize_text_preserve_newlines(raw: &str, max_chars: usize) -> String {
    // Tokenize into words and line separators, preserving exact separator positions
    let tokens = tokenize_with_line_breaks(raw);
    let sanitized = sanitize_tokens(tokens);
    truncate(&sanitized, max_chars)
}

#[derive(Clone, Copy)]
enum Token<'a> {
    Word(&'a str),
    LineBreak(&'a str),
}

/// Tokenizes text into words and line separators, preserving exact positions.
/// Line separators (\n, \r\n, \r) are emitted as separate tokens.
fn tokenize_with_line_breaks(text: &str) -> Vec<Token<'_>> {
    let mut tokens = Vec::new();
    let mut word_start = None;
    let mut i = 0;
    let bytes = text.as_bytes();

    while i < bytes.len() {
        let c = bytes[i] as char;
        if c == '\r' {
            // Flush any pending word
            if let Some(start) = word_start {
                tokens.push(Token::Word(&text[start..i]));
                word_start = None;
            }
            // Check for \r\n
            if i + 1 < bytes.len() && bytes[i + 1] == b'\n' {
                tokens.push(Token::LineBreak("\r\n"));
                i += 2;
            } else {
                tokens.push(Token::LineBreak("\r"));
                i += 1;
            }
        } else if c == '\n' {
            // Flush any pending word
            if let Some(start) = word_start {
                tokens.push(Token::Word(&text[start..i]));
                word_start = None;
            }
            tokens.push(Token::LineBreak("\n"));
            i += 1;
        } else if c.is_whitespace() {
            // Flush any pending word on whitespace
            if let Some(start) = word_start {
                tokens.push(Token::Word(&text[start..i]));
                word_start = None;
            }
            i += 1;
        } else {
            // Non-whitespace character
            if word_start.is_none() {
                word_start = Some(i);
            }
            i += 1;
        }
    }

    // Flush final word
    if let Some(start) = word_start {
        tokens.push(Token::Word(&text[start..]));
    }

    tokens
}

/// Sanitizes a token stream, preserving line break positions.
/// Reuses `sanitize_words` for the actual redaction logic.
fn sanitize_tokens(tokens: Vec<Token<'_>>) -> String {
    // Collect word tokens for sanitization
    let words: Vec<&str> = tokens
        .iter()
        .filter_map(|t| match t {
            Token::Word(w) => Some(*w),
            Token::LineBreak(_) => None,
        })
        .collect();

    // Sanitize all words as a single block to preserve state across line boundaries.
    // Each entry lines up with the corresponding input word (None means it was
    // consumed as part of an earlier redaction).
    let sanitized = sanitize_words(&words);

    // Reconstruct output, replacing word tokens with their sanitized counterpart
    let mut result = String::new();
    let mut word_index = 0;
    let mut at_line_start = true;

    for token in tokens {
        match token {
            Token::Word(_) => {
                if let Some(Some(word)) = sanitized.get(word_index) {
                    if !at_line_start && !result.is_empty() {
                        result.push(' ');
                    }
                    result.push_str(word);
                    at_line_start = false;
                }
                word_index += 1;
            }
            Token::LineBreak(separator) => {
                result.push_str(separator);
                at_line_start = true;
            }
        }
    }

    result
}

/// Returns a sanitized word for each input word. `None` means the word was
/// consumed as part of an earlier redaction (e.g. the value after a secret key).
fn sanitize_words(words: &[&str]) -> Vec<Option<String>> {
    let mut result = vec![None; words.len()];
    let mut index = 0;
    while index < words.len() {
        let word = words[index];
        if is_authentication_scheme(word)
            && words
                .get(index + 1)
                .is_some_and(|next| is_auth_credential_value(next))
        {
            let scheme = if word.eq_ignore_ascii_case("basic") {
                "Basic"
            } else {
                "Bearer"
            };
            result[index] = Some(format!("{scheme} {REDACTED}"));
            index = index.saturating_add(2);
            continue;
        }

        let separator = word.find(['=', ':']);
        let key = separator.map_or(word, |position| &word[..position]);
        if is_sensitive_key(key) || is_sensitive_key(word) {
            let separator_char =
                separator.map_or('=', |position| word.as_bytes()[position] as char);
            result[index] = Some(format!("{key}{separator_char}{REDACTED}"));
            let inline_value = separator.and_then(|position| word.get(position + 1..));
            index += 1;
            if let Some(quote) = inline_value.and_then(unterminated_opening_quote) {
                while let Some(fragment) = words.get(index) {
                    result[index] = None;
                    index += 1;
                    if contains_unescaped_quote(fragment, quote) {
                        break;
                    }
                }
            } else if inline_value.is_some_and(is_authentication_scheme) {
                if index < words.len() {
                    result[index] = None;
                }
                index = index.saturating_add(1).min(words.len());
            } else if inline_value.is_none_or(str::is_empty) {
                let next_is_separator = words
                    .get(index)
                    .is_some_and(|next| *next == "=" || *next == ":");
                let json_like_key = key.starts_with(['{', '[']);
                if separator.is_some() || next_is_separator || key.starts_with('-') || json_like_key
                {
                    if next_is_separator {
                        result[index] = None;
                        index += 1;
                    }
                    if words
                        .get(index)
                        .is_some_and(|next| is_authentication_scheme(next))
                    {
                        result[index] = None;
                        index += 1;
                    }
                    if let Some(quote) = words
                        .get(index)
                        .and_then(|value| unterminated_opening_quote(value))
                    {
                        result[index] = None;
                        index += 1;
                        while let Some(fragment) = words.get(index) {
                            result[index] = None;
                            index += 1;
                            if contains_unescaped_quote(fragment, quote) {
                                break;
                            }
                        }
                    } else if index < words.len() {
                        result[index] = None;
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
            result[index] = Some(format!("{prefix}{REDACTED}"));
        } else {
            result[index] = Some(word.to_owned());
        }
        index += 1;
    }
    result
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

fn is_auth_credential_value(value: &str) -> bool {
    is_secret_token(value) || is_basic_auth_credential(value)
}

fn is_basic_auth_credential(value: &str) -> bool {
    let trimmed = value.trim_matches(|character: char| {
        !character.is_ascii_alphanumeric()
            && character != '+'
            && character != '/'
            && character != '='
    });
    let has_mixed_case = trimmed.chars().any(|c| c.is_ascii_uppercase())
        && trimmed.chars().any(|c| c.is_ascii_lowercase());
    let has_base64_specific = trimmed
        .chars()
        .any(|character| character.is_ascii_digit() || "+/=".contains(character));
    trimmed.len() > AUTH_CREDENTIAL_MIN_CHARS
        && has_mixed_case
        && has_base64_specific
        && trimmed.chars().all(|character| {
            character.is_ascii_alphanumeric()
                || character == '+'
                || character == '/'
                || character == '='
        })
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
    if super::SECRET_KEYS.contains(&normalized.as_str()) {
        return true;
    }
    let tokens = crate::words(value);
    SENSITIVE_KEY_FRAGMENTS.iter().any(|fragment| {
        for start in 0..tokens.len() {
            let mut combined = String::with_capacity(fragment.len());
            for token in &tokens[start..] {
                combined.push_str(token);
                if combined.ends_with(fragment) {
                    return true;
                }
            }
        }
        false
    })
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
        || super::is_aws_access_key_id(trimmed)
        || lower.starts_with("aiza")
}

fn normalize_key(value: &str) -> String {
    value
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .flat_map(char::to_lowercase)
        .collect()
}

pub(crate) fn truncate(message: &str, max_chars: usize) -> String {
    if max_chars == 0 {
        return String::new();
    }
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
            "API_KEY=[redacted] Authorization:[redacted] --password=[redacted] foo=[redacted]"
        );
    }

    #[test]
    fn redacts_adjacent_bearer_scheme_and_token() {
        let sanitized = sanitize_text("Authorization:Bearer visible-token trailing", 80);
        assert_eq!(sanitized, "Authorization:[redacted] trailing");
    }

    #[test]
    fn redacts_basic_auth_credentials_completely() {
        let sanitized = sanitize_text("Authorization: Basic dXNlcjpwYXNzd29yZA== trailing", 80);
        assert_eq!(sanitized, "Authorization:[redacted] trailing");
    }

    #[test]
    fn redacts_standalone_basic_auth_credentials() {
        let sanitized = sanitize_text("value Basic dXNlcjpwYXNzd29yZA== trailing", 80);
        assert_eq!(sanitized, "value Basic [redacted] trailing");
    }

    #[test]
    fn redacts_credentials_embedded_in_urls_and_tokens() {
        let sanitized = sanitize_text(
            "https://host.test/path?api_key=visible glpat-visible prefix-ghp_visible",
            80,
        );
        assert_eq!(sanitized, "https:[redacted] [redacted] [redacted]");
    }

    #[test]
    fn redacts_complete_multi_word_quoted_value_in_malformed_json() {
        let sanitized = sanitize_text(r#"{"password":"two words"#, 80);
        assert_eq!(sanitized, r#"{"password":[redacted]"#);
        assert!(!sanitized.contains("two"));
        assert!(!sanitized.contains("words"));
    }

    #[test]
    fn redacts_complete_multi_word_quoted_value_after_spaced_separator() {
        let sanitized = sanitize_text(r#"{"password": "two words""#, 80);
        assert_eq!(sanitized, r#"{"password":[redacted]"#);
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
        assert_eq!(sanitized, "check authorization=[redacted] header format");
    }

    #[test]
    fn preserves_secretary_as_an_ordinary_word() {
        let sanitized = sanitize_text("ask the secretary for the schedule", 80);
        assert_eq!(sanitized, "ask the secretary for the schedule");
    }

    #[test]
    fn preserves_passwordless_as_an_ordinary_word() {
        let sanitized = sanitize_text("this endpoint uses passwordless login", 80);
        assert_eq!(sanitized, "this endpoint uses passwordless login");
    }

    #[test]
    fn redacts_separator_split_compound_secret_keys() {
        let sanitized = sanitize_text("client_secret=abc123 clientSecret=xyz789", 80);
        assert!(!sanitized.contains("abc123"));
        assert!(!sanitized.contains("xyz789"));
    }

    #[test]
    fn redacts_unseparated_compound_ending_in_a_sensitive_fragment() {
        let sanitized = sanitize_text("vaultsecret=abc123", 80);
        assert!(!sanitized.contains("abc123"));
    }

    #[test]
    fn short_secret_prefix_requires_word_boundary() {
        let sanitized = sanitize_text("desk-top risk-taking sk-visible", 80);
        assert_eq!(sanitized, "desk-top risk-taking [redacted]");
    }

    #[test]
    fn redacts_secret_tokens_in_compact_malformed_json() {
        let sanitized = sanitize_text(r#"{"user":"bob","id":"AKIA0123456789ABCDEF""#, 200);
        assert!(!sanitized.contains("AKIA0123456789ABCDEF"));
    }

    #[test]
    fn redacts_asia_access_key_id_in_compact_malformed_json() {
        let sanitized = sanitize_text(r#"{"user":"bob","id":"ASIA0123456789ABCDEF""#, 200);
        assert!(!sanitized.contains("ASIA0123456789ABCDEF"));
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

    #[test]
    fn preserves_basic_in_prose() {
        let sanitized = sanitize_text("the basic idea is simple", 80);
        assert_eq!(sanitized, "the basic idea is simple");
    }

    #[test]
    fn preserves_standalone_basic_prose_of_eight_or_more_letters() {
        let sanitized = sanitize_text("value Basic training is required", 80);
        assert_eq!(sanitized, "value Basic training is required");
    }

    #[test]
    fn preserves_bearer_in_prose() {
        let sanitized = sanitize_text("the bearer of bad news", 80);
        assert_eq!(sanitized, "the bearer of bad news");
    }

    #[test]
    fn preserves_token_substrings_in_prose() {
        let sanitized = sanitize_text("use max_tokens and tokenizer tokens carefully", 80);
        assert_eq!(sanitized, "use max_tokens and tokenizer tokens carefully");
    }

    #[test]
    fn redacts_sensitive_key_value_split_across_lines() {
        let input = "password:\nplain-value";
        let sanitized = sanitize_text_preserve_newlines(input, 200);
        assert!(sanitized.contains("[redacted]"));
        assert!(!sanitized.contains("plain-value"));
    }

    #[test]
    fn redacts_sensitive_key_value_split_across_lines_with_colon() {
        let input = "password:\nplain-value";
        let sanitized = sanitize_text_preserve_newlines(input, 200);
        assert!(sanitized.contains("[redacted]"));
        assert!(!sanitized.contains("plain-value"));
    }

    #[test]
    fn preserves_lf_line_breaks() {
        let input = "line1\nline2\nline3";
        let sanitized = sanitize_text_preserve_newlines(input, 200);
        assert_eq!(sanitized, "line1\nline2\nline3");
    }

    #[test]
    fn preserves_crlf_line_breaks() {
        let input = "line1\r\nline2\r\nline3";
        let sanitized = sanitize_text_preserve_newlines(input, 200);
        assert_eq!(sanitized, "line1\r\nline2\r\nline3");
    }

    #[test]
    fn preserves_cr_line_breaks() {
        let input = "line1\rline2\rline3";
        let sanitized = sanitize_text_preserve_newlines(input, 200);
        assert_eq!(sanitized, "line1\rline2\rline3");
    }

    #[test]
    fn preserves_leading_line_breaks() {
        let input = "\nline1\nline2";
        let sanitized = sanitize_text_preserve_newlines(input, 200);
        assert_eq!(sanitized, "\nline1\nline2");
    }

    #[test]
    fn preserves_trailing_line_breaks() {
        let input = "line1\nline2\n";
        let sanitized = sanitize_text_preserve_newlines(input, 200);
        assert_eq!(sanitized, "line1\nline2\n");
    }

    #[test]
    fn preserves_consecutive_line_breaks() {
        let input = "line1\n\n\nline2";
        let sanitized = sanitize_text_preserve_newlines(input, 200);
        assert_eq!(sanitized, "line1\n\n\nline2");
    }

    #[test]
    fn preserves_mixed_line_endings() {
        let input = "line1\nline2\r\nline3\rline4";
        let sanitized = sanitize_text_preserve_newlines(input, 200);
        assert_eq!(sanitized, "line1\nline2\r\nline3\rline4");
    }

    #[test]
    fn redacts_key_value_across_crlf() {
        let input = "password:\r\nsecret123";
        let sanitized = sanitize_text_preserve_newlines(input, 200);
        assert!(sanitized.contains("[redacted]"));
        assert!(!sanitized.contains("secret123"));
        assert!(sanitized.contains("\r\n"));
    }

    #[test]
    fn redacts_key_value_across_cr() {
        let input = "password:\rsecret123";
        let sanitized = sanitize_text_preserve_newlines(input, 200);
        assert!(sanitized.contains("[redacted]"));
        assert!(!sanitized.contains("secret123"));
        assert!(sanitized.contains('\r'));
    }

    #[test]
    fn preserves_empty_lines() {
        let input = "line1\n\nline2";
        let sanitized = sanitize_text_preserve_newlines(input, 200);
        assert_eq!(sanitized, "line1\n\nline2");
    }

    #[test]
    fn handles_text_with_only_line_breaks() {
        let input = "\n\n\n";
        let sanitized = sanitize_text_preserve_newlines(input, 200);
        assert_eq!(sanitized, "\n\n\n");
    }

    #[test]
    fn redacts_bearer_token_across_line_break() {
        let input = "Authorization:\nBearer sk-secret";
        let sanitized = sanitize_text_preserve_newlines(input, 200);
        assert!(sanitized.contains("[redacted]"));
        assert!(!sanitized.contains("sk-secret"));
    }

    #[test]
    fn redacts_key_value_and_keeps_remaining_words_on_their_lines() {
        let input = "password:\nfoo bar\r\nbaz";
        let sanitized = sanitize_text_preserve_newlines(input, 200);
        assert_eq!(sanitized, "password:[redacted]\nbar\r\nbaz");
    }

    #[test]
    fn redacts_password_across_lf_crlf_cr() {
        let input = "password=secret\nline2\r\nline3\rline4";
        let sanitized = sanitize_text_preserve_newlines(input, 200);
        assert!(sanitized.contains("password=[redacted]"));
        assert!(!sanitized.contains("secret"));
        assert!(sanitized.contains("\nline2\r\nline3\rline4"));
    }
}

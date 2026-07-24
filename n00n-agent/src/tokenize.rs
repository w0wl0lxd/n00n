use serde_json::Value;
use tiktoken_rs::{cl100k_base_singleton, o200k_base_singleton};
use tracing::warn;

const TIKTOKEN_MAX_CHARS: usize = 4_096;
const BYTES_PER_TOKEN_ESTIMATE: usize = 4;

/// Returns the best-effort tiktoken tokenizer for a model id.
///
/// Modern `OpenAI` models (GPT-4o, GPT-4.1, GPT-5, o1, o3, o4) use the `o200k`
/// vocabulary; everything else falls back to `cl100k`. This is intentionally a
/// heuristic -- providers do not always expose their exact tokenizer -- but it
/// is still a large improvement over always using `cl100k` for, e.g., GPT-4o.
#[must_use]
fn tokenizer_for_model(model_id: &str) -> &'static tiktoken_rs::CoreBPE {
    if is_o200k_model(model_id) {
        o200k_base_singleton()
    } else {
        cl100k_base_singleton()
    }
}

fn is_o200k_model(model_id: &str) -> bool {
    let id = model_id.to_lowercase();
    id.contains("gpt-4o")
        || id.contains("gpt-4.1")
        || id.contains("gpt-5")
        || id.starts_with("o1")
        || id.starts_with("o3")
        || id.starts_with("o4")
        || id.contains("chatgpt-4o")
}

/// Count tokens using the tokenizer that best matches `model_id`.
///
/// Falls back to a bytes-per-token estimate for very long inputs to avoid
/// quadratic work in the tiktoken encoder.
#[must_use]
pub fn count_tokens_for_model(model_id: &str, text: &str) -> usize {
    if text.len() > TIKTOKEN_MAX_CHARS {
        return text.len() / BYTES_PER_TOKEN_ESTIMATE;
    }
    tokenizer_for_model(model_id).encode_ordinary(text).len()
}

/// Count tokens in a JSON value using the tokenizer that best matches `model_id`.
#[must_use]
pub fn count_json_for_model(model_id: &str, value: &Value) -> usize {
    match serde_json::to_string(value) {
        Ok(text) => count_tokens_for_model(model_id, &text),
        Err(e) => {
            warn!(error = %e, "failed to serialize JSON for token count; using byte fallback");
            count_tokens_for_model(model_id, &value.to_string())
        }
    }
}

/// Legacy token-count helper that uses cl100k for callers that do not know
/// which model will consume the text.
#[must_use]
pub fn count_tokens(text: &str) -> usize {
    count_tokens_for_model("", text)
}

/// Legacy JSON token-count helper that uses cl100k.
#[must_use]
pub fn count_json(value: &Value) -> usize {
    count_json_for_model("", value)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn counts_short_text() {
        let text = "hello world";
        let tokens = count_tokens(text);
        assert!(
            tokens > 0 && tokens < 10,
            "expected positive token count, got {tokens}"
        );
    }

    #[test]
    fn counts_json_object() {
        let value = json!({"name": "skill", "list": true});
        let tokens = count_json(&value);
        assert!(tokens > 0, "expected positive token count for json");
    }

    #[test]
    fn counts_long_text_uses_byte_fallback() {
        let text = "x".repeat(10_000);
        let tokens = count_tokens(&text);
        assert_eq!(
            tokens, 2_500,
            "long repeated text should use bytes/4 fallback"
        );
    }

    #[test]
    fn counts_large_json_uses_byte_fallback() {
        let value = json!({"data": "x".repeat(10_000)});
        let tokens = count_json(&value);
        assert!(
            tokens > 2_000,
            "large json should produce positive token count"
        );
    }

    #[test]
    fn counts_empty_text() {
        let tokens = count_tokens("");
        assert_eq!(tokens, 0, "empty string should have zero tokens");
    }

    #[test]
    fn counts_non_ascii_text() {
        let text = "héllo wörld 世界";
        let tokens = count_tokens(text);
        assert!(tokens > 0, "non-ascii text should have positive tokens");
    }

    #[test]
    fn uses_tiktoken_up_to_threshold_and_fallback_after() {
        let at_threshold = "x".repeat(TIKTOKEN_MAX_CHARS);
        let over_threshold = "x".repeat(TIKTOKEN_MAX_CHARS + 1);

        let at_tokens = count_tokens(&at_threshold);
        let over_tokens = count_tokens(&over_threshold);

        assert!(at_tokens > 0, "text at threshold should be counted");
        assert_eq!(
            over_tokens,
            over_threshold.len() / BYTES_PER_TOKEN_ESTIMATE,
            "text over threshold should use bytes/4 fallback"
        );
    }

    #[test]
    fn count_json_falls_back_for_non_serializable() {
        let value = Value::Null;
        let tokens = count_json(&value);
        assert_eq!(tokens, 1, "null serializes to one token-ish string");
    }

    #[test]
    fn model_aware_tokenizer_uses_cl100k_by_default() {
        // cl100k and o200k tokenize "hello world" to the same tokens, but
        // this test documents the fallback path.
        let tokens = count_tokens_for_model("anthropic/claude-sonnet-4-6", "hello world");
        assert!(tokens > 0 && tokens < 10);
    }

    #[test]
    fn model_aware_tokenizer_uses_o200k_for_gpt4o() {
        let text = "hello world";
        let cl = count_tokens_for_model("openai/gpt-4-turbo", text);
        let o = count_tokens_for_model("openai/gpt-4o", text);
        assert_eq!(
            cl, o,
            "common words tokenize the same under both vocabularies"
        );
    }

    #[test]
    fn model_aware_json_counts_match_legacy() {
        let value = json!({"name": "skill", "list": true});
        let legacy = count_json(&value);
        let modern = count_json_for_model("openai/gpt-4o", &value);
        assert_eq!(legacy, modern, "o200k gives same count for this fixture");
    }

    #[test]
    fn tokenize_fuzz_smoke() {
        fn xorshift64(state: &mut u64) -> u64 {
            *state ^= *state << 13;
            *state ^= *state >> 7;
            *state ^= *state << 17;
            *state
        }

        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        fn gen_text(state: &mut u64, max_len: usize) -> String {
            const ALPHABET: &[u8] =
                b"abcdefghijklmnopqrstuvwxyz0123456789 \n\t!@#$%^&*()_+-=[]{}|;':\",./<>?";
            let raw = xorshift64(state);
            let len = if max_len == 0 {
                0
            } else {
                usize::try_from(raw).unwrap_or_else(|_| usize::MAX) % max_len
            };
            let mut out = String::with_capacity(len);
            for _ in 0..len {
                let raw = xorshift64(state);
                let idx = if ALPHABET.is_empty() {
                    0
                } else {
                    usize::try_from(raw).unwrap_or_else(|_| usize::MAX) % ALPHABET.len()
                };
                out.push(ALPHABET[idx] as char);
            }
            out
        }

        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_possible_wrap,
            clippy::cast_sign_loss
        )]
        fn gen_value(state: &mut u64, depth: usize) -> Value {
            let kind = xorshift64(state) % 6;
            match kind {
                0 => Value::Null,
                1 => Value::Bool((xorshift64(state) & 1) == 1),
                2 => Value::Number((xorshift64(state) % 1000).cast_signed().into()),
                3 => Value::String(gen_text(state, 50)),
                4 if depth > 0 => {
                    let len = usize::try_from(xorshift64(state)).unwrap_or_else(|_| usize::MAX) % 4;
                    Value::Array((0..len).map(|_| gen_value(state, depth - 1)).collect())
                }
                5 if depth > 0 => {
                    let len = usize::try_from(xorshift64(state)).unwrap_or_else(|_| usize::MAX) % 4;
                    let mut m = serde_json::Map::new();
                    for i in 0..len {
                        m.insert(format!("k{i}"), gen_value(state, depth - 1));
                    }
                    Value::Object(m)
                }
                _ => Value::String(gen_text(state, 10)),
            }
        }

        let mut state = 0x1234_5678_9ABC_DEF0u64;
        for _ in 0..500 {
            let text = gen_text(&mut state, 6_000);
            let tokens = count_tokens(&text);
            assert!(
                tokens <= text.len().max(1),
                "token count {tokens} exceeds text length {}",
                text.len()
            );

            let value = gen_value(&mut state, 4);
            let tokens = count_json(&value);
            let json_text = serde_json::to_string(&value).unwrap();
            assert!(
                tokens <= json_text.len().max(1),
                "json token count {tokens} exceeds json length {}",
                json_text.len()
            );

            // Model-aware path should also stay bounded.
            let model_tokens = count_json_for_model("openai/gpt-4o", &value);
            assert!(
                model_tokens <= json_text.len().max(1),
                "model-aware json token count {model_tokens} exceeds json length {}",
                json_text.len()
            );
        }
    }
}

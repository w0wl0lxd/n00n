use serde_json::Value;
use tiktoken_rs::cl100k_base_singleton;

const TIKTOKEN_MAX_CHARS: usize = 4_096;
const BYTES_PER_TOKEN_ESTIMATE: usize = 4;

#[must_use]
pub fn count_tokens(text: &str) -> usize {
    if text.len() > TIKTOKEN_MAX_CHARS {
        return text.len() / BYTES_PER_TOKEN_ESTIMATE;
    }
    cl100k_base_singleton().encode_ordinary(text).len()
}

#[must_use]
pub fn count_json(value: &Value) -> usize {
    match serde_json::to_string(value) {
        Ok(text) => count_tokens(&text),
        Err(_) => 0,
    }
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
}

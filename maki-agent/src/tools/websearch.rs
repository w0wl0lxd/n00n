use std::env;
use std::fmt::Write;
use std::io::Read;
use std::time::Duration;

use maki_tool_macro::Tool;
use serde_json::{Value, json};
use ureq::Agent;

use maki_providers::{ToolInput, ToolOutput};

use super::MAX_RESPONSE_BYTES;
use super::truncate_output;

const EXA_API_ENDPOINT: &str = "https://api.exa.ai/search";
const EXA_API_KEY_ENV: &str = "EXA_API_KEY";
const REQUEST_TIMEOUT_SECS: u64 = 25;
const DEFAULT_NUM_RESULTS: u64 = 8;
const HIGHLIGHT_MAX_CHARS: u64 = 4000;

#[derive(Tool, Debug, Clone)]
pub struct WebSearch {
    #[param(description = "Search query")]
    query: String,
    #[param(description = "Number of results to return (default 8)")]
    num_results: Option<u64>,
}

impl WebSearch {
    pub const NAME: &str = "websearch";
    pub const DESCRIPTION: &str = include_str!("websearch.md");
    pub const EXAMPLES: Option<&str> = Some(
        r#"[
  {"query": "rust tokio spawn blocking best practices"},
  {"query": "serde deserialize enum tag", "num_results": 5}
]"#,
    );

    pub fn execute(&self, _ctx: &super::ToolContext) -> Result<ToolOutput, String> {
        let api_key =
            env::var(EXA_API_KEY_ENV).map_err(|_| format!("{EXA_API_KEY_ENV} not set"))?;

        let num_results = self.num_results.unwrap_or(DEFAULT_NUM_RESULTS);

        let payload = json!({
            "query": self.query,
            "numResults": num_results,
            "type": "auto",
            "contents": {
                "highlights": {
                    "maxCharacters": HIGHLIGHT_MAX_CHARS
                }
            }
        });

        let agent: Agent = Agent::config_builder()
            .http_status_as_error(false)
            .timeout_global(Some(Duration::from_secs(REQUEST_TIMEOUT_SECS)))
            .build()
            .into();

        let body = serde_json::to_string(&payload).map_err(|e| format!("serialize: {e}"))?;

        let response = agent
            .post(EXA_API_ENDPOINT)
            .header("Content-Type", "application/json")
            .header("Accept", "application/json")
            .header("x-api-key", &api_key)
            .send(body.as_str())
            .map_err(|e| format!("request failed: {e}"))?;

        let status = response.status().as_u16();
        let mut body = String::new();
        response
            .into_body()
            .into_reader()
            .take(MAX_RESPONSE_BYTES as u64)
            .read_to_string(&mut body)
            .map_err(|e| format!("read error: {e}"))?;

        if !(200..300).contains(&status) {
            return Err(format!("HTTP {status}: {}", &body[..body.len().min(200)]));
        }

        let parsed: Value =
            serde_json::from_str(&body).map_err(|e| format!("JSON parse error: {e}"))?;

        let text = format_results(&parsed)?;
        Ok(ToolOutput::Plain(truncate_output(text)))
    }

    pub fn start_summary(&self) -> String {
        self.query.clone()
    }

    pub fn start_input(&self) -> Option<ToolInput> {
        None
    }

    pub fn start_output(&self) -> Option<ToolOutput> {
        None
    }

    pub fn mutable_path(&self) -> Option<&str> {
        None
    }
}

fn format_results(response: &Value) -> Result<String, String> {
    let results = response["results"]
        .as_array()
        .ok_or("missing results array")?;

    if results.is_empty() {
        return Ok("No results found".into());
    }

    let mut out = String::new();
    for (i, r) in results.iter().enumerate() {
        if i > 0 {
            out.push('\n');
        }
        let title = r["title"].as_str().unwrap_or("Untitled");
        let url = r["url"].as_str().unwrap_or("");
        let _ = writeln!(out, "Title: {title}");
        let _ = writeln!(out, "URL: {url}");

        if let Some(highlights) = r["highlights"].as_array() {
            for h in highlights {
                if let Some(s) = h.as_str() {
                    let _ = writeln!(out, "{s}");
                }
            }
        }
    }

    Ok(out.trim_end().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use test_case::test_case;

    fn make_response(results: Value) -> Value {
        json!({ "results": results })
    }

    const NO_RESULTS_MSG: &str = "No results found";
    const MISSING_ARRAY_MSG: &str = "missing results array";

    #[test]
    fn format_results_happy_path() {
        let resp = make_response(json!([
            { "title": "Rust Lang", "url": "https://rust-lang.org", "highlights": ["Fast and safe", "Memory safety"] },
            { "title": "Docs", "url": "https://doc.rust-lang.org", "highlights": ["API reference"] }
        ]));
        let out = format_results(&resp).unwrap();
        assert!(out.contains("Title: Rust Lang"));
        assert!(out.contains("URL: https://rust-lang.org"));
        assert!(out.contains("Fast and safe"));
        assert!(out.contains("Memory safety"));
        assert!(out.contains("Title: Docs"));
        assert!(out.contains("API reference"));
        let first = out.find("Title: Rust Lang").unwrap();
        let second = out.find("Title: Docs").unwrap();
        assert!(first < second);
    }

    #[test_case(make_response(json!([])), Ok(NO_RESULTS_MSG.into()) ; "empty_results")]
    #[test_case(json!({}), Err(MISSING_ARRAY_MSG.into()) ; "missing_results_key")]
    #[test_case(json!({"results": "bad"}), Err(MISSING_ARRAY_MSG.into()) ; "results_not_array")]
    #[test_case(make_response(json!([{"other": "data"}])), Ok("Title: Untitled\nURL:".into()) ; "missing_fields_uses_defaults")]
    fn format_results_edge_cases(input: Value, expected: Result<String, String>) {
        assert_eq!(format_results(&input), expected);
    }
}

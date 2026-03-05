use std::io::Read;
use std::time::Duration;

use maki_tool_macro::Tool;
use serde_json::{Value, json};
use ureq::Agent;

use crate::{ToolInput, ToolOutput};

use super::MAX_RESPONSE_BYTES;
use super::truncate_output;

const EXA_MCP_ENDPOINT: &str = "https://mcp.exa.ai/mcp";
const REQUEST_TIMEOUT_SECS: u64 = 25;
const DEFAULT_NUM_RESULTS: u64 = 8;
const NO_RESULTS_MSG: &str = "No search results found";

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
        let num_results = self.num_results.unwrap_or(DEFAULT_NUM_RESULTS);

        let payload = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {
                "name": "web_search_exa",
                "arguments": {
                    "query": self.query,
                    "numResults": num_results,
                    "type": "auto",
                    "livecrawl": "fallback",
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
            .post(EXA_MCP_ENDPOINT)
            .header("Content-Type", "application/json")
            .header("Accept", "application/json, text/event-stream")
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

        let text = parse_sse_response(&body)?;
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

fn parse_sse_response(body: &str) -> Result<String, String> {
    for line in body.lines() {
        let Some(data) = line.strip_prefix("data: ") else {
            continue;
        };
        let parsed: Value =
            serde_json::from_str(data).map_err(|e| format!("SSE JSON parse error: {e}"))?;

        if let Some(text) = parsed
            .pointer("/result/content")
            .and_then(Value::as_array)
            .and_then(|arr| arr.first())
            .and_then(|item| item["text"].as_str())
            && !text.is_empty()
        {
            return Ok(text.to_string());
        }
    }
    Ok(NO_RESULTS_MSG.into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use test_case::test_case;

    fn sse_line(result_json: &Value) -> String {
        format!("data: {result_json}")
    }

    fn make_sse(text: &str) -> String {
        sse_line(&json!({
            "jsonrpc": "2.0",
            "result": {
                "content": [{"type": "text", "text": text}]
            }
        }))
    }

    #[test]
    fn parse_sse_response_extracts_text() {
        let body = format!(
            "event: message\n{}\n",
            make_sse("Rust is a systems language")
        );
        let result = parse_sse_response(&body).unwrap();
        assert_eq!(result, "Rust is a systems language");
    }

    #[test]
    fn parse_sse_response_first_data_line_wins() {
        let body = format!("{}\n{}\n", make_sse("first"), make_sse("second"));
        assert_eq!(parse_sse_response(&body).unwrap(), "first");
    }

    #[test_case(""                              ; "empty_body")]
    #[test_case(&sse_line(&json!({"result": {"content": []}})) ; "empty_content_array")]
    #[test_case(&sse_line(&json!({"result": {}})) ; "missing_content_key")]
    fn parse_sse_response_no_results(input: &str) {
        assert_eq!(parse_sse_response(input).unwrap(), NO_RESULTS_MSG);
    }

    #[test]
    fn parse_sse_response_empty_text_falls_through() {
        let body = format!("{}\n{}", make_sse(""), make_sse("actual result"));
        assert_eq!(parse_sse_response(&body).unwrap(), "actual result");
    }

    #[test]
    fn parse_sse_response_malformed_json_is_error() {
        let body = "data: {not valid json}";
        assert!(parse_sse_response(body).is_err());
    }
}

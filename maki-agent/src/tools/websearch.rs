use std::io::Read;
use std::time::Duration;

use maki_tool_macro::Tool;
use serde_json::{Value, json};
use ureq::Agent;

use maki_providers::{ToolInput, ToolOutput};

use super::MAX_RESPONSE_BYTES;
use super::truncate_output;

const EXA_MCP_ENDPOINT: &str = "https://mcp.exa.ai/mcp";
const REQUEST_TIMEOUT_SECS: u64 = 25;
const DEFAULT_NUM_RESULTS: u64 = 8;

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
                    "livecrawl": "fallback",
                    "type": "auto"
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
            .send(body.as_str())
            .map_err(|e| format!("request failed: {e}"))?;

        let status = response.status().as_u16();
        if !(200..300).contains(&status) {
            return Err(format!("HTTP {status}"));
        }

        let mut body = String::new();
        response
            .into_body()
            .into_reader()
            .take(MAX_RESPONSE_BYTES as u64)
            .read_to_string(&mut body)
            .map_err(|e| format!("read error: {e}"))?;

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
            serde_json::from_str(data).map_err(|e| format!("JSON parse error: {e}"))?;
        if let Some(text) = parsed
            .pointer("/result/content/0/text")
            .and_then(Value::as_str)
        {
            return Ok(text.to_string());
        }
        if let Some(err) = parsed.pointer("/error/message").and_then(Value::as_str) {
            return Err(format!("Exa error: {err}"));
        }
    }
    Err("no result in SSE response".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use test_case::test_case;

    const VALID_SSE: &str =
        "data: {\"result\":{\"content\":[{\"type\":\"text\",\"text\":\"search results here\"}]}}";
    const ERROR_SSE: &str = "data: {\"error\":{\"code\":-1,\"message\":\"rate limited\"}}";
    const NO_DATA_SSE: &str = "event: message\nid: 1";
    const MULTI_LINE_SSE: &str = "event: message\ndata: {\"result\":{\"content\":[{\"type\":\"text\",\"text\":\"found it\"}]}}\n\n";

    #[test_case(VALID_SSE, Ok("search results here".into()) ; "valid_result")]
    #[test_case(ERROR_SSE, Err("Exa error: rate limited".into()) ; "error_response")]
    #[test_case(NO_DATA_SSE, Err("no result in SSE response".into()) ; "no_data_lines")]
    #[test_case(MULTI_LINE_SSE, Ok("found it".into()) ; "multi_line_sse")]
    fn parse_sse_response_cases(input: &str, expected: Result<String, String>) {
        assert_eq!(parse_sse_response(input), expected);
    }
}

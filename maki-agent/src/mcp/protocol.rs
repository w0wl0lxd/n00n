use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Serialize)]
pub struct JsonRpcRequest<'a> {
    pub jsonrpc: &'static str,
    pub id: u64,
    pub method: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub params: Option<Value>,
}

impl<'a> JsonRpcRequest<'a> {
    pub fn new(id: u64, method: &'a str, params: Option<Value>) -> Self {
        Self {
            jsonrpc: "2.0",
            id,
            method,
            params,
        }
    }
}

#[derive(Serialize)]
pub struct JsonRpcNotification<'a> {
    pub jsonrpc: &'static str,
    pub method: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub params: Option<Value>,
}

impl<'a> JsonRpcNotification<'a> {
    pub fn new(method: &'a str, params: Option<Value>) -> Self {
        Self {
            jsonrpc: "2.0",
            method,
            params,
        }
    }
}

#[derive(Deserialize)]
pub struct JsonRpcResponse {
    pub id: Option<u64>,
    pub result: Option<Value>,
    pub error: Option<JsonRpcError>,
}

#[derive(Deserialize)]
pub struct JsonRpcError {
    pub code: i64,
    pub message: String,
}

pub fn initialize_params() -> Value {
    serde_json::json!({
        "protocolVersion": "2024-11-05",
        "capabilities": {},
        "clientInfo": {
            "name": "maki",
            "version": env!("CARGO_PKG_VERSION"),
        }
    })
}

#[derive(Deserialize)]
pub struct ToolInfo {
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default, rename = "inputSchema")]
    pub input_schema: Value,
}

#[derive(Deserialize)]
pub struct ToolsListResult {
    pub tools: Vec<ToolInfo>,
}

#[derive(Deserialize)]
pub struct CallToolContent {
    #[serde(default)]
    pub text: String,
}

#[derive(Deserialize)]
pub struct CallToolResult {
    pub content: Vec<CallToolContent>,
    #[serde(default, rename = "isError")]
    pub is_error: bool,
}

impl CallToolResult {
    pub fn joined_text(&self) -> String {
        self.content
            .iter()
            .map(|c| c.text.as_str())
            .collect::<Vec<_>>()
            .join("\n")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn request_skips_none_params() {
        let with =
            serde_json::to_value(JsonRpcRequest::new(1, "init", Some(json!({"k": 1})))).unwrap();
        assert_eq!(with["params"]["k"], 1);

        let without = serde_json::to_value(JsonRpcRequest::new(2, "tools/list", None)).unwrap();
        assert!(without.get("params").is_none());
    }

    #[test]
    fn tool_info_honours_input_schema_rename() {
        let raw = json!({"tools": [{"name": "read_file", "description": "Read a file", "inputSchema": {"type": "object"}}]});
        let result: ToolsListResult = serde_json::from_value(raw).unwrap();
        assert_eq!(result.tools[0].name, "read_file");
        assert_eq!(result.tools[0].input_schema["type"], "object");
    }

    #[test]
    fn call_tool_result_honours_is_error_rename() {
        let raw = json!({"content": [{"text": "hello"}], "isError": true});
        let result: CallToolResult = serde_json::from_value(raw).unwrap();
        assert!(result.is_error);
        assert_eq!(result.joined_text(), "hello");
    }
}

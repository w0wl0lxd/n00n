use std::borrow::Cow;

use serde_json::{Value, json};

use crate::ToolOutput;
use crate::tools::registry::{HeaderFuture, HeaderResult, ParseError, ToolInvocation};
use crate::tools::schema::ToolInputErrorKind;
use crate::tools::{DescriptionContext, ToolContext, ToolExecResult};

pub struct ToolSearch;

impl ToolSearch {
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

impl Default for ToolSearch {
    fn default() -> Self {
        Self::new()
    }
}

struct ToolSearchInvocation {
    query: String,
    namespace: Option<String>,
}

impl ToolInvocation for ToolSearchInvocation {
    fn start_header(&self) -> HeaderFuture {
        HeaderFuture::Ready(HeaderResult::plain(format!("search_tools: {}", self.query)))
    }

    fn execute(self: Box<Self>, ctx: &ToolContext) -> crate::tools::ExecFuture<'_> {
        Box::pin(async move {
            if self.query.trim().is_empty() {
                return ToolExecResult::from(Err::<ToolOutput, _>(
                    "search query must not be empty".to_string(),
                ));
            }
            let results = ctx.registry.search(&self.query);
            let filtered: Vec<_> = if let Some(ns) = &self.namespace {
                results
                    .into_iter()
                    .filter(|r| r.namespace.as_deref() == Some(ns.as_str()))
                    .collect()
            } else {
                results
            };
            let mut output = filtered
                .iter()
                .map(|r| {
                    json!({
                        "name": r.name,
                        "namespace": r.namespace,
                        "description": r.description
                    })
                })
                .collect::<Vec<_>>();
            if let Some(mcp) = &ctx.mcp {
                match mcp.search_tools(&self.query) {
                    Ok(result) => output.push(json!({ "mcp": result })),
                    Err(error) => return ToolExecResult::from(Err::<ToolOutput, _>(error)),
                }
            }
            ToolExecResult::from(Ok(ToolOutput::Plain(json!(output).to_string().into())))
        })
    }
}

impl crate::tools::registry::Tool for ToolSearch {
    fn name(&self) -> &'static str {
        "search_tools"
    }

    fn aliases(&self) -> Vec<&str> {
        vec!["tool_search"]
    }

    fn description(&self, _ctx: &DescriptionContext) -> Cow<'_, str> {
        "Search deferred tools by name or description. Use when a needed capability is not loaded and its canonical name is unknown. Do not use when a loaded sibling already matches the task; call that sibling directly.".into()
    }

    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "Capability, canonical tool name, or description text to match."
                },
                "namespace": {
                    "type": "string",
                    "description": "Optional namespace in which to search for deferred tools."
                }
            },
            "required": ["query"],
            "additionalProperties": false
        })
    }

    fn parse(&self, input: &Value) -> Result<Box<dyn ToolInvocation>, ParseError> {
        let query = input
            .get("query")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ParseError {
                path: crate::tools::schema::JsonPath::default(),
                kind: ToolInputErrorKind::InternalBug {
                    detail: "missing required field 'query'".to_string(),
                },
            })?
            .to_string();
        let namespace = input
            .get("namespace")
            .and_then(|v| v.as_str())
            .map(String::from);
        Ok(Box::new(ToolSearchInvocation { query, namespace }))
    }
}

pub struct LoadNamespace;

impl LoadNamespace {
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

impl Default for LoadNamespace {
    fn default() -> Self {
        Self::new()
    }
}

struct LoadNamespaceInvocation {
    namespace: String,
}

impl ToolInvocation for LoadNamespaceInvocation {
    fn start_header(&self) -> HeaderFuture {
        HeaderFuture::Ready(HeaderResult::plain(format!(
            "load_toolset: {}",
            self.namespace
        )))
    }

    fn execute(self: Box<Self>, ctx: &ToolContext) -> crate::tools::ExecFuture<'_> {
        Box::pin(async move {
            let tools: Vec<String> = ctx
                .registry
                .snapshot()
                .iter()
                .filter(|t| t.namespace.as_deref() == Some(self.namespace.as_str()))
                .map(|t| t.name().to_string())
                .collect();
            let output = json!({
                "namespace": self.namespace,
                "tools": tools
            });
            ToolExecResult::from(Ok(ToolOutput::Plain(output.to_string().into())))
        })
    }
}

impl crate::tools::registry::Tool for LoadNamespace {
    fn name(&self) -> &'static str {
        "load_toolset"
    }

    fn aliases(&self) -> Vec<&str> {
        vec!["load_namespace"]
    }

    fn description(&self, _ctx: &DescriptionContext) -> Cow<'_, str> {
        "Load all deferred tools from one namespace. Use when several sibling tools from that namespace are needed. Do not use for one capability or an unknown canonical name; use `search_tools` instead.".into()
    }

    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "namespace": {
                    "type": "string",
                    "description": "Exact deferred namespace returned by `search_tools`."
                }
            },
            "required": ["namespace"],
            "additionalProperties": false
        })
    }

    fn parse(&self, input: &Value) -> Result<Box<dyn ToolInvocation>, ParseError> {
        let namespace = input
            .get("namespace")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ParseError {
                path: crate::tools::schema::JsonPath::default(),
                kind: ToolInputErrorKind::InternalBug {
                    detail: "missing required field 'namespace'".to_string(),
                },
            })?
            .to_string();
        Ok(Box::new(LoadNamespaceInvocation { namespace }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value as JsonValue;

    #[test]
    fn tool_search_json_escaping() {
        let description_with_special_chars = r#"Test with backslash \ and quote " and newline
"#;
        let result = json!([{
            "name": "test_tool",
            "namespace": "test_ns",
            "description": description_with_special_chars
        }]);
        let output = result.to_string();

        // Verify it's valid JSON
        let parsed: JsonValue = serde_json::from_str(&output).expect("output should be valid JSON");

        // Verify round-trip
        let array = parsed.as_array().expect("should be an array");
        let first = array.first().expect("should have one element");
        assert_eq!(first["name"], "test_tool");
        assert_eq!(first["namespace"], "test_ns");
        assert_eq!(first["description"], description_with_special_chars);
    }
}

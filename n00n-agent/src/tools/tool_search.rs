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
        HeaderFuture::Ready(HeaderResult::plain(format!("tool_search: {}", self.query)))
    }

    fn execute(self: Box<Self>, ctx: &ToolContext) -> crate::tools::ExecFuture<'_> {
        Box::pin(async move {
            let results = ctx.registry.search(&self.query);
            let filtered: Vec<_> = if let Some(ns) = &self.namespace {
                results
                    .into_iter()
                    .filter(|r| r.namespace.as_deref() == Some(ns.as_str()))
                    .collect()
            } else {
                results
            };
            let output = json!(
                filtered
                    .iter()
                    .map(|r| json!({
                        "name": r.name,
                        "namespace": r.namespace,
                        "description": r.description
                    }))
                    .collect::<Vec<_>>()
            );
            ToolExecResult::from(Ok(ToolOutput::Plain(output.to_string().into())))
        })
    }
}

impl crate::tools::registry::Tool for ToolSearch {
    fn name(&self) -> &'static str {
        "tool_search"
    }

    fn description(&self, _ctx: &DescriptionContext) -> Cow<'_, str> {
        "Search for deferred tools by name or description. Returns a list of tools that can be loaded on demand.".into()
    }

    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "Search query to match tool names or descriptions"
                },
                "namespace": {
                    "type": "string",
                    "description": "Optional namespace filter"
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
            "load_namespace: {}",
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
        "load_namespace"
    }

    fn description(&self, _ctx: &DescriptionContext) -> Cow<'_, str> {
        "Load all tools from a namespace. Returns the list of tools that were loaded.".into()
    }

    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "namespace": {
                    "type": "string",
                    "description": "Namespace to load"
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

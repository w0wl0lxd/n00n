use std::borrow::Cow;
use std::collections::BTreeSet;

use serde_json::{Value, json};

use crate::ToolOutput;
use crate::tools::registry::{HeaderFuture, HeaderResult, ParseError, ToolInvocation};
use crate::tools::schema::ToolInputErrorKind;
use crate::tools::{DescriptionContext, ToolContext, ToolExecResult};

const SEARCH_RESULTS_LIMIT: usize = 5;

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
            let description_ctx = DescriptionContext {
                filter: &ctx.tool_filter,
                audience: ctx.audience,
                workflow: ctx.workflow,
            };
            let results = ctx
                .registry
                .search(&self.query, &description_ctx, usize::MAX);
            let filtered: Vec<_> = results
                .into_iter()
                .filter(|result| {
                    self.namespace.as_ref().is_none_or(|namespace| {
                        result.namespace.as_deref() == Some(namespace.as_str())
                    })
                })
                .take(SEARCH_RESULTS_LIMIT)
                .collect();
            let mut output: Vec<Value> = filtered
                .iter()
                .map(|result| {
                    json!({
                        "name": result.name,
                        "namespace": result.namespace,
                        "description": result.description
                    })
                })
                .collect();
            if self
                .namespace
                .as_deref()
                .is_none_or(|namespace| namespace == "mcp")
                && let Some(mcp) = &ctx.mcp
            {
                let before: BTreeSet<String> = mcp.loaded_tool_names().into_iter().collect();
                let message = mcp.search_tools(&self.query);
                if let Ok(description) = message {
                    output.extend(
                        mcp.loaded_tool_names()
                            .into_iter()
                            .filter(|name| !before.contains(name))
                            .map(|name| {
                                json!({
                                    "name": name,
                                    "namespace": "mcp",
                                    "description": description
                                })
                            }),
                    );
                }
            }
            let output = Value::Array(output);
            ToolExecResult::from(Ok(ToolOutput::Plain(output.to_string().into())))
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
        "Search deferred built-in and MCP tools by name or description when the needed capability is absent. Loaded tools become callable on the next turn. Do not use this when a loaded sibling already matches the task.".into()
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
            "load_toolset: {}",
            self.namespace
        )))
    }

    fn execute(self: Box<Self>, ctx: &ToolContext) -> crate::tools::ExecFuture<'_> {
        Box::pin(async move {
            let description_ctx = DescriptionContext {
                filter: &ctx.tool_filter,
                audience: ctx.audience,
                workflow: ctx.workflow,
            };
            let tools = ctx
                .registry
                .deferred_namespace_tools(&self.namespace, &description_ctx);
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
        "Load all deferred tools from a namespace when several sibling tools are needed. Do not use this for one known tool; use search_tools instead.".into()
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
    use crate::tools::Tool;
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

    #[test]
    fn built_in_search_loads_matching_mcp_tool_for_next_request() {
        let registry = std::sync::Arc::new(crate::tools::ToolRegistry::new());
        let mcp = crate::mcp::stub_session(&[("srv.fetch_issue", "Fetch a GitHub issue")]);
        let mut ctx = crate::tools::test_support::stub_ctx(&crate::AgentMode::Build);
        ctx.registry = registry;
        ctx.mcp = Some(mcp.clone());
        let invocation = ToolSearch::new()
            .parse(&json!({ "query": "GitHub issue" }))
            .expect("search input");

        let result = smol::block_on(invocation.execute(&ctx));
        let output = result.output.expect("search output").as_text();
        let parsed: Value = serde_json::from_str(&output).expect("search JSON");
        assert_eq!(parsed[0]["name"], "srv__fetch_issue");
        assert_eq!(parsed[0]["namespace"], "mcp");

        let mut next_tools = json!([{ "name": "search_tools" }]);
        mcp.extend_tools(&mut next_tools);
        assert!(next_tools.as_array().is_some_and(|tools| {
            tools
                .iter()
                .any(|tool| tool["name"].as_str() == Some("srv__fetch_issue"))
        }));
    }
}

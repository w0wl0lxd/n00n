use std::fmt::Write;

use serde::Deserialize;
use serde_json::Value;

use maki_tool_macro::Tool;

use super::{ToolCall, ToolContext};

const MAX_BATCH_SIZE: usize = 25;

#[derive(Debug, Clone, Deserialize)]
pub(super) struct BatchEntry {
    tool: String,
    parameters: Value,
}

impl BatchEntry {
    fn item_schema() -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "tool": { "type": "string", "description": "The name of the tool to execute" },
                "parameters": { "type": "object", "description": "Parameters for the tool" }
            },
            "required": ["tool", "parameters"]
        })
    }
}

#[derive(Tool, Debug, Clone)]
pub struct Batch {
    #[param(description = "Array of tool calls to execute in parallel")]
    tool_calls: Vec<BatchEntry>,
}

impl Batch {
    pub const NAME: &str = "batch";
    pub const DESCRIPTION: &str = include_str!("batch.md");

    pub fn execute(&self, ctx: &ToolContext) -> Result<String, String> {
        if self.tool_calls.is_empty() {
            return Err("provide at least one tool call".into());
        }

        let active = &self.tool_calls[..self.tool_calls.len().min(MAX_BATCH_SIZE)];
        let discarded = &self.tool_calls[active.len()..];

        let results: Vec<_> = std::thread::scope(|s| {
            let handles: Vec<_> = active
                .iter()
                .map(|entry| {
                    s.spawn(|| {
                        if entry.tool == Self::NAME {
                            return Err("cannot nest batch inside batch".into());
                        }
                        let call = ToolCall::from_api(&entry.tool, &entry.parameters)
                            .map_err(|e| e.to_string())?;
                        let done = call.execute(ctx);
                        if done.is_error {
                            Err(done.content)
                        } else {
                            Ok(done.content)
                        }
                    })
                })
                .collect();

            handles
                .into_iter()
                .map(|h| h.join().unwrap_or(Err("tool thread panicked".into())))
                .collect()
        });

        let total = results.len() + discarded.len();
        let mut failed = discarded.len();
        let mut output = String::new();

        for (entry, result) in active.iter().zip(&results) {
            let _ = writeln!(output, "## {}", entry.tool);
            match result {
                Ok(content) => output.push_str(content),
                Err(err) => {
                    failed += 1;
                    let _ = write!(output, "[ERROR] {err}");
                }
            }
            output.push_str("\n\n");
        }

        for entry in discarded {
            let _ = write!(
                output,
                "## {}\n[ERROR] maximum of {MAX_BATCH_SIZE} tools per batch\n\n",
                entry.tool
            );
        }

        let succeeded = total - failed;
        if failed > 0 {
            let _ = write!(
                output,
                "Executed {succeeded}/{total} successfully. {failed} failed."
            );
        } else {
            let _ = write!(output, "All {total} tools executed successfully.");
        }

        Ok(output)
    }

    pub fn start_summary(&self) -> String {
        format!("{} tools", self.tool_calls.len())
    }

    pub fn mutable_path(&self) -> Option<&str> {
        None
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use crate::AgentMode;
    use crate::tools::test_support::stub_ctx;

    use super::*;

    #[test]
    fn empty_batch_returns_error() {
        let ctx = stub_ctx(&AgentMode::Build);
        let batch = Batch::parse_input(&json!({"tool_calls": []})).unwrap();
        assert!(batch.execute(&ctx).is_err());
    }

    #[test]
    fn nested_batch_rejected() {
        let ctx = stub_ctx(&AgentMode::Build);
        let batch = Batch::parse_input(&json!({
            "tool_calls": [{"tool": "batch", "parameters": {"tool_calls": []}}]
        }))
        .unwrap();

        let result = batch.execute(&ctx).unwrap();
        assert!(result.contains("[ERROR]"));
        assert!(result.contains("failed"));
    }

    #[test]
    fn parallel_execution_with_mixed_results() {
        let ctx = stub_ctx(&AgentMode::Build);
        let dir = tempfile::TempDir::new().unwrap();
        let f = dir.path().join("a.txt");
        std::fs::write(&f, "content").unwrap();

        let batch = Batch::parse_input(&json!({
            "tool_calls": [
                {"tool": "read", "parameters": {"path": f.to_str().unwrap()}},
                {"tool": "read", "parameters": {"path": "/nonexistent/path.txt"}}
            ]
        }))
        .unwrap();

        let result = batch.execute(&ctx).unwrap();
        assert!(result.contains("content"));
        assert!(result.contains("[ERROR]"));
    }

    #[test]
    fn exceeds_max_batch_size_discards_excess() {
        let ctx = stub_ctx(&AgentMode::Build);
        let calls: Vec<Value> = (0..MAX_BATCH_SIZE + 2)
            .map(|_| json!({"tool": "nonexistent", "parameters": {}}))
            .collect();

        let batch = Batch::parse_input(&json!({"tool_calls": calls})).unwrap();
        let result = batch.execute(&ctx).unwrap();
        assert!(result.contains(&format!("maximum of {MAX_BATCH_SIZE}")));
    }
}

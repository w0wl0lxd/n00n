use std::fmt::Write;

use maki_providers::{AgentEvent, BatchToolEntry, BatchToolStatus, ToolInput, ToolOutput};
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

    fn to_batch_entry(
        &self,
        status: BatchToolStatus,
        output: Option<ToolOutput>,
    ) -> BatchToolEntry {
        let call = ToolCall::from_api(&self.tool, &self.parameters).ok();
        BatchToolEntry {
            tool: self.tool.clone(),
            summary: call.as_ref().map(|c| c.start_summary()).unwrap_or_default(),
            status,
            input: call.and_then(|c| c.start_input()),
            output,
        }
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
    pub const EXAMPLES: Option<&str> = Some(
        r#"[
  {"tool_calls": [
    {"tool": "read", "parameters": {"path": "/home/user/project/src/main.rs"}},
    {"tool": "grep", "parameters": {"pattern": "TODO", "include": "*.rs"}}
  ]}
]"#,
    );

    pub fn execute(&self, ctx: &ToolContext) -> Result<ToolOutput, String> {
        if self.tool_calls.is_empty() {
            return Err("provide at least one tool call".into());
        }

        let batch_id = ctx.tool_use_id.unwrap_or_default().to_owned();

        let active = &self.tool_calls[..self.tool_calls.len().min(MAX_BATCH_SIZE)];
        let discarded = &self.tool_calls[active.len()..];

        let parsed: Vec<_> = active
            .iter()
            .map(|entry| {
                if entry.tool == Self::NAME {
                    return Err("cannot nest batch inside batch".into());
                }
                ToolCall::from_api(&entry.tool, &entry.parameters).map_err(|e| e.to_string())
            })
            .collect();

        let inner_id = |i: usize| format!("{batch_id}__{i}");

        let send_progress = |index: usize, status: BatchToolStatus| {
            let _ = ctx.event_tx.send(
                AgentEvent::BatchProgress {
                    batch_id: batch_id.clone(),
                    index,
                    status,
                }
                .into(),
            );
        };

        let results: Vec<(Result<String, String>, Option<ToolOutput>)> = std::thread::scope(|s| {
            let handles: Vec<_> = parsed
                .iter()
                .enumerate()
                .map(|(i, parsed_call)| {
                    let id = inner_id(i);
                    s.spawn(move || {
                        send_progress(i, BatchToolStatus::InProgress);
                        let (result, output) = match parsed_call {
                            Ok(call) => {
                                let inner_ctx = ToolContext {
                                    tool_use_id: Some(&id),
                                    ..*ctx
                                };
                                let done = call.execute(&inner_ctx, id.clone());
                                let text = done.output.as_text();
                                let result = if done.is_error { Err(text) } else { Ok(text) };
                                (result, Some(done.output))
                            }
                            Err(e) => (Err(e.clone()), None),
                        };
                        let status = if result.is_ok() {
                            BatchToolStatus::Success
                        } else {
                            BatchToolStatus::Error
                        };
                        send_progress(i, status);
                        (result, output)
                    })
                })
                .collect();

            handles
                .into_iter()
                .map(|h| {
                    h.join()
                        .unwrap_or((Err("tool thread panicked".into()), None))
                })
                .collect()
        });

        let total = results.len() + discarded.len();
        let mut failed = discarded.len();
        let mut output = String::new();
        let mut entries: Vec<BatchToolEntry> = active
            .iter()
            .zip(&results)
            .map(|(entry, (result, output))| {
                let status = if result.is_ok() {
                    BatchToolStatus::Success
                } else {
                    BatchToolStatus::Error
                };
                entry.to_batch_entry(status, output.clone())
            })
            .collect();

        for (entry, (result, _)) in active.iter().zip(&results) {
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
            entries.push(entry.to_batch_entry(BatchToolStatus::Error, None));
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

        Ok(ToolOutput::Batch {
            entries,
            text: output,
        })
    }

    pub fn start_summary(&self) -> String {
        format!("{} tools", self.tool_calls.len())
    }

    pub fn start_input(&self) -> Option<ToolInput> {
        None
    }

    pub fn start_output(&self) -> Option<ToolOutput> {
        let entries = self
            .tool_calls
            .iter()
            .map(|entry| entry.to_batch_entry(BatchToolStatus::Pending, None))
            .collect();
        Some(ToolOutput::Batch {
            entries,
            text: String::new(),
        })
    }

    pub fn mutable_path(&self) -> Option<&str> {
        None
    }
}

#[cfg(test)]
mod tests {
    use maki_providers::BatchToolEntry;
    use serde_json::json;

    use crate::AgentMode;
    use crate::tools::test_support::stub_ctx;

    use super::*;

    fn run_batch(input: Value) -> (Vec<BatchToolEntry>, String) {
        let ctx = stub_ctx(&AgentMode::Build);
        execute_batch(&ctx, input)
    }

    fn execute_batch(ctx: &ToolContext, input: Value) -> (Vec<BatchToolEntry>, String) {
        let batch = Batch::parse_input(&input).unwrap();
        match batch.execute(ctx).unwrap() {
            ToolOutput::Batch { entries, text } => (entries, text),
            other => panic!("expected Batch, got {other:?}"),
        }
    }

    #[test]
    fn empty_batch_returns_error() {
        let ctx = stub_ctx(&AgentMode::Build);
        let batch = Batch::parse_input(&json!({"tool_calls": []})).unwrap();
        assert!(batch.execute(&ctx).is_err());
    }

    #[test]
    fn nested_batch_rejected() {
        let (entries, _) = run_batch(json!({
            "tool_calls": [{"tool": "batch", "parameters": {"tool_calls": []}}]
        }));
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].status, BatchToolStatus::Error);
        assert_eq!(entries[0].tool, "batch");
    }

    #[test]
    fn parallel_execution_with_mixed_results() {
        let dir = tempfile::TempDir::new().unwrap();
        let f = dir.path().join("a.txt");
        std::fs::write(&f, "content").unwrap();

        let (entries, text) = run_batch(json!({
            "tool_calls": [
                {"tool": "read", "parameters": {"path": f.to_str().unwrap()}},
                {"tool": "read", "parameters": {"path": "/nonexistent/path.txt"}}
            ]
        }));
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].status, BatchToolStatus::Success);
        assert_eq!(entries[1].status, BatchToolStatus::Error);
        assert!(text.contains("content"));
    }

    #[test]
    fn exceeds_max_batch_size_discards_excess() {
        let calls: Vec<Value> = (0..MAX_BATCH_SIZE + 2)
            .map(|_| json!({"tool": "read", "parameters": {"path": "/tmp"}}))
            .collect();
        let (entries, _) = run_batch(json!({"tool_calls": calls}));
        assert_eq!(entries.len(), MAX_BATCH_SIZE + 2);
        let discarded: Vec<_> = entries[MAX_BATCH_SIZE..].iter().collect();
        assert!(discarded.iter().all(|e| e.status == BatchToolStatus::Error));
    }
}

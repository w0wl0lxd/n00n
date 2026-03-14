use std::fmt::Write;

use crate::agent::{ResolvedCall, resolve_tool};
use crate::{AgentEvent, BatchToolEntry, BatchToolStatus, ToolOutput};
use serde::Deserialize;
use serde_json::Value;

use crate::task_set::TaskSet;

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
            annotation: None,
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
    {"tool": "glob", "parameters": {"pattern": "src/**/*.ts"}},
    {"tool": "grep", "parameters": {"pattern": "import", "include": "*.ts"}},
    {"tool": "read", "parameters": {"path": "package.json"}}
  ]}
]"#,
    );

    pub async fn execute(&self, ctx: &ToolContext) -> Result<ToolOutput, String> {
        if self.tool_calls.is_empty() {
            return Err("provide at least one tool call".into());
        }

        let batch_id = ctx.tool_use_id.clone().unwrap_or_default();

        let active = &self.tool_calls[..self.tool_calls.len().min(MAX_BATCH_SIZE)];
        let discarded = &self.tool_calls[active.len()..];

        let mcp = ctx.mcp.as_deref();

        let parsed: Vec<Result<ResolvedCall, String>> = active
            .iter()
            .map(|entry| {
                if entry.tool == Self::NAME {
                    return Err("cannot nest batch inside batch".into());
                }
                resolve_tool(&entry.tool, &entry.parameters, mcp).map_err(|e| e.to_string())
            })
            .collect();

        let inner_id = |i: usize| format!("{batch_id}__{i}");

        let mut set = TaskSet::new();
        for (i, parsed_call) in parsed.iter().enumerate() {
            let id = inner_id(i);
            let batch_id = batch_id.clone();
            let ctx = ctx.clone();
            let parsed_call = parsed_call.clone();
            set.spawn(async move {
                ctx.event_tx.try_send(AgentEvent::BatchProgress {
                    batch_id: batch_id.clone(),
                    index: i,
                    status: BatchToolStatus::InProgress,
                    output: None,
                });
                let (result, output) = match parsed_call {
                    Ok(call) => {
                        let inner_ctx = ToolContext {
                            tool_use_id: Some(id.clone()),
                            ..ctx.clone()
                        };
                        let done = call.execute(&inner_ctx, id).await;
                        let text = done.output.as_text();
                        let result = if done.is_error { Err(text) } else { Ok(text) };
                        (result, Some(done.output))
                    }
                    Err(e) => (Err(e.to_string()), None),
                };
                let status = if result.is_ok() {
                    BatchToolStatus::Success
                } else {
                    BatchToolStatus::Error
                };
                ctx.event_tx.try_send(AgentEvent::BatchProgress {
                    batch_id,
                    index: i,
                    status,
                    output: output.clone(),
                });
                (i, result, output)
            });
        }

        let mut results: Vec<(Result<String, String>, Option<ToolOutput>)> =
            vec![(Err("tool task panicked".into()), None); parsed.len()];
        let all = ctx.cancel.race(set.join_all()).await?;
        for (i, result, output) in all.into_iter().flatten() {
            results[i] = (result, output);
        }

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
}

impl super::ToolDefaults for Batch {
    fn start_output(&self) -> Option<ToolOutput> {
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
}

#[cfg(test)]
mod tests {
    use crate::BatchToolEntry;
    use serde_json::json;

    use crate::AgentMode;
    use crate::tools::test_support::stub_ctx;

    use super::*;

    async fn run_batch(input: Value) -> (Vec<BatchToolEntry>, String) {
        let ctx = stub_ctx(&AgentMode::Build);
        execute_batch(&ctx, input).await
    }

    async fn execute_batch(ctx: &ToolContext, input: Value) -> (Vec<BatchToolEntry>, String) {
        let batch = Batch::parse_input(&input).unwrap();
        match batch.execute(ctx).await.unwrap() {
            ToolOutput::Batch { entries, text } => (entries, text),
            other => panic!("expected Batch, got {other:?}"),
        }
    }

    #[test]
    fn empty_batch_returns_error() {
        smol::block_on(async {
            let ctx = stub_ctx(&AgentMode::Build);
            let batch = Batch::parse_input(&json!({"tool_calls": []})).unwrap();
            assert!(batch.execute(&ctx).await.is_err());
        });
    }

    #[test]
    fn nested_batch_rejected() {
        smol::block_on(async {
            let (entries, _) = run_batch(json!({
                "tool_calls": [{"tool": "batch", "parameters": {"tool_calls": []}}]
            }))
            .await;
            assert_eq!(entries.len(), 1);
            assert_eq!(entries[0].status, BatchToolStatus::Error);
            assert_eq!(entries[0].tool, "batch");
        });
    }

    #[test]
    fn parallel_execution_with_mixed_results() {
        smol::block_on(async {
            let dir = tempfile::TempDir::new().unwrap();
            let f = dir.path().join("a.txt");
            std::fs::write(&f, "content").unwrap();

            let (entries, text) = run_batch(json!({
                "tool_calls": [
                    {"tool": "read", "parameters": {"path": f.to_str().unwrap()}},
                    {"tool": "read", "parameters": {"path": "/nonexistent/path.txt"}}
                ]
            }))
            .await;
            assert_eq!(entries.len(), 2);
            assert_eq!(entries[0].status, BatchToolStatus::Success);
            assert_eq!(entries[1].status, BatchToolStatus::Error);
            assert!(text.contains("content"));
        });
    }

    #[test]
    fn cancel_stops_batch() {
        smol::block_on(async {
            let (trigger, cancel) = crate::cancel::CancelToken::new();
            let mut ctx = stub_ctx(&AgentMode::Build);
            ctx.cancel = cancel;
            let batch = Batch::parse_input(&json!({
                "tool_calls": [
                    {"tool": "read", "parameters": {"path": "/dev/null"}}
                ]
            }))
            .unwrap();
            trigger.cancel();
            let err = batch.execute(&ctx).await.unwrap_err();
            assert!(err.contains("cancelled"));
        });
    }

    #[test]
    fn exceeds_max_batch_size_discards_excess() {
        smol::block_on(async {
            let calls: Vec<Value> = (0..MAX_BATCH_SIZE + 2)
                .map(|_| json!({"tool": "read", "parameters": {"path": "/tmp"}}))
                .collect();
            let (entries, _) = run_batch(json!({"tool_calls": calls})).await;
            assert_eq!(entries.len(), MAX_BATCH_SIZE + 2);
            let discarded: Vec<_> = entries[MAX_BATCH_SIZE..].iter().collect();
            assert!(discarded.iter().all(|e| e.status == BatchToolStatus::Error));
        });
    }
}

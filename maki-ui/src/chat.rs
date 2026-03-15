//! `history_to_display` rebuilds UI messages from raw API history + stored tool outputs.
//! Stored `ToolOutput` gets syntax highlighting; absent outputs fall back to plain text
//! from `ToolResult`. Webfetch bodies are hidden to save screen space.

use std::collections::HashMap;
use std::path::Path;

use crate::components::messages::MessagesPanel;
use crate::components::tool_display::{output_limits, tool_output_annotation};
use crate::components::{DisplayMessage, DisplayRole, ToolStatus};
use crate::markdown::truncate_lines;

use maki_agent::tools::{ToolCall, WEBFETCH_TOOL_NAME};
use maki_agent::{AgentEvent, BatchToolStatus, NO_FILES_FOUND, QuestionInfo, ToolOutput};
use maki_providers::{ContentBlock, Message, Role, TokenUsage};
use ratatui::Frame;
use ratatui::layout::Rect;

pub enum ChatEventResult {
    Continue,
    Done,
    Error(String),
    QueueItemConsumed,
    QuestionPrompt { questions: Vec<QuestionInfo> },
    AuthRequired,
}

pub struct Chat {
    pub name: String,
    pub token_usage: TokenUsage,
    pub context_size: u32,
    pub model_id: Option<String>,
    pending_turn_usage: Option<String>,
    messages_panel: MessagesPanel,
}

impl Chat {
    pub fn new(name: String) -> Self {
        Self {
            name,
            token_usage: TokenUsage::default(),
            context_size: 0,
            model_id: None,
            pending_turn_usage: None,
            messages_panel: MessagesPanel::new(),
        }
    }

    pub fn set_pending_turn_usage(&mut self, usage: String) {
        self.pending_turn_usage = Some(usage);
    }

    pub fn handle_event(&mut self, event: AgentEvent, plan_path: Option<&str>) -> ChatEventResult {
        match event {
            AgentEvent::ThinkingDelta { text } => self.messages_panel.thinking_delta(&text),
            AgentEvent::TextDelta { text } => self.messages_panel.text_delta(&text),
            AgentEvent::ToolPending { id, name } => self.messages_panel.tool_pending(id, &name),
            AgentEvent::ToolStart(e) => self.messages_panel.tool_start(e),
            AgentEvent::ToolOutput { id, content } => {
                self.messages_panel.tool_output(&id, &content)
            }
            AgentEvent::ToolDone(e) => {
                let is_plan_write = plan_path.is_some_and(|pp| {
                    e.written_path()
                        .is_some_and(|wp| wp == pp || Path::new(pp).ends_with(wp))
                });
                self.messages_panel.tool_done(e);
                if is_plan_write {
                    let pp = plan_path.unwrap();
                    if let Ok(content) = std::fs::read_to_string(pp) {
                        self.messages_panel
                            .push(DisplayMessage::plan(content, pp.to_string()));
                    }
                }
            }
            AgentEvent::BatchProgress {
                batch_id,
                index,
                status,
                output,
            } => {
                self.messages_panel
                    .batch_progress(&batch_id, index, status, output);
            }
            AgentEvent::QuestionPrompt { questions, .. } => {
                return ChatEventResult::QuestionPrompt { questions };
            }
            AgentEvent::TurnComplete { .. } => {}
            AgentEvent::ToolResultsSubmitted { .. } => {
                if let Some(usage) = self.pending_turn_usage.take() {
                    self.messages_panel.set_turn_usage_on_last_tool(usage);
                }
            }
            AgentEvent::AutoCompacting => {
                self.messages_panel.flush();
                self.messages_panel.push(DisplayMessage::new(
                    DisplayRole::Assistant,
                    "Auto-compacting conversation...".into(),
                ));
            }
            AgentEvent::QueueItemConsumed => {
                return ChatEventResult::QueueItemConsumed;
            }
            AgentEvent::Retry { .. } => unreachable!("handled before handle_event"),
            AgentEvent::Done { .. } => {
                self.messages_panel.flush();
                return ChatEventResult::Done;
            }
            AgentEvent::Error { message } => {
                self.messages_panel.flush();
                return ChatEventResult::Error(message);
            }
            AgentEvent::AuthRequired => {
                return ChatEventResult::AuthRequired;
            }
        }
        ChatEventResult::Continue
    }

    pub fn scroll(&mut self, delta: i32) {
        self.messages_panel.scroll(delta);
    }

    pub fn half_page(&self) -> i32 {
        self.messages_panel.half_page()
    }

    pub fn auto_scroll(&self) -> bool {
        self.messages_panel.auto_scroll()
    }

    pub fn scroll_to_top(&mut self) {
        self.messages_panel.scroll_to_top();
    }

    pub fn enable_auto_scroll(&mut self) {
        self.messages_panel.enable_auto_scroll();
    }

    pub fn scroll_to_segment(&mut self, segment_index: usize) {
        self.messages_panel.scroll_to_segment(segment_index);
    }

    pub fn set_highlight_segment(&mut self, idx: Option<usize>) {
        self.messages_panel.set_highlight_segment(idx);
    }

    pub fn is_animating(&self) -> bool {
        self.messages_panel.is_animating()
    }

    pub fn view(&mut self, frame: &mut Frame, area: Rect, has_selection: bool) {
        self.messages_panel.view(frame, area, has_selection);
    }

    pub fn scroll_top(&self) -> u16 {
        self.messages_panel.scroll_top()
    }

    pub fn segment_heights(&self) -> &[u16] {
        self.messages_panel.segment_heights()
    }

    pub fn segment_copy_texts(&self) -> Vec<&str> {
        self.messages_panel.segment_copy_texts()
    }

    pub fn stream_reset(&mut self) {
        self.messages_panel.stream_reset();
    }

    pub fn flush(&mut self) {
        self.messages_panel.flush();
    }

    pub fn fail_in_progress(&mut self) {
        self.messages_panel.fail_in_progress();
    }

    pub fn push(&mut self, msg: DisplayMessage) {
        self.messages_panel.push(msg);
    }

    pub fn update_tool_summary(&mut self, tool_id: &str, summary: &str) {
        self.messages_panel.update_tool_summary(tool_id, summary);
    }

    pub fn update_tool_model(&mut self, tool_id: &str, model: &str) {
        self.messages_panel.update_tool_model(tool_id, model);
    }

    pub fn load_messages(&mut self, msgs: Vec<DisplayMessage>) {
        self.messages_panel.load_messages(msgs);
    }

    pub fn push_user_message(&mut self, text: &str) {
        self.messages_panel
            .push(DisplayMessage::new(DisplayRole::User, text.to_string()));
    }

    #[cfg(test)]
    pub fn message_count(&self) -> usize {
        self.messages_panel.message_count()
    }

    #[cfg(test)]
    pub fn in_progress_count(&self) -> usize {
        self.messages_panel.in_progress_count()
    }

    #[cfg(test)]
    pub fn last_message_text(&self) -> &str {
        self.messages_panel.last_message_text()
    }

    #[cfg(test)]
    pub fn last_message_is_plan(&self) -> bool {
        self.messages_panel.last_message_is_plan()
    }
}

pub fn history_to_display(
    messages: &[Message],
    tool_outputs: &HashMap<String, ToolOutput>,
) -> Vec<DisplayMessage> {
    let results = build_tool_results_map(messages);
    let mut display = Vec::new();
    for msg in messages {
        match msg.role {
            Role::User => {
                if let Some(text) = msg.user_text() {
                    display.push(DisplayMessage::new(DisplayRole::User, text.to_owned()));
                }
            }
            Role::Assistant => {
                for block in &msg.content {
                    match block {
                        ContentBlock::Text { text } if !text.is_empty() => {
                            display.push(DisplayMessage::new(DisplayRole::Assistant, text.clone()));
                        }
                        ContentBlock::ToolUse { id, name, input } => {
                            let static_name = ToolCall::static_name(name).unwrap_or("unknown");
                            let tool_call = ToolCall::from_api(name, input).ok();
                            let summary = tool_call
                                .as_ref()
                                .map(|tc| tc.start_summary())
                                .unwrap_or_else(|| name.clone());
                            let tool_input = tool_call.as_ref().and_then(|tc| tc.start_input());
                            let (status, result_text) = results
                                .get(id.as_str())
                                .map(|(err, text)| {
                                    let s = if *err {
                                        ToolStatus::Error
                                    } else {
                                        ToolStatus::Success
                                    };
                                    (s, Some(&**text))
                                })
                                .unwrap_or((ToolStatus::Success, None));
                            let stored = tool_outputs.get(id).cloned();
                            let (text, tool_output, annotation) =
                                build_loaded_tool(static_name, &summary, stored, result_text);
                            display.push(DisplayMessage {
                                role: DisplayRole::Tool {
                                    id: id.clone(),
                                    status,
                                    name: static_name,
                                },
                                text,
                                tool_input,
                                tool_output,
                                annotation,
                                plan_path: None,
                                timestamp: None,
                                turn_usage: None,
                            });
                        }
                        _ => {}
                    }
                }
            }
        }
    }
    display
}

/// Replicate the live `tool_done` rendering: set structured output for
/// syntax highlighting, truncate plain text bodies, compute annotations.
fn build_loaded_tool(
    tool: &str,
    summary: &str,
    reconstructed: Option<ToolOutput>,
    result_text: Option<&str>,
) -> (String, Option<ToolOutput>, Option<String>) {
    match reconstructed {
        Some(ref output @ ToolOutput::GlobResult { .. }) => {
            let annotation = tool_output_annotation(output, tool);
            let text = if output.is_empty_result() {
                format!("{summary}\n{NO_FILES_FOUND}")
            } else {
                let display = output.as_display_text();
                let (max, keep) = output_limits(tool);
                let truncated = truncate_lines(&display, max, keep);
                format!("{summary}\n{truncated}")
            };
            (text, reconstructed, annotation)
        }
        Some(ref output @ ToolOutput::GrepResult { .. }) => {
            let annotation = tool_output_annotation(output, tool);
            (summary.to_owned(), reconstructed, annotation)
        }
        Some(ToolOutput::Batch { ref entries, .. }) => {
            let failed = entries
                .iter()
                .filter(|e| e.status == BatchToolStatus::Error)
                .count();
            let text = if failed > 0 {
                let total = entries.len();
                format!("{}/{total} tools succeeded", total - failed)
            } else {
                summary.to_owned()
            };
            (text, reconstructed, None)
        }
        Some(ref output) => {
            let annotation = tool_output_annotation(output, tool);
            (summary.to_owned(), reconstructed, annotation)
        }
        None => {
            let result = result_text.unwrap_or("");
            let annotation = if !result.is_empty() {
                tool_output_annotation(&ToolOutput::Plain(result.into()), tool)
            } else {
                None
            };
            if result.is_empty() || matches!(tool, WEBFETCH_TOOL_NAME) {
                (summary.to_owned(), None, annotation)
            } else {
                let (max, keep) = output_limits(tool);
                let truncated = truncate_lines(result, max, keep);
                (format!("{summary}\n{truncated}"), None, annotation)
            }
        }
    }
}

fn build_tool_results_map(messages: &[Message]) -> HashMap<&str, (bool, &str)> {
    let mut map = HashMap::new();
    for msg in messages {
        if !matches!(msg.role, Role::User) {
            continue;
        }
        for block in &msg.content {
            if let ContentBlock::ToolResult {
                tool_use_id,
                content,
                is_error,
            } = block
            {
                map.insert(tool_use_id.as_str(), (*is_error, content.as_str()));
            }
        }
    }
    map
}

#[cfg(test)]
mod tests {
    use super::*;
    use maki_agent::{
        AgentEvent, BatchToolEntry, BatchToolStatus, DiffHunk, DiffLine, DiffSpan, ToolDoneEvent,
        ToolInput, ToolOutput, ToolStartEvent,
    };

    fn tool_start(id: &str, tool: &'static str) -> AgentEvent {
        AgentEvent::ToolStart(ToolStartEvent {
            id: id.into(),
            tool,
            summary: String::new(),
            annotation: None,
            input: None,
            output: None,
        })
    }

    fn write_done(id: &str, path: &str) -> AgentEvent {
        AgentEvent::ToolDone(ToolDoneEvent {
            id: id.into(),
            tool: "write",
            output: ToolOutput::WriteCode {
                path: path.into(),
                byte_count: 42,
                lines: vec![],
            },
            is_error: false,
        })
    }

    fn empty_outputs() -> HashMap<String, ToolOutput> {
        HashMap::new()
    }

    #[test]
    fn tool_lifecycle() {
        let mut chat = Chat::new("Main".into());
        chat.handle_event(tool_start("t1", "bash"), None);
        assert_eq!(chat.in_progress_count(), 1);

        chat.handle_event(
            AgentEvent::ToolDone(ToolDoneEvent {
                id: "t1".into(),
                tool: "bash",
                output: ToolOutput::Plain("ok".into()),
                is_error: false,
            }),
            None,
        );
        assert_eq!(chat.in_progress_count(), 0);
    }

    #[test]
    fn plan_write_renders_file_content() {
        let mut chat = Chat::new("Main".into());
        let dir = tempfile::tempdir().unwrap();
        let plan_path = dir.path().join("plan.md");
        std::fs::write(&plan_path, "# My Plan\n\n- Step 1").unwrap();
        let plan_str = plan_path.to_str().unwrap();

        chat.handle_event(tool_start("w1", "write"), Some(plan_str));
        chat.handle_event(write_done("w1", plan_str), Some(plan_str));

        assert!(chat.last_message_is_plan());
        let last = chat.last_message_text();
        assert!(last.contains("# My Plan"));
        assert!(last.contains("- Step 1"));
    }

    #[test]
    fn plan_write_ignores_different_path() {
        let mut chat = Chat::new("Main".into());
        chat.handle_event(tool_start("w1", "write"), Some("/plans/123.md"));
        chat.handle_event(write_done("w1", "src/main.rs"), Some("/plans/123.md"));
        assert!(!chat.last_message_is_plan());
    }

    #[test]
    fn history_skips_empty_text() {
        let msgs = vec![Message {
            role: Role::User,
            content: vec![ContentBlock::Text {
                text: String::new(),
            }],
            ..Default::default()
        }];
        assert!(history_to_display(&msgs, &empty_outputs()).is_empty());
    }

    #[test]
    fn history_tool_use_with_result() {
        let msgs = vec![
            Message {
                role: Role::Assistant,
                content: vec![ContentBlock::ToolUse {
                    id: "t1".into(),
                    name: "bash".into(),
                    input: serde_json::json!({"command": "ls", "description": "list files"}),
                }],
                ..Default::default()
            },
            Message {
                role: Role::User,
                content: vec![ContentBlock::ToolResult {
                    tool_use_id: "t1".into(),
                    content: "file.txt".into(),
                    is_error: false,
                }],
                ..Default::default()
            },
        ];
        let display = history_to_display(&msgs, &empty_outputs());
        assert_eq!(display.len(), 1);
        assert!(matches!(
            display[0].role,
            DisplayRole::Tool {
                ref id,
                status: ToolStatus::Success,
                name: "bash",
            } if id == "t1"
        ));
        assert!(display[0].text.contains("file.txt"));
    }

    #[test]
    fn history_tool_error_result() {
        let msgs = vec![
            Message {
                role: Role::Assistant,
                content: vec![ContentBlock::ToolUse {
                    id: "t2".into(),
                    name: "read".into(),
                    input: serde_json::json!({"path": "/missing"}),
                }],
                ..Default::default()
            },
            Message {
                role: Role::User,
                content: vec![ContentBlock::ToolResult {
                    tool_use_id: "t2".into(),
                    content: "not found".into(),
                    is_error: true,
                }],
                ..Default::default()
            },
        ];
        let display = history_to_display(&msgs, &empty_outputs());
        assert_eq!(display.len(), 1);
        assert!(matches!(
            display[0].role,
            DisplayRole::Tool {
                status: ToolStatus::Error,
                ..
            }
        ));
    }

    #[test]
    fn history_mixed_conversation() {
        let msgs = vec![
            Message::user("do something".into()),
            Message {
                role: Role::Assistant,
                content: vec![
                    ContentBlock::Text {
                        text: "Sure, let me help.".into(),
                    },
                    ContentBlock::ToolUse {
                        id: "t1".into(),
                        name: "bash".into(),
                        input: serde_json::json!({"command": "echo hi"}),
                    },
                ],
                ..Default::default()
            },
            Message {
                role: Role::User,
                content: vec![ContentBlock::ToolResult {
                    tool_use_id: "t1".into(),
                    content: "hi".into(),
                    is_error: false,
                }],
                ..Default::default()
            },
            Message {
                role: Role::Assistant,
                content: vec![ContentBlock::Text {
                    text: "Done!".into(),
                }],
                ..Default::default()
            },
        ];
        let display = history_to_display(&msgs, &empty_outputs());
        assert_eq!(display.len(), 4);
        assert_eq!(display[0].role, DisplayRole::User);
        assert_eq!(display[1].role, DisplayRole::Assistant);
        assert!(matches!(display[2].role, DisplayRole::Tool { .. }));
        assert_eq!(display[3].role, DisplayRole::Assistant);
        assert_eq!(display[3].text, "Done!");
    }

    #[test]
    fn history_stored_output_variants_pass_through() {
        let variants: Vec<(&str, serde_json::Value, ToolOutput)> = vec![
            (
                "edit",
                serde_json::json!({"path": "a", "old_string": "x", "new_string": "y"}),
                ToolOutput::Diff {
                    path: "a".into(),
                    hunks: vec![DiffHunk {
                        start_line: 1,
                        lines: vec![
                            DiffLine::Removed(vec![DiffSpan::plain("x".into())]),
                            DiffLine::Added(vec![DiffSpan::plain("y".into())]),
                        ],
                    }],
                    summary: "edited a".into(),
                },
            ),
            (
                "read",
                serde_json::json!({"path": "/src/main.rs"}),
                ToolOutput::ReadCode {
                    path: "/src/main.rs".into(),
                    start_line: 1,
                    lines: vec!["fn main() {}".into()],
                },
            ),
            (
                "grep",
                serde_json::json!({"pattern": "TODO"}),
                ToolOutput::GrepResult { entries: vec![] },
            ),
            (
                "todowrite",
                serde_json::json!({"todos": []}),
                ToolOutput::TodoList(vec![]),
            ),
        ];
        for (tool_name, input_json, output) in variants {
            let discriminant = std::mem::discriminant(&output);
            let msgs = vec![
                Message {
                    role: Role::Assistant,
                    content: vec![ContentBlock::ToolUse {
                        id: "t1".into(),
                        name: tool_name.into(),
                        input: input_json,
                    }],
                    ..Default::default()
                },
                Message {
                    role: Role::User,
                    content: vec![ContentBlock::ToolResult {
                        tool_use_id: "t1".into(),
                        content: "ok".into(),
                        is_error: false,
                    }],
                    ..Default::default()
                },
            ];
            let outputs = HashMap::from([("t1".into(), output)]);
            let display = history_to_display(&msgs, &outputs);
            assert_eq!(
                std::mem::discriminant(display[0].tool_output.as_ref().unwrap()),
                discriminant,
                "stored {tool_name} output should pass through"
            );
        }
    }

    #[test]
    fn history_stored_write_has_annotation() {
        let write_output = ToolOutput::WriteCode {
            path: "/src/main.rs".into(),
            byte_count: 12,
            lines: vec!["fn main() {}".into()],
        };
        let msgs = vec![
            Message {
                role: Role::Assistant,
                content: vec![ContentBlock::ToolUse {
                    id: "t1".into(),
                    name: "write".into(),
                    input: serde_json::json!({
                        "path": "/src/main.rs",
                        "content": "fn main() {}"
                    }),
                }],
                ..Default::default()
            },
            Message {
                role: Role::User,
                content: vec![ContentBlock::ToolResult {
                    tool_use_id: "t1".into(),
                    content: "wrote 12 bytes".into(),
                    is_error: false,
                }],
                ..Default::default()
            },
        ];
        let outputs = HashMap::from([("t1".into(), write_output)]);
        let display = history_to_display(&msgs, &outputs);
        assert!(display[0].annotation.is_some());
    }

    #[test]
    fn history_bash_has_code_input() {
        let msgs = vec![Message {
            role: Role::Assistant,
            content: vec![ContentBlock::ToolUse {
                id: "t1".into(),
                name: "bash".into(),
                input: serde_json::json!({"command": "echo hi"}),
            }],
            ..Default::default()
        }];
        let display = history_to_display(&msgs, &empty_outputs());
        assert!(
            matches!(&display[0].tool_input, Some(ToolInput::Code { .. })),
            "bash tool should produce Code input for syntax highlighting"
        );
    }

    #[test]
    fn history_bash_output_truncated() {
        let long_output = (0..200).map(|i| format!("line {i}")).collect::<Vec<_>>();
        let joined = long_output.join("\n");
        let msgs = vec![
            Message {
                role: Role::Assistant,
                content: vec![ContentBlock::ToolUse {
                    id: "t1".into(),
                    name: "bash".into(),
                    input: serde_json::json!({"command": "cmd", "description": "test"}),
                }],
                ..Default::default()
            },
            Message {
                role: Role::User,
                content: vec![ContentBlock::ToolResult {
                    tool_use_id: "t1".into(),
                    content: joined,
                    is_error: false,
                }],
                ..Default::default()
            },
        ];
        let display = history_to_display(&msgs, &empty_outputs());
        let line_count = display[0].text.lines().count();
        assert!(
            line_count < long_output.len(),
            "output should be truncated, got {line_count} lines for {} input lines",
            long_output.len()
        );
    }

    #[test]
    fn history_webfetch_hides_body() {
        let msgs = vec![
            Message {
                role: Role::Assistant,
                content: vec![ContentBlock::ToolUse {
                    id: "t1".into(),
                    name: "webfetch".into(),
                    input: serde_json::json!({"url": "https://example.com"}),
                }],
                ..Default::default()
            },
            Message {
                role: Role::User,
                content: vec![ContentBlock::ToolResult {
                    tool_use_id: "t1".into(),
                    content: "fetched content\nmore lines".into(),
                    is_error: false,
                }],
                ..Default::default()
            },
        ];
        let display = history_to_display(&msgs, &empty_outputs());
        assert!(
            !display[0].text.contains('\n'),
            "webfetch should hide body text"
        );
    }

    #[test]
    fn history_stored_batch_with_errors_shows_count() {
        let batch_output = ToolOutput::Batch {
            entries: vec![
                BatchToolEntry {
                    tool: "read".into(),
                    summary: "/a.rs".into(),
                    status: BatchToolStatus::Success,
                    input: None,
                    output: None,
                    annotation: None,
                },
                BatchToolEntry {
                    tool: "read".into(),
                    summary: "/missing".into(),
                    status: BatchToolStatus::Error,
                    input: None,
                    output: None,
                    annotation: None,
                },
            ],
            text: String::new(),
        };
        let msgs = vec![
            Message {
                role: Role::Assistant,
                content: vec![ContentBlock::ToolUse {
                    id: "b1".into(),
                    name: "batch".into(),
                    input: serde_json::json!({"tool_calls": []}),
                }],
                ..Default::default()
            },
            Message {
                role: Role::User,
                content: vec![ContentBlock::ToolResult {
                    tool_use_id: "b1".into(),
                    content: String::new(),
                    is_error: false,
                }],
                ..Default::default()
            },
        ];
        let outputs = HashMap::from([("b1".into(), batch_output)]);
        let display = history_to_display(&msgs, &outputs);
        let ToolOutput::Batch { entries, .. } = display[0].tool_output.as_ref().unwrap() else {
            panic!("expected Batch output");
        };
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[1].status, BatchToolStatus::Error);
        assert!(display[0].text.contains("1/2"));
    }

    #[test]
    fn history_no_stored_output_falls_back_to_plain_text() {
        let msgs = vec![
            Message {
                role: Role::Assistant,
                content: vec![ContentBlock::ToolUse {
                    id: "t1".into(),
                    name: "read".into(),
                    input: serde_json::json!({"path": "/src/main.rs"}),
                }],
                ..Default::default()
            },
            Message {
                role: Role::User,
                content: vec![ContentBlock::ToolResult {
                    tool_use_id: "t1".into(),
                    content: "1: fn main() {}".into(),
                    is_error: false,
                }],
                ..Default::default()
            },
        ];
        let display = history_to_display(&msgs, &empty_outputs());
        assert!(display[0].tool_output.is_none());
        assert!(display[0].text.contains("fn main"));
    }

    #[test]
    fn history_grep_output_shows_matches() {
        use maki_agent::{GrepFileEntry, GrepMatch};
        let output = ToolOutput::GrepResult {
            entries: vec![GrepFileEntry {
                path: "src/lib.rs".into(),
                matches: vec![GrepMatch {
                    line_nr: 42,
                    text: "fn run()".into(),
                }],
            }],
        };
        let msgs = vec![
            Message {
                role: Role::Assistant,
                content: vec![ContentBlock::ToolUse {
                    id: "t1".into(),
                    name: "grep".into(),
                    input: serde_json::json!({"pattern": "fn run"}),
                }],
                ..Default::default()
            },
            Message {
                role: Role::User,
                content: vec![ContentBlock::ToolResult {
                    tool_use_id: "t1".into(),
                    content: String::new(),
                    is_error: false,
                }],
                ..Default::default()
            },
        ];
        let outputs = HashMap::from([("t1".into(), output)]);
        let display = history_to_display(&msgs, &outputs);
        assert!(display[0].tool_output.is_some());
        assert!(
            !display[0].text.contains("src/lib.rs"),
            "grep body rendered via code_view, not in text"
        );
    }
}

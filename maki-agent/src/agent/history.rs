use maki_providers::{ContentBlock, Message, Role};

const CANCEL_MARKER: &str = "[Cancelled by user]";

pub struct History {
    messages: Vec<Message>,
}

impl History {
    pub fn new(messages: Vec<Message>) -> Self {
        Self { messages }
    }

    pub fn as_slice(&self) -> &[Message] {
        &self.messages
    }

    pub fn push(&mut self, msg: Message) {
        self.messages.push(msg);
    }

    pub fn len(&self) -> usize {
        self.messages.len()
    }

    pub fn is_empty(&self) -> bool {
        self.messages.is_empty()
    }

    pub fn replace(&mut self, messages: Vec<Message>) {
        self.messages = messages;
    }

    pub fn truncate(&mut self, len: usize) {
        self.messages.truncate(len);
    }
}

pub(crate) fn sanitize_cancelled_history(history: &mut History, rollback_len: usize) {
    if history.len() <= rollback_len {
        return;
    }
    let last = history.as_slice().last().unwrap();
    if matches!(last.role, Role::Assistant) && last.has_tool_calls() {
        let error_results: Vec<ContentBlock> = last
            .tool_uses()
            .map(|(id, _, _)| ContentBlock::ToolResult {
                tool_use_id: id.to_owned(),
                content: CANCEL_MARKER.to_owned(),
                is_error: true,
            })
            .collect();
        history.push(Message {
            role: Role::User,
            content: error_results,
            display_text: Some(String::new()),
        });
    }
    history.push(Message::synthetic(CANCEL_MARKER.into()));
}

#[cfg(test)]
mod tests {
    use maki_providers::{ContentBlock, Message, Role};
    use test_case::test_case;

    use super::*;

    #[track_caller]
    fn assert_ends_with_cancel_marker(history: &History) {
        let last = history.as_slice().last().unwrap();
        assert!(matches!(last.role, Role::User));
        assert!(matches!(&last.content[0], ContentBlock::Text { text } if text == CANCEL_MARKER));
    }

    #[test_case(
        vec![Message::user("old".into())],
        1,
        1,
        false
        ; "no_new_messages_is_noop"
    )]
    #[test_case(
        vec![Message::user("hello".into())],
        0,
        2,
        true
        ; "user_only_appends_marker"
    )]
    #[test_case(
        vec![
            Message::user("hello".into()),
            Message { role: Role::Assistant, content: vec![ContentBlock::Text { text: "hi".into() }], ..Default::default() },
        ],
        0,
        3,
        true
        ; "complete_turn_appends_marker"
    )]
    fn sanitize_cancelled_history_cases(
        messages: Vec<Message>,
        rollback_len: usize,
        expected_len: usize,
        expect_cancel_marker: bool,
    ) {
        let mut history = History::new(messages);
        sanitize_cancelled_history(&mut history, rollback_len);
        assert_eq!(history.len(), expected_len);
        if expect_cancel_marker {
            assert_ends_with_cancel_marker(&history);
        }
    }

    #[test]
    fn sanitize_dangling_tool_use_adds_error_results() {
        let mut history = History::new(vec![
            Message::user("hello".into()),
            Message {
                role: Role::Assistant,
                content: vec![
                    ContentBlock::Text {
                        text: "let me check".into(),
                    },
                    ContentBlock::ToolUse {
                        id: "t1".into(),
                        name: "read".into(),
                        input: serde_json::json!({"path": "/tmp"}),
                    },
                    ContentBlock::ToolUse {
                        id: "t2".into(),
                        name: "glob".into(),
                        input: serde_json::json!({"pattern": "*.rs"}),
                    },
                ],
                ..Default::default()
            },
        ]);
        sanitize_cancelled_history(&mut history, 0);

        let tool_result_msg = &history.as_slice()[2];
        let error_ids: Vec<&str> = tool_result_msg
            .content
            .iter()
            .filter_map(|b| match b {
                ContentBlock::ToolResult {
                    tool_use_id,
                    is_error: true,
                    ..
                } => Some(tool_use_id.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(error_ids, ["t1", "t2"]);
        assert_ends_with_cancel_marker(&history);
    }
}

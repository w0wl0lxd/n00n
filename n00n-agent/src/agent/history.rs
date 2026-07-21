use std::sync::Arc;

use arc_swap::ArcSwap;
use n00n_providers::{ContentBlock, Message, Role};
use n00n_storage::sessions::TranscriptEntry;
use tracing::warn;

const CANCEL_MARKER: &str = "[Cancelled by user]";
const UNAVAILABLE_RESULT: &str = "[Tool result not available]";

pub type SharedMessages = Arc<ArcSwap<Vec<Message>>>;
pub type SharedTranscript = Arc<ArcSwap<Vec<TranscriptEntry<Message>>>>;

pub struct History {
    messages: Vec<Message>,
    transcript: Vec<TranscriptEntry<Message>>,
    mirror: Option<SharedMessages>,
    transcript_mirror: Option<SharedTranscript>,
}

impl History {
    pub fn new(messages: Vec<Message>) -> Self {
        Self {
            transcript: messages
                .iter()
                .cloned()
                .map(TranscriptEntry::Message)
                .collect(),
            messages,
            mirror: None,
            transcript_mirror: None,
        }
    }

    #[must_use]
    pub fn restored(messages: Vec<Message>) -> Self {
        Self::restored_with_transcript(messages, Vec::new())
    }

    pub fn restored_with_transcript(
        mut messages: Vec<Message>,
        transcript: Vec<TranscriptEntry<Message>>,
    ) -> Self {
        sanitize_restored(&mut messages);
        let mut transcript = transcript;
        if transcript.is_empty() {
            transcript.extend(messages.iter().cloned().map(TranscriptEntry::Message));
        } else {
            rebuild_transcript(&mut transcript, &messages);
        }
        Self {
            transcript,
            messages,
            mirror: None,
            transcript_mirror: None,
        }
    }

    #[must_use]
    pub fn with_mirror(mut self, mirror: SharedMessages) -> Self {
        self.mirror = Some(mirror);
        self.publish();
        self
    }

    #[must_use]
    pub fn with_transcript_mirror(mut self, mirror: SharedTranscript) -> Self {
        self.transcript_mirror = Some(mirror);
        self.publish();
        self
    }

    #[must_use]
    pub fn transcript(&self) -> &[TranscriptEntry<Message>] {
        &self.transcript
    }

    pub fn compact_boundary(&mut self, prompt: Message, summary: Message) {
        let previous = std::mem::take(&mut self.transcript);
        self.transcript = vec![TranscriptEntry::Compaction {
            entries: previous,
            generated_summary: Some(summary.clone()),
        }];
        self.messages = vec![prompt.clone(), summary.clone()];
        self.transcript.extend([
            TranscriptEntry::GeneratedMessage(prompt),
            TranscriptEntry::GeneratedMessage(summary),
        ]);
        self.publish();
    }

    #[must_use]
    pub fn as_slice(&self) -> &[Message] {
        &self.messages
    }

    pub fn push(&mut self, msg: Message) {
        self.messages.push(msg.clone());
        self.transcript.push(TranscriptEntry::Message(msg));
        self.publish();
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.messages.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.messages.is_empty()
    }

    #[must_use]
    pub fn ends_with_tool_results(&self) -> bool {
        self.messages.last().is_some_and(|message| {
            message
                .content
                .iter()
                .any(|block| matches!(block, ContentBlock::ToolResult { .. }))
        })
    }

    pub fn replace(&mut self, messages: Vec<Message>) {
        rebuild_transcript(&mut self.transcript, &messages);
        self.messages = messages;
        self.publish();
    }

    pub fn truncate(&mut self, len: usize) {
        self.messages.truncate(len);
        rebuild_transcript(&mut self.transcript, &self.messages);
        self.publish();
    }

    #[must_use]
    pub fn into_vec(self) -> Vec<Message> {
        self.messages
    }

    fn edit(&mut self, f: impl FnOnce(&mut Vec<Message>)) {
        let before = self.messages.len();
        f(&mut self.messages);
        for message in self.messages.iter().skip(before) {
            self.transcript
                .push(TranscriptEntry::Message(message.clone()));
        }
        self.publish();
    }

    fn publish(&self) {
        let Some(mirror) = &self.mirror else { return };
        let mut snapshot = self.messages.clone();
        close_dangling_tool_calls(&mut snapshot, UNAVAILABLE_RESULT);
        mirror.store(Arc::new(snapshot));
        if let Some(transcript) = &self.transcript_mirror {
            transcript.store(Arc::new(self.transcript.clone()));
        }
    }
}

pub fn rebuild_transcript(transcript: &mut Vec<TranscriptEntry<Message>>, messages: &[Message]) {
    if messages.is_empty() {
        transcript.clear();
        return;
    }

    let active_entries: Vec<_> = transcript
        .iter()
        .filter(|entry| !matches!(entry, TranscriptEntry::Compaction { .. }))
        .cloned()
        .collect();
    transcript.retain(|entry| matches!(entry, TranscriptEntry::Compaction { .. }));
    transcript.extend(messages.iter().enumerate().map(|(index, message)| {
        match active_entries.get(index) {
            Some(TranscriptEntry::GeneratedMessage(saved))
                if same_message_identity(saved, message) =>
            {
                TranscriptEntry::GeneratedMessage(message.clone())
            }
            _ => TranscriptEntry::Message(message.clone()),
        }
    }));
}

fn same_message_identity(left: &Message, right: &Message) -> bool {
    let left = match serde_json::to_vec(left) {
        Ok(identity) => identity,
        Err(error) => {
            warn!(%error, "failed to serialize saved transcript message identity");
            return false;
        }
    };
    match serde_json::to_vec(right) {
        Ok(identity) => left == identity,
        Err(error) => {
            warn!(%error, "failed to serialize active transcript message identity");
            false
        }
    }
}

/// Restored sessions can have orphaned `tool_results` or unclosed `tool_uses`
/// (e.g. the process was killed mid-turn). The API returns 400 if it sees those.
fn sanitize_restored(messages: &mut Vec<Message>) {
    let len_before = messages.len();
    let mut i = 0;
    while i < messages.len() {
        if !matches!(messages[i].role, Role::User) {
            i += 1;
            continue;
        }

        let valid_ids: Vec<String> = if i > 0 && matches!(messages[i - 1].role, Role::Assistant) {
            messages[i - 1]
                .tool_uses()
                .map(|(id, _, _)| id.to_owned())
                .collect()
        } else {
            Vec::new()
        };

        let (mut had_results, mut kept_results) = (false, false);
        messages[i].content.retain(|b| match b {
            ContentBlock::ToolResult { tool_use_id, .. } => {
                had_results = true;
                let keep = valid_ids.iter().any(|id| id == tool_use_id);
                kept_results |= keep;
                keep
            }
            _ => true,
        });
        // A tool-returned image whose results were all orphaned would float
        // with no context, so it goes too. Chat-pasted images live in
        // messages without tool results and stay untouched.
        if had_results && !kept_results {
            messages[i]
                .content
                .retain(|b| !matches!(b, ContentBlock::Image { .. }));
        }

        if messages[i].content.is_empty() {
            messages.remove(i);
        } else {
            i += 1;
        }
    }

    close_dangling_tool_calls(messages, UNAVAILABLE_RESULT);

    if messages.len() != len_before {
        warn!(
            before = len_before,
            after = messages.len(),
            "sanitized restored history"
        );
    }
}

fn close_dangling_tool_calls(messages: &mut Vec<Message>, note: &str) {
    let Some(last) = messages.last() else { return };
    if !matches!(last.role, Role::Assistant) || !last.has_tool_calls() {
        return;
    }
    let error_results: Vec<ContentBlock> = last
        .tool_uses()
        .map(|(id, _, _)| ContentBlock::ToolResult {
            tool_use_id: id.to_owned(),
            content: note.to_owned(),
            is_error: true,
        })
        .collect();
    messages.push(Message {
        role: Role::User,
        content: error_results,
        display_text: Some(String::new()),
    });
}

pub(crate) fn sanitize_cancelled_history(history: &mut History, rollback_len: usize) {
    if history.len() <= rollback_len {
        return;
    }
    history.edit(|msgs| {
        close_dangling_tool_calls(msgs, CANCEL_MARKER);
        msgs.push(Message::synthetic(CANCEL_MARKER.into()));
    });
}

#[cfg(test)]
mod tests {
    use n00n_providers::{ContentBlock, Message, Role};
    use n00n_storage::sessions::TranscriptEntry;
    use test_case::test_case;

    use super::*;

    fn assistant(text: &str) -> Message {
        Message {
            role: Role::Assistant,
            content: vec![ContentBlock::Text { text: text.into() }],
            ..Default::default()
        }
    }

    fn compact(history: &mut History, summary: &str) {
        history.compact_boundary(Message::user("summary prompt".into()), assistant(summary));
    }

    #[track_caller]
    fn assert_ends_with_cancel_marker(history: &History) {
        let last = history.as_slice().last().unwrap();
        assert!(matches!(last.role, Role::User));
        assert!(matches!(&last.content[0], ContentBlock::Text { text } if text == CANCEL_MARKER));
    }

    #[test]
    fn compaction_boundary_preserves_prior_transcript_and_summary() {
        let mut history = History::new(vec![Message::user("old".into())]);
        history.push(Message::synthetic("reply".into()));
        compact(&mut history, "summary");

        assert_eq!(history.as_slice().len(), 2);
        assert!(matches!(
            history.transcript(),
            [
                TranscriptEntry::Compaction {
                    entries,
                    generated_summary: Some(_),
                },
                TranscriptEntry::GeneratedMessage(_),
                TranscriptEntry::GeneratedMessage(_),
            ] if entries.len() == 2
        ));
    }

    #[test]
    fn later_compactions_nest_independently() {
        let mut history = History::new(vec![Message::user("one".into())]);
        compact(&mut history, "summary one");
        history.push(Message::user("two".into()));
        compact(&mut history, "summary two");

        assert!(matches!(
            history.transcript(),
            [TranscriptEntry::Compaction { entries, .. }, TranscriptEntry::GeneratedMessage(_), TranscriptEntry::GeneratedMessage(_)]
                if matches!(entries.as_slice(), [TranscriptEntry::Compaction { .. }, TranscriptEntry::GeneratedMessage(_), TranscriptEntry::GeneratedMessage(_), TranscriptEntry::Message(_)])
        ));
    }

    #[test]
    fn restored_transcript_survives_another_compaction() {
        let prior = vec![
            TranscriptEntry::Compaction {
                entries: vec![TranscriptEntry::Message(Message::user("original".into()))],
                generated_summary: Some(assistant("first summary")),
            },
            TranscriptEntry::GeneratedMessage(Message::user("summary prompt".into())),
            TranscriptEntry::GeneratedMessage(assistant("first summary")),
        ];
        let message_mirror = make_mirror();
        let transcript_mirror: SharedTranscript = Arc::new(ArcSwap::from_pointee(Vec::new()));
        let mut history = History::restored_with_transcript(
            vec![
                Message::user("summary prompt".into()),
                assistant("first summary"),
            ],
            prior,
        )
        .with_mirror(message_mirror)
        .with_transcript_mirror(Arc::clone(&transcript_mirror));

        history.push(Message::synthetic("continued".into()));
        compact(&mut history, "second summary");

        assert!(matches!(
            history.transcript(),
            [TranscriptEntry::Compaction { entries, .. }, TranscriptEntry::GeneratedMessage(_), TranscriptEntry::GeneratedMessage(_)]
                if matches!(entries.as_slice(), [TranscriptEntry::Compaction { entries, .. }, TranscriptEntry::GeneratedMessage(_), TranscriptEntry::GeneratedMessage(_), TranscriptEntry::Message(_)] if matches!(entries.as_slice(), [TranscriptEntry::Message(_)]))
        ));
        let snapshot = transcript_mirror.load();
        assert!(matches!(
            snapshot.as_slice(),
            [TranscriptEntry::Compaction { entries, .. }, TranscriptEntry::GeneratedMessage(_), TranscriptEntry::GeneratedMessage(_)]
                if matches!(entries.as_slice(), [TranscriptEntry::Compaction { .. }, TranscriptEntry::GeneratedMessage(_), TranscriptEntry::GeneratedMessage(_), TranscriptEntry::Message(_)])
        ));
    }

    #[test]
    fn restored_empty_transcript_falls_back_to_flat_messages() {
        let messages = vec![Message::user("legacy".into())];
        let history = History::restored_with_transcript(messages, Vec::new());

        assert!(matches!(
            history.transcript(),
            [TranscriptEntry::Message(message)] if message.user_text() == Some("legacy")
        ));
    }

    #[test]
    fn truncate_preserves_nested_compactions_and_removes_active_tail() {
        let nested = TranscriptEntry::Compaction {
            entries: vec![TranscriptEntry::Compaction {
                entries: vec![TranscriptEntry::Message(Message::user("oldest".into()))],
                generated_summary: None,
            }],
            generated_summary: Some(assistant("summary")),
        };
        let active = vec![
            Message::user("summary prompt".into()),
            Message::user("summary".into()),
            Message::user("keep".into()),
            Message::user("kept reply".into()),
            Message::user("remove".into()),
            Message::user("removed reply".into()),
        ];
        let mut transcript = vec![nested];
        transcript.extend([
            TranscriptEntry::GeneratedMessage(active[0].clone()),
            TranscriptEntry::GeneratedMessage(active[1].clone()),
        ]);
        transcript.extend(active.iter().skip(2).cloned().map(TranscriptEntry::Message));
        let mut history = History::restored_with_transcript(active, transcript);

        history.truncate(4);
        compact(&mut history, "new summary");

        let [
            TranscriptEntry::Compaction { entries, .. },
            TranscriptEntry::GeneratedMessage(_),
            TranscriptEntry::GeneratedMessage(_),
        ] = history.transcript()
        else {
            panic!("truncated history should remain recursively compactable");
        };
        assert!(matches!(
            entries.first(),
            Some(TranscriptEntry::Compaction { .. })
        ));
        assert_eq!(
            entries
                .iter()
                .filter_map(|entry| match entry {
                    TranscriptEntry::Message(message)
                    | TranscriptEntry::GeneratedMessage(message) => message.user_text(),
                    TranscriptEntry::Compaction { .. } => None,
                })
                .collect::<Vec<_>>(),
            vec!["summary prompt", "summary", "keep", "kept reply"]
        );
    }

    #[test]
    fn truncate_to_empty_clears_compacted_transcript() {
        let mut history = History::new(vec![Message::user("old".into())]);
        compact(&mut history, "summary");

        history.truncate(0);

        assert!(history.as_slice().is_empty());
        assert!(history.transcript().is_empty());
    }

    #[test]
    fn rebuild_preserves_generated_prefix_without_tagging_ordinary_tail() {
        let mut history = History::new(vec![Message::user("old".into())]);
        compact(&mut history, "summary");
        history.push(Message::user("ordinary follow-up".into()));
        history.push(assistant("ordinary answer"));

        history.truncate(4);

        assert!(matches!(
            history.transcript(),
            [
                TranscriptEntry::Compaction { .. },
                TranscriptEntry::GeneratedMessage(_),
                TranscriptEntry::GeneratedMessage(_),
                TranscriptEntry::Message(_),
                TranscriptEntry::Message(_),
            ]
        ));
    }

    #[test]
    fn rebuild_does_not_transfer_generated_tags_to_replacements() {
        let mut history = History::new(vec![Message::user("old".into())]);
        compact(&mut history, "summary");

        history.replace(vec![
            Message::user("ordinary replacement".into()),
            assistant("ordinary answer"),
        ]);

        assert!(matches!(
            history.transcript(),
            [
                TranscriptEntry::Compaction { .. },
                TranscriptEntry::Message(_),
                TranscriptEntry::Message(_),
            ]
        ));
    }

    fn make_tool_use_msg(ids: &[&str]) -> Message {
        Message {
            role: Role::Assistant,
            content: ids
                .iter()
                .map(|id| ContentBlock::ToolUse {
                    id: id.to_string(),
                    name: "read".into(),
                    input: serde_json::json!({}),
                })
                .collect(),
            ..Default::default()
        }
    }

    fn make_tool_result_msg(ids: &[&str]) -> Message {
        Message {
            role: Role::User,
            content: ids
                .iter()
                .map(|id| ContentBlock::ToolResult {
                    tool_use_id: id.to_string(),
                    content: "ok".into(),
                    is_error: false,
                })
                .collect(),
            display_text: Some(String::new()),
        }
    }

    fn make_mirror() -> SharedMessages {
        Arc::new(ArcSwap::from_pointee(Vec::new()))
    }

    #[track_caller]
    fn extract_error_ids(msg: &Message) -> Vec<&str> {
        msg.content
            .iter()
            .filter_map(|b| match b {
                ContentBlock::ToolResult {
                    tool_use_id,
                    is_error: true,
                    ..
                } => Some(tool_use_id.as_str()),
                _ => None,
            })
            .collect()
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
            make_tool_use_msg(&["t1", "t2"]),
        ]);
        sanitize_cancelled_history(&mut history, 0);

        assert_eq!(extract_error_ids(&history.as_slice()[2]), ["t1", "t2"]);
        assert_ends_with_cancel_marker(&history);
    }

    #[test]
    fn mirror_sequential_mutations_always_consistent() {
        let mirror = make_mirror();
        let mut history = History::new(Vec::new()).with_mirror(Arc::clone(&mirror));

        for i in 0..10 {
            history.push(Message::user(format!("msg-{i}")));
            assert_eq!(mirror.load().len(), i + 1);
        }

        history.truncate(3);
        assert_eq!(mirror.load().len(), 3);

        history.replace(vec![Message::user("fresh".into())]);
        assert_eq!(mirror.load().len(), 1);

        history.push(make_tool_use_msg(&["t_final"]));
        assert_eq!(history.len(), 2, "history has 2");
        assert_eq!(mirror.load().len(), 3, "mirror has 3 (dangling closed)");
    }

    #[test]
    fn snapshot_closes_dangling_tool_uses_without_mutating_history() {
        let mirror = make_mirror();
        let history = History::new(vec![
            Message::user("go".into()),
            make_tool_use_msg(&["t1", "t2"]),
        ])
        .with_mirror(Arc::clone(&mirror));

        assert_eq!(history.len(), 2, "history itself unchanged");

        let snap = mirror.load();
        assert_eq!(snap.len(), 3, "snapshot has extra closing message");

        let closing = &snap[2];
        assert!(matches!(closing.role, Role::User));
        assert_eq!(extract_error_ids(closing), ["t1", "t2"]);
        assert_eq!(closing.display_text.as_deref(), Some(""));
    }

    #[test]
    fn snapshot_not_dangling_when_tool_result_already_present() {
        let mirror = make_mirror();
        let mut history =
            History::new(vec![Message::user("go".into()), make_tool_use_msg(&["t1"])])
                .with_mirror(Arc::clone(&mirror));

        assert_eq!(mirror.load().len(), 3, "dangling before result");

        history.push(Message {
            role: Role::User,
            content: vec![ContentBlock::ToolResult {
                tool_use_id: "t1".into(),
                content: "file contents".into(),
                is_error: false,
            }],
            ..Default::default()
        });

        let snap = mirror.load();
        assert_eq!(snap.len(), 3, "no extra closing after real result");
    }

    #[test]
    fn into_vec_returns_inner_not_snapshot() {
        let mirror = make_mirror();
        let history = History::new(vec![Message::user("go".into()), make_tool_use_msg(&["t1"])])
            .with_mirror(Arc::clone(&mirror));

        assert_eq!(mirror.load().len(), 3, "snapshot has closing message");
        assert_eq!(history.into_vec().len(), 2, "into_vec returns raw messages");
    }

    #[test]
    fn sanitize_cancelled_on_mirrored_history() {
        let mirror = make_mirror();
        let mut history =
            History::new(vec![Message::user("go".into()), make_tool_use_msg(&["t1"])])
                .with_mirror(Arc::clone(&mirror));

        sanitize_cancelled_history(&mut history, 0);

        let snap = mirror.load();
        assert_eq!(snap.len(), history.len(), "mirror matches history length");

        let last = snap.last().unwrap();
        assert!(matches!(&last.content[0], ContentBlock::Text { text } if text == CANCEL_MARKER));

        let tool_result_msg = &snap[snap.len() - 2];
        assert!(tool_result_msg.content.iter().any(|b| matches!(
            b,
            ContentBlock::ToolResult { content, is_error: true, .. } if content == CANCEL_MARKER
        )));
    }

    fn text_msg(role: Role, text: &str) -> Message {
        Message {
            role,
            content: vec![ContentBlock::Text { text: text.into() }],
            ..Default::default()
        }
    }

    #[test_case(
        vec![make_tool_result_msg(&["t1"])],
        0
        ; "orphan_at_start_removed"
    )]
    #[test_case(
        vec![
            Message::user("go".into()),
            text_msg(Role::Assistant, "done"),
            make_tool_result_msg(&["orphan1", "orphan2"]),
        ],
        2
        ; "orphans_after_non_tool_assistant_removed"
    )]
    #[test_case(
        vec![
            Message::user("go".into()),
            make_tool_use_msg(&["t1", "t2"]),
            make_tool_result_msg(&["t1", "t2"]),
        ],
        3
        ; "valid_pairing_preserved"
    )]
    #[test_case(
        vec![Message::user("go".into()), make_tool_use_msg(&["t1"])],
        3
        ; "dangling_tool_use_closed_with_synthetic_result"
    )]
    fn sanitize_restored_cases(messages: Vec<Message>, expected_len: usize) {
        let history = History::restored(messages);
        assert_eq!(history.len(), expected_len);
    }

    #[test]
    fn sanitize_restored_drops_image_when_all_results_orphaned() {
        let image_block = ContentBlock::Image {
            source: n00n_providers::ImageSource::new(
                n00n_providers::ImageMediaType::Png,
                std::sync::Arc::from("aGVsbG8="),
            ),
        };
        let mut orphaned = make_tool_result_msg(&["orphan"]);
        orphaned.content.push(image_block.clone());
        let history = History::restored(vec![Message::user("go".into()), orphaned]);
        assert_eq!(history.len(), 1);

        // Chat-pasted image (no tool results) is untouched.
        let history = History::restored(vec![Message {
            role: Role::User,
            content: vec![image_block],
            ..Default::default()
        }]);
        assert_eq!(history.len(), 1);
        assert!(matches!(
            history.as_slice()[0].content[0],
            ContentBlock::Image { .. }
        ));
    }

    #[test]
    fn sanitize_restored_keeps_image_when_any_result_survives() {
        let mut msg = make_tool_result_msg(&["t1", "orphan"]);
        msg.content.push(ContentBlock::Image {
            source: n00n_providers::ImageSource::new(
                n00n_providers::ImageMediaType::Png,
                std::sync::Arc::from("aGVsbG8="),
            ),
        });
        let history = History::restored(vec![
            Message::user("go".into()),
            make_tool_use_msg(&["t1"]),
            msg,
        ]);
        let content = &history.as_slice()[2].content;
        assert_eq!(content.len(), 2);
        assert!(matches!(
            &content[0],
            ContentBlock::ToolResult { tool_use_id, .. } if tool_use_id == "t1"
        ));
        assert!(matches!(content[1], ContentBlock::Image { .. }));
    }

    #[test]
    fn sanitize_restored_partial_orphan_keeps_matched_ids() {
        let history = History::restored(vec![
            Message::user("go".into()),
            make_tool_use_msg(&["t1"]),
            make_tool_result_msg(&["t1", "t2"]),
        ]);
        let results: Vec<&str> = history.as_slice()[2]
            .content
            .iter()
            .filter_map(|b| match b {
                ContentBlock::ToolResult { tool_use_id, .. } => Some(tool_use_id.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(results, ["t1"]);
    }

    #[test_case(vec![Message::user("go".into())], false ; "no_tool_results")]
    #[test_case(
        vec![Message::user("go".into()), make_tool_result_msg(&["t1"])],
        true
        ; "ends_with_tool_result"
    )]
    #[test_case(
        vec![make_tool_result_msg(&["t1"]), Message::user("continue".into())],
        false
        ; "tool_result_is_not_last"
    )]
    fn ends_with_tool_results(messages: Vec<Message>, expected: bool) {
        assert_eq!(History::new(messages).ends_with_tool_results(), expected);
    }
}

use std::sync::Arc;

use flume::Sender;
use futures_lite::future;
use n00n_providers::provider::Provider;
use n00n_providers::{ContentBlock, Message, Model, ProviderEvent, RequestOptions, System};
use serde_json::Value;

use crate::components::btw_modal::BtwEvent;

use super::App;

const BTW_REMINDER: &str = "<system-reminder>\nThis is a side question. Answer it directly in a \
single response.\n- You have NO tools: you cannot read files, run commands, or take any action.\n\
- One-off response: there are no follow-up turns.\n- Answer ONLY from the existing conversation \
context.\n- Never say \"Let me...\", \"I'll now...\", or promise any action.\n- If you don't know, \
say so; do not offer to look it up.\n</system-reminder>";

const BTW_FALLBACK_SYSTEM: &str = "You are a helpful coding assistant. Answer concisely \
from the conversation context.";
const PROVIDER_EVENT_QUEUE_CAPACITY: usize = 256;

/// The reminder leads so the model treats the question as a quick aside, not a task to act on.
pub(crate) fn btw_question(question: &str) -> Message {
    Message::user(format!("{BTW_REMINDER}\n\n{question}"))
}

fn btw_history(messages: &[Message]) -> Vec<Message> {
    messages
        .iter()
        .filter_map(|message| {
            let content = message
                .content
                .iter()
                .filter(|block| {
                    matches!(
                        block,
                        ContentBlock::Text { .. }
                            | ContentBlock::Image { .. }
                            | ContentBlock::File { .. }
                    )
                })
                .cloned()
                .collect::<Vec<_>>();
            (!content.is_empty()).then(|| Message {
                role: message.role.clone(),
                content,
                display_text: None,
                control: message.control,
            })
        })
        .collect()
}

impl App {
    pub(crate) fn start_btw(&mut self, question: &str, provider: Arc<dyn Provider>, model: Model) {
        let mut messages = self
            .shared_history
            .as_ref()
            .map_or_else(Vec::new, |history| btw_history(&history.load()));
        let system = self
            .btw_system
            .as_ref()
            .map(|s| System::clone(&s.load()))
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| System::from(BTW_FALLBACK_SYSTEM));
        messages.push(btw_question(question));

        let (tx, rx) = flume::bounded(64);
        self.btw_modal.open(question, rx);

        smol::spawn(run_btw(provider, model, system, messages, tx)).detach();
    }
}

async fn run_btw(
    provider: Arc<dyn Provider>,
    model: Model,
    system: System,
    messages: Vec<Message>,
    btw_tx: Sender<BtwEvent>,
) {
    let (event_tx, event_rx) = flume::bounded(PROVIDER_EVENT_QUEUE_CAPACITY);
    let tools = Value::Array(vec![]);
    let messages = n00n_providers::adapt_images_for_model(&model, &messages);
    let messages = n00n_providers::adapt_files_for_model(&model, &messages);

    let _permit = n00n_providers::admission::ProviderAdmission::global()
        .acquire(model.provider.as_ref())
        .await;
    let stream_fut = async move {
        let result = provider
            .stream_message(
                &model,
                &messages,
                &system,
                &tools,
                &event_tx,
                RequestOptions::default(),
                None,
            )
            .await;
        drop(event_tx);
        result
    };

    let forward_fut = async {
        while let Ok(event) = event_rx.recv_async().await {
            let (ProviderEvent::TextDelta { text: delta }
            | ProviderEvent::ThinkingDelta { text: delta }) = event
            else {
                continue;
            };
            if btw_tx.send_async(BtwEvent::TextDelta(delta)).await.is_err() {
                return;
            }
        }
    };

    let (result, ()) = future::zip(stream_fut, forward_fut).await;

    match result {
        Ok(_) => {
            let _ = btw_tx.send_async(BtwEvent::Done).await;
        }
        Err(e) => {
            let _ = btw_tx.send_async(BtwEvent::Error(e.to_string())).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    const Q: &str = "why sqlite?";
    const TEST_COMPLETION_TIMEOUT: Duration = Duration::from_secs(5);

    fn user_text(msg: &Message) -> String {
        msg.content
            .iter()
            .filter_map(|b| match b {
                n00n_providers::ContentBlock::Text { text } => Some(text.as_str()),
                _ => None,
            })
            .collect()
    }

    struct ImmediateProvider;

    impl Provider for ImmediateProvider {
        fn stream_message<'a>(
            &'a self,
            _: &'a Model,
            _: &'a [Message],
            _: &'a System,
            _: &'a Value,
            event_tx: &'a flume::Sender<ProviderEvent>,
            _: RequestOptions,
            _: Option<&'a n00n_storage::id::SessionRef>,
        ) -> n00n_providers::provider::BoxFuture<
            'a,
            Result<n00n_providers::StreamResponse, n00n_providers::AgentError>,
        > {
            Box::pin(async move {
                event_tx
                    .send_async(ProviderEvent::TextDelta {
                        text: "first".into(),
                    })
                    .await?;
                event_tx
                    .send_async(ProviderEvent::ThinkingDelta {
                        text: "second".into(),
                    })
                    .await?;
                Ok(n00n_providers::StreamResponse {
                    message: Message::assistant("done".into()),
                    usage: n00n_providers::TokenUsage::default(),
                    stop_reason: Some(n00n_providers::StopReason::EndTurn),
                })
            })
        }

        fn list_models(
            &self,
        ) -> n00n_providers::provider::BoxFuture<
            '_,
            Result<Vec<n00n_providers::ModelInfo>, n00n_providers::AgentError>,
        > {
            Box::pin(async { Ok(Vec::new()) })
        }
    }

    #[test]
    fn completed_provider_finishes_btw_stream() {
        let (btw_tx, btw_rx) = flume::bounded(4);
        let completed = smol::block_on(async {
            future::race(
                async {
                    run_btw(
                        Arc::new(ImmediateProvider),
                        Model::from_spec("anthropic/test").unwrap(),
                        System::from("system"),
                        vec![Message::user("question".into())],
                        btw_tx,
                    )
                    .await;
                    true
                },
                async {
                    smol::Timer::after(TEST_COMPLETION_TIMEOUT).await;
                    false
                },
            )
            .await
        });

        assert!(completed, "completed provider left BTW task parked");
        assert!(matches!(
            btw_rx.try_recv(),
            Ok(BtwEvent::TextDelta(text)) if text == "first"
        ));
        assert!(matches!(
            btw_rx.try_recv(),
            Ok(BtwEvent::TextDelta(text)) if text == "second"
        ));
        assert!(matches!(btw_rx.try_recv(), Ok(BtwEvent::Done)));
    }

    #[test_case::test_case(())]
    fn provider_history_excludes_tool_and_provider_protocol_blocks(_unit: ()) {
        let history = vec![
            Message {
                role: n00n_providers::Role::Assistant,
                content: vec![
                    ContentBlock::Text {
                        text: "working".into(),
                    },
                    ContentBlock::ToolUse {
                        id: "call-1".into(),
                        name: "read_file".into(),
                        input: serde_json::json!({}),
                    },
                    ContentBlock::ProviderItem {
                        provider: "openai".into(),
                        data: serde_json::json!({"type":"reasoning","id":"reason-1"}),
                    },
                    ContentBlock::Thinking {
                        thinking: "private".into(),
                        signature: None,
                    },
                ],
                ..Default::default()
            },
            Message {
                role: n00n_providers::Role::User,
                content: vec![ContentBlock::ToolResult {
                    tool_use_id: "call-1".into(),
                    content: "result".into(),
                    is_error: false,
                }],
                ..Default::default()
            },
        ];

        let sanitized = btw_history(&history);

        assert_eq!(sanitized.len(), 1);
        assert!(matches!(
            sanitized[0].content.as_slice(),
            [ContentBlock::Text { text }] if text == "working"
        ));
    }

    #[test]
    fn injects_reminder_before_question() {
        let text = user_text(&btw_question(Q));
        assert!(text.starts_with(BTW_REMINDER), "reminder leads the message");
        assert!(text.ends_with(Q), "question trails the message");
    }
}

use n00n_providers::provider::Provider;
use n00n_providers::retry::{MAX_RETRIES, RetryState};
use n00n_providers::{Message, Model, ProviderEvent, RequestOptions, StreamResponse};
use n00n_storage::id::SessionRef;
use serde_json::Value;
use tracing::warn;

use crate::cancel::CancelToken;
use crate::{AgentError, AgentEvent, EventSender};

pub(crate) struct StreamContext<'a> {
    pub provider: &'a dyn Provider,
    pub model: &'a Model,
    pub messages: &'a [Message],
    pub system: &'a str,
    pub tools: &'a Value,
    pub event_tx: &'a EventSender,
    pub cancel: &'a CancelToken,
    pub opts: RequestOptions,
    pub session_id: Option<&'a SessionRef>,
}

async fn forward_provider_events(
    prx: flume::Receiver<ProviderEvent>,
    event_tx: &EventSender,
) -> bool {
    let mut emitted_output = false;
    while let Ok(pe) = prx.recv_async().await {
        emitted_output |= matches!(
            pe,
            ProviderEvent::TextDelta { .. }
                | ProviderEvent::ThinkingDelta { .. }
                | ProviderEvent::ToolUseStart { .. }
        );
        let ae = match pe {
            ProviderEvent::TextDelta { text } => AgentEvent::TextDelta { text },
            ProviderEvent::ThinkingDelta { text } => AgentEvent::ThinkingDelta { text },
            ProviderEvent::ToolUseStart { id, name } => AgentEvent::ToolPending { id, name },
            ProviderEvent::PromptProgress {
                processed,
                total,
                cache,
            } => AgentEvent::PromptProgress {
                processed,
                total,
                cache,
            },
        };
        if event_tx.send(ae).is_err() {
            break;
        }
    }
    emitted_output
}

pub(crate) async fn stream_with_retry(
    ctx: StreamContext<'_>,
) -> Result<StreamResponse, AgentError> {
    let opts = ctx.opts.clamped(ctx.model);
    let messages = n00n_providers::adapt_images_for_model(ctx.model, ctx.messages);
    let messages = &*messages;
    let mut retry = RetryState::new();
    loop {
        let (ptx, prx) = flume::unbounded();
        let forwarder = smol::spawn({
            let event_tx = ctx.event_tx.clone();
            async move { forward_provider_events(prx, &event_tx).await }
        });
        let result = futures_lite::future::race(
            ctx.provider.stream_message(
                ctx.model,
                messages,
                ctx.system,
                ctx.tools,
                &ptx,
                opts,
                ctx.session_id,
            ),
            async {
                ctx.cancel.cancelled().await;
                Err(AgentError::Cancelled)
            },
        )
        .await;
        drop(ptx);
        let emitted_output = forwarder.await;
        match result {
            Ok(r) => return Ok(r),
            Err(AgentError::Cancelled) => return Err(AgentError::Cancelled),
            Err(e) if e.is_retryable() && !emitted_output => {
                if e.should_rotate_key()
                    && let Ok(true) = ctx.provider.rotate_key().await
                {
                    warn!("rotated API key after error: {e}");
                }
                let (attempt, delay) = retry.next_delay();
                if attempt > MAX_RETRIES {
                    return Err(e);
                }
                let delay_ms = u64::try_from(delay.as_millis()).unwrap_or_else(|_| u64::MAX);
                warn!(attempt, delay_ms, error = %e, "retryable, will retry");
                ctx.event_tx.send(AgentEvent::Retry {
                    attempt,
                    message: e.retry_message(),
                    delay_ms,
                })?;
                futures_lite::future::race(
                    async {
                        smol::Timer::after(delay).await;
                    },
                    ctx.cancel.cancelled(),
                )
                .await;
                if ctx.cancel.is_cancelled() {
                    return Err(AgentError::Cancelled);
                }
            }
            Err(e) => return Err(e),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use n00n_providers::provider::BoxFuture;

    use super::*;
    use crate::Envelope;

    struct RequestSentProvider {
        calls: AtomicUsize,
    }

    impl Provider for RequestSentProvider {
        fn stream_message<'a>(
            &'a self,
            _: &'a Model,
            _: &'a [Message],
            _: &'a str,
            _: &'a Value,
            _: &'a flume::Sender<ProviderEvent>,
            _: RequestOptions,
            _: Option<&'a SessionRef>,
        ) -> BoxFuture<'a, Result<StreamResponse, AgentError>> {
            self.calls.fetch_add(1, Ordering::Relaxed);
            Box::pin(async {
                Err(AgentError::RequestSent {
                    message: "fake transport closed after send".into(),
                    metadata: None,
                })
            })
        }

        fn list_models(&self) -> BoxFuture<'_, Result<Vec<n00n_providers::ModelInfo>, AgentError>> {
            Box::pin(async { Ok(Vec::new()) })
        }
    }

    #[test]
    fn task_agent_post_send_transport_failure_is_not_retried() {
        smol::block_on(async {
            let provider = RequestSentProvider {
                calls: AtomicUsize::new(0),
            };
            let model = Model::from_spec("openai/gpt-5.6").unwrap();
            let (agent_tx, _agent_rx) = flume::unbounded::<Envelope>();
            let event_tx = EventSender::new(agent_tx, 1);

            let result = stream_with_retry(StreamContext {
                provider: &provider,
                model: &model,
                messages: &[Message::user("task".into())],
                system: "system",
                tools: &serde_json::json!([]),
                event_tx: &event_tx,
                cancel: &CancelToken::none(),
                opts: RequestOptions::default(),
                session_id: None,
            })
            .await;

            assert!(matches!(result, Err(AgentError::RequestSent { .. })));
            assert_eq!(provider.calls.load(Ordering::Relaxed), 1);
        });
    }

    #[test]
    fn forwarded_content_marks_attempt_as_non_retryable() {
        smol::block_on(async {
            let (provider_tx, provider_rx) = flume::unbounded();
            let (agent_tx, _agent_rx) = flume::unbounded::<Envelope>();
            let event_tx = EventSender::new(agent_tx, 1);
            provider_tx
                .send(ProviderEvent::PromptProgress {
                    processed: 1,
                    total: 2,
                    cache: 0,
                })
                .unwrap();
            provider_tx
                .send(ProviderEvent::TextDelta { text: "a".into() })
                .unwrap();
            drop(provider_tx);

            assert!(forward_provider_events(provider_rx, &event_tx).await);
        });
    }

    #[test]
    fn prompt_progress_alone_allows_retry() {
        smol::block_on(async {
            let (provider_tx, provider_rx) = flume::unbounded();
            let (agent_tx, _agent_rx) = flume::unbounded::<Envelope>();
            let event_tx = EventSender::new(agent_tx, 1);
            provider_tx
                .send(ProviderEvent::PromptProgress {
                    processed: 1,
                    total: 2,
                    cache: 0,
                })
                .unwrap();
            drop(provider_tx);

            assert!(!forward_provider_events(provider_rx, &event_tx).await);
        });
    }
}

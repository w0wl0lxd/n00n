use n00n_providers::provider::Provider;
use n00n_providers::retry::{MAX_TIMEOUT_RETRIES, RetryState};
use n00n_providers::{Message, Model, ProviderEvent, RequestOptions, StreamResponse};
use n00n_storage::id::SessionRef;
use serde_json::Value;
use tracing::warn;

use crate::cancel::CancelToken;
use crate::{AgentError, AgentEvent, EventSender};

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

#[allow(clippy::too_many_arguments)]
pub(crate) async fn stream_with_retry(
    provider: &dyn Provider,
    model: &Model,
    messages: &[Message],
    system: &str,
    tools: &Value,
    event_tx: &EventSender,
    cancel: &CancelToken,
    opts: RequestOptions,
    session_id: Option<&SessionRef>,
) -> Result<StreamResponse, AgentError> {
    let opts = opts.clamped(model);
    let messages = n00n_providers::adapt_images_for_model(model, messages);
    let messages = &*messages;
    let mut retry = RetryState::new();
    loop {
        let (ptx, prx) = flume::unbounded();
        let forwarder = smol::spawn({
            let event_tx = event_tx.clone();
            async move { forward_provider_events(prx, &event_tx).await }
        });
        let result = futures_lite::future::race(
            provider.stream_message(model, messages, system, tools, &ptx, opts, session_id),
            async {
                cancel.cancelled().await;
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
                    && let Ok(true) = provider.rotate_key().await
                {
                    warn!("rotated API key after error: {e}");
                }
                let (attempt, delay) = retry.next_delay();
                if matches!(e, AgentError::Timeout { .. }) && attempt > MAX_TIMEOUT_RETRIES {
                    return Err(e);
                }
                let delay_ms = delay.as_millis() as u64;
                warn!(attempt, delay_ms, error = %e, "retryable, will retry");
                event_tx.send(AgentEvent::Retry {
                    attempt,
                    message: e.retry_message(),
                    delay_ms,
                })?;
                futures_lite::future::race(
                    async {
                        smol::Timer::after(delay).await;
                    },
                    cancel.cancelled(),
                )
                .await;
                if cancel.is_cancelled() {
                    return Err(AgentError::Cancelled);
                }
            }
            Err(e) => return Err(e),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Envelope;

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

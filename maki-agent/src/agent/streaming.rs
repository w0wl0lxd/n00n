use std::sync::Arc;
use std::time::Duration;

use event_listener::Event;
use maki_providers::provider::Provider;
use maki_providers::retry::{MAX_TIMEOUT_RETRIES, RetryState};
use maki_providers::{Message, Model, ProviderEvent, StreamResponse};
use serde_json::Value;
use tracing::warn;

use crate::cancel::CancelToken;
use crate::{AgentError, AgentEvent, EventSender};

const STREAM_INACTIVITY_TIMEOUT: Duration = Duration::from_secs(30);

async fn forward_provider_events(
    prx: flume::Receiver<ProviderEvent>,
    event_tx: &EventSender,
    activity: &Event,
) {
    while let Ok(pe) = prx.recv_async().await {
        activity.notify(usize::MAX);
        let ae = match pe {
            ProviderEvent::TextDelta { text } => AgentEvent::TextDelta { text },
            ProviderEvent::ThinkingDelta { text } => AgentEvent::ThinkingDelta { text },
            ProviderEvent::ToolUseStart { id, name } => AgentEvent::ToolPending { id, name },
        };
        if event_tx.send(ae).is_err() {
            break;
        }
    }
}

async fn wait_for_inactivity(activity: &Event, timeout: Duration) {
    loop {
        let listener = activity.listen();
        let timed_out = futures_lite::future::or(
            async {
                async_io::Timer::after(timeout).await;
                true
            },
            async {
                listener.await;
                false
            },
        )
        .await;
        if timed_out {
            return;
        }
    }
}

pub(crate) async fn stream_with_retry(
    provider: &dyn Provider,
    model: &Model,
    messages: &[Message],
    system: &str,
    tools: &Value,
    event_tx: &EventSender,
    cancel: &CancelToken,
) -> Result<StreamResponse, AgentError> {
    let mut retry = RetryState::new();
    loop {
        let (ptx, prx) = flume::unbounded();
        let activity = Arc::new(Event::new());
        let forwarder = smol::spawn({
            let event_tx = event_tx.clone();
            let activity = Arc::clone(&activity);
            async move { forward_provider_events(prx, &event_tx, &activity).await }
        });
        let result = futures_lite::future::race(
            futures_lite::future::race(
                provider.stream_message(model, messages, system, tools, &ptx),
                async {
                    cancel.cancelled().await;
                    Err(AgentError::Cancelled)
                },
            ),
            async {
                wait_for_inactivity(&activity, STREAM_INACTIVITY_TIMEOUT).await;
                Err(AgentError::Timeout {
                    secs: STREAM_INACTIVITY_TIMEOUT.as_secs(),
                })
            },
        )
        .await;
        drop(ptx);
        let _ = forwarder.await;
        match result {
            Ok(r) => return Ok(r),
            Err(AgentError::Cancelled) => return Err(AgentError::Cancelled),
            Err(e) if e.is_retryable() => {
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
                        async_io::Timer::after(delay).await;
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

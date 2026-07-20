use std::time::{Duration, Instant};

use async_tungstenite::WebSocketStream;
use async_tungstenite::tungstenite::http::Request;
use async_tungstenite::tungstenite::{Error as WsError, Message as WsMessage};
use flume::Sender;
use futures_lite::StreamExt;
use serde_json::{Value, json};
use smol::Timer;
use tracing::debug;

use super::responses::{ResponseAccumulator, build_body};
use crate::model::Model;
use crate::providers::ResolvedAuth;
use crate::{AgentError, Message, ProviderEvent, StreamResponse};

const RESPONSES_WS_URL: &str = "wss://api.openai.com/v1/responses";

pub(crate) fn is_websocket_model(model_id: &str) -> bool {
    model_id.starts_with("gpt-5.6")
}

pub(crate) async fn stream_message(
    model: &Model,
    messages: &[Message],
    system: &str,
    tools: &Value,
    event_tx: &Sender<ProviderEvent>,
    auth: &ResolvedAuth,
    stream_timeout: Duration,
) -> Result<StreamResponse, AgentError> {
    let mut builder = Request::builder().uri(RESPONSES_WS_URL).method("GET");
    for (key, value) in &auth.headers {
        builder = builder.header(key.as_str(), value.as_str());
    }
    let request = builder.body(()).map_err(|e| AgentError::Config {
        message: e.to_string(),
    })?;

    let (mut ws, _) = async_tungstenite::smol::connect_async(request)
        .await
        .map_err(ws_err)?;

    let mut create_event = build_body(model, messages, system, tools)
        .as_object()
        .cloned()
        .unwrap_or_default();
    create_event.remove("stream");
    create_event.insert("type".into(), json!("response.create"));
    send_json(&mut ws, &Value::Object(create_event)).await?;

    let mut acc = ResponseAccumulator::new();
    let mut deadline = Instant::now() + stream_timeout;
    loop {
        let msg = next_message(&mut ws, &mut deadline, stream_timeout).await?;
        match msg {
            WsMessage::Text(text) => {
                let event: Value = serde_json::from_str(&text).map_err(AgentError::Json)?;
                match event.get("type").and_then(Value::as_str) {
                    Some("error") => return Err(error_from_event(&event)),
                    Some(event_type) if acc.handle_event(event_type, &event, event_tx).await? => {
                        break;
                    }
                    _ => {}
                }
            }
            WsMessage::Close(_) => break,
            _ => {}
        }
    }

    let _ = ws.close(None).await;
    Ok(acc.into_stream_response())
}

fn ws_err(e: WsError) -> AgentError {
    match e {
        WsError::Io(io) => AgentError::Io(io),
        other => AgentError::Api {
            status: 500,
            message: other.to_string(),
        },
    }
}

async fn send_json<S>(ws: &mut WebSocketStream<S>, value: &Value) -> Result<(), AgentError>
where
    S: futures_lite::AsyncRead + futures_lite::AsyncWrite + Unpin + Send,
{
    let text = value.to_string();
    debug!(event = %value["type"].as_str().unwrap_or("unknown"), bytes = text.len(), "sending websocket event");
    ws.send(WsMessage::Text(text.into())).await.map_err(ws_err)
}

async fn next_message<S>(
    ws: &mut WebSocketStream<S>,
    deadline: &mut Instant,
    timeout: Duration,
) -> Result<WsMessage, AgentError>
where
    S: futures_lite::AsyncRead + futures_lite::AsyncWrite + Unpin + Send,
{
    let remaining = deadline.saturating_duration_since(Instant::now());
    let result: Result<Option<WsMessage>, AgentError> = futures_lite::future::or(
        async { ws.next().await.transpose().map_err(ws_err) },
        async {
            Timer::after(remaining).await;
            Err(AgentError::Timeout {
                secs: timeout.as_secs(),
            })
        },
    )
    .await;

    if matches!(result, Ok(Some(WsMessage::Text(_) | WsMessage::Binary(_)))) {
        *deadline = Instant::now() + timeout;
    }

    match result {
        Ok(Some(msg)) => Ok(msg),
        Ok(None) => Err(AgentError::Api {
            status: 500,
            message: "websocket closed unexpectedly".into(),
        }),
        Err(e) => Err(e),
    }
}

fn error_from_event(event: &Value) -> AgentError {
    let err = event
        .get("error")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    let error_type = err.get("type").and_then(Value::as_str).unwrap_or("");
    let message = err
        .get("message")
        .and_then(Value::as_str)
        .unwrap_or("websocket error")
        .to_string();

    let status = if let Some(s) = event.get("status").and_then(Value::as_u64) {
        s as u16
    } else {
        match error_type {
            "overloaded_error" => 529,
            "api_error" | "server_error" => 500,
            "rate_limit_error" | "rate_limit_exceeded" | "tokens" => 429,
            "request_too_large" => 413,
            "not_found_error" => 404,
            "permission_error" => 403,
            "billing_error" | "insufficient_quota" => 402,
            "authentication_error" | "invalid_api_key" => 401,
            _ => 400,
        }
    };

    AgentError::Api { status, message }
}

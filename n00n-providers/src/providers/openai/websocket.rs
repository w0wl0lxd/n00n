use std::time::{Duration, Instant};

use async_tungstenite::WebSocketStream;
use async_tungstenite::tungstenite::client::IntoClientRequest;
use async_tungstenite::tungstenite::http::{HeaderName, HeaderValue};
use async_tungstenite::tungstenite::{Error as WsError, Message as WsMessage};
use flume::Sender;
use futures_lite::StreamExt;
use serde_json::{Value, json};
use smol::Timer;
use tracing::debug;

use super::responses::ResponseAccumulator;
use crate::providers::ResolvedAuth;
use crate::{AgentError, ProviderEvent, StreamResponse};

#[cfg(test)]
use super::responses::build_body;
#[cfg(test)]
use crate::model::Model;
#[cfg(test)]
use crate::{Message, RequestOptions, dialect};

const DEFAULT_RESPONSES_WS_URL: &str = "wss://api.openai.com/v1/responses";

pub(crate) fn is_websocket_model(model_id: &str) -> bool {
    model_id.starts_with("gpt-5.6") && !model_id.contains("-codex")
}

#[cfg(test)]
pub(crate) fn build_request_body(
    model: &Model,
    messages: &[Message],
    system: &str,
    tools: &Value,
    opts: RequestOptions,
) -> Value {
    let mut body = build_body(model, messages, system, tools, None, None);
    if let Some(effort) = opts.thinking.effort_str(&dialect::STANDARD, model) {
        body["reasoning"] = json!({"effort": effort});
    }
    body
}

fn build_create_event(body: &Value) -> Value {
    let mut event = body.as_object().cloned().unwrap_or_default();
    event.remove("stream");
    event.insert("type".into(), json!("response.create"));
    Value::Object(event)
}

fn responses_websocket_url(base_url: Option<&str>) -> String {
    let Some(base) = base_url else {
        return DEFAULT_RESPONSES_WS_URL.into();
    };

    let mut url = if let Some(rest) = base.strip_prefix("https://") {
        format!("wss://{rest}")
    } else if let Some(rest) = base.strip_prefix("http://") {
        format!("ws://{rest}")
    } else {
        format!("wss://{base}")
    };

    if !url.ends_with("/responses") {
        if !url.ends_with('/') {
            url.push('/');
        }
        url.push_str("responses");
    }

    url
}

pub(crate) async fn stream_message(
    body: Value,
    event_tx: &Sender<ProviderEvent>,
    auth: &ResolvedAuth,
    stream_timeout: Duration,
) -> Result<(Option<String>, StreamResponse), AgentError> {
    let url = responses_websocket_url(auth.base_url.as_deref());
    let mut request = url.into_client_request().map_err(ws_err)?;
    for (key, value) in &auth.headers {
        let name = key.parse::<HeaderName>().map_err(|e| AgentError::Config {
            message: format!("invalid WebSocket header name {key}: {e}"),
        })?;
        let value = value
            .parse::<HeaderValue>()
            .map_err(|e| AgentError::Config {
                message: format!("invalid WebSocket header value for {key}: {e}"),
            })?;
        request.headers_mut().insert(name, value);
    }

    let (mut ws, _) = async_tungstenite::smol::connect_async(request)
        .await
        .map_err(ws_err)?;

    let create_event = build_create_event(&body);
    send_json(&mut ws, &create_event).await?;

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
            WsMessage::Close(_) => {
                return Err(AgentError::Api {
                    status: 422,
                    message: "Responses WebSocket closed without a terminal event".into(),
                });
            }
            _ => {}
        }
    }

    let _ = ws.close(None).await;
    let response_id = acc.response_id().map(std::string::ToString::to_string);
    Ok((response_id, acc.into_stream_response()))
}

fn ws_err(e: WsError) -> AgentError {
    match e {
        WsError::Io(io) => AgentError::Io(io),
        WsError::Http(resp) => {
            let status = resp.status().as_u16();
            let message = resp.body().as_ref().map_or_else(
                || "websocket handshake failed".into(),
                |b| String::from_utf8_lossy(b).into_owned(),
            );
            AgentError::Api { status, message }
        }
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

    if matches!(result, Ok(Some(_))) {
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use test_case::test_case;

    #[test_case("gpt-5.6", true)]
    #[test_case("gpt-5.6-luna", true)]
    #[test_case("gpt-5.6-terra-preview", true)]
    #[test_case("gpt-5.6-codex", false ; "codex_models_use_http")]
    #[test_case("gpt-5.5", false)]
    #[test_case("gpt-5.3-codex", false)]
    fn is_websocket_model_recognizes_gpt_5_6(model_id: &str, expected: bool) {
        assert_eq!(is_websocket_model(model_id), expected);
    }

    #[test_case(None, "wss://api.openai.com/v1/responses")]
    #[test_case(Some("https://api.openai.com/v1"), "wss://api.openai.com/v1/responses")]
    #[test_case(
        Some("https://proxy.example.com/v1/"),
        "wss://proxy.example.com/v1/responses"
    )]
    #[test_case(Some("http://local.dev"), "ws://local.dev/responses")]
    #[test_case(
        Some(crate::providers::openai::auth::CODING_PLAN_BASE_URL),
        "wss://chatgpt.com/backend-api/codex/responses"
    )]
    fn responses_websocket_url_derives_from_base_url(base_url: Option<&str>, expected: &str) {
        assert_eq!(super::responses_websocket_url(base_url), expected);
    }

    #[test]
    fn create_event_uses_responses_reasoning_shape() {
        let model = Model::from_spec("openai/gpt-5.6").unwrap();
        let opts = RequestOptions {
            thinking: crate::ThinkingConfig::Effort(crate::Effort::High),
            ..Default::default()
        };
        let body = build_request_body(&model, &[], "system", &json!([]), opts);
        let event = build_create_event(&body);
        assert_eq!(event["reasoning"], json!({"effort":"high"}));
        assert!(event.get("reasoning_effort").is_none());
        assert_eq!(event["type"], "response.create");
        assert!(event.get("stream").is_none());
    }

    #[test]
    fn websocket_request_includes_handshake_headers() {
        let request =
            responses_websocket_url(Some(crate::providers::openai::auth::CODING_PLAN_BASE_URL))
                .into_client_request()
                .unwrap();
        for header in [
            "host",
            "upgrade",
            "connection",
            "sec-websocket-key",
            "sec-websocket-version",
        ] {
            assert!(request.headers().contains_key(header), "missing {header}");
        }
    }

    #[test]
    fn ws_err_extracts_http_status_and_body() {
        let resp = async_tungstenite::tungstenite::http::Response::builder()
            .status(401)
            .body(Some(b"bad key".to_vec()))
            .unwrap();
        match super::ws_err(WsError::Http(Box::new(resp))) {
            AgentError::Api { status, message } => {
                assert_eq!(status, 401);
                assert_eq!(message, "bad key");
            }
            other => panic!("expected AgentError::Api, got {other:?}"),
        }
    }

    #[test]
    fn error_from_event_uses_status_field_and_error_type() {
        let event = json!({
            "type": "error",
            "status": 429,
            "error": { "type": "rate_limit_error", "message": "Rate limit hit" }
        });
        match error_from_event(&event) {
            AgentError::Api { status, message } => {
                assert_eq!(status, 429);
                assert_eq!(message, "Rate limit hit");
            }
            other => panic!("expected AgentError::Api, got {other:?}"),
        }
    }

    #[test]
    fn error_from_event_maps_error_type_to_status_when_status_missing() {
        let event = json!({
            "type": "error",
            "error": { "type": "authentication_error", "message": "bad key" }
        });
        match error_from_event(&event) {
            AgentError::Api { status, message } => {
                assert_eq!(status, 401);
                assert_eq!(message, "bad key");
            }
            other => panic!("expected AgentError::Api, got {other:?}"),
        }
    }
}

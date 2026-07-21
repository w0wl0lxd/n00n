use std::io::{Error as IoError, ErrorKind};
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

use super::responses::{ResponseAccumulator, build_body};
use crate::model::Model;
use crate::providers::ResolvedAuth;
use crate::{AgentError, Message, ProviderEvent, RequestOptions, StreamResponse, dialect};

const DEFAULT_RESPONSES_WS_URL: &str = "wss://api.openai.com/v1/responses";
const RESPONSES_WEBSOCKET_BETA: &str = "responses_websockets=2026-02-06";
const MAX_CONNECTION_AGE: Duration = Duration::from_mins(55);

type ResponsesSocket = WebSocketStream<async_tungstenite::smol::ConnectStream>;

pub(crate) struct ResponsesWebSocket {
    socket: ResponsesSocket,
    opened_at: Instant,
}

pub(crate) struct WebSocketAttemptError {
    pub(crate) error: AgentError,
    pub(crate) emitted_event: bool,
    pub(crate) transport_failure: bool,
    pub(crate) request_sent: bool,
    pub(crate) reconnect_safe: bool,
}

impl WebSocketAttemptError {
    pub(crate) fn transport(error: AgentError, emitted_event: bool, request_sent: bool) -> Self {
        Self {
            error,
            emitted_event,
            transport_failure: true,
            request_sent,
            reconnect_safe: false,
        }
    }

    fn response(error: AgentError, emitted_event: bool) -> Self {
        Self {
            error,
            emitted_event,
            transport_failure: false,
            request_sent: true,
            reconnect_safe: false,
        }
    }

    fn reconnect(error: AgentError) -> Self {
        Self {
            error,
            emitted_event: false,
            transport_failure: true,
            request_sent: true,
            reconnect_safe: true,
        }
    }
}

/// Select a process-wide Rustls provider before its configuration builder
/// attempts feature-based auto-detection.
///
/// A binary can enable both `ring` and `aws-lc-rs` through its dependency
/// graph. Rustls intentionally refuses to choose between them.
fn ensure_rustls_crypto_provider() {
    if rustls::crypto::CryptoProvider::get_default().is_none()
        && rustls::crypto::ring::default_provider()
            .install_default()
            .is_err()
    {
        debug!("Rustls CryptoProvider was installed concurrently");
    }
}

pub(crate) fn build_request_body(
    model: &Model,
    messages: &[Message],
    system: &str,
    tools: &Value,
    opts: RequestOptions,
    previous_response_id: Option<&str>,
    prompt_cache_key: Option<&str>,
    store: bool,
) -> Value {
    let mut body = build_body(
        model,
        messages,
        system,
        tools,
        previous_response_id,
        prompt_cache_key,
        store,
    );
    if let Some(effort) = opts.thinking.effort_str(&dialect::STANDARD, model) {
        body["reasoning"]["effort"] = json!(effort);
    }
    body
}

fn build_create_event(body: &Value) -> Value {
    let mut event = body
        .as_object()
        .cloned()
        .unwrap_or_else(serde_json::Map::new);
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

#[allow(clippy::large_futures)]
pub(crate) async fn stream_message(
    body: &Value,
    event_tx: &Sender<ProviderEvent>,
    auth: &ResolvedAuth,
    connect_timeout: Duration,
    stream_timeout: Duration,
) -> Result<(Option<String>, StreamResponse), WebSocketAttemptError> {
    let mut connection = ResponsesWebSocket::connect(auth, connect_timeout)
        .await
        .map_err(|error| WebSocketAttemptError::transport(error, false, false))?;
    let result = connection
        .stream_message(body, event_tx, stream_timeout)
        .await;
    connection.close().await;
    result
}

impl ResponsesWebSocket {
    #[allow(clippy::large_futures)]
    pub(crate) async fn connect(
        auth: &ResolvedAuth,
        connect_timeout: Duration,
    ) -> Result<Self, AgentError> {
        ensure_rustls_crypto_provider();

        let url = responses_websocket_url(auth.base_url.as_deref());
        let mut request = url.into_client_request().map_err(ws_err)?;
        request.headers_mut().insert(
            HeaderName::from_static("openai-beta"),
            HeaderValue::from_static(RESPONSES_WEBSOCKET_BETA),
        );
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

        let connect = async_tungstenite::smol::connect_async(request);
        let (socket, _) = {
            #[allow(clippy::large_futures)]
            futures_lite::future::or(async { connect.await.map_err(ws_err) }, async {
                Timer::after(connect_timeout).await;
                Err(AgentError::Timeout {
                    secs: connect_timeout.as_secs(),
                })
            })
            .await?
        };
        Ok(Self {
            socket,
            opened_at: Instant::now(),
        })
    }

    pub(crate) fn is_expired(&self) -> bool {
        self.opened_at.elapsed() >= MAX_CONNECTION_AGE
    }

    pub(crate) async fn stream_message(
        &mut self,
        body: &Value,
        event_tx: &Sender<ProviderEvent>,
        stream_timeout: Duration,
    ) -> Result<(Option<String>, StreamResponse), WebSocketAttemptError> {
        let create_event = build_create_event(body);
        send_json(&mut self.socket, &create_event)
            .await
            .map_err(|error| WebSocketAttemptError::transport(error, false, true))?;

        let mut acc = ResponseAccumulator::new();
        let mut deadline = Instant::now() + stream_timeout;
        loop {
            let msg = next_message(&mut self.socket, &mut deadline, stream_timeout)
                .await
                .map_err(|error| {
                    WebSocketAttemptError::transport(error, acc.emitted_event(), true)
                })?;
            match msg {
                WsMessage::Text(text) => {
                    let event: Value = serde_json::from_str(&text).map_err(|error| {
                        WebSocketAttemptError::transport(
                            AgentError::Json(error),
                            acc.emitted_event(),
                            true,
                        )
                    })?;
                    match event.get("type").and_then(Value::as_str) {
                        Some("error") => {
                            let error = error_from_event(&event);
                            if is_connection_limit_event(&event) && !acc.emitted_event() {
                                return Err(WebSocketAttemptError::reconnect(error));
                            }
                            let transport_failure = matches!(&error, AgentError::Io(_));
                            return Err(if transport_failure {
                                WebSocketAttemptError::transport(error, acc.emitted_event(), true)
                            } else {
                                WebSocketAttemptError::response(error, acc.emitted_event())
                            });
                        }
                        Some(event_type) => {
                            match acc.handle_event(event_type, &event, event_tx).await {
                                Ok(true) => break,
                                Ok(false) => {}
                                Err(error)
                                    if is_connection_limit_event(&event)
                                        && !acc.emitted_event() =>
                                {
                                    return Err(WebSocketAttemptError::reconnect(error));
                                }
                                Err(error) => {
                                    return Err(WebSocketAttemptError::response(
                                        error,
                                        acc.emitted_event(),
                                    ));
                                }
                            }
                        }
                        None => {}
                    }
                }
                WsMessage::Ping(payload) => {
                    self.socket
                        .send(WsMessage::Pong(payload))
                        .await
                        .map_err(ws_err)
                        .map_err(|error| {
                            WebSocketAttemptError::transport(error, acc.emitted_event(), true)
                        })?;
                }
                WsMessage::Close(_) => {
                    return Err(WebSocketAttemptError::transport(
                        IoError::new(
                            ErrorKind::ConnectionAborted,
                            "Responses WebSocket closed without a terminal event",
                        )
                        .into(),
                        acc.emitted_event(),
                        true,
                    ));
                }
                _ => {}
            }
        }

        let response_id = acc.response_id().map(ToOwned::to_owned);
        Ok((response_id, acc.into_stream_response()))
    }

    async fn close(&mut self) {
        let _ = self.socket.close(None).await;
    }
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
    let event_type = value["type"].as_str().map_or_else(|| "unknown", |s| s);
    debug!(event = %event_type, bytes = text.len(), "sending websocket event");
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
        Ok(None) => Err(IoError::new(
            ErrorKind::UnexpectedEof,
            "Responses WebSocket closed unexpectedly",
        )
        .into()),
        Err(e) => Err(e),
    }
}

fn is_connection_limit_event(event: &Value) -> bool {
    event
        .pointer("/error/code")
        .or_else(|| event.pointer("/response/error/code"))
        .and_then(Value::as_str)
        == Some("websocket_connection_limit_reached")
}

fn error_from_event(event: &Value) -> AgentError {
    let err = event
        .get("error")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_else(serde_json::Map::new);
    let error_type = err
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or_else(|| "");
    let error_code = err
        .get("code")
        .and_then(Value::as_str)
        .unwrap_or_else(|| "");
    let raw_message = err
        .get("message")
        .and_then(Value::as_str)
        .unwrap_or_else(|| "websocket error");

    if error_code == "websocket_connection_limit_reached" {
        return IoError::new(ErrorKind::ConnectionAborted, raw_message).into();
    }
    let message = if error_code.is_empty() {
        raw_message.to_owned()
    } else {
        format!("{error_code}: {raw_message}")
    };

    let status = if let Some(s) = event.get("status").and_then(Value::as_u64) {
        #[allow(clippy::manual_unwrap_or)]
        match u16::try_from(s) {
            Ok(v) => v,
            Err(_) => 500,
        }
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
        let body = build_request_body(&model, &[], "system", &json!([]), opts, None, None, true);
        let event = build_create_event(&body);
        assert_eq!(
            event["reasoning"],
            json!({"effort":"high","summary":"auto"})
        );
        assert!(event.get("reasoning_effort").is_none());
        assert_eq!(event["type"], "response.create");
        assert_eq!(event["store"], true);
        assert!(event.get("stream").is_none());
    }

    #[test]
    fn accumulator_captures_response_id_from_websocket_event() {
        smol::block_on(async {
            let (event_tx, _) = flume::unbounded();
            let mut accumulator = ResponseAccumulator::new();
            let completed = accumulator
                .handle_event(
                    "response.created",
                    &json!({"response": {"id": "resp_1"}}),
                    &event_tx,
                )
                .await
                .unwrap();

            assert!(!completed);
            assert_eq!(accumulator.response_id(), Some("resp_1"));
        });
    }
    #[test]
    #[allow(clippy::large_futures)]
    fn fake_transport_close_after_send_is_not_synthetic_422() {
        smol::block_on(async {
            let listener = smol::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            let server = smol::spawn(async move {
                let (stream, _) = listener.accept().await.unwrap();
                let mut socket = async_tungstenite::accept_async(stream).await.unwrap();
                let _ = socket.next().await;
                socket.close(None).await.unwrap();
            });
            let auth = ResolvedAuth {
                base_url: Some(format!("http://{address}/v1")),
                headers: Vec::new(),
            };
            let mut connection = ResponsesWebSocket::connect(&auth, Duration::from_secs(2))
                .await
                .unwrap();
            let (event_tx, _) = flume::unbounded();
            let error = connection
                .stream_message(
                    &json!({"model":"test","input":[]}),
                    &event_tx,
                    Duration::from_secs(2),
                )
                .await
                .unwrap_err();
            server.await;

            assert!(error.request_sent);
            assert!(error.transport_failure);
            assert!(!matches!(error.error, AgentError::Api { status: 422, .. }));
        });
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
    fn rustls_crypto_provider_is_selected_before_building_a_client_config() {
        super::ensure_rustls_crypto_provider();
        assert!(rustls::crypto::CryptoProvider::get_default().is_some());
        let _ = rustls::ClientConfig::builder();
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

    #[test]
    fn error_from_event_preserves_previous_response_not_found_code() {
        let event = json!({
            "type": "error",
            "status": 400,
            "error": {
                "code": "previous_response_not_found",
                "message": "Previous response not found"
            }
        });
        match error_from_event(&event) {
            AgentError::Api { status, message } => {
                assert_eq!(status, 400);
                assert!(message.starts_with("previous_response_not_found:"));
            }
            other => panic!("expected AgentError::Api, got {other:?}"),
        }
    }

    #[test]
    fn connection_limit_events_require_a_fresh_socket() {
        let direct = json!({
            "type": "error",
            "error": { "code": "websocket_connection_limit_reached" }
        });
        let failed = json!({
            "type": "response.failed",
            "response": {
                "error": { "code": "websocket_connection_limit_reached" }
            }
        });

        assert!(is_connection_limit_event(&direct));
        assert!(is_connection_limit_event(&failed));
    }

    #[test]
    fn connection_limit_error_is_retryable() {
        let event = json!({
            "type": "error",
            "status": 400,
            "error": {
                "code": "websocket_connection_limit_reached",
                "message": "Create a new websocket connection"
            }
        });
        assert!(error_from_event(&event).is_retryable());
    }
}

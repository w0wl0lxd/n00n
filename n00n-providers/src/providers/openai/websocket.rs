use std::io::{Error as IoError, ErrorKind};
use std::time::{Duration, Instant, SystemTime};

use async_tungstenite::WebSocketStream;
use async_tungstenite::tungstenite::client::IntoClientRequest;
use async_tungstenite::tungstenite::http::{HeaderName, HeaderValue};
use async_tungstenite::tungstenite::protocol::CloseFrame;
use async_tungstenite::tungstenite::{Error as WsError, Message as WsMessage};
use flume::Sender;
use futures::SinkExt;
use futures_lite::StreamExt;
use serde_json::{Value, json};
use smol::Timer;
use tracing::{debug, warn};

use super::responses::{
    ResponseAccumulator, is_semantic_progress_event, response_in_flight_timeout,
};
use crate::model::Model;
use crate::providers::ResolvedAuth;
use crate::{
    AgentError, Message, ProviderEvent, RequestDeliveryMetadata, RequestDeliveryPhase,
    RequestOptions, StreamResponse, System,
};

const DEFAULT_RESPONSES_WS_URL: &str = "wss://api.openai.com/v1/responses";
const RESPONSES_WEBSOCKET_BETA: &str = "responses_websockets=2026-02-06";
const MAX_CONNECTION_AGE: Duration = Duration::from_mins(55);
const CONNECTION_RETIRE_MIN_MARGIN: Duration = Duration::from_secs(10);
const MAX_POOL_IDLE: Duration = Duration::from_secs(30);
const KEEPALIVE_INTERVAL: Duration = Duration::from_secs(20);
const PREFLIGHT_PING_PAYLOAD: &[u8] = b"n00n-preflight";
const MAX_SAFE_CLOSE_REASON_CHARS: usize = 120;
const MALFORMED_RETRY_AFTER_DELAY: Duration = Duration::from_mins(1);

type ResponsesSocket = WebSocketStream<async_tungstenite::smol::ConnectStream>;

pub(crate) struct ResponsesWebSocket {
    socket: ResponsesSocket,
    opened_at: Instant,
    available_since: Instant,
    validated_for_send: bool,
}

#[derive(Debug)]
pub(crate) struct WebSocketAttemptError {
    pub(crate) error: AgentError,
    pub(crate) transport_failure: bool,
    pub(crate) delivery: RequestDeliveryMetadata,
}

impl WebSocketAttemptError {
    pub(crate) fn transport(
        error: AgentError,
        emitted_event: bool,
        mut delivery: RequestDeliveryMetadata,
    ) -> Self {
        delivery.emitted_event = emitted_event;
        Self {
            error,
            transport_failure: true,
            delivery,
        }
    }

    fn response(
        error: AgentError,
        emitted_event: bool,
        mut delivery: RequestDeliveryMetadata,
    ) -> Self {
        delivery.emitted_event = emitted_event;
        Self {
            error,
            transport_failure: false,
            delivery,
        }
    }

    pub(crate) fn request_sent(&self) -> bool {
        self.delivery.phase != RequestDeliveryPhase::NotSent
    }

    pub(crate) fn definitive_rejection(&self) -> bool {
        if self.delivery.emitted_or_accepted() {
            return false;
        }
        !self.transport_failure
            || self.delivery.phase == RequestDeliveryPhase::NotSent
                && matches!(
                    self.error,
                    AgentError::Api { .. } | AgentError::CodingPlanAdmission { .. }
                )
    }

    pub(crate) fn into_agent_error(self) -> AgentError {
        if !self.delivery.emitted_event
            && matches!(self.error, AgentError::Api { .. })
            && self.error.is_retryable()
        {
            return self.error;
        }
        if self.request_sent() && (self.transport_failure || self.delivery.emitted_or_accepted()) {
            AgentError::RequestSent {
                message: self.error.to_string(),
                metadata: Some(self.delivery),
            }
        } else {
            self.error
        }
    }
}

/// Select a process-wide Rustls provider before its configuration builder
/// attempts feature-based auto-detection.
///
/// A binary can enable both `ring` and `aws-lc-rs` through its dependency
/// graph. Rustls intentionally refuses to choose between them.
pub fn ensure_rustls_crypto_provider() {
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
    system: &System,
    tools: &Value,
    previous_response_id: Option<&str>,
    prompt_cache_key: Option<&str>,
    store: bool,
    opts: &RequestOptions,
    parallel_tool_calls: bool,
) -> Value {
    super::responses::build_body(
        model,
        messages,
        system,
        tools,
        previous_response_id,
        prompt_cache_key,
        store,
        opts,
        parallel_tool_calls,
    )
}

fn build_create_event(body: &Value) -> Value {
    let mut event = body
        .as_object()
        .cloned()
        .unwrap_or_else(serde_json::Map::new);
    event.remove("stream");
    event.remove("background");
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
            futures_lite::future::or(
                async { connect.await.map_err(|error| ws_connect_err(auth, error)) },
                async {
                    Timer::after(connect_timeout).await;
                    Err(AgentError::Timeout {
                        secs: connect_timeout.as_secs(),
                    })
                },
            )
            .await?
        };
        let now = Instant::now();
        Ok(Self {
            socket,
            opened_at: now,
            available_since: now,
            validated_for_send: true,
        })
    }

    pub(crate) fn should_retire_before_send(&self, stream_timeout: Duration) -> bool {
        self.opened_at.elapsed().saturating_add(
            response_in_flight_timeout(stream_timeout).saturating_add(CONNECTION_RETIRE_MIN_MARGIN),
        ) >= MAX_CONNECTION_AGE
    }

    pub(crate) fn age(&self) -> Duration {
        self.opened_at.elapsed()
    }

    pub(crate) fn idle_for(&self) -> Duration {
        self.available_since.elapsed()
    }

    pub(crate) fn is_idle(&self) -> bool {
        self.idle_for() >= MAX_POOL_IDLE
    }

    #[cfg(test)]
    pub(crate) fn set_age_for_test(&mut self, age: Duration) {
        self.opened_at = Instant::now()
            .checked_sub(age)
            .expect("test socket age must fit in Instant");
    }

    pub(crate) fn is_validated_for_send(&self) -> bool {
        self.validated_for_send
    }

    pub(crate) async fn preflight(&mut self, timeout: Duration) -> Result<(), AgentError> {
        let deadline = Instant::now() + timeout;
        if !send_message_until(
            &mut self.socket,
            WsMessage::Ping(PREFLIGHT_PING_PAYLOAD.to_vec().into()),
            deadline,
        )
        .await?
        {
            return Err(AgentError::Timeout {
                secs: timeout.as_secs(),
            });
        }
        loop {
            let Some(message) = next_message_until(&mut self.socket, deadline).await? else {
                return Err(AgentError::Timeout {
                    secs: timeout.as_secs(),
                });
            };
            match message {
                WsMessage::Pong(payload) if payload.as_ref() == PREFLIGHT_PING_PAYLOAD => {
                    self.validated_for_send = true;
                    return Ok(());
                }
                WsMessage::Ping(_) => {
                    if !flush_until(&mut self.socket, deadline).await? {
                        return Err(AgentError::Timeout {
                            secs: timeout.as_secs(),
                        });
                    }
                }
                WsMessage::Close(_) => {
                    return Err(IoError::new(
                        ErrorKind::ConnectionAborted,
                        "pooled Responses WebSocket closed during liveness check",
                    )
                    .into());
                }
                WsMessage::Text(_) | WsMessage::Binary(_) => {
                    return Err(IoError::new(
                        ErrorKind::InvalidData,
                        "pooled Responses WebSocket had unread application data",
                    )
                    .into());
                }
                _ => {}
            }
        }
    }

    pub(crate) async fn stream_message(
        &mut self,
        body: &Value,
        event_tx: &Sender<ProviderEvent>,
        stream_timeout: Duration,
    ) -> Result<(Option<String>, StreamResponse), WebSocketAttemptError> {
        self.stream_message_with_keepalive(body, event_tx, stream_timeout, KEEPALIVE_INTERVAL)
            .await
    }

    #[allow(clippy::too_many_lines)]
    async fn stream_message_with_keepalive(
        &mut self,
        body: &Value,
        event_tx: &Sender<ProviderEvent>,
        stream_timeout: Duration,
        keepalive_interval: Duration,
    ) -> Result<(Option<String>, StreamResponse), WebSocketAttemptError> {
        let mut delivery = RequestDeliveryMetadata::new(RequestDeliveryPhase::NotSent);
        if self.should_retire_before_send(stream_timeout) {
            return Err(WebSocketAttemptError::transport(
                IoError::new(
                    ErrorKind::ConnectionAborted,
                    "Responses WebSocket retired before request send",
                )
                .into(),
                false,
                delivery,
            ));
        }
        let create_event = build_create_event(body);
        delivery.phase = RequestDeliveryPhase::SentAwaitingAcceptance;
        let response_timeout = response_in_flight_timeout(stream_timeout);
        let response_deadline = Instant::now() + response_timeout;
        let mut progress_deadline = Instant::now() + stream_timeout;
        let create_sent = send_json_until(
            &mut self.socket,
            &create_event,
            progress_deadline.min(response_deadline),
        )
        .await
        .map_err(|error| WebSocketAttemptError::transport(error, false, delivery.clone()))?;
        if !create_sent {
            return Err(WebSocketAttemptError::transport(
                AgentError::Timeout {
                    secs: stream_timeout.as_secs(),
                },
                false,
                delivery,
            ));
        }

        let mut acc = ResponseAccumulator::new(None);
        let mut keepalive_deadline = Instant::now() + keepalive_interval;
        loop {
            let now = Instant::now();
            if now >= response_deadline || now >= progress_deadline {
                let timeout = if now >= response_deadline {
                    response_timeout
                } else {
                    stream_timeout
                };
                return Err(WebSocketAttemptError::transport(
                    AgentError::Timeout {
                        secs: timeout.as_secs(),
                    },
                    acc.emitted_event(),
                    delivery,
                ));
            }
            if now >= keepalive_deadline {
                let ping_deadline = progress_deadline.min(response_deadline);
                let ping_sent = send_message_until(
                    &mut self.socket,
                    WsMessage::Ping(Vec::new().into()),
                    ping_deadline,
                )
                .await
                .map_err(|error| {
                    WebSocketAttemptError::transport(error, acc.emitted_event(), delivery.clone())
                })?;
                if !ping_sent {
                    return Err(WebSocketAttemptError::transport(
                        AgentError::Timeout {
                            secs: stream_timeout.as_secs(),
                        },
                        acc.emitted_event(),
                        delivery,
                    ));
                }
                keepalive_deadline = Instant::now() + keepalive_interval;
                continue;
            }

            let wake_at = progress_deadline
                .min(response_deadline)
                .min(keepalive_deadline);
            let Some(message) = next_message_until(&mut self.socket, wake_at)
                .await
                .map_err(|error| {
                    WebSocketAttemptError::transport(error, acc.emitted_event(), delivery.clone())
                })?
            else {
                continue;
            };
            match message {
                WsMessage::Text(text) => {
                    let event: Value = serde_json::from_str(&text).map_err(|error| {
                        WebSocketAttemptError::transport(
                            AgentError::Json(error),
                            acc.emitted_event(),
                            delivery.clone(),
                        )
                    })?;
                    let event_type = event.get("type").and_then(Value::as_str);
                    update_delivery_metadata(&mut delivery, event_type, &event);
                    match event_type {
                        Some("error") => {
                            let error = error_from_event(&event);
                            let transport_failure = matches!(&error, AgentError::Io(_));
                            return Err(if transport_failure {
                                WebSocketAttemptError::transport(
                                    error,
                                    acc.emitted_event(),
                                    delivery,
                                )
                            } else {
                                WebSocketAttemptError::response(
                                    error,
                                    acc.emitted_event(),
                                    delivery,
                                )
                            });
                        }
                        Some(event_type) => {
                            let semantic_progress = is_semantic_progress_event(event_type, &event);
                            match acc.handle_event(event_type, &event, event_tx).await {
                                Ok(true) => break,
                                Ok(false) if semantic_progress => {
                                    progress_deadline = Instant::now() + stream_timeout;
                                }
                                Ok(false) => {}
                                Err(error) => {
                                    return Err(WebSocketAttemptError::response(
                                        error,
                                        acc.emitted_event(),
                                        delivery,
                                    ));
                                }
                            }
                        }
                        None => {}
                    }
                }
                WsMessage::Ping(_) => {
                    let flush_deadline = progress_deadline.min(response_deadline);
                    let flushed = flush_until(&mut self.socket, flush_deadline)
                        .await
                        .map_err(|error| {
                            WebSocketAttemptError::transport(
                                error,
                                acc.emitted_event(),
                                delivery.clone(),
                            )
                        })?;
                    if !flushed {
                        return Err(WebSocketAttemptError::transport(
                            AgentError::Timeout {
                                secs: stream_timeout.as_secs(),
                            },
                            acc.emitted_event(),
                            delivery,
                        ));
                    }
                }
                WsMessage::Close(frame) => {
                    add_close_metadata(&mut delivery, frame.as_ref());
                    return Err(WebSocketAttemptError::transport(
                        IoError::new(
                            ErrorKind::ConnectionAborted,
                            "Responses WebSocket closed without a terminal event",
                        )
                        .into(),
                        acc.emitted_event(),
                        delivery,
                    ));
                }
                _ => {}
            }
        }

        self.available_since = Instant::now();
        self.validated_for_send = false;
        let response_id = acc.response_id().map(ToOwned::to_owned);
        Ok((response_id, acc.into_stream_response()))
    }

    #[cfg(test)]
    async fn close(&mut self) {
        let _ = self.socket.close(None).await;
    }
}

fn ws_connect_err(auth: &ResolvedAuth, error: WsError) -> AgentError {
    let WsError::Http(response) = error else {
        return ws_err(error);
    };
    let is_coding_plan =
        auth.base_url.as_deref() == Some(crate::providers::openai::auth::CODING_PLAN_BASE_URL);
    let status = response.status().as_u16();
    let empty_body = response
        .body()
        .as_ref()
        .is_none_or(|body| body.iter().all(u8::is_ascii_whitespace));
    if is_coding_plan && status == 403 && empty_body {
        return AgentError::CodingPlanAdmission {
            retry_after: retry_after(
                response
                    .headers()
                    .get("retry-after")
                    .map(HeaderValue::as_bytes),
            ),
        };
    }
    ws_err(WsError::Http(response))
}

pub(crate) fn retry_after(value: Option<&[u8]>) -> Option<Duration> {
    retry_after_at(value, SystemTime::now())
}

fn retry_after_at(value: Option<&[u8]>, now: SystemTime) -> Option<Duration> {
    let value = value?;
    if let Ok(delay) = parse_retry_after(value, now) {
        Some(delay)
    } else {
        warn!(
            fallback_seconds = MALFORMED_RETRY_AFTER_DELAY.as_secs(),
            "provider returned malformed Retry-After; using conservative delay"
        );
        Some(MALFORMED_RETRY_AFTER_DELAY)
    }
}

fn parse_retry_after(value: &[u8], now: SystemTime) -> Result<Duration, ()> {
    let value = std::str::from_utf8(value).map_err(|_| ())?.trim();
    if let Ok(seconds) = value.parse::<u64>() {
        return Ok(Duration::from_secs(seconds));
    }
    let deadline = httpdate::parse_http_date(value).map_err(|_| ())?;
    match deadline.duration_since(now) {
        Ok(delay) => Ok(delay),
        Err(_) => Ok(Duration::ZERO),
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

async fn send_json_until<S>(
    ws: &mut WebSocketStream<S>,
    value: &Value,
    deadline: Instant,
) -> Result<bool, AgentError>
where
    S: futures_lite::AsyncRead + futures_lite::AsyncWrite + Unpin + Send,
{
    let text = value.to_string();
    let event_type = value["type"].as_str().map_or_else(|| "unknown", |s| s);
    debug!(event = %event_type, bytes = text.len(), "sending websocket event");
    send_message_until(ws, WsMessage::Text(text.into()), deadline).await
}

async fn send_message_until<S>(
    ws: &mut WebSocketStream<S>,
    message: WsMessage,
    deadline: Instant,
) -> Result<bool, AgentError>
where
    S: futures_lite::AsyncRead + futures_lite::AsyncWrite + Unpin + Send,
{
    let remaining = deadline.saturating_duration_since(Instant::now());
    let result = futures_lite::future::or(
        async { Some(ws.send(message).await.map_err(ws_err)) },
        async {
            Timer::after(remaining).await;
            None
        },
    )
    .await;

    match result {
        Some(Ok(())) => Ok(true),
        Some(Err(error)) => Err(error),
        None => Ok(false),
    }
}

async fn flush_until<S>(ws: &mut WebSocketStream<S>, deadline: Instant) -> Result<bool, AgentError>
where
    S: futures_lite::AsyncRead + futures_lite::AsyncWrite + Unpin + Send,
{
    let remaining = deadline.saturating_duration_since(Instant::now());
    let result =
        futures_lite::future::or(async { Some(ws.flush().await.map_err(ws_err)) }, async {
            Timer::after(remaining).await;
            None
        })
        .await;

    match result {
        Some(Ok(())) => Ok(true),
        Some(Err(error)) => Err(error),
        None => Ok(false),
    }
}

async fn next_message_until<S>(
    ws: &mut WebSocketStream<S>,
    deadline: Instant,
) -> Result<Option<WsMessage>, AgentError>
where
    S: futures_lite::AsyncRead + futures_lite::AsyncWrite + Unpin + Send,
{
    let remaining = deadline.saturating_duration_since(Instant::now());
    let result = futures_lite::future::or(
        async { Some(ws.next().await.transpose().map_err(ws_err)) },
        async {
            Timer::after(remaining).await;
            None
        },
    )
    .await;

    match result {
        Some(Ok(Some(message))) => Ok(Some(message)),
        Some(Ok(None)) => Err(IoError::new(
            ErrorKind::UnexpectedEof,
            "Responses WebSocket closed unexpectedly",
        )
        .into()),
        Some(Err(error)) => Err(error),
        None => Ok(None),
    }
}

fn update_delivery_metadata(
    delivery: &mut RequestDeliveryMetadata,
    event_type: Option<&str>,
    event: &Value,
) {
    if event_type == Some("response.created") {
        delivery.phase = RequestDeliveryPhase::Accepted;
    }
    if let Some(response_id) = event.pointer("/response/id").and_then(Value::as_str) {
        delivery.phase = RequestDeliveryPhase::Accepted;
        delivery.response_id = Some(response_id.to_owned());
    }
}

fn add_close_metadata(delivery: &mut RequestDeliveryMetadata, frame: Option<&CloseFrame>) {
    let Some(frame) = frame else {
        return;
    };
    delivery.close_code = Some(u16::from(frame.code));
    delivery.close_reason = sanitize_close_reason(frame.reason.as_ref());
}

fn sanitize_close_reason(reason: &str) -> Option<String> {
    let mut sanitized = String::with_capacity(reason.len().min(MAX_SAFE_CLOSE_REASON_CHARS));
    let mut character_count = 0;
    for character in reason.chars() {
        let character = if character.is_control() || character.is_whitespace() {
            ' '
        } else {
            character
        };
        if character == ' ' && sanitized.ends_with(' ') {
            continue;
        }
        if character_count == MAX_SAFE_CLOSE_REASON_CHARS {
            break;
        }
        sanitized.push(character);
        character_count += 1;
    }
    let trimmed = sanitized.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_owned())
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
    use std::io::Result as IoResult;
    use std::pin::Pin;
    use std::task::{Context, Poll};

    use super::super::responses::MODERATION_MODEL;
    use super::*;
    use crate::ContentBlock;
    use crate::types::{FileSource, ImageDetail, ImageSource, Message};
    use async_tungstenite::tungstenite::protocol::frame::coding::CloseCode;
    use async_tungstenite::tungstenite::protocol::{CloseFrame, Role};
    use futures_lite::io::{AsyncRead, AsyncWrite};
    use serde_json::json;
    use test_case::test_case;

    const CONNECTION_RESET_MESSAGE: &str = "connection reset";

    struct PendingIo;

    impl AsyncRead for PendingIo {
        fn poll_read(
            self: Pin<&mut Self>,
            _context: &mut Context<'_>,
            _buffer: &mut [u8],
        ) -> Poll<IoResult<usize>> {
            Poll::Pending
        }
    }

    impl AsyncWrite for PendingIo {
        fn poll_write(
            self: Pin<&mut Self>,
            _context: &mut Context<'_>,
            _buffer: &[u8],
        ) -> Poll<IoResult<usize>> {
            Poll::Pending
        }

        fn poll_flush(self: Pin<&mut Self>, _context: &mut Context<'_>) -> Poll<IoResult<()>> {
            Poll::Pending
        }

        fn poll_close(self: Pin<&mut Self>, _context: &mut Context<'_>) -> Poll<IoResult<()>> {
            Poll::Pending
        }
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
    fn websocket_writes_obey_deadline() {
        smol::block_on(async {
            let mut socket = WebSocketStream::from_raw_socket(PendingIo, Role::Client, None).await;
            let sent = send_message_until(
                &mut socket,
                WsMessage::Ping(Vec::new().into()),
                Instant::now(),
            )
            .await
            .unwrap();
            let mut socket = WebSocketStream::from_raw_socket(PendingIo, Role::Client, None).await;
            let flushed = flush_until(&mut socket, Instant::now()).await.unwrap();

            assert!(!sent);
            assert!(!flushed);
        });
    }

    #[test]
    #[allow(clippy::large_futures)]
    fn preflight_rejects_unread_application_data() {
        smol::block_on(async {
            let listener = smol::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            let server = smol::spawn(async move {
                let (stream, _) = listener.accept().await.unwrap();
                let mut socket = async_tungstenite::accept_async(stream).await.unwrap();
                socket
                    .send(WsMessage::Text(
                        json!({"type":"unknown.stale"}).to_string().into(),
                    ))
                    .await
                    .unwrap();
                let _ = socket.next().await;
            });
            let auth = ResolvedAuth {
                base_url: Some(format!("http://{address}/v1")),
                headers: Vec::new(),
            };
            let mut connection = ResponsesWebSocket::connect(&auth, Duration::from_secs(2))
                .await
                .unwrap();

            let error = connection
                .preflight(Duration::from_secs(2))
                .await
                .unwrap_err();
            server.await;

            assert!(matches!(
                error,
                AgentError::Io(error) if error.kind() == ErrorKind::InvalidData
            ));
        });
    }

    #[test]
    fn create_event_uses_responses_reasoning_shape() {
        let model = Model::from_spec("openai/gpt-5.6").unwrap();
        let opts = RequestOptions {
            thinking: crate::ThinkingConfig::Effort(crate::Effort::High),
            fast: true,
            safety_identifier: Some("test-id".to_string()),
            moderation: true,
            ..Default::default()
        };
        let body = build_request_body(
            &model,
            &[],
            &System::from("system"),
            &json!([]),
            None,
            None,
            true,
            &opts,
            true,
        );
        let event = build_create_event(&body);
        assert_eq!(
            event["reasoning"],
            json!({"effort":"high","summary":"auto"})
        );
        assert!(event.get("reasoning_effort").is_none());
        assert_eq!(event["type"], "response.create");
        assert_eq!(event["store"], true);
        assert!(event.get("stream").is_none());
        assert!(event.get("background").is_none());
        // Verify new fields are preserved
        assert_eq!(event["service_tier"], "fast");
        assert_eq!(event["safety_identifier"], "test-id");
        assert_eq!(event["moderation"], json!({"model": MODERATION_MODEL}));
    }

    #[test]
    fn create_event_includes_image_detail_and_file_input() {
        let model = Model::from_spec("openai/gpt-5.6").unwrap();
        let opts = RequestOptions::default();
        let mut message = Message::user_with_images(
            "test".to_string(),
            vec![ImageSource::url(
                "https://example.com/image.png",
                Some(ImageDetail::High),
            )],
        );
        message.content.push(ContentBlock::File {
            source: FileSource::file_id("file_123", None),
        });
        let messages = vec![message];
        let body = build_request_body(
            &model,
            &messages,
            &System::default(),
            &json!([]),
            None,
            None,
            true,
            &opts,
            true,
        );
        let event = build_create_event(&body);
        // Verify image detail is preserved in input
        let input = event["input"].as_array().unwrap();
        let content = input[0]["content"].as_array().unwrap();
        let image = content[0].as_object().unwrap();
        assert_eq!(image["type"], "input_image");
        assert_eq!(image["detail"], "high");
        assert_eq!(image["image_url"], "https://example.com/image.png");
        let file = content[2].as_object().unwrap();
        assert_eq!(file["type"], "input_file");
        assert_eq!(file["file_id"], "file_123");
    }

    #[test]
    fn create_event_includes_builtin_tool_config() {
        let model = Model::from_spec("openai/gpt-5.6").unwrap();
        let opts = RequestOptions::default();
        let tools = json!([{
            "origin": "openai",
            "name": "file_search",
            "input_schema": {"type": "object"},
            "vector_store_ids": ["vs_123"],
            "max_num_results": 5
        }]);
        let body = build_request_body(
            &model,
            &[],
            &System::default(),
            &tools,
            None,
            None,
            true,
            &opts,
            true,
        );
        let event = build_create_event(&body);
        let wire_tools = event["tools"].as_array().unwrap();
        assert_eq!(wire_tools.len(), 1);
        assert_eq!(wire_tools[0]["type"], "file_search");
        assert_eq!(wire_tools[0]["vector_store_ids"], json!(["vs_123"]));
        assert_eq!(wire_tools[0]["max_num_results"], 5);
    }

    #[test]
    fn accumulator_captures_response_id_from_websocket_event() {
        smol::block_on(async {
            let (event_tx, _) = flume::unbounded();
            let mut accumulator = ResponseAccumulator::new(None);
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
    fn response_created_without_id_marks_request_accepted() {
        let mut delivery =
            RequestDeliveryMetadata::new(RequestDeliveryPhase::SentAwaitingAcceptance);

        update_delivery_metadata(
            &mut delivery,
            Some("response.created"),
            &json!({"type":"response.created","response":{}}),
        );

        assert_eq!(delivery.phase, RequestDeliveryPhase::Accepted);
        assert!(delivery.response_id.is_none());
    }

    #[test_case(400, false, false)]
    #[test_case(401, false, true)]
    #[test_case(429, true, false)]
    #[test_case(500, true, false)]
    fn provider_response_error_after_send_stays_retryable_before_output(
        status: u16,
        retryable: bool,
        auth_error: bool,
    ) {
        let error = WebSocketAttemptError::response(
            AgentError::Api {
                status,
                message: "provider rejected an already-written create".into(),
            },
            false,
            RequestDeliveryMetadata::new(RequestDeliveryPhase::SentAwaitingAcceptance),
        )
        .into_agent_error();

        assert!(!matches!(error, AgentError::RequestSent { .. }));
        assert_eq!(error.is_retryable(), retryable);
        assert_eq!(error.is_auth_error(), auth_error);
    }

    #[test]
    fn server_overload_after_send_is_not_wrapped_in_request_sent() {
        let error = WebSocketAttemptError::response(
            AgentError::Api {
                status: 400,
                message: "server_is_overloaded: Our servers are currently overloaded".into(),
            },
            false,
            RequestDeliveryMetadata::new(RequestDeliveryPhase::SentAwaitingAcceptance),
        )
        .into_agent_error();

        assert!(!matches!(error, AgentError::RequestSent { .. }));
        assert!(error.is_server_overloaded());
        assert!(error.is_retryable());
        assert_eq!(
            error.user_message(),
            "provider is overloaded, try again later"
        );
    }

    #[test]
    fn server_error_500_after_accepted_response_id_is_not_wrapped_in_request_sent() {
        let mut delivery =
            RequestDeliveryMetadata::new(RequestDeliveryPhase::SentAwaitingAcceptance);
        delivery.phase = RequestDeliveryPhase::Accepted;
        delivery.response_id = Some("resp_close".into());

        let error = WebSocketAttemptError::response(
            AgentError::Api {
                status: 500,
                message: "WebSocket protocol error: Connection reset without closing handshake"
                    .into(),
            },
            false,
            delivery,
        )
        .into_agent_error();

        assert!(matches!(error, AgentError::Api { status, .. } if status == 500));
        assert!(error.is_retryable());
        assert!(!matches!(error, AgentError::RequestSent { .. }));
    }

    #[test_case(
        RequestDeliveryPhase::SentAwaitingAcceptance,
        None;
        "after_send"
    )]
    #[test_case(RequestDeliveryPhase::Accepted, Some("resp_close"); "after_acceptance")]
    fn transport_failure_after_send_is_wrapped_in_request_sent(
        phase: RequestDeliveryPhase,
        response_id: Option<&str>,
    ) {
        let mut delivery = RequestDeliveryMetadata::new(phase);
        delivery.response_id = response_id.map(String::from);
        let error = WebSocketAttemptError::transport(
            AgentError::Io(IoError::new(
                ErrorKind::ConnectionReset,
                CONNECTION_RESET_MESSAGE,
            )),
            false,
            delivery,
        )
        .into_agent_error();

        assert!(matches!(error, AgentError::RequestSent { .. }));
        assert!(!error.is_retryable());
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

            assert!(error.request_sent());
            assert_eq!(
                error.delivery.phase,
                RequestDeliveryPhase::SentAwaitingAcceptance
            );
            assert!(error.transport_failure);
            assert!(!matches!(error.error, AgentError::Api { status: 422, .. }));
        });
    }

    #[test]
    #[allow(clippy::large_futures)]
    fn malformed_response_after_send_preserves_delivery_metadata() {
        smol::block_on(async {
            let listener = smol::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            let server = smol::spawn(async move {
                let (stream, _) = listener.accept().await.unwrap();
                let mut socket = async_tungstenite::accept_async(stream).await.unwrap();
                assert!(matches!(socket.next().await, Some(Ok(WsMessage::Text(_)))));
                socket.send(WsMessage::Text("{".into())).await.unwrap();
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

            assert!(matches!(
                error.into_agent_error(),
                AgentError::RequestSent {
                    metadata: Some(RequestDeliveryMetadata {
                        phase: RequestDeliveryPhase::SentAwaitingAcceptance,
                        ..
                    }),
                    ..
                }
            ));
        });
    }
    #[test]
    #[allow(clippy::large_futures)]
    fn close_after_response_created_preserves_delivery_metadata() {
        smol::block_on(async {
            let listener = smol::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            let server = smol::spawn(async move {
                let (stream, _) = listener.accept().await.unwrap();
                let mut socket = async_tungstenite::accept_async(stream).await.unwrap();
                assert!(matches!(socket.next().await, Some(Ok(WsMessage::Text(_)))));
                socket
                    .send(WsMessage::Text(
                        json!({"type":"response.created","response":{"id":"resp_close"}})
                            .to_string()
                            .into(),
                    ))
                    .await
                    .unwrap();
                socket
                    .close(Some(CloseFrame {
                        code: CloseCode::Restart,
                        reason: "proxy restart\nrequest details removed".into(),
                    }))
                    .await
                    .unwrap();
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

            assert_eq!(error.delivery.phase, crate::RequestDeliveryPhase::Accepted);
            assert_eq!(error.delivery.response_id.as_deref(), Some("resp_close"));
            assert_eq!(error.delivery.close_code, Some(1012));
            assert_eq!(
                error.delivery.close_reason.as_deref(),
                Some("proxy restart request details removed")
            );
            let agent_error = error.into_agent_error();
            assert!(matches!(
                agent_error,
                AgentError::RequestSent { metadata: Some(metadata), .. }
                    if metadata.close_code == Some(1012)
                        && metadata.response_id.as_deref() == Some("resp_close")
            ));
        });
    }

    #[test]
    #[allow(clippy::large_futures)]
    fn idle_response_sends_client_keepalive() {
        smol::block_on(async {
            let listener = smol::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            let server = smol::spawn(async move {
                let (stream, _) = listener.accept().await.unwrap();
                let mut socket = async_tungstenite::accept_async(stream).await.unwrap();
                assert!(matches!(socket.next().await, Some(Ok(WsMessage::Text(_)))));
                assert!(matches!(socket.next().await, Some(Ok(WsMessage::Ping(_)))));
                socket
                    .send(WsMessage::Text(
                        json!({"type":"response.created","response":{"id":"resp_idle"}})
                            .to_string()
                            .into(),
                    ))
                    .await
                    .unwrap();
                socket
                    .send(WsMessage::Text(
                        json!({
                            "type":"response.completed",
                            "response":{"id":"resp_idle","status":"completed"}
                        })
                        .to_string()
                        .into(),
                    ))
                    .await
                    .unwrap();
                while let Some(Ok(message)) = socket.next().await {
                    if matches!(message, WsMessage::Close(_)) {
                        break;
                    }
                }
            });
            let auth = ResolvedAuth {
                base_url: Some(format!("http://{address}/v1")),
                headers: Vec::new(),
            };
            let mut connection = ResponsesWebSocket::connect(&auth, Duration::from_secs(2))
                .await
                .unwrap();
            let (event_tx, _) = flume::unbounded();
            let (response_id, _) = connection
                .stream_message_with_keepalive(
                    &json!({"model":"test","input":[]}),
                    &event_tx,
                    Duration::from_secs(2),
                    Duration::from_millis(10),
                )
                .await
                .unwrap();
            connection.close().await;
            server.await;

            assert_eq!(response_id.as_deref(), Some("resp_idle"));
        });
    }

    #[test]
    #[allow(clippy::large_futures)]
    fn pong_heartbeats_do_not_extend_response_progress_timeout() {
        smol::block_on(async {
            let listener = smol::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            let server = smol::spawn(async move {
                let (stream, _) = listener.accept().await.unwrap();
                let mut socket = async_tungstenite::accept_async(stream).await.unwrap();
                assert!(matches!(socket.next().await, Some(Ok(WsMessage::Text(_)))));
                for _ in 0..6 {
                    Timer::after(Duration::from_millis(15)).await;
                    if socket
                        .send(WsMessage::Pong(Vec::new().into()))
                        .await
                        .is_err()
                    {
                        break;
                    }
                }
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
                .stream_message_with_keepalive(
                    &json!({"model":"test","input":[]}),
                    &event_tx,
                    Duration::from_millis(40),
                    Duration::from_millis(10),
                )
                .await
                .unwrap_err();
            server.await;

            assert!(matches!(error.error, AgentError::Timeout { .. }));
            assert_eq!(
                error.delivery.phase,
                crate::RequestDeliveryPhase::SentAwaitingAcceptance
            );
        });
    }

    #[test]
    #[allow(clippy::large_futures)]
    fn unknown_json_events_do_not_extend_response_progress_timeout() {
        smol::block_on(async {
            let listener = smol::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            let server = smol::spawn(async move {
                let (stream, _) = listener.accept().await.unwrap();
                let mut socket = async_tungstenite::accept_async(stream).await.unwrap();
                assert!(matches!(socket.next().await, Some(Ok(WsMessage::Text(_)))));
                for sequence in 0..10 {
                    Timer::after(Duration::from_millis(20)).await;
                    if socket
                        .send(WsMessage::Text(
                            json!({"type":"unknown.noop","sequence":sequence})
                                .to_string()
                                .into(),
                        ))
                        .await
                        .is_err()
                    {
                        break;
                    }
                }
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
                .stream_message_with_keepalive(
                    &json!({"model":"test","input":[]}),
                    &event_tx,
                    Duration::from_millis(100),
                    Duration::from_secs(1),
                )
                .await
                .unwrap_err();
            server.await;

            assert!(matches!(error.error, AgentError::Timeout { .. }));
            assert_eq!(
                error.delivery.phase,
                crate::RequestDeliveryPhase::SentAwaitingAcceptance
            );
        });
    }

    #[test]
    #[allow(clippy::large_futures)]
    fn repeated_in_progress_events_hit_absolute_response_deadline() {
        smol::block_on(async {
            let listener = smol::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            let server = smol::spawn(async move {
                let (stream, _) = listener.accept().await.unwrap();
                let mut socket = async_tungstenite::accept_async(stream).await.unwrap();
                assert!(matches!(socket.next().await, Some(Ok(WsMessage::Text(_)))));
                for sequence in 0..40 {
                    if sequence > 0 {
                        Timer::after(Duration::from_millis(10)).await;
                    }
                    if socket
                        .send(WsMessage::Text(
                            json!({
                                "type":"response.in_progress",
                                "response":{"id":"resp_progress","sequence":sequence}
                            })
                            .to_string()
                            .into(),
                        ))
                        .await
                        .is_err()
                    {
                        break;
                    }
                }
            });
            let auth = ResolvedAuth {
                base_url: Some(format!("http://{address}/v1")),
                headers: Vec::new(),
            };
            let mut connection = ResponsesWebSocket::connect(&auth, Duration::from_secs(2))
                .await
                .unwrap();
            let (event_tx, _) = flume::unbounded();
            let started = Instant::now();

            let error = connection
                .stream_message_with_keepalive(
                    &json!({"model":"test","input":[]}),
                    &event_tx,
                    Duration::from_millis(20),
                    Duration::from_secs(1),
                )
                .await
                .unwrap_err();
            server.await;

            assert!(matches!(error.error, AgentError::Timeout { .. }));
            assert!(started.elapsed() < Duration::from_secs(2));
            assert_eq!(error.delivery.response_id.as_deref(), Some("resp_progress"));
        });
    }

    #[test]
    fn close_reason_is_control_free_and_character_capped() {
        let reason = format!(
            "restart\n{}\u{7}",
            "x".repeat(MAX_SAFE_CLOSE_REASON_CHARS * 2)
        );
        let sanitized = sanitize_close_reason(&reason).unwrap();

        assert!(sanitized.chars().all(|character| !character.is_control()));
        assert!(sanitized.chars().count() <= MAX_SAFE_CLOSE_REASON_CHARS);
        assert!(sanitized.starts_with("restart "));
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
    fn empty_coding_plan_handshake_forbidden_is_typed_admission_error() {
        let auth = ResolvedAuth {
            base_url: Some(crate::providers::openai::auth::CODING_PLAN_BASE_URL.into()),
            headers: Vec::new(),
        };
        let response = async_tungstenite::tungstenite::http::Response::builder()
            .status(403)
            .header("retry-after", "7")
            .body(Some(b" \n".to_vec()))
            .unwrap();

        match super::ws_connect_err(&auth, WsError::Http(Box::new(response))) {
            AgentError::CodingPlanAdmission { retry_after } => {
                assert_eq!(retry_after, Some(Duration::from_secs(7)));
            }
            other => panic!("expected CodingPlanAdmission, got {other:?}"),
        }
    }

    #[test_case(b"invalid"; "nonnumeric")]
    #[test_case(b"\xff"; "non_utf8")]
    fn malformed_retry_after_uses_conservative_delay(value: &[u8]) {
        assert_eq!(
            super::retry_after(Some(value)),
            Some(MALFORMED_RETRY_AFTER_DELAY)
        );
    }

    #[test]
    fn http_date_retry_after_uses_remaining_delay() {
        let now = std::time::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
        let value = httpdate::fmt_http_date(now + Duration::from_secs(7));

        assert_eq!(
            super::retry_after_at(Some(value.as_bytes()), now),
            Some(Duration::from_secs(7))
        );
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

    #[test]
    fn build_request_body_with_tools_adds_parallel_tool_calls() {
        let model = Model::from_spec("openai/gpt-5.6").unwrap();
        let opts = RequestOptions::default();
        let tools = json!([{
            "name": "bash",
            "description": "run shell commands",
            "input_schema": {"type": "object"}
        }]);
        let body = build_request_body(
            &model,
            &[],
            &System::from("system"),
            &tools,
            None,
            None,
            true,
            &opts,
            true,
        );
        assert_eq!(body["parallel_tool_calls"], true);

        let event = build_create_event(&body);
        assert_eq!(event["parallel_tool_calls"], true);
    }

    #[test]
    fn build_request_body_with_tools_omits_parallel_tool_calls_when_disabled() {
        let model = Model::from_spec("openai/gpt-5.6").unwrap();
        let opts = RequestOptions::default();
        let tools = json!([{
            "name": "bash",
            "description": "run shell commands",
            "input_schema": {"type": "object"}
        }]);
        let body = build_request_body(
            &model,
            &[],
            &System::from("system"),
            &tools,
            None,
            None,
            true,
            &opts,
            false,
        );
        assert!(body.get("parallel_tool_calls").is_none());
    }
}

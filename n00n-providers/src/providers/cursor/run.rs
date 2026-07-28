//! HTTP/2 Connect driver for `agent.v1.AgentService/Run`.
//!
//! This module is not yet wired into the Cursor provider; it's prepared for
//! future native integration to replace the cursor-agent subprocess approach.

#![allow(dead_code)]

use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::{Duration, Instant};

use futures_lite::AsyncReadExt;
use futures_lite::io::AsyncRead;
use isahc::config::{Configurable, VersionNegotiation};
use isahc::{AsyncBody, AsyncReadResponseExt, HttpClient, Request};
use n00n_storage::id::n00nId;
use uuid::Uuid;

use crate::AgentError;

use super::connect::{ConnectFrame, FrameBuffer};
use super::proto::{
    AGENT_MODE_AGENT, RunFrameParams, build_run_frames, extract_text_deltas,
    extract_thinking_deltas, has_exec_server_message, heartbeat_frame,
};
use super::wire::{
    CLIENT_TYPE, CLIENT_VERSION, CONNECT_CONTENT_TYPE, CONNECT_PROTOCOL_VERSION, wire_model_id,
};

const AGENT_PATH: &str = "/agent.v1.AgentService/Run";
const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(5);
const FIRST_FRAME_PACE: Duration = Duration::from_millis(1500);
const SECOND_FRAME_PACE: Duration = Duration::from_millis(800);
const MARKER_FRAME_PACE: Duration = Duration::from_millis(400);
const FIRST_BYTE_TIMEOUT: Duration = Duration::from_mins(1);
const IDLE_TIMEOUT: Duration = Duration::from_secs(8);
const CLIENT_VERSION_ENV: &str = "N00N_CURSOR_CLIENT_VERSION";
const TOOL_NOT_SUPPORTED: &str = "cursor_tool_not_supported";

#[derive(Debug, Clone)]
pub(crate) struct RunResult {
    pub text: String,
    pub thinking: String,
    pub conversation_id: String,
    pub http_status: u16,
}

struct PacedBody {
    frames: Vec<Vec<u8>>,
    index: usize,
    offset: usize,
    next_at: Instant,
    heartbeats: bool,
    pending_heartbeat: Option<Vec<u8>>,
    closed: bool,
}

impl PacedBody {
    fn new(frames: Vec<Vec<u8>>) -> Self {
        Self {
            frames,
            index: 0,
            offset: 0,
            next_at: Instant::now(),
            heartbeats: false,
            pending_heartbeat: None,
            closed: false,
        }
    }

    fn pace_after_frame(index: usize, total: usize) -> Duration {
        match index {
            0 => Duration::ZERO,
            1 => FIRST_FRAME_PACE,
            2 => SECOND_FRAME_PACE,
            _ if index < total => MARKER_FRAME_PACE,
            _ => HEARTBEAT_INTERVAL,
        }
    }
}

impl AsyncRead for PacedBody {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut [u8],
    ) -> Poll<std::io::Result<usize>> {
        if self.closed || buf.is_empty() {
            return Poll::Ready(Ok(0));
        }
        let now = Instant::now();
        if now < self.next_at {
            let waker = cx.waker().clone();
            let wait = self.next_at.saturating_duration_since(now);
            smol::spawn(async move {
                smol::Timer::after(wait).await;
                waker.wake();
            })
            .detach();
            return Poll::Pending;
        }

        if let Some(hb) = self.pending_heartbeat.as_mut() {
            let n = hb.len().min(buf.len());
            buf[..n].copy_from_slice(&hb[..n]);
            if n == hb.len() {
                self.pending_heartbeat = None;
                self.next_at = Instant::now() + HEARTBEAT_INTERVAL;
            } else {
                hb.drain(..n);
            }
            return Poll::Ready(Ok(n));
        }

        if self.index < self.frames.len() {
            let frame_len = self.frames[self.index].len();
            let remaining = frame_len - self.offset;
            let n = remaining.min(buf.len());
            buf[..n].copy_from_slice(&self.frames[self.index][self.offset..self.offset + n]);
            self.offset += n;
            if self.offset >= frame_len {
                self.offset = 0;
                self.index += 1;
                let total = self.frames.len();
                self.next_at = Instant::now() + Self::pace_after_frame(self.index, total);
                if self.index >= total {
                    self.heartbeats = true;
                }
            }
            return Poll::Ready(Ok(n));
        }

        if self.heartbeats {
            self.pending_heartbeat = Some(heartbeat_frame().map_err(std::io::Error::other)?);
            return self.poll_read(cx, buf);
        }

        Poll::Ready(Ok(0))
    }
}

fn client_version() -> String {
    match std::env::var(CLIENT_VERSION_ENV) {
        Ok(value) if !value.trim().is_empty() => value,
        _ => CLIENT_VERSION.to_string(),
    }
}

fn http2_client() -> Result<HttpClient, AgentError> {
    HttpClient::builder()
        .version_negotiation(VersionNegotiation::http2())
        .timeout(Duration::from_mins(2))
        .build()
        .map_err(|error| AgentError::Config {
            message: format!("cursor run http2 client: {error}"),
        })
}

fn cursor_wire_id() -> String {
    Uuid::from_bytes(*n00nId::generate().as_bytes()).to_string()
}

pub(crate) async fn run_text_turn(
    token: &str,
    agent_base_url: &str,
    display_model: &str,
    prompt: &str,
    cwd: &str,
) -> Result<RunResult, AgentError> {
    let conversation_id = cursor_wire_id();
    let message_id = cursor_wire_id();
    let model_id = wire_model_id(display_model).to_string();
    let frames = build_run_frames(&RunFrameParams {
        prompt,
        model_id: &model_id,
        cwd,
        conversation_id: &conversation_id,
        message_id: &message_id,
        mode: AGENT_MODE_AGENT,
    })
    .map_err(|message| AgentError::Api {
        status: 502,
        message,
    })?;

    let url = format!("{}{}", agent_base_url.trim_end_matches('/'), AGENT_PATH);
    let body = AsyncBody::from_reader(PacedBody::new(frames));
    let request = Request::builder()
        .method("POST")
        .uri(&url)
        .header("authorization", format!("Bearer {token}"))
        .header("content-type", CONNECT_CONTENT_TYPE)
        .header("connect-protocol-version", CONNECT_PROTOCOL_VERSION)
        .header("connect-accept-encoding", "identity")
        .header("user-agent", "connect-es/1.6.1")
        .header("x-cursor-client-type", CLIENT_TYPE)
        .header("x-cursor-client-version", client_version())
        .header("x-ghost-mode", "true")
        .header("x-cursor-streaming", "true")
        .header("x-request-id", &message_id)
        .header("x-original-request-id", &message_id)
        .body(body)
        .map_err(|error| AgentError::Config {
            message: format!("cursor run request: {error}"),
        })?;

    let client = http2_client()?;
    let mut response = client
        .send_async(request)
        .await
        .map_err(|error| AgentError::Api {
            status: 502,
            message: format!("cursor run transport: {error}"),
        })?;
    let http_status = response.status().as_u16();
    if !(200..300).contains(&http_status) {
        let err_body = match response.text().await {
            Ok(body) => body,
            Err(error) => format!("failed to read error body: {error}"),
        };
        return Err(AgentError::Api {
            status: http_status,
            message: err_body,
        });
    }

    let mut frame_buf = FrameBuffer::default();
    let mut read_buf = [0u8; 8192];
    let mut text = String::new();
    let mut thinking = String::new();
    let started = Instant::now();
    let mut last_data = Instant::now();
    let mut got_frame = false;

    loop {
        if !got_frame && started.elapsed() > FIRST_BYTE_TIMEOUT {
            return Err(AgentError::Api {
                status: 504,
                message: "cursor run first-byte timeout".into(),
            });
        }
        if got_frame && last_data.elapsed() > IDLE_TIMEOUT {
            return Err(AgentError::Api {
                status: 504,
                message: "cursor run idle timeout".into(),
            });
        }

        let n = {
            let body = response.body_mut();
            match smol::future::or(
                async {
                    let n = AsyncReadExt::read(body, &mut read_buf)
                        .await
                        .map_err(|error| AgentError::Api {
                            status: 502,
                            message: format!("cursor run read: {error}"),
                        })?;
                    Ok::<_, AgentError>(Some(n))
                },
                async {
                    smol::Timer::after(Duration::from_millis(250)).await;
                    Ok(None)
                },
            )
            .await?
            {
                Some(0) => break,
                Some(n) => n,
                None => continue,
            }
        };

        got_frame = true;
        last_data = Instant::now();
        frame_buf.push(&read_buf[..n]);
        while let Some(frame) = frame_buf.next_frame() {
            let frame = frame.map_err(|message| AgentError::Api {
                status: 502,
                message,
            })?;
            if frame.end_stream {
                if let Ok(err) = serde_json::from_slice::<serde_json::Value>(&frame.payload)
                    && err.get("error").is_some()
                {
                    return Err(AgentError::Api {
                        status: 502,
                        message: String::from_utf8_lossy(&frame.payload).into_owned(),
                    });
                }
                break;
            }
            handle_data_frame(&frame, &mut text, &mut thinking)?;
        }
    }

    Ok(RunResult {
        text,
        thinking,
        conversation_id,
        http_status,
    })
}

fn handle_data_frame(
    frame: &ConnectFrame,
    text: &mut String,
    thinking: &mut String,
) -> Result<(), AgentError> {
    if frame.compressed {
        return Err(AgentError::Api {
            status: 502,
            message: "cursor run gzip frames not supported yet".into(),
        });
    }
    if has_exec_server_message(&frame.payload).map_err(|message| AgentError::Api {
        status: 502,
        message,
    })? {
        return Err(AgentError::Api {
            status: 501,
            message: TOOL_NOT_SUPPORTED.into(),
        });
    }
    for delta in extract_text_deltas(&frame.payload).map_err(|message| AgentError::Api {
        status: 502,
        message,
    })? {
        text.push_str(&delta);
    }
    for delta in extract_thinking_deltas(&frame.payload).map_err(|message| AgentError::Api {
        status: 502,
        message,
    })? {
        thinking.push_str(&delta);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const LIVE_ENV: &str = "N00N_CURSOR_LIVE_TESTS";

    fn live_enabled() -> bool {
        std::env::var(LIVE_ENV)
            .is_ok_and(|value| value == "1" || value.eq_ignore_ascii_case("true"))
    }

    #[test]
    fn run_default_model_live() {
        if !live_enabled() {
            return;
        }
        smol::block_on(async {
            let token = super::super::auth::read_ide_access_token().expect("IDE token");
            let base = super::super::discovery::fetch_agent_base_url(&token).expect("agent url");
            let result = run_text_turn(&token, &base, "auto", "Reply with exactly: pong", "/tmp")
                .await
                .expect("run");
            assert_eq!(result.http_status, 200);
            assert!(!result.conversation_id.is_empty());
            let _ = &result.thinking;
            assert!(
                result.text.to_lowercase().contains("pong"),
                "unexpected text: {}",
                result.text
            );
        });
    }
}

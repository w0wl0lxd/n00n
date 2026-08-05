//! HTTP/2 Connect driver for `agent.v1.AgentService/Run`.
//!
//! Transport uses `reqwest` streaming bodies (same duplex model as shunt). isahc
//! `AsyncBody::from_reader` stops draining the request after response headers
//! (`enqueued≫sent`), so the paced marker/heartbeat frames never reach the wire.
//!
//! This module is not yet wired into the Cursor provider; it's prepared for
//! future native integration to replace the cursor-agent subprocess approach.

#![allow(dead_code)]

use std::collections::VecDeque;
use std::pin::Pin;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};
use std::time::{Duration, Instant};

use bytes::Bytes;
use futures::StreamExt;
use futures_lite::io::AsyncRead;
use n00n_storage::id::n00nId;
use uuid::Uuid;

use crate::AgentError;

use super::checkpoint::{
    KvServerOp, SharedCheckpointStore, encode_get_blob_result, encode_set_blob_result,
    parse_kv_server_message, shared_store,
};
use super::checksum::{
    client_key_from_token, generate_checksum, resolve_machine_id, session_id_from_token,
};
use super::connect::{ConnectFrame, FrameBuffer, decode_frame_payload, encode_frame};
use super::proto::{
    AGENT_MODE_AGENT, RunFrameParams, build_run_frames, extract_text_deltas,
    extract_thinking_deltas, has_exec_server_message, heartbeat_frame, iter_fields,
};
use super::wire::{
    CLIENT_TYPE, CLIENT_VERSION, CONNECT_CONTENT_TYPE, CONNECT_PROTOCOL_VERSION, wire_model_id,
};

const AGENT_PATH: &str = "/agent.v1.AgentService/Run";
const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(5);
const FIRST_FRAME_PACE: Duration = Duration::from_millis(1500);
const SECOND_FRAME_PACE: Duration = Duration::from_millis(800);
const MARKER_FRAME_PACE: Duration = Duration::from_millis(400);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const FIRST_BYTE_TIMEOUT: Duration = Duration::from_mins(1);
const IDLE_TIMEOUT: Duration = Duration::from_mins(1);
const CLIENT_VERSION_ENV: &str = "N00N_CURSOR_CLIENT_VERSION";
const CHECKPOINT_LOCK_POISONED: &str = "cursor checkpoint store lock poisoned";
const OUTBOUND_LOCK_POISONED: &str = "cursor run outbound lock poisoned";
const STALL_DUMP_PATH: &str = "/tmp/n00n_cursor_run_stall_frames.bin";
const CLIENT_OS: &str = "linux";
#[cfg(target_arch = "aarch64")]
const CLIENT_ARCH: &str = "arm64";
#[cfg(not(target_arch = "aarch64"))]
const CLIENT_ARCH: &str = "x64";
const CLIENT_DEVICE_TYPE: &str = "desktop";
const CLIENT_TIMEZONE: &str = "UTC";
const TOKIO_RUNTIME_BUILD: &str = "cursor run tokio runtime";
const MIN_READ_BUDGET: Duration = Duration::from_millis(1);

static STALL_DUMP: Mutex<Vec<Vec<u8>>> = Mutex::new(Vec::new());

#[derive(Debug, Clone)]
pub(crate) struct RunResult {
    pub text: String,
    pub thinking: String,
    pub conversation_id: String,
    pub http_status: u16,
    pub frames_seen: u32,
    pub exec_skipped: u32,
    pub text_deltas: u32,
    pub kv_ops: u32,
    pub frames_enqueued: u32,
    pub frames_sent: u32,
}

type OutboundQueue = Arc<Mutex<OutboundState>>;

struct OutboundState {
    queue: VecDeque<Vec<u8>>,
    notify_tx: flume::Sender<()>,
}

impl OutboundState {
    fn new(notify_tx: flume::Sender<()>) -> Self {
        Self {
            queue: VecDeque::new(),
            notify_tx,
        }
    }

    fn push(&mut self, frame: Vec<u8>) {
        self.queue.push_back(frame);
        match self.notify_tx.try_send(()) {
            Ok(()) | Err(flume::TrySendError::Full(()) | flume::TrySendError::Disconnected(())) => {
            }
        }
    }

    fn pop(&mut self) -> Option<Vec<u8>> {
        self.queue.pop_front()
    }
}

fn new_outbound_queue() -> (OutboundQueue, flume::Receiver<()>) {
    let (notify_tx, notify_rx) = flume::bounded(1);
    (
        Arc::new(Mutex::new(OutboundState::new(notify_tx))),
        notify_rx,
    )
}

fn pace_after_send(index: usize) -> Duration {
    match index {
        0 => FIRST_FRAME_PACE,
        1 => SECOND_FRAME_PACE,
        _ => MARKER_FRAME_PACE,
    }
}

fn take_outbound(outbound: &OutboundQueue) -> Option<Vec<u8>> {
    let Ok(mut state) = outbound.lock() else {
        return None;
    };
    state.pop()
}

async fn wait_pace_or_notify(pace: Duration, notify_rx: &flume::Receiver<()>) {
    () = smol::future::or(
        async {
            smol::Timer::after(pace).await;
        },
        async {
            match notify_rx.recv_async().await {
                Ok(()) | Err(_) => {}
            }
        },
    )
    .await;
}

async fn flush_outbound(
    tx: &flume::Sender<Vec<u8>>,
    outbound: &OutboundQueue,
    enqueued: &AtomicU32,
) -> Result<(), ()> {
    while let Some(frame) = take_outbound(outbound) {
        tx.send_async(frame).await.map_err(|_| ())?;
        enqueued.fetch_add(1, Ordering::Relaxed);
    }
    Ok(())
}

/// Independent producer (shunt pattern): pace/heartbeats keep flowing while the
/// response side is read. Returning `Pending` from `poll_read` alone is not enough
/// with isahc — upload stalls if the reader is not driven concurrently.
fn spawn_paced_body(
    frames: Vec<Vec<u8>>,
    outbound: OutboundQueue,
    notify_rx: flume::Receiver<()>,
) -> (ChannelBody, Arc<AtomicU32>, Arc<AtomicU32>) {
    let frames_enqueued = Arc::new(AtomicU32::new(0));
    let frames_sent = Arc::new(AtomicU32::new(0));
    let (tx, rx) = flume::bounded::<Vec<u8>>(8);
    let enqueued = Arc::clone(&frames_enqueued);
    smol::spawn(async move {
        for (idx, frame) in frames.into_iter().enumerate() {
            if flush_outbound(&tx, &outbound, &enqueued).await.is_err() {
                return;
            }
            if tx.send_async(frame).await.is_err() {
                return;
            }
            enqueued.fetch_add(1, Ordering::Relaxed);
            wait_pace_or_notify(pace_after_send(idx), &notify_rx).await;
        }
        loop {
            if flush_outbound(&tx, &outbound, &enqueued).await.is_err() {
                return;
            }
            let Ok(hb) = heartbeat_frame() else {
                return;
            };
            if tx.send_async(hb).await.is_err() {
                return;
            }
            enqueued.fetch_add(1, Ordering::Relaxed);
            wait_pace_or_notify(HEARTBEAT_INTERVAL, &notify_rx).await;
        }
    })
    .detach();
    (
        ChannelBody {
            rx,
            pending: None,
            recv_slot: Arc::new(Mutex::new(None)),
            waiting: false,
            frames_sent: Arc::clone(&frames_sent),
            counted_pending: false,
        },
        frames_enqueued,
        frames_sent,
    )
}

struct ChannelBody {
    rx: flume::Receiver<Vec<u8>>,
    pending: Option<Vec<u8>>,
    recv_slot: Arc<Mutex<Option<Vec<u8>>>>,
    waiting: bool,
    frames_sent: Arc<AtomicU32>,
    counted_pending: bool,
}

impl AsyncRead for ChannelBody {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut [u8],
    ) -> Poll<std::io::Result<usize>> {
        if buf.is_empty() {
            return Poll::Ready(Ok(0));
        }
        if self.pending.is_none() {
            let slotted = self.recv_slot.lock().ok().and_then(|mut slot| slot.take());
            if let Some(frame) = slotted {
                self.waiting = false;
                self.counted_pending = false;
                self.pending = Some(frame);
            }
        }
        if self.pending.is_none() {
            match self.rx.try_recv() {
                Ok(frame) => {
                    self.waiting = false;
                    self.counted_pending = false;
                    self.pending = Some(frame);
                }
                Err(flume::TryRecvError::Disconnected) => return Poll::Ready(Ok(0)),
                Err(flume::TryRecvError::Empty) => {
                    if !self.waiting {
                        self.waiting = true;
                        let waker = cx.waker().clone();
                        let rx = self.rx.clone();
                        let slot = Arc::clone(&self.recv_slot);
                        smol::spawn(async move {
                            if let Ok(frame) = rx.recv_async().await
                                && let Ok(mut guard) = slot.lock()
                            {
                                *guard = Some(frame);
                            }
                            waker.wake();
                        })
                        .detach();
                    }
                    return Poll::Pending;
                }
            }
        }
        let Some(pending) = self.pending.as_mut() else {
            return Poll::Ready(Ok(0));
        };
        let n = pending.len().min(buf.len());
        buf[..n].copy_from_slice(&pending[..n]);
        let exhausted = n == pending.len();
        if !exhausted {
            pending.drain(..n);
        }
        // End borrow of `pending` before touching other fields.
        if exhausted {
            self.pending = None;
        }
        if !self.counted_pending {
            self.frames_sent.fetch_add(1, Ordering::Relaxed);
            self.counted_pending = true;
        }
        if exhausted {
            self.counted_pending = false;
        }
        Poll::Ready(Ok(n))
    }
}

fn client_version() -> String {
    match std::env::var(CLIENT_VERSION_ENV) {
        Ok(value) if !value.trim().is_empty() => value,
        _ => CLIENT_VERSION.to_string(),
    }
}

fn cursor_wire_id() -> String {
    Uuid::from_bytes(*n00nId::generate().as_bytes()).to_string()
}

fn http2_client() -> Result<reqwest::Client, AgentError> {
    // No overall request timeout: AgentService/Run can stream for longer than a
    // few minutes while active. First-byte / idle stalls are enforced in the
    // read loop instead.
    reqwest::Client::builder()
        .connect_timeout(CONNECT_TIMEOUT)
        .build()
        .map_err(|error| AgentError::Config {
            message: format!("cursor run http2 client: {error}"),
        })
}

type BodyTx = tokio::sync::mpsc::Sender<Result<Bytes, std::io::Error>>;

async fn flush_outbound_tokio(
    tx: &BodyTx,
    outbound: &OutboundQueue,
    enqueued: &AtomicU32,
) -> Result<(), ()> {
    while let Some(frame) = take_outbound(outbound) {
        tx.send(Ok(Bytes::from(frame))).await.map_err(|_| ())?;
        enqueued.fetch_add(1, Ordering::Relaxed);
    }
    Ok(())
}

async fn wait_pace_or_notify_tokio(pace: Duration, notify_rx: &flume::Receiver<()>) {
    tokio::select! {
        () = tokio::time::sleep(pace) => {}
        result = notify_rx.recv_async() => {
            match result {
                Ok(()) | Err(_) => {}
            }
        }
    }
}

fn spawn_paced_reqwest_body(
    frames: Vec<Vec<u8>>,
    outbound: OutboundQueue,
    notify_rx: flume::Receiver<()>,
) -> (reqwest::Body, Arc<AtomicU32>) {
    let frames_enqueued = Arc::new(AtomicU32::new(0));
    let (tx, rx) = tokio::sync::mpsc::channel::<Result<Bytes, std::io::Error>>(8);
    let enqueued = Arc::clone(&frames_enqueued);
    tokio::spawn(async move {
        for (idx, frame) in frames.into_iter().enumerate() {
            if flush_outbound_tokio(&tx, &outbound, &enqueued)
                .await
                .is_err()
            {
                return;
            }
            if tx.send(Ok(Bytes::from(frame))).await.is_err() {
                return;
            }
            enqueued.fetch_add(1, Ordering::Relaxed);
            wait_pace_or_notify_tokio(pace_after_send(idx), &notify_rx).await;
        }
        let mut ticker = tokio::time::interval_at(
            tokio::time::Instant::now() + HEARTBEAT_INTERVAL,
            HEARTBEAT_INTERVAL,
        );
        loop {
            if flush_outbound_tokio(&tx, &outbound, &enqueued)
                .await
                .is_err()
            {
                return;
            }
            tokio::select! {
                _ = ticker.tick() => {
                    let Ok(hb) = heartbeat_frame() else {
                        return;
                    };
                    if tx.send(Ok(Bytes::from(hb))).await.is_err() {
                        return;
                    }
                    enqueued.fetch_add(1, Ordering::Relaxed);
                }
                result = notify_rx.recv_async() => {
                    match result {
                        Ok(()) | Err(_) => {}
                    }
                }
            }
        }
    });
    let stream = futures::stream::unfold(rx, |mut rx| async move {
        rx.recv().await.map(|item| (item, rx))
    });
    (reqwest::Body::wrap_stream(stream), frames_enqueued)
}

pub(crate) async fn run_text_turn(
    token: &str,
    agent_base_url: &str,
    display_model: &str,
    prompt: &str,
    cwd: &str,
) -> Result<RunResult, AgentError> {
    run_text_turn_mode(
        token,
        agent_base_url,
        display_model,
        prompt,
        cwd,
        AGENT_MODE_AGENT,
    )
    .await
}

pub(crate) async fn run_text_turn_mode(
    token: &str,
    agent_base_url: &str,
    display_model: &str,
    prompt: &str,
    cwd: &str,
    mode: u64,
) -> Result<RunResult, AgentError> {
    let token = token.to_owned();
    let agent_base_url = agent_base_url.to_owned();
    let display_model = display_model.to_owned();
    let prompt = prompt.to_owned();
    let cwd = cwd.to_owned();
    smol::unblock(move || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|error| AgentError::Config {
                message: format!("{TOKIO_RUNTIME_BUILD}: {error}"),
            })?;
        runtime.block_on(run_text_turn_mode_tokio(
            &token,
            &agent_base_url,
            &display_model,
            &prompt,
            &cwd,
            mode,
        ))
    })
    .await
}

async fn run_text_turn_mode_tokio(
    token: &str,
    agent_base_url: &str,
    display_model: &str,
    prompt: &str,
    cwd: &str,
    mode: u64,
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
        mode,
    })
    .map_err(|message| AgentError::Api {
        status: 500,
        message,
    })?;

    if let Ok(mut dumps) = STALL_DUMP.lock() {
        dumps.clear();
    }
    let (outbound, notify_rx) = new_outbound_queue();
    let checkpoints = shared_store();
    let url = format!("{}{}", agent_base_url.trim_end_matches('/'), AGENT_PATH);
    let (body, frames_enqueued) =
        spawn_paced_reqwest_body(frames, Arc::clone(&outbound), notify_rx);
    let machine_id = resolve_machine_id();
    let checksum = generate_checksum(token, machine_id.as_deref());
    let session_id = session_id_from_token(token);
    let client_key = client_key_from_token(token);
    let config_version = cursor_wire_id();
    let client = http2_client()?;
    let response = client
        .post(&url)
        .header("authorization", format!("Bearer {token}"))
        .header("content-type", CONNECT_CONTENT_TYPE)
        .header("connect-protocol-version", CONNECT_PROTOCOL_VERSION)
        .header("connect-accept-encoding", "gzip,br")
        .header("user-agent", "connect-es/1.6.1")
        .header("x-cursor-client-type", CLIENT_TYPE)
        .header("x-cursor-client-version", client_version())
        .header("x-cursor-client-os", CLIENT_OS)
        .header("x-cursor-client-arch", CLIENT_ARCH)
        .header("x-cursor-client-device-type", CLIENT_DEVICE_TYPE)
        .header("x-cursor-timezone", CLIENT_TIMEZONE)
        .header("x-cursor-checksum", &checksum)
        .header("x-cursor-config-version", &config_version)
        .header("x-client-key", &client_key)
        .header("x-session-id", &session_id)
        .header("x-ghost-mode", "true")
        .header("x-request-id", &message_id)
        .header("x-original-request-id", &message_id)
        .header("x-amzn-trace-id", format!("Root={message_id}"))
        .body(body)
        .send()
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
    let mut text = String::new();
    let mut thinking = String::new();
    let mut frames_seen = 0u32;
    let mut exec_skipped = 0u32;
    let mut text_deltas = 0u32;
    let mut kv_ops = 0u32;
    let mut top_fields: Vec<u64> = Vec::new();
    let mut interaction_fields: Vec<u64> = Vec::new();
    let mut interaction_sample = String::new();
    let started = Instant::now();
    let mut last_data = Instant::now();
    let mut got_any_data = false;
    let mut got_output = false;
    let mut bytes = response.bytes_stream();

    loop {
        // Once any Connect bytes/frames arrive, stall on idle-since-last-data —
        // KV/checkpoint traffic is progress even before text/thinking deltas.
        let pre_output_elapsed = if got_any_data {
            last_data.elapsed()
        } else {
            started.elapsed()
        };
        if !got_output && pre_output_elapsed > FIRST_BYTE_TIMEOUT {
            if let Ok(dumps) = STALL_DUMP.lock() {
                let mut blob = Vec::new();
                for payload in dumps.iter() {
                    if let Ok(frame) = encode_frame(0, payload) {
                        blob.extend(frame);
                    }
                }
                if std::fs::write(STALL_DUMP_PATH, &blob).is_err() {
                    // Best-effort diagnostic artifact for live spike debugging.
                }
            }
            let enqueued = frames_enqueued.load(Ordering::Relaxed);
            return Err(AgentError::Api {
                status: 504,
                message: if got_any_data {
                    format!(
                        "cursor run stalled before assistant output \
                         (frames={frames_seen} enqueued={enqueued} sent={enqueued} \
                         kv_ops={kv_ops} exec_skipped={exec_skipped} \
                         top_fields={top_fields:?} interaction_fields={interaction_fields:?} \
                         sample={interaction_sample:?})"
                    )
                } else {
                    format!("cursor run first-byte timeout (enqueued={enqueued})")
                },
            });
        }
        if got_output && last_data.elapsed() > IDLE_TIMEOUT {
            break;
        }

        let budget = if got_output {
            match IDLE_TIMEOUT.checked_sub(last_data.elapsed()) {
                Some(remaining) if !remaining.is_zero() => remaining,
                Some(_) | None => MIN_READ_BUDGET,
            }
        } else {
            match FIRST_BYTE_TIMEOUT.checked_sub(pre_output_elapsed) {
                Some(remaining) if !remaining.is_zero() => remaining,
                Some(_) | None => MIN_READ_BUDGET,
            }
        };

        match tokio::time::timeout(budget, bytes.next()).await {
            Ok(Some(Ok(chunk))) => {
                got_any_data = true;
                last_data = Instant::now();
                frame_buf.push(&chunk);
                let mut stream_ended = false;
                while let Some(frame) = frame_buf.next_frame() {
                    let frame = frame.map_err(|message| AgentError::Api {
                        status: 502,
                        message,
                    })?;
                    let end_stream = frame.end_stream;
                    if end_stream
                        && let Ok(err) = serde_json::from_slice::<serde_json::Value>(&frame.payload)
                        && err.get("error").is_some()
                    {
                        return Err(AgentError::Api {
                            status: 502,
                            message: String::from_utf8_lossy(&frame.payload).into_owned(),
                        });
                    }
                    // End-stream frames can still carry a final protobuf payload
                    // (text/thinking/KV); handle before exiting the read loop.
                    frames_seen = frames_seen.saturating_add(1);
                    if let Ok(payload) = decode_frame_payload(&frame) {
                        if let Ok(mut dumps) = STALL_DUMP.lock() {
                            dumps.push(payload.clone());
                        }
                        for field in iter_fields(&payload).flatten() {
                            top_fields.push(field.0);
                            if field.0 == 1 && field.1 == 2 {
                                for nested in iter_fields(field.2).flatten() {
                                    interaction_fields.push(nested.0);
                                    if interaction_sample.is_empty() && nested.1 == 2 {
                                        let preview: String = nested
                                            .2
                                            .iter()
                                            .take(96)
                                            .map(|b| {
                                                if (0x20..=0x7e).contains(b) {
                                                    char::from(*b)
                                                } else {
                                                    '.'
                                                }
                                            })
                                            .collect();
                                        interaction_sample = format!("f{}:{preview}", nested.0);
                                    }
                                }
                            }
                            if field.0 == 2 && field.1 == 2 {
                                let preview: String = field
                                    .2
                                    .iter()
                                    .take(200)
                                    .map(|b| {
                                        if (0x20..=0x7e).contains(b) {
                                            char::from(*b)
                                        } else {
                                            '.'
                                        }
                                    })
                                    .collect();
                                interaction_sample = format!(
                                    "{interaction_sample}|f2(len={}):{preview}",
                                    field.2.len()
                                );
                            }
                        }
                    }
                    let outcome = handle_data_frame(
                        &frame,
                        &mut text,
                        &mut thinking,
                        &checkpoints,
                        &outbound,
                    )?;
                    exec_skipped = exec_skipped.saturating_add(u32::from(outcome.exec_skipped));
                    text_deltas = text_deltas.saturating_add(outcome.text_deltas);
                    kv_ops = kv_ops.saturating_add(u32::from(outcome.kv_op));
                    if outcome.text_deltas > 0 || !thinking.is_empty() {
                        got_output = true;
                    }
                    if end_stream {
                        stream_ended = true;
                        break;
                    }
                }
                if stream_ended {
                    break;
                }
            }
            Ok(Some(Err(error))) => {
                return Err(AgentError::Api {
                    status: 502,
                    message: format!("cursor run read: {error}"),
                });
            }
            Ok(None) => break,
            Err(_) => {
                // Read budget elapsed; loop re-checks first-byte / idle deadlines.
            }
        }
    }

    let enqueued = frames_enqueued.load(Ordering::Relaxed);
    Ok(RunResult {
        text,
        thinking,
        conversation_id,
        http_status,
        frames_seen,
        exec_skipped,
        text_deltas,
        kv_ops,
        frames_enqueued: enqueued,
        frames_sent: enqueued,
    })
}

struct FrameHandleOutcome {
    exec_skipped: bool,
    text_deltas: u32,
    kv_op: bool,
}

fn handle_data_frame(
    frame: &ConnectFrame,
    text: &mut String,
    thinking: &mut String,
    checkpoints: &SharedCheckpointStore,
    outbound: &OutboundQueue,
) -> Result<FrameHandleOutcome, AgentError> {
    let payload = decode_frame_payload(frame).map_err(|message| AgentError::Api {
        status: 502,
        message,
    })?;
    let bad_frame = |message| AgentError::Api {
        status: 502,
        message,
    };

    match parse_kv_server_message(&payload) {
        Ok(Some(op)) => {
            queue_checkpoint_reply(op, checkpoints, outbound)?;
            return Ok(FrameHandleOutcome {
                exec_skipped: false,
                text_deltas: 0,
                kv_op: true,
            });
        }
        Ok(None) => {}
        Err(_) if frame.end_stream => {
            return Ok(FrameHandleOutcome {
                exec_skipped: false,
                text_deltas: 0,
                kv_op: false,
            });
        }
        Err(message) => return Err(bad_frame(message)),
    }

    let outcome: Result<FrameHandleOutcome, String> = (|| {
        if has_exec_server_message(&payload)? {
            // Phase 0: n00n owns tools; ignore Cursor-side exec until Phase 1 maps them.
            // Aborting the whole turn drops text deltas that often follow.
            return Ok(FrameHandleOutcome {
                exec_skipped: true,
                text_deltas: 0,
                kv_op: false,
            });
        }
        let mut deltas = 0u32;
        for delta in extract_text_deltas(&payload)? {
            text.push_str(&delta);
            deltas = deltas.saturating_add(1);
        }
        for delta in extract_thinking_deltas(&payload)? {
            thinking.push_str(&delta);
        }
        Ok(FrameHandleOutcome {
            exec_skipped: false,
            text_deltas: deltas,
            kv_op: false,
        })
    })();

    match outcome {
        Ok(outcome) => Ok(outcome),
        Err(_) if frame.end_stream => Ok(FrameHandleOutcome {
            exec_skipped: false,
            text_deltas: 0,
            kv_op: false,
        }),
        Err(message) => Err(bad_frame(message)),
    }
}

fn queue_checkpoint_reply(
    op: KvServerOp,
    checkpoints: &SharedCheckpointStore,
    outbound: &OutboundQueue,
) -> Result<(), AgentError> {
    let reply = match op {
        KvServerOp::Get { id, blob_id } => {
            let store = checkpoints.lock().map_err(|_| AgentError::Api {
                status: 500,
                message: CHECKPOINT_LOCK_POISONED.into(),
            })?;
            let data = store.get(&blob_id).map(<[u8]>::to_vec);
            encode_frame(0, &encode_get_blob_result(id, data.as_deref())).map_err(|message| {
                AgentError::Api {
                    status: 500,
                    message,
                }
            })?
        }
        KvServerOp::Set { id, blob_id, data } => {
            let mut store = checkpoints.lock().map_err(|_| AgentError::Api {
                status: 500,
                message: CHECKPOINT_LOCK_POISONED.into(),
            })?;
            store.set(blob_id, data);
            encode_frame(0, &encode_set_blob_result(id)).map_err(|message| AgentError::Api {
                status: 500,
                message,
            })?
        }
    };
    let mut queue = outbound.lock().map_err(|_| AgentError::Api {
        status: 500,
        message: OUTBOUND_LOCK_POISONED.into(),
    })?;
    queue.push(reply);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::providers::cursor::proto::{AGENT_MODE_ASK, field_bytes, field_ld, field_varint};
    use flate2::Compression;
    use flate2::write::GzEncoder;
    use futures_lite::AsyncReadExt;
    use std::io::Write;
    use std::path::PathBuf;

    const LIVE_ENV: &str = "N00N_CURSOR_LIVE_TESTS";

    fn live_enabled() -> bool {
        std::env::var(LIVE_ENV)
            .is_ok_and(|value| value == "1" || value.eq_ignore_ascii_case("true"))
    }

    fn gzip(bytes: &[u8]) -> Vec<u8> {
        let mut encoder = GzEncoder::new(Vec::new(), Compression::fast());
        encoder.write_all(bytes).expect("gzip write");
        encoder.finish().expect("gzip finish")
    }

    #[test]
    fn handle_data_frame_accepts_gzip_text_delta() {
        // interaction_update(f1) → text_delta(f1) → text(f1) = "pong"
        let text_delta = field_ld(1, &field_bytes(1, b"pong"));
        let interaction = field_ld(1, &text_delta);
        let frame = ConnectFrame {
            end_stream: false,
            compressed: true,
            payload: gzip(&interaction),
        };
        let store = shared_store();
        let (outbound, _notify) = new_outbound_queue();
        let mut text = String::new();
        let mut thinking = String::new();
        handle_data_frame(&frame, &mut text, &mut thinking, &store, &outbound).expect("handle");
        assert_eq!(text, "pong");
        assert!(thinking.is_empty());
        assert!(outbound.lock().expect("lock").queue.is_empty());
    }

    #[test]
    fn handle_data_frame_rejects_unknown_wire_type_three_payload() {
        let frame = ConnectFrame {
            end_stream: false,
            compressed: false,
            payload: vec![0x0b, 0x0c],
        };
        let store = shared_store();
        let (outbound, _notify) = new_outbound_queue();
        let mut text = String::new();
        let mut thinking = String::new();

        let Err(error) = handle_data_frame(&frame, &mut text, &mut thinking, &store, &outbound)
        else {
            panic!("unknown protobuf wire types must fail the frame");
        };

        assert!(matches!(error, AgentError::Api { status: 502, .. }));
        assert!(error.to_string().contains("wire type 3"));
    }

    #[test]
    fn handle_data_frame_queues_set_blob_ack() {
        let mut args = field_bytes(1, b"blob-id");
        args.extend(field_bytes(2, b"blob-data"));
        let mut kv = field_varint(1, 9);
        kv.extend(field_ld(3, &args));
        let payload = field_ld(4, &kv);
        let frame = ConnectFrame {
            end_stream: false,
            compressed: false,
            payload,
        };
        let store = shared_store();
        let (outbound, _notify) = new_outbound_queue();
        let mut text = String::new();
        let mut thinking = String::new();
        handle_data_frame(&frame, &mut text, &mut thinking, &store, &outbound).expect("handle");
        assert!(text.is_empty());
        assert_eq!(
            store.lock().expect("lock").get(b"blob-id"),
            Some(b"blob-data".as_slice())
        );
        assert_eq!(outbound.lock().expect("lock").queue.len(), 1);
    }

    #[test]
    fn handle_data_frame_ignores_non_protobuf_end_stream() {
        let frame = ConnectFrame {
            end_stream: true,
            compressed: false,
            payload: b"{}".to_vec(),
        };
        let store = shared_store();
        let (outbound, _notify) = new_outbound_queue();
        let mut text = String::new();
        let mut thinking = String::new();
        let outcome =
            handle_data_frame(&frame, &mut text, &mut thinking, &store, &outbound).expect("handle");
        assert!(!outcome.exec_skipped);
        assert_eq!(outcome.text_deltas, 0);
        assert!(text.is_empty());
        assert!(thinking.is_empty());
    }

    #[test]
    fn handle_data_frame_rejects_corrupt_compression() {
        let frame = ConnectFrame {
            end_stream: false,
            compressed: true,
            payload: b"not-gzip".to_vec(),
        };
        let store = shared_store();
        let (outbound, _notify) = new_outbound_queue();
        let mut text = String::new();
        let mut thinking = String::new();

        let Err(error) = handle_data_frame(&frame, &mut text, &mut thinking, &store, &outbound)
        else {
            panic!("corrupt compression must fail");
        };

        assert!(matches!(error, AgentError::Api { status: 502, .. }));
        assert!(error.to_string().contains("gzip"));
    }

    #[test]
    fn paced_body_flushes_outbound_during_pace_wait() {
        smol::block_on(async {
            let frame = encode_frame(0, b"a").expect("frame");
            let (outbound, notify_rx) = new_outbound_queue();
            let (mut body, _enqueued, _sent) =
                spawn_paced_body(vec![frame.clone()], Arc::clone(&outbound), notify_rx);
            let mut got = vec![0u8; frame.len()];
            AsyncReadExt::read_exact(&mut body, &mut got)
                .await
                .expect("frame");
            assert_eq!(got, frame);
            let reply = encode_frame(0, b"kv").expect("reply");
            outbound.lock().expect("lock").push(reply.clone());
            let mut got_reply = vec![0u8; reply.len()];
            AsyncReadExt::read_exact(&mut body, &mut got_reply)
                .await
                .expect("reply");
            assert_eq!(got_reply, reply);
        });
    }

    #[test]
    fn paced_body_paces_heartbeats() {
        smol::block_on(async {
            let frame = encode_frame(0, b"a").expect("frame");
            let (outbound, notify_rx) = new_outbound_queue();
            let (mut body, _enqueued, _sent) =
                spawn_paced_body(vec![frame.clone()], Arc::clone(&outbound), notify_rx);
            // Start before the first read: pacing begins when the first frame is
            // released, not when the consumer finishes reading it — under load the
            // post-read Instant can land after most of FIRST_FRAME_PACE already elapsed.
            let started = Instant::now();
            let mut got = vec![0u8; frame.len()];
            AsyncReadExt::read_exact(&mut body, &mut got)
                .await
                .expect("frame");
            let hb = heartbeat_frame().unwrap();
            let mut got_hb = vec![0u8; hb.len()];
            AsyncReadExt::read_exact(&mut body, &mut got_hb)
                .await
                .expect("heartbeat");
            assert_eq!(got_hb, hb);
            assert!(started.elapsed() >= FIRST_FRAME_PACE);
        });
    }

    #[test]
    fn capture_run_resp_extracts_pong() {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(
            "../spikes/cursor-capture-tmux-20260727-233059/bodies/\
             037_agentn.global.api5.cursor.sh_agent.v1.resp.bin",
        );
        if !path.exists() {
            return;
        }
        let bytes = std::fs::read(&path).expect("read capture");
        let mut buf = FrameBuffer::default();
        buf.push(&bytes);
        let store = shared_store();
        let (outbound, _notify) = new_outbound_queue();
        let mut text = String::new();
        let mut thinking = String::new();
        while let Some(frame) = buf.next_frame() {
            let frame = frame.expect("frame");
            let end_stream = frame.end_stream;
            handle_data_frame(&frame, &mut text, &mut thinking, &store, &outbound).expect("handle");
            if end_stream {
                break;
            }
        }
        assert!(
            text.to_lowercase().contains("pong"),
            "capture text={text:?} thinking={thinking:?}"
        );
    }

    #[test]
    fn extract_text_deltas_rejects_invalid_protobuf() {
        let malformed = [0x0b, 0x0c];
        let err = extract_text_deltas(&malformed).expect_err("must fail");
        assert!(err.contains("wire type 3"));

        let truncated = [0x0a, 0x05, 0x01, 0x02];
        let err = extract_text_deltas(&truncated).expect_err("must fail");
        assert!(err.contains("truncated"));
    }

    #[test]
    fn extract_thinking_deltas_rejects_invalid_protobuf() {
        let malformed = [0x0b, 0x0c];
        let err = extract_thinking_deltas(&malformed).expect_err("must fail");
        assert!(err.contains("wire type 3"));

        let truncated = [0x0a, 0x05, 0x01, 0x02];
        let err = extract_thinking_deltas(&truncated).expect_err("must fail");
        assert!(err.contains("truncated"));
    }

    #[test]
    fn has_exec_server_message_rejects_invalid_protobuf() {
        let malformed = [0x0b, 0x0c];
        let err = has_exec_server_message(&malformed).expect_err("must fail");
        assert!(err.contains("wire type 3"));

        let truncated = [0x0a, 0x05, 0x01, 0x02];
        let err = has_exec_server_message(&truncated).expect_err("must fail");
        assert!(err.contains("truncated"));
    }

    #[test]
    fn run_default_model_live() {
        if !live_enabled() {
            return;
        }
        smol::block_on(async {
            let token = super::super::auth::read_ide_access_token().expect("IDE token");
            let _models =
                super::super::discovery::fetch_usable_models(&token).expect("warm GetUsableModels");
            let base = super::super::discovery::fetch_agent_base_url(&token).expect("agent url");
            // ASK mode: text-only path; AGENT may wait on tool exec we do not answer.
            let result = run_text_turn_mode(
                &token,
                &base,
                "auto",
                "Reply with exactly: pong",
                "/tmp",
                AGENT_MODE_ASK,
            )
            .await
            .unwrap_or_else(|error| panic!("run failed: {error}"));
            assert_eq!(result.http_status, 200);
            assert!(!result.conversation_id.is_empty());
            let _ = &result.thinking;
            assert!(
                result.text.to_lowercase().contains("pong"),
                "unexpected text={:?} thinking={:?} frames={} enqueued={} sent={} exec_skipped={} text_deltas={} kv_ops={}",
                result.text,
                result.thinking,
                result.frames_seen,
                result.frames_enqueued,
                result.frames_sent,
                result.exec_skipped,
                result.text_deltas,
                result.kv_ops
            );
        });
    }
}

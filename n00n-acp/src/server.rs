use std::io::Write;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use agent_client_protocol_schema::{
    AgentNotification, AgentRequest, AgentResponse, ContentBlock, CurrentModeUpdate,
    EmbeddedResourceResource, Error as AcpError, ImageContent, JsonRpcMessage, LoadSessionRequest,
    NewSessionRequest, Notification, PromptRequest, PromptResponse, Request, RequestId,
    RequestPermissionRequest, RequestPermissionResponse, Response, SessionId, SessionModeId,
    SessionNotification, SessionUpdate, SetSessionConfigOptionRequest,
    SetSessionConfigOptionResponse, SetSessionModeRequest, SetSessionModeResponse, StopReason,
    TextContent, ToolCallId, ToolCallUpdate, ToolCallUpdateFields,
};
use flume::{Receiver, Sender};
use n00n_agent::headless::{self, InteractiveHandle, InteractiveParams};
use n00n_agent::types::AgentEvent;
use n00n_agent::{
    AgentInput, AgentMode, Envelope, ImageMediaType, ImageSource, mode_and_plan_from_stored,
};
use n00n_providers::Message;
use n00n_providers::TokenUsage;
use n00n_providers::model::Model;
use n00n_providers::provider::available_model_specs;
use n00n_storage::id::{SessionRef, n00nId};
use n00n_storage::sessions::Session;
use serde::Serialize;
use serde_json::Value;
use smol::io::AsyncBufReadExt;
use tracing::{debug, warn};

use crate::{AcpParams, methods, permissions, translate};

const FIRST_OUTGOING_REQUEST_ID: i64 = 1000;

type PendingPrompt = Arc<Mutex<PendingPromptState>>;

#[derive(Default)]
struct PendingPromptState {
    id: Option<RequestId>,
    cancel_requested: bool,
}

struct SessionState {
    handle: InteractiveHandle,
    current_mode: AgentMode,
    plan_path: Option<PathBuf>,
    current_model: String,
    pending_prompt: PendingPrompt,
    event_pump: smol::Task<()>,
    _daemon: Option<crate::SessionDaemonGuard>,
}

struct Server {
    out_tx: Sender<Value>,
    model_specs: Vec<String>,
    session: Option<SessionState>,
}

impl Server {
    fn respond(&self, id: RequestId, result: Result<AgentResponse, AcpError>) {
        send(&self.out_tx, Response::new(id, result));
    }
}

/// Runs the ACP server.
///
/// # Errors
/// Returns an error if stdin reading fails or JSON parsing fails.
pub async fn serve(params: AcpParams) -> color_eyre::Result<()> {
    let (out_tx, out_rx) = flume::unbounded::<Value>();

    let writer_task = smol::spawn(async move {
        let stdout = std::io::stdout();
        while let Ok(msg) = out_rx.recv_async().await {
            let mut handle = stdout.lock();
            if serde_json::to_writer(&mut handle, &msg).is_ok() {
                let _ = handle.write_all(b"\n");
                let _ = handle.flush();
            }
        }
    });

    let mut server = Server {
        out_tx,
        model_specs: available_model_specs(),
        session: None,
    };

    let stdin = smol::Unblock::new(std::io::stdin());
    let mut reader = smol::io::BufReader::new(stdin);
    let mut line = String::new();

    loop {
        line.clear();
        match reader.read_line(&mut line).await {
            Ok(0) => break,
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::InvalidData => {
                warn!(error = %e, "invalid UTF-8 on stdin");
                server.respond(RequestId::Null, Err(AcpError::parse_error()));
                continue;
            }
            Err(e) => {
                warn!(error = %e, "I/O error reading from stdin");
                return Err(color_eyre::eyre::eyre!(e));
            }
        }

        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        let mut stream = serde_json::Deserializer::from_str(trimmed).into_iter::<Value>();
        for result in &mut stream {
            let raw = match result {
                Ok(v) => v,
                Err(e) => {
                    warn!(error = %e, "invalid JSON on stdin");
                    server.respond(RequestId::Null, Err(AcpError::parse_error()));
                    break;
                }
            };

            let id = match raw.get("id").map(request_id).transpose() {
                Ok(Some(id)) => Some(id),
                Ok(None) => None,
                Err(e) => {
                    server.respond(RequestId::Null, Err(e));
                    break;
                }
            };

            if raw.get("result").is_some() || raw.get("error").is_some() {
                handle_incoming_response(&server, &raw);
            } else if let Some(method) = raw.get("method").and_then(Value::as_str) {
                match id {
                    Some(id) => handle_request(&mut server, method, id, &raw, &params).await,
                    None => handle_notification(&server, method),
                }
            } else if let Some(id) = id {
                server.respond(id, Err(AcpError::invalid_request()));
            }
        }
    }

    retire_session(&mut server).await;
    drop(server);
    writer_task.await;

    Ok(())
}

fn request_id(v: &Value) -> Result<RequestId, AcpError> {
    serde_json::from_value(v.clone()).map_err(|e| AcpError::invalid_request().data(json_str(&e)))
}

async fn handle_request(
    srv: &mut Server,
    method: &str,
    id: RequestId,
    raw: &Value,
    params: &AcpParams,
) {
    let result = match method {
        "initialize" => Ok(AgentResponse::InitializeResponse(
            methods::initialize_response(),
        )),
        "session/new" => handle_new_session(srv, raw, params).await,
        "session/load" => handle_load_session(srv, raw, params).await,
        "session/prompt" => match handle_prompt(srv, raw, &id) {
            Ok(()) => return,
            Err(e) => Err(e),
        },
        "session/set_mode" => handle_set_mode(srv, raw),
        "session/set_config_option" => handle_set_config(srv, raw),
        _ => Err(AcpError::method_not_found()),
    };
    srv.respond(id, result);
}

async fn handle_new_session(
    srv: &mut Server,
    raw: &Value,
    params: &AcpParams,
) -> Result<AgentResponse, AcpError> {
    let req = parse_params::<NewSessionRequest>(raw)?;
    retire_session(srv).await;
    let handle = spawn_session(params, req.cwd, None, Vec::new());
    let spec = params.model.spec();
    let resp = methods::new_session_response(handle.session_id.as_str())
        .config_options(vec![methods::model_config_option(&spec, &srv.model_specs)]);
    install_session(srv, handle, spec, AgentMode::Build, None, params);
    Ok(AgentResponse::NewSessionResponse(resp))
}

async fn handle_load_session(
    srv: &mut Server,
    raw: &Value,
    params: &AcpParams,
) -> Result<AgentResponse, AcpError> {
    let req = parse_params::<LoadSessionRequest>(raw)?;
    let session_ref: SessionRef = req
        .session_id
        .0
        .parse()
        .map_err(|_| AcpError::resource_not_found(Some(req.session_id.0.to_string())))?;
    let storage = n00n_storage::StateDir::resolve()
        .map_err(|error| AcpError::internal_error().data(json_str(&error)))?;
    let stored = load_session_from(&storage, session_ref.id())?;
    let (current_mode, plan_path) = mode_and_plan_from_stored(&storage, &stored.meta)
        .map_err(|error| AcpError::internal_error().data(json_str(&error)))?;
    let history = stored.messages;
    let sid = SessionId::from(session_ref.to_string());
    for update in translate::replay_history(&history) {
        session_update(&srv.out_tx, &sid, update);
    }
    retire_session(srv).await;
    let handle = spawn_session(params, req.cwd, Some(session_ref), history);
    let spec = params.model.spec();
    let resp = methods::load_session_response()
        .config_options(vec![methods::model_config_option(&spec, &srv.model_specs)]);
    install_session(srv, handle, spec, current_mode, plan_path, params);
    Ok(AgentResponse::LoadSessionResponse(resp))
}

async fn retire_session(srv: &mut Server) {
    let Some(session) = srv.session.take() else {
        return;
    };
    let SessionState {
        handle,
        event_pump,
        pending_prompt,
        ..
    } = session;
    if let Some((id, _)) = take_pending(&pending_prompt) {
        let response = PromptResponse::new(StopReason::Cancelled);
        send(
            &srv.out_tx,
            Response::new(id, Ok(AgentResponse::PromptResponse(response))),
        );
    }
    let _ = handle.cancel_tx.try_send(());
    event_pump.cancel().await;
    handle.task.cancel().await;
}

fn spawn_session(
    params: &AcpParams,
    cwd: PathBuf,
    session_id: Option<SessionRef>,
    history: Vec<Message>,
) -> InteractiveHandle {
    headless::spawn_interactive(InteractiveParams {
        model: params.model.clone(),
        config: Arc::clone(&params.config),
        permissions_config: params.permissions_config.clone(),
        timeouts: params.timeouts,
        openai_options: params.openai_options,
        prompt_slots: Arc::clone(&params.prompt_slots),
        state_persistence: params.state_persistence.clone(),
        excluded_tools: Vec::new(),
        mcp_handle: params.mcp_handle.clone(),
        initial_wd: cwd,
        session_id,
        initial_history: history,
        yolo: params.yolo,
        system_prompt_override: None,
        append_system_prompt: None,
        workflow: false,
        mode: AgentMode::Build,
    })
}

fn install_session(
    srv: &mut Server,
    handle: InteractiveHandle,
    current_model: String,
    current_mode: AgentMode,
    plan_path: Option<PathBuf>,
    params: &AcpParams,
) {
    let pending = Arc::new(Mutex::new(PendingPromptState::default()));
    let event_pump = start_event_pump(
        handle.event_rx.clone(),
        handle.session_id.clone(),
        srv.out_tx.clone(),
        Arc::clone(&pending),
    );
    let daemon = params.session_daemon_register.and_then(|register| {
        n00n_storage::StateDir::resolve()
            .ok()
            .and_then(|storage| register(storage.path(), &handle, &current_model))
    });
    srv.session = Some(SessionState {
        handle,
        current_mode,
        plan_path,
        current_model,
        pending_prompt: pending,
        event_pump,
        _daemon: daemon,
    });
}

type StoredSession = Session<Message, TokenUsage, n00n_agent::ToolOutput>;

fn load_session_from(
    storage: &n00n_storage::StateDir,
    session_id: n00nId,
) -> Result<StoredSession, AcpError> {
    Session::load(session_id, storage).map_err(|e| {
        AcpError::resource_not_found(Some(format!("session/{session_id}"))).data(json_str(&e))
    })
}

#[cfg(test)]
fn load_history_from(
    storage: &n00n_storage::StateDir,
    session_id: n00nId,
) -> Result<Vec<Message>, AcpError> {
    Ok(load_session_from(storage, session_id)?.messages)
}

fn handle_prompt(srv: &mut Server, raw: &Value, id: &RequestId) -> Result<(), AcpError> {
    let req: PromptRequest = parse_params(raw)?;
    let session = srv.session.as_ref().ok_or_else(no_session)?;

    let (message, images) = extract_prompt_content(&req.prompt);
    let input = AgentInput {
        message,
        mode: session.current_mode.clone(),
        images,
        preamble: Vec::new(),
        thinking: n00n_providers::ThinkingConfig::default(),
        fast: false,
        workflow: false,
        control: false,
        prompt: None,
        plan_path: session.plan_path.clone(),
    };

    let mut pending = session
        .pending_prompt
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if pending.id.is_some() {
        return Err(AcpError::new(-32600, "a prompt is already running"));
    }
    pending.id = Some(id.clone());
    pending.cancel_requested = false;
    if session.handle.input_tx.send(input).is_err() {
        pending.id = None;
        return Err(AcpError::new(-32603, "session ended"));
    }
    Ok(())
}

fn handle_set_mode(srv: &mut Server, raw: &Value) -> Result<AgentResponse, AcpError> {
    let req: SetSessionModeRequest = parse_params(raw)?;
    let mode_str = req.mode_id.0.to_string();
    let new_mode = methods::mode_id_to_agent_mode(&mode_str)
        .ok_or_else(|| AcpError::new(-32602, format!("unknown mode: {mode_str}")))?;

    let session = srv.session.as_mut().ok_or_else(no_session)?;
    session.current_mode = new_mode;
    if let AgentMode::Plan(path) = &session.current_mode {
        session.plan_path = Some(path.clone());
    }

    let sid = SessionId::from(session.handle.session_id.to_string());
    session_update(
        &srv.out_tx,
        &sid,
        SessionUpdate::CurrentModeUpdate(CurrentModeUpdate::new(SessionModeId::from(mode_str))),
    );
    Ok(AgentResponse::SetSessionModeResponse(
        SetSessionModeResponse::new(),
    ))
}

fn handle_set_config(srv: &mut Server, raw: &Value) -> Result<AgentResponse, AcpError> {
    let req: SetSessionConfigOptionRequest = parse_params(raw)?;
    if req.config_id.0.as_ref() != methods::MODEL_CONFIG_ID {
        let detail = format!("unknown config option: {}", req.config_id);
        return Err(AcpError::invalid_params().data(json_str(&detail)));
    }

    let spec = req.value.0.to_string();
    let model =
        Model::from_spec(&spec).map_err(|e| AcpError::invalid_params().data(json_str(&e)))?;

    let session = srv.session.as_mut().ok_or_else(no_session)?;
    session
        .handle
        .model_tx
        .send(model)
        .map_err(|_| AcpError::new(-32603, "session ended"))?;
    session.current_model.clone_from(&spec);

    Ok(AgentResponse::SetSessionConfigOptionResponse(
        SetSessionConfigOptionResponse::new(vec![methods::model_config_option(
            &spec,
            &srv.model_specs,
        )]),
    ))
}

fn handle_notification(srv: &Server, method: &str) {
    if method == "session/cancel" {
        if let Some(session) = &srv.session {
            let mut pending = session
                .pending_prompt
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if pending.id.is_some() {
                pending.cancel_requested = true;
                let _ = session.handle.cancel_tx.try_send(());
            }
        }
    } else {
        debug!(method, "unknown notification");
    }
}

fn handle_incoming_response(srv: &Server, raw: &Value) {
    let Some(session) = &srv.session else { return };

    if let Some(result) = raw.get("result")
        && let Ok(resp) = serde_json::from_value::<RequestPermissionResponse>(result.clone())
    {
        let answer = permissions::outcome_to_answer(&resp.outcome);
        let _ = session.handle.answer_tx.send(answer.encode());
    }
}

fn extract_prompt_content(blocks: &[ContentBlock]) -> (String, Vec<ImageSource>) {
    let mut text = String::new();
    let mut images = Vec::new();

    for block in blocks {
        match block {
            ContentBlock::Text(TextContent { text: t, .. }) => append(&mut text, t),
            ContentBlock::Image(ImageContent {
                data, mime_type, ..
            }) => images.push(ImageSource::new(
                image_media_type(mime_type),
                Arc::from(data.as_str()),
            )),
            ContentBlock::Resource(res) => {
                if let EmbeddedResourceResource::TextResourceContents(trc) = &res.resource {
                    append(&mut text, &format!("--- {} ---\n{}", trc.uri, trc.text));
                }
            }
            ContentBlock::ResourceLink(rl) => append(&mut text, &format!("[Resource: {}]", rl.uri)),
            _ => {}
        }
    }

    (text, images)
}

fn append(text: &mut String, part: &str) {
    if !text.is_empty() {
        text.push('\n');
    }
    text.push_str(part);
}

fn image_media_type(mime: &str) -> ImageMediaType {
    match mime {
        "image/png" => ImageMediaType::Png,
        "image/gif" => ImageMediaType::Gif,
        "image/webp" => ImageMediaType::Webp,
        _ => ImageMediaType::Jpeg,
    }
}

fn start_event_pump(
    event_rx: Receiver<Envelope>,
    session_id: SessionRef,
    out_tx: Sender<Value>,
    pending: PendingPrompt,
) -> smol::Task<()> {
    smol::spawn(async move {
        let sid = SessionId::from(session_id.to_string());
        let mut next_request_id = FIRST_OUTGOING_REQUEST_ID;

        while let Ok(Envelope {
            event, subagent, ..
        }) = event_rx.recv_async().await
        {
            if subagent.is_some() {
                continue;
            }

            let update = match event {
                AgentEvent::TextDelta { text } => translate::text_delta(&text),
                AgentEvent::ThinkingDelta { text } => translate::thinking_delta(&text),
                AgentEvent::ToolPending { id, name } => translate::tool_pending(&id, &name),
                AgentEvent::ToolStart(event) => translate::tool_start(&event),
                AgentEvent::ToolOutput { id, content } => translate::tool_output(&id, &content),
                AgentEvent::ToolDone(event) => translate::tool_done(&event),
                AgentEvent::PermissionRequest { id, tool, scopes } => {
                    let fields =
                        ToolCallUpdateFields::new().title(format!("{tool}: {}", scopes.join(", ")));
                    let request =
                        AgentRequest::RequestPermissionRequest(RequestPermissionRequest::new(
                            sid.clone(),
                            ToolCallUpdate::new(ToolCallId::from(id), fields),
                            permissions::permission_options(),
                        ));
                    next_request_id += 1;
                    send(
                        &out_tx,
                        Request {
                            id: RequestId::Number(next_request_id),
                            method: Arc::from(request.method()),
                            params: Some(request),
                        },
                    );
                    continue;
                }
                AgentEvent::Done { stop_reason, .. } => {
                    if let Some((id, _)) = take_pending(&pending) {
                        let resp = PromptResponse::new(translate::map_stop_reason(stop_reason));
                        send(
                            &out_tx,
                            Response::new(id, Ok(AgentResponse::PromptResponse(resp))),
                        );
                    }
                    continue;
                }
                AgentEvent::Error { message } => {
                    if let Some((id, cancel_requested)) = take_pending(&pending) {
                        if cancel_requested {
                            let resp = PromptResponse::new(StopReason::Cancelled);
                            send(
                                &out_tx,
                                Response::new(id, Ok(AgentResponse::PromptResponse(resp))),
                            );
                        } else {
                            let error = AcpError::internal_error().data(Value::String(message));
                            send(&out_tx, Response::<AgentResponse>::new(id, Err(error)));
                        }
                    }
                    continue;
                }
                _ => continue,
            };
            session_update(&out_tx, &sid, update);
        }
    })
}

fn take_pending(pending: &PendingPrompt) -> Option<(RequestId, bool)> {
    let mut pending = pending
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    pending
        .id
        .take()
        .map(|id| (id, std::mem::take(&mut pending.cancel_requested)))
}

fn send(out_tx: &Sender<Value>, msg: impl Serialize) {
    if let Ok(json) = serde_json::to_value(JsonRpcMessage::wrap(msg)) {
        let _ = out_tx.send(json);
    }
}

fn session_update(out_tx: &Sender<Value>, sid: &SessionId, update: SessionUpdate) {
    let notification =
        AgentNotification::SessionNotification(SessionNotification::new(sid.clone(), update));
    send(
        out_tx,
        Notification {
            method: Arc::from("session/update"),
            params: Some(notification),
        },
    );
}

fn no_session() -> AcpError {
    AcpError::new(-32600, "no active session")
}

fn parse_params<T: serde::de::DeserializeOwned>(raw: &Value) -> Result<T, AcpError> {
    serde_json::from_value(
        raw.get("params")
            .cloned()
            .map_or(Value::Null, std::convert::identity),
    )
    .map_err(|e| AcpError::invalid_params().data(json_str(&e)))
}

fn json_str(e: &impl std::fmt::Display) -> Value {
    Value::String(e.to_string())
}

#[cfg(test)]
mod tests {
    use n00n_providers::{ContentBlock as MsgBlock, Role, TokenUsage};
    use n00n_storage::StateDir;
    use n00n_storage::sessions::Session;
    use tempfile::TempDir;

    use super::*;

    #[test]
    fn load_history_round_trips_stored_messages() {
        let tmp = TempDir::new().unwrap();
        let dir = StateDir::from_path(tmp.path().to_path_buf());
        let messages = vec![
            Message::user("rename foo to bar".into()),
            Message {
                role: Role::Assistant,
                content: vec![MsgBlock::Text {
                    text: "done".into(),
                }],
                display_text: None,
                control: false,
            },
        ];
        let mut session: Session<Message, TokenUsage, n00n_agent::ToolOutput> =
            Session::new("anthropic/test-model", "/project");
        session.messages = messages.clone();
        session.save(&dir).unwrap();

        let id: n00nId = session.id;
        let history = load_history_from(&dir, id).unwrap();
        assert_eq!(
            serde_json::to_value(&history).unwrap(),
            serde_json::to_value(&messages).unwrap()
        );
    }

    #[test]
    fn load_missing_session_is_resource_not_found() {
        let tmp = TempDir::new().unwrap();
        let dir = StateDir::from_path(tmp.path().to_path_buf());
        let err = load_history_from(&dir, n00nId::generate()).unwrap_err();
        assert_eq!(err.code, AcpError::resource_not_found(None).code);
    }
}

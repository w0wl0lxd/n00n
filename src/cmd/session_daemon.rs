//! In-process session registration for headless modes (print, ACP).

use std::path::Path;
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};

use n00n_agent::headless::InteractiveHandle;
use n00n_agent::{AgentInput, mode_and_plan_from_stored};
use n00n_daemon::backend::WorkerBackend;
use n00n_daemon::error::{ControlError, ControlResult};
use n00n_daemon::lock::DaemonRole;
use n00n_daemon::protocol::{AgentRecord, BackendKind, MessageOpts};
use n00n_daemon::registry::{ControlPlane, TuiCallbackBackend};
use n00n_daemon::server;
use n00n_providers::Message;
use n00n_providers::ThinkingConfig;
use n00n_providers::TokenUsage;
use n00n_storage::StateDir;
use n00n_storage::id::SessionRef;
use n00n_storage::sessions::{Session, StoredMode};
use serde_json::Value;

const STATUS_WORKING: &str = "working";
const PRINT_TITLE: &str = "print";
const PRINT_MESSAGE_ERR: &str = "print mode is one-shot; message not supported";
const NO_SESSION_ERR: &str = "no live headless session";

/// Register an ACP/interactive session on `daemon.sock` until the returned guard is dropped.
pub fn register_acp_session(
    state_dir: &Path,
    handle: &InteractiveHandle,
    model: &str,
) -> Option<SessionDaemonHandle> {
    let session_id = handle.session_id.to_string();
    let model = model.to_owned();
    let status = Arc::new(Mutex::new(STATUS_WORKING.to_owned()));
    try_spawn(
        state_dir,
        AgentRecord {
            id: session_id.clone(),
            backend: BackendKind::Tui,
            session_id: Some(session_id.clone()),
            status: STATUS_WORKING.into(),
            title: None,
            model: Some(model.clone()),
            output: None,
            cwd: None,
        },
        {
            let status = Arc::clone(&status);
            let session_id = session_id.clone();
            let model = model.clone();
            move || list_one(&status, &session_id, &model, None)
        },
        {
            let status = Arc::clone(&status);
            let session_id = session_id.clone();
            move |id| status_one(&status, id, &session_id, &model, None)
        },
        {
            let input_tx = handle.input_tx.clone();
            let session_id = session_id.clone();
            let state_dir = state_dir.to_path_buf();
            move |id, text, opts| {
                message_interactive(&input_tx, id, &session_id, text, opts, &state_dir)
            }
        },
        {
            let cancel_tx = handle.cancel_tx.clone();
            move |id| stop_interactive(&cancel_tx, id, &session_id)
        },
    )
}

/// Register a one-shot print session (list/status only; message/stop unsupported).
#[must_use]
pub fn register_print_session(
    state_dir: &Path,
    session_id: &SessionRef,
    model: &str,
    status: &Arc<Mutex<String>>,
) -> Option<SessionDaemonHandle> {
    let sid = session_id.to_string();
    let model_owned = model.to_owned();
    try_spawn(
        state_dir,
        AgentRecord {
            id: sid.clone(),
            backend: BackendKind::Tui,
            session_id: Some(sid.clone()),
            status: STATUS_WORKING.into(),
            title: Some(PRINT_TITLE.into()),
            model: Some(model_owned.clone()),
            output: None,
            cwd: None,
        },
        {
            let status = Arc::clone(status);
            let sid = sid.clone();
            let model_owned = model_owned.clone();
            move || list_one(&status, &sid, &model_owned, Some(PRINT_TITLE))
        },
        {
            let status = Arc::clone(status);
            move |id| status_one(&status, id, &sid, &model_owned, Some(PRINT_TITLE))
        },
        |_id, _text, _opts| Err(ControlError::Unavailable(PRINT_MESSAGE_ERR.into())),
        |_id| Err(ControlError::Unavailable(PRINT_MESSAGE_ERR.into())),
    )
}

pub struct SessionDaemonHandle {
    cancel: flume::Sender<()>,
    join: Option<JoinHandle<()>>,
}

impl Drop for SessionDaemonHandle {
    fn drop(&mut self) {
        let _ = self.cancel.send(());
        if let Some(handle) = self.join.take()
            && handle.join().is_err()
        {
            tracing::warn!("session daemon listener thread panicked on drop");
        }
    }
}

fn try_spawn(
    state_dir: &Path,
    _seed: AgentRecord,
    list: impl Fn() -> ControlResult<Vec<AgentRecord>> + Send + Sync + 'static,
    status: impl Fn(&str) -> ControlResult<AgentRecord> + Send + Sync + 'static,
    message: impl Fn(&str, &str, &MessageOpts) -> ControlResult<Value> + Send + Sync + 'static,
    stop: impl Fn(&str) -> ControlResult<()> + Send + Sync + 'static,
) -> Option<SessionDaemonHandle> {
    match spawn(state_dir, list, status, message, stop) {
        Ok(h) => Some(h),
        Err(e) => {
            tracing::warn!(error = %e, "failed to start session daemon listener");
            None
        }
    }
}

fn spawn(
    state_dir: &Path,
    list: impl Fn() -> ControlResult<Vec<AgentRecord>> + Send + Sync + 'static,
    status: impl Fn(&str) -> ControlResult<AgentRecord> + Send + Sync + 'static,
    message: impl Fn(&str, &str, &MessageOpts) -> ControlResult<Value> + Send + Sync + 'static,
    stop: impl Fn(&str) -> ControlResult<()> + Send + Sync + 'static,
) -> ControlResult<SessionDaemonHandle> {
    let plane = Arc::new(ControlPlane::new(
        Some(Arc::new(TuiCallbackBackend::new(
            list,
            status,
            message,
            |_id| {
                Err(ControlError::Unsupported {
                    backend: BackendKind::Tui,
                    verb: "resume",
                })
            },
            stop,
        ))),
        Some(Arc::new(WorkerBackend::new(state_dir))),
    ));
    let (cancel, cancel_rx) = flume::bounded(1);
    let dir = state_dir.to_path_buf();
    let join = thread::Builder::new()
        .name("n00n-session-daemon".into())
        .spawn(move || {
            if let Err(e) =
                smol::block_on(server::serve(&dir, plane, cancel_rx, DaemonRole::Headless))
            {
                match e {
                    ControlError::Unavailable(_) => {
                        tracing::debug!(error = %e, "session daemon listener stopped");
                    }
                    _ => {
                        tracing::warn!(error = %e, "session daemon listener stopped");
                    }
                }
            }
        })
        .map_err(ControlError::io)?;
    Ok(SessionDaemonHandle {
        cancel,
        join: Some(join),
    })
}

fn list_one(
    status: &Arc<Mutex<String>>,
    session_id: &str,
    model: &str,
    title: Option<&str>,
) -> ControlResult<Vec<AgentRecord>> {
    let status = status
        .lock()
        .map_err(|e| ControlError::Unavailable(e.to_string()))?;
    Ok(vec![AgentRecord {
        id: session_id.to_owned(),
        backend: BackendKind::Tui,
        session_id: Some(session_id.to_owned()),
        status: status.clone(),
        title: title.map(str::to_owned),
        model: Some(model.to_owned()),
        output: None,
        cwd: None,
    }])
}

fn status_one(
    status: &Arc<Mutex<String>>,
    id: &str,
    session_id: &str,
    model: &str,
    title: Option<&str>,
) -> ControlResult<AgentRecord> {
    if id != session_id {
        return Err(ControlError::NotFound(id.to_owned()));
    }
    let status = status
        .lock()
        .map_err(|e| ControlError::Unavailable(e.to_string()))?;
    Ok(AgentRecord {
        id: session_id.to_owned(),
        backend: BackendKind::Tui,
        session_id: Some(session_id.to_owned()),
        status: status.clone(),
        title: title.map(str::to_owned),
        model: Some(model.to_owned()),
        output: None,
        cwd: None,
    })
}

fn message_interactive(
    input_tx: &flume::Sender<AgentInput>,
    id: &str,
    session_id: &str,
    text: &str,
    opts: &MessageOpts,
    state_dir: &Path,
) -> ControlResult<Value> {
    if id != session_id {
        return Err(ControlError::NotFound(id.to_owned()));
    }
    let (mode, plan_path) = {
        let session_ref = session_id
            .parse::<SessionRef>()
            .map_err(|_| ControlError::InvalidId(session_id.to_owned()))?;
        let storage = StateDir::from_path(state_dir.to_path_buf());
        let mut session = Session::<Message, TokenUsage, n00n_agent::ToolOutput>::load(
            session_ref.id(),
            &storage,
        )
        .map_err(|e| ControlError::Unavailable(e.to_string()))?;
        mode_and_plan_for_daemon(&storage, &mut session)?
    };
    input_tx
        .try_send(AgentInput {
            message: text.to_owned(),
            mode,
            images: Vec::new(),
            preamble: Vec::new(),
            thinking: ThinkingConfig::default(),
            fast: false,
            workflow: false,
            prompt: None,
            control: opts.control,
            plan_path,
        })
        .map_err(|_| ControlError::Unavailable(NO_SESSION_ERR.into()))?;
    Ok(serde_json::json!({"queued": true, "steer": opts.steer, "control": opts.control}))
}

fn mode_and_plan_for_daemon(
    storage: &StateDir,
    session: &mut Session<Message, TokenUsage, n00n_agent::ToolOutput>,
) -> ControlResult<(n00n_agent::AgentMode, Option<std::path::PathBuf>)> {
    let (mode, plan_path) = mode_and_plan_from_stored(storage, &session.meta);
    if session.meta.mode == Some(StoredMode::Plan) && session.meta.plan_path.is_none() {
        let plan_path = plan_path
            .as_ref()
            .ok_or_else(|| ControlError::Unavailable("missing generated plan path".into()))?;
        session.meta.plan_path = Some(plan_path.display().to_string());
        session
            .save(storage)
            .map_err(|e| ControlError::Unavailable(e.to_string()))?;
    }
    Ok((mode, plan_path))
}

fn stop_interactive(
    cancel_tx: &flume::Sender<()>,
    id: &str,
    session_id: &str,
) -> ControlResult<()> {
    if id != session_id {
        return Err(ControlError::NotFound(id.to_owned()));
    }
    cancel_tx
        .try_send(())
        .map_err(|_| ControlError::Unavailable(NO_SESSION_ERR.into()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use n00n_agent::AgentMode;

    use super::*;

    #[test]
    fn legacy_plan_session_persists_generated_path() {
        let temp = tempfile::tempdir().unwrap();
        let storage = StateDir::from_path(temp.path().to_path_buf());
        let mut session =
            Session::<Message, TokenUsage, n00n_agent::ToolOutput>::new("model", "/tmp");
        session.meta.mode = Some(StoredMode::Plan);
        session.save(&storage).unwrap();

        let (mode, plan_path) = mode_and_plan_for_daemon(&storage, &mut session).unwrap();
        let plan_path = plan_path.unwrap();
        assert!(matches!(mode, AgentMode::Plan(_)));

        let mut persisted =
            Session::<Message, TokenUsage, n00n_agent::ToolOutput>::load(session.id, &storage)
                .unwrap();
        assert_eq!(persisted.meta.plan_path.as_deref(), plan_path.to_str());
        assert_eq!(
            mode_and_plan_for_daemon(&storage, &mut persisted)
                .unwrap()
                .1,
            Some(plan_path)
        );
    }
}

//! Union control plane over TUI + worker backends.

use std::sync::Arc;

use crate::backend::ControlBackend;
use crate::error::{ControlError, ControlResult};
use crate::protocol::{
    AgentRecord, BackendKind, ControlRequest, ControlResponse, MessageOpts, PROTOCOL_VERSION,
};

type ListCb = Box<dyn Fn() -> ControlResult<Vec<AgentRecord>> + Send + Sync>;
type StatusCb = Box<dyn Fn(&str) -> ControlResult<AgentRecord> + Send + Sync>;
type MessageCb =
    Box<dyn Fn(&str, &str, &MessageOpts) -> ControlResult<sonic_rs::Value> + Send + Sync>;
type StopCb = Box<dyn Fn(&str) -> ControlResult<()> + Send + Sync>;
type ResumeCb = Box<dyn Fn(&str) -> ControlResult<()> + Send + Sync>;

#[derive(Clone)]
pub struct ControlPlane {
    tui: Option<Arc<dyn ControlBackend>>,
    worker: Option<Arc<dyn ControlBackend>>,
}

impl ControlPlane {
    #[must_use]
    pub fn new(
        tui: Option<Arc<dyn ControlBackend>>,
        worker: Option<Arc<dyn ControlBackend>>,
    ) -> Self {
        Self { tui, worker }
    }

    /// # Errors
    /// Propagates backend failures.
    pub fn handle(&self, req: ControlRequest) -> ControlResult<ControlResponse> {
        match req {
            ControlRequest::Health => Ok(ControlResponse::health_ok()),
            ControlRequest::List => {
                let mut agents = Vec::new();
                if let Some(tui) = &self.tui {
                    agents.extend(tui.list()?);
                }
                if let Some(worker) = &self.worker {
                    agents.extend(worker.list()?);
                }
                Ok(ControlResponse::Ok {
                    agents: Some(agents),
                    agent: None,
                    version: Some(PROTOCOL_VERSION),
                    state: None,
                })
            }
            ControlRequest::Status { id, backend } => {
                let agent = self.resolve(&id, backend, |b, id| b.status(id))?;
                Ok(ControlResponse::Ok {
                    agents: None,
                    agent: Some(agent),
                    version: None,
                    state: None,
                })
            }
            ControlRequest::Message {
                id,
                text,
                backend,
                opts,
            } => {
                let state = self.resolve(&id, backend, |b, id| b.message(id, &text, &opts))?;
                Ok(ControlResponse::Ok {
                    agents: None,
                    agent: None,
                    version: None,
                    state: Some(state),
                })
            }
            ControlRequest::Pause { id, backend } => {
                self.resolve(&id, backend, |b, id| b.pause(id))?;
                Ok(ControlResponse::Ok {
                    agents: None,
                    agent: None,
                    version: None,
                    state: Some(sonic_rs::json!({"paused": true, "id": id})),
                })
            }
            ControlRequest::Resume { id, backend } => {
                self.resolve(&id, backend, |b, id| b.resume(id))?;
                Ok(ControlResponse::Ok {
                    agents: None,
                    agent: None,
                    version: None,
                    state: Some(sonic_rs::json!({"resumed": true, "id": id})),
                })
            }
            ControlRequest::Stop { id, backend } => {
                self.resolve(&id, backend, |b, id| b.stop(id))?;
                Ok(ControlResponse::Ok {
                    agents: None,
                    agent: None,
                    version: None,
                    state: Some(sonic_rs::json!({"stopped": true, "id": id})),
                })
            }
        }
    }

    fn backend(&self, kind: BackendKind) -> ControlResult<&Arc<dyn ControlBackend>> {
        match kind {
            BackendKind::Tui => self
                .tui
                .as_ref()
                .ok_or_else(|| ControlError::Unavailable("tui backend not registered".into())),
            BackendKind::Worker => self
                .worker
                .as_ref()
                .ok_or_else(|| ControlError::Unavailable("worker backend not registered".into())),
        }
    }

    fn resolve<T>(
        &self,
        id: &str,
        hint: Option<BackendKind>,
        f: impl Fn(&dyn ControlBackend, &str) -> ControlResult<T>,
    ) -> ControlResult<T> {
        if let Some(kind) = hint {
            return f(self.backend(kind)?.as_ref(), id);
        }
        if let Some(tui) = &self.tui {
            match f(tui.as_ref(), id) {
                Ok(v) => return Ok(v),
                Err(ControlError::NotFound(_)) => {}
                Err(e) => return Err(e),
            }
        }
        if let Some(worker) = &self.worker {
            return f(worker.as_ref(), id);
        }
        Err(ControlError::NotFound(id.to_owned()))
    }
}

/// TUI backend: pause stays unsupported; resume uses host callback when supplied.
pub struct TuiCallbackBackend {
    list: ListCb,
    status: StatusCb,
    message: MessageCb,
    resume: ResumeCb,
    stop: StopCb,
}

impl TuiCallbackBackend {
    #[must_use]
    pub fn new(
        list: impl Fn() -> ControlResult<Vec<AgentRecord>> + Send + Sync + 'static,
        status: impl Fn(&str) -> ControlResult<AgentRecord> + Send + Sync + 'static,
        message: impl Fn(&str, &str, &MessageOpts) -> ControlResult<sonic_rs::Value>
        + Send
        + Sync
        + 'static,
        resume: impl Fn(&str) -> ControlResult<()> + Send + Sync + 'static,
        stop: impl Fn(&str) -> ControlResult<()> + Send + Sync + 'static,
    ) -> Self {
        Self {
            list: Box::new(list),
            status: Box::new(status),
            message: Box::new(message),
            resume: Box::new(resume),
            stop: Box::new(stop),
        }
    }
}

impl ControlBackend for TuiCallbackBackend {
    fn list(&self) -> ControlResult<Vec<AgentRecord>> {
        (self.list)()
    }

    fn status(&self, id: &str) -> ControlResult<AgentRecord> {
        (self.status)(id)
    }

    fn message(&self, id: &str, text: &str, opts: &MessageOpts) -> ControlResult<sonic_rs::Value> {
        (self.message)(id, text, opts)
    }

    fn pause(&self, _id: &str) -> ControlResult<()> {
        Err(ControlError::Unsupported {
            backend: BackendKind::Tui,
            verb: "pause",
        })
    }

    fn resume(&self, id: &str) -> ControlResult<()> {
        (self.resume)(id)
    }

    fn stop(&self, id: &str) -> ControlResult<()> {
        (self.stop)(id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::WorkerBackend;
    use std::sync::Mutex;
    use tempfile::TempDir;

    fn mem_tui() -> Arc<TuiCallbackBackend> {
        let agents = Arc::new(Mutex::new(vec![AgentRecord {
            id: "tui-1".into(),
            backend: BackendKind::Tui,
            session_id: Some("tui-1".into()),
            status: "idle".into(),
            title: Some("main".into()),
            model: None,
            output: None,
        }]));
        let agents_list = Arc::clone(&agents);
        let agents_status = Arc::clone(&agents);
        Arc::new(TuiCallbackBackend::new(
            move || {
                agents_list
                    .lock()
                    .map(|g| g.clone())
                    .map_err(|e| ControlError::Unavailable(e.to_string()))
            },
            move |id| {
                let guard = agents_status
                    .lock()
                    .map_err(|e| ControlError::Unavailable(e.to_string()))?;
                guard
                    .iter()
                    .find(|a| a.id == id)
                    .cloned()
                    .ok_or_else(|| ControlError::NotFound(id.to_owned()))
            },
            |_id, _text, _opts| Ok(sonic_rs::json!({"queued": true})),
            |_id| {
                Err(ControlError::Unsupported {
                    backend: BackendKind::Tui,
                    verb: "resume",
                })
            },
            |_id| Ok(()),
        ))
    }

    #[test]
    fn list_unions_backends() -> Result<(), String> {
        let tmp = TempDir::new().map_err(|e| e.to_string())?;
        let worker = Arc::new(WorkerBackend::new(tmp.path()));
        let plane = ControlPlane::new(Some(mem_tui()), Some(worker));
        let resp = plane
            .handle(ControlRequest::List)
            .map_err(|e| e.to_string())?;
        match resp {
            ControlResponse::Ok {
                agents: Some(agents),
                ..
            } => {
                assert_eq!(agents.len(), 1);
                assert_eq!(agents[0].backend, BackendKind::Tui);
            }
            other => return Err(format!("unexpected {other:?}")),
        }
        Ok(())
    }

    #[test]
    fn pause_on_tui_is_unsupported() -> Result<(), String> {
        let plane = ControlPlane::new(Some(mem_tui()), None);
        let err = match plane.handle(ControlRequest::Pause {
            id: "tui-1".into(),
            backend: Some(BackendKind::Tui),
        }) {
            Err(e) => e,
            Ok(r) => return Err(format!("expected error, got {r:?}")),
        };
        match err {
            ControlError::Unsupported { backend, verb } => {
                assert_eq!(backend, BackendKind::Tui);
                assert_eq!(verb, "pause");
            }
            other => return Err(format!("expected Unsupported, got {other:?}")),
        }
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn uds_client_server_health_and_list() -> Result<(), String> {
        use crate::client;
        use crate::server;
        use std::time::Duration;

        let tmp = TempDir::new().map_err(|e| e.to_string())?;
        let plane = Arc::new(ControlPlane::new(Some(mem_tui()), None));
        let (cancel_tx, cancel_rx) = flume::bounded(1);
        let dir_serve = tmp.path().to_path_buf();
        let plane_serve = Arc::clone(&plane);
        let handle = std::thread::spawn(move || {
            smol::block_on(server::serve(
                &dir_serve,
                plane_serve,
                cancel_rx,
                crate::lock::DaemonRole::Tui,
            ))
        });

        let sock = crate::paths::daemon_socket_in(tmp.path());
        let mut connected = false;
        for _ in 0..50 {
            std::thread::sleep(Duration::from_millis(20));
            if !sock.exists() {
                continue;
            }
            match client::call_blocking(tmp.path(), &ControlRequest::Health) {
                Ok(ControlResponse::Ok {
                    version: Some(v), ..
                }) if v == PROTOCOL_VERSION => {
                    connected = true;
                    break;
                }
                _ => {}
            }
        }
        if !connected {
            let _ = cancel_tx.send(());
            let _ = handle.join();
            return Err("failed to connect to daemon".into());
        }

        let list =
            client::call_blocking(tmp.path(), &ControlRequest::List).map_err(|e| e.to_string())?;
        match list {
            ControlResponse::Ok {
                agents: Some(agents),
                ..
            } => {
                assert_eq!(agents.len(), 1);
                assert_eq!(agents[0].backend, BackendKind::Tui);
            }
            other => {
                let _ = cancel_tx.send(());
                let _ = handle.join();
                return Err(format!("bad list: {other:?}"));
            }
        }

        let _ = cancel_tx.send(());
        match handle.join() {
            Ok(Ok(())) => Ok(()),
            Ok(Err(e)) => Err(e.to_string()),
            Err(_) => Err("server thread panicked".into()),
        }
    }

    #[cfg(windows)]
    #[test]
    fn tcp_client_server_health_and_list() -> Result<(), String> {
        use crate::client;
        use crate::server;
        use std::time::Duration;

        let tmp = TempDir::new().map_err(|e| e.to_string())?;
        let plane = Arc::new(ControlPlane::new(Some(mem_tui()), None));
        let (cancel_tx, cancel_rx) = flume::bounded(1);
        let dir_serve = tmp.path().to_path_buf();
        let plane_serve = Arc::clone(&plane);
        let handle = std::thread::spawn(move || {
            smol::block_on(server::serve(
                &dir_serve,
                plane_serve,
                cancel_rx,
                crate::lock::DaemonRole::Tui,
            ))
        });

        let mut connected = false;
        for _ in 0..50 {
            std::thread::sleep(Duration::from_millis(20));
            match client::call_blocking(tmp.path(), &ControlRequest::Health) {
                Ok(ControlResponse::Ok {
                    version: Some(v), ..
                }) if v == PROTOCOL_VERSION => {
                    connected = true;
                    break;
                }
                _ => {}
            }
        }
        if !connected {
            let _ = cancel_tx.send(());
            let _ = handle.join();
            return Err("failed to connect to tcp daemon".into());
        }

        let list =
            client::call_blocking(tmp.path(), &ControlRequest::List).map_err(|e| e.to_string())?;
        match list {
            ControlResponse::Ok {
                agents: Some(agents),
                ..
            } => {
                assert_eq!(agents.len(), 1);
                assert_eq!(agents[0].backend, BackendKind::Tui);
            }
            other => {
                let _ = cancel_tx.send(());
                let _ = handle.join();
                return Err(format!("bad list: {other:?}"));
            }
        }

        let _ = cancel_tx.send(());
        match handle.join() {
            Ok(Ok(())) => Ok(()),
            Ok(Err(e)) => Err(e.to_string()),
            Err(_) => Err("server thread panicked".into()),
        }
    }

    fn write_worker_fixture(
        dir: &std::path::Path,
        id: &str,
        socket_path: &std::path::Path,
    ) -> Result<(), String> {
        use std::fs;

        const AGENTS_SUBDIR: &str = "agents";
        const STATE_FILE: &str = "agent.json";
        let agent_dir = dir.join(AGENTS_SUBDIR).join(id);
        fs::create_dir_all(&agent_dir).map_err(|e| e.to_string())?;
        let state = sonic_rs::json!({
            "id": id,
            "session_id": "sess-1",
            "socket_path": socket_path.to_string_lossy(),
            "status": "running",
            "model": "test/model",
            "prompt": "smoke",
            "updated_at": 1,
        });
        let encoded = sonic_rs::to_string_pretty(&state).map_err(|e| e.to_string())?;
        fs::write(agent_dir.join(STATE_FILE), encoded).map_err(|e| e.to_string())?;
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn uds_lists_worker_fixture_and_pause_tui_is_unsupported() -> Result<(), String> {
        use crate::client;
        use crate::server;
        use std::time::Duration;

        let tmp = TempDir::new().map_err(|e| e.to_string())?;
        write_worker_fixture(tmp.path(), "worker-1", &tmp.path().join("unused.sock"))?;

        let worker = Arc::new(WorkerBackend::new(tmp.path()));
        let plane = Arc::new(ControlPlane::new(Some(mem_tui()), Some(worker)));
        let (cancel_tx, cancel_rx) = flume::bounded(1);
        let dir_serve = tmp.path().to_path_buf();
        let plane_serve = Arc::clone(&plane);
        let handle = std::thread::spawn(move || {
            smol::block_on(server::serve(
                &dir_serve,
                plane_serve,
                cancel_rx,
                crate::lock::DaemonRole::Worker,
            ))
        });

        let mut connected = false;
        for _ in 0..50 {
            std::thread::sleep(Duration::from_millis(20));
            match client::call_blocking(tmp.path(), &ControlRequest::Health) {
                Ok(ControlResponse::Ok {
                    version: Some(v), ..
                }) if v == PROTOCOL_VERSION => {
                    connected = true;
                    break;
                }
                _ => {}
            }
        }
        if !connected {
            let _ = cancel_tx.send(());
            let _ = handle.join();
            return Err("failed to connect to daemon".into());
        }

        let list =
            client::call_blocking(tmp.path(), &ControlRequest::List).map_err(|e| e.to_string())?;
        match list {
            ControlResponse::Ok {
                agents: Some(agents),
                ..
            } => {
                assert_eq!(agents.len(), 2);
                assert!(
                    agents
                        .iter()
                        .any(|a| a.id == "tui-1" && a.backend == BackendKind::Tui),
                    "missing tui row: {agents:?}"
                );
                assert!(
                    agents
                        .iter()
                        .any(|a| a.id == "worker-1" && a.backend == BackendKind::Worker),
                    "missing worker row: {agents:?}"
                );
            }
            other => {
                let _ = cancel_tx.send(());
                let _ = handle.join();
                return Err(format!("bad list: {other:?}"));
            }
        }

        let pause_tui = client::call_blocking(
            tmp.path(),
            &ControlRequest::Pause {
                id: "tui-1".into(),
                backend: Some(BackendKind::Tui),
            },
        )
        .map_err(|e| e.to_string())?;
        match pause_tui {
            ControlResponse::Err {
                code: Some(ref code),
                ..
            } if code == "unsupported" => {}
            other => {
                let _ = cancel_tx.send(());
                let _ = handle.join();
                return Err(format!("expected pause unsupported on tui, got {other:?}"));
            }
        }

        let _ = cancel_tx.send(());
        match handle.join() {
            Ok(Ok(())) => Ok(()),
            Ok(Err(e)) => Err(e.to_string()),
            Err(_) => Err("server thread panicked".into()),
        }
    }
}

use std::path::Path;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use n00n_daemon::ControlError;
use n00n_daemon::ControlResult;
use n00n_daemon::backend::ControlBackend;
use n00n_daemon::client;
use n00n_daemon::lock::DaemonRole;
use n00n_daemon::protocol::{
    AgentRecord, BackendKind, ControlRequest, ControlResponse, MessageOpts,
};
use n00n_daemon::registry::ControlPlane;
use n00n_daemon::server;
use tempfile::TempDir;

const AGENT_ID: &str = "test-agent";

fn sample_record() -> AgentRecord {
    AgentRecord {
        id: AGENT_ID.into(),
        backend: BackendKind::Worker,
        session_id: Some("session-1".into()),
        status: "running".into(),
        title: Some("test run".into()),
        model: Some("test/m".into()),
        output: None,
        cwd: Some("/tmp".into()),
    }
}

#[derive(Debug)]
struct MockBackend {
    records: Arc<Mutex<Vec<AgentRecord>>>,
    events: Arc<Mutex<Vec<String>>>,
}

impl MockBackend {
    fn new() -> Self {
        Self {
            records: Arc::new(Mutex::new(vec![sample_record()])),
            events: Arc::new(Mutex::new(Vec::new())),
        }
    }

    fn events(&self) -> Vec<String> {
        self.events
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }
}

impl ControlBackend for MockBackend {
    fn list(&self) -> ControlResult<Vec<AgentRecord>> {
        Ok(self
            .records
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone())
    }

    fn status(&self, id: &str) -> ControlResult<AgentRecord> {
        self.records
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .iter()
            .find(|r| r.id == id)
            .cloned()
            .ok_or_else(|| ControlError::NotFound(id.into()))
    }

    fn message(
        &self,
        id: &str,
        text: &str,
        _opts: &MessageOpts,
    ) -> ControlResult<serde_json::Value> {
        self.events
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(format!("message:{id}:{text}"));
        Ok(serde_json::json!({"queued": true}))
    }

    fn pause(&self, id: &str) -> ControlResult<()> {
        self.events
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(format!("pause:{id}"));
        Ok(())
    }

    fn resume(&self, id: &str) -> ControlResult<()> {
        self.events
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(format!("resume:{id}"));
        Ok(())
    }

    fn stop(&self, id: &str) -> ControlResult<()> {
        self.events
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(format!("stop:{id}"));
        Ok(())
    }
}

fn wait_for_socket(path: &std::path::Path) -> Result<(), String> {
    for _ in 0..100 {
        if path.exists() {
            return Ok(());
        }
        thread::sleep(Duration::from_millis(20));
    }
    Err("daemon socket was not created".into())
}

fn with_server<F>(backend: &Arc<MockBackend>, f: F) -> Result<(), String>
where
    F: FnOnce(&Path, &MockBackend) -> Result<(), String>,
{
    let tmp = TempDir::new().map_err(|e| e.to_string())?;
    let state_dir = tmp.path().to_path_buf();

    let backend_dyn: Arc<dyn ControlBackend> = Arc::<MockBackend>::clone(backend);
    let plane = Arc::new(ControlPlane::new(Some(backend_dyn), None));
    let (cancel_tx, cancel_rx) = flume::bounded(1);

    let server_dir = state_dir.clone();
    let handle = thread::spawn(move || {
        smol::block_on(server::serve(
            &server_dir,
            plane,
            cancel_rx,
            DaemonRole::Tui,
        ))
        .map_err(|e| e.to_string())
    });

    let socket_path = state_dir.join("daemon.sock");
    wait_for_socket(&socket_path)?;

    let result = f(&state_dir, backend);

    let _ = cancel_tx.send(());
    if let Err(e) = handle.join() {
        return Err(format!("server thread panicked: {e:?}"));
    }

    result
}

#[test]
fn health_returns_protocol_version() -> Result<(), String> {
    with_server(&Arc::new(MockBackend::new()), |state_dir, _| {
        let resp =
            client::call_blocking(state_dir, &ControlRequest::Health).map_err(|e| e.to_string())?;
        match resp {
            ControlResponse::Ok { version, .. } => {
                assert_eq!(version, Some(n00n_daemon::protocol::PROTOCOL_VERSION));
            }
            ControlResponse::Err { error, .. } => return Err(error),
        }
        Ok(())
    })
}

#[test]
fn list_returns_agents() -> Result<(), String> {
    with_server(&Arc::new(MockBackend::new()), |state_dir, _| {
        let resp =
            client::call_blocking(state_dir, &ControlRequest::List).map_err(|e| e.to_string())?;
        match resp {
            ControlResponse::Ok { agents, .. } => {
                let agents = agents.ok_or("missing agents list")?;
                assert_eq!(agents.len(), 1);
                assert_eq!(agents[0].id, AGENT_ID);
            }
            ControlResponse::Err { error, .. } => return Err(error),
        }
        Ok(())
    })
}

#[test]
fn status_returns_agent() -> Result<(), String> {
    with_server(&Arc::new(MockBackend::new()), |state_dir, _| {
        let req = ControlRequest::Status {
            id: AGENT_ID.into(),
            backend: None,
        };
        let resp = client::call_blocking(state_dir, &req).map_err(|e| e.to_string())?;
        match resp {
            ControlResponse::Ok { agent, .. } => {
                let agent = agent.ok_or("missing agent record")?;
                assert_eq!(agent.id, AGENT_ID);
                assert_eq!(agent.status, "running");
            }
            ControlResponse::Err { error, .. } => return Err(error),
        }
        Ok(())
    })
}

#[test]
fn message_records_queued_state() -> Result<(), String> {
    with_server(&Arc::new(MockBackend::new()), |state_dir, _| {
        let req = ControlRequest::Message {
            id: AGENT_ID.into(),
            text: "continue".into(),
            backend: None,
            opts: MessageOpts::default(),
        };
        let resp = client::call_blocking(state_dir, &req).map_err(|e| e.to_string())?;
        match resp {
            ControlResponse::Ok { state, .. } => {
                assert!(state.is_some());
            }
            ControlResponse::Err { error, .. } => return Err(error),
        }
        Ok(())
    })
}

#[test]
fn pause_resume_and_stop_are_relayed() -> Result<(), String> {
    let backend = Arc::new(MockBackend::new());
    with_server(&backend, |state_dir, _| {
        for cmd in [
            ControlRequest::Pause {
                id: AGENT_ID.into(),
                backend: None,
            },
            ControlRequest::Resume {
                id: AGENT_ID.into(),
                backend: None,
            },
            ControlRequest::Stop {
                id: AGENT_ID.into(),
                backend: None,
            },
        ] {
            let resp = client::call_blocking(state_dir, &cmd).map_err(|e| e.to_string())?;
            match resp {
                ControlResponse::Ok { .. } => {}
                ControlResponse::Err { error, .. } => return Err(error),
            }
        }
        Ok(())
    })?;
    let events = backend.events();
    assert_eq!(events.len(), 3);
    assert!(events.iter().any(|e| e == "pause:test-agent"));
    assert!(events.iter().any(|e| e == "resume:test-agent"));
    assert!(events.iter().any(|e| e == "stop:test-agent"));
    Ok(())
}

#[test]
fn unknown_agent_returns_not_found() -> Result<(), String> {
    with_server(&Arc::new(MockBackend::new()), |state_dir, _| {
        let req = ControlRequest::Status {
            id: "missing".into(),
            backend: None,
        };
        let resp = client::call_blocking(state_dir, &req).map_err(|e| e.to_string())?;
        match resp {
            ControlResponse::Err { code, .. } => {
                assert_eq!(code.as_deref(), Some("not_found"));
            }
            ControlResponse::Ok { .. } => return Err("expected not_found error".into()),
        }
        Ok(())
    })
}

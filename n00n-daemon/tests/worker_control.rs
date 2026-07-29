use std::path::Path;
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use n00n_daemon::backend::WorkerBackend;
use n00n_daemon::client;
use n00n_daemon::lock::DaemonRole;
use n00n_daemon::protocol::{BackendKind, ControlRequest, ControlResponse, MessageOpts};
use n00n_daemon::registry::ControlPlane;
use n00n_daemon::server;
use tempfile::TempDir;

fn wait_for_socket(path: &Path) -> Result<(), String> {
    for _ in 0..100 {
        if path.exists() {
            return Ok(());
        }
        thread::sleep(Duration::from_millis(20));
    }
    Err("daemon socket was not created".into())
}

fn write_worker_fixture(dir: &Path, id: &str, socket_path: &Path) -> Result<(), String> {
    let agent_dir = dir.join("agents").join(id);
    std::fs::create_dir_all(&agent_dir).map_err(|e| e.to_string())?;
    let state = serde_json::json!({
        "id": id,
        "session_id": "sess-1",
        "socket_path": socket_path.to_string_lossy(),
        "status": "running",
        "model": "test/model",
        "prompt": "hello world",
        "updated_at": 1,
    });
    let encoded = serde_json::to_string_pretty(&state).map_err(|e| e.to_string())?;
    std::fs::write(agent_dir.join("agent.json"), encoded).map_err(|e| e.to_string())?;
    Ok(())
}

fn start_fake_worker(
    socket_path: &Path,
    response: &[u8],
    expected_cmd: &str,
) -> thread::JoinHandle<Result<(), String>> {
    let path = socket_path.to_path_buf();
    let expected = expected_cmd.to_owned();
    let response = response.to_owned();
    thread::spawn(move || {
        smol::block_on(async {
            use futures_lite::{AsyncBufReadExt, AsyncWriteExt, io::BufReader};
            use smol::net::unix::UnixListener;

            let parent = path.parent().ok_or("no socket parent")?;
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
            if path.exists() {
                std::fs::remove_file(&path).map_err(|e| e.to_string())?;
            }
            let listener = UnixListener::bind(&path).map_err(|e| e.to_string())?;
            let (stream, _) = listener.accept().await.map_err(|e| e.to_string())?;
            let (reader, mut writer) = futures_lite::io::split(stream);
            let mut reader = BufReader::new(reader);
            let mut line = String::new();
            reader
                .read_line(&mut line)
                .await
                .map_err(|e| e.to_string())?;
            if !line.contains(&format!("\"cmd\":\"{expected}\"")) {
                return Err(format!("unexpected command line: {line}"));
            }
            writer
                .write_all(&response)
                .await
                .map_err(|e| e.to_string())?;
            writer.write_all(b"\n").await.map_err(|e| e.to_string())?;
            writer.flush().await.map_err(|e| e.to_string())?;
            Ok::<(), String>(())
        })
    })
}

fn with_worker_server<F>(state_dir: &Path, f: F) -> Result<(), String>
where
    F: FnOnce(&Path) -> Result<(), String>,
{
    let plane = Arc::new(ControlPlane::new(
        None,
        Some(Arc::new(WorkerBackend::new(state_dir))),
    ));
    let (cancel_tx, cancel_rx) = flume::bounded(1);

    let server_dir = state_dir.to_path_buf();
    let handle = thread::spawn(move || {
        smol::block_on(server::serve(
            &server_dir,
            plane,
            cancel_rx,
            DaemonRole::Worker,
        ))
        .map_err(|e| e.to_string())
    });

    let socket_path = state_dir.join("daemon.sock");
    wait_for_socket(&socket_path)?;

    let result = f(state_dir);

    let _ = cancel_tx.send(());
    if let Err(e) = handle.join() {
        return Err(format!("server thread panicked: {e:?}"));
    }

    result
}

fn message_worker(
    state_dir: &Path,
    worker_sock: &Path,
    agent_id: &str,
) -> Result<ControlResponse, String> {
    let _worker = start_fake_worker(worker_sock, br#"{"type":"done"}"#, "message");
    let mut resp = None;
    with_worker_server(state_dir, |state_dir| {
        let req = ControlRequest::Message {
            id: agent_id.into(),
            text: "continue".into(),
            backend: Some(BackendKind::Worker),
            opts: MessageOpts::default(),
        };
        resp = Some(client::call_blocking(state_dir, &req).map_err(|e| e.to_string())?);
        Ok(())
    })?;
    resp.ok_or("no response".into())
}

#[test]
fn message_reaches_worker_and_returns_queued() -> Result<(), String> {
    let tmp = TempDir::new().map_err(|e| e.to_string())?;
    let state_dir = tmp.path();
    let worker_sock = state_dir.join("worker.sock");
    write_worker_fixture(state_dir, "demo", &worker_sock)?;

    let resp = message_worker(state_dir, &worker_sock, "demo")?;
    match resp {
        ControlResponse::Ok { state, .. } => {
            let state = state.ok_or("missing state")?;
            assert_eq!(state["queued"], true);
            assert_eq!(state["id"], "demo");
        }
        ControlResponse::Err { error, .. } => return Err(error),
    }
    Ok(())
}

fn simple_worker_roundtrip(cmd: &ControlRequest, expected_cmd: &str) -> Result<(), String> {
    let tmp = TempDir::new().map_err(|e| e.to_string())?;
    let state_dir = tmp.path();
    let worker_sock = state_dir.join("worker.sock");
    let agent_id = "demo";
    write_worker_fixture(state_dir, agent_id, &worker_sock)?;

    let _worker = start_fake_worker(&worker_sock, br#"{"ok":true}"#, expected_cmd);
    with_worker_server(state_dir, |state_dir| {
        let resp = client::call_blocking(state_dir, cmd).map_err(|e| e.to_string())?;
        match resp {
            ControlResponse::Ok { .. } => Ok(()),
            ControlResponse::Err { error, .. } => Err(error),
        }
    })
}

#[test]
fn pause_reaches_worker() -> Result<(), String> {
    simple_worker_roundtrip(
        &ControlRequest::Pause {
            id: "demo".into(),
            backend: Some(BackendKind::Worker),
        },
        "pause",
    )
}

#[test]
fn resume_reaches_worker() -> Result<(), String> {
    simple_worker_roundtrip(
        &ControlRequest::Resume {
            id: "demo".into(),
            backend: Some(BackendKind::Worker),
        },
        "resume",
    )
}

#[test]
fn stop_reaches_worker() -> Result<(), String> {
    simple_worker_roundtrip(
        &ControlRequest::Stop {
            id: "demo".into(),
            backend: Some(BackendKind::Worker),
        },
        "stop",
    )
}

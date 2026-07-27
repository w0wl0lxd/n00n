//! PR #134-compatible worker backend: `state_dir/agents/<id>/agent.json` + `control.sock`.

use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::backend::ControlBackend;
use crate::error::{ControlError, ControlResult};
use crate::protocol::{AgentRecord, BackendKind, MessageOpts};

const AGENTS_SUBDIR: &str = "agents";
const STATE_FILE: &str = "agent.json";
const MAX_AGENT_ID_LEN: usize = 64;

#[derive(Debug, Clone, Serialize, Deserialize)]
struct WorkerStateFile {
    id: String,
    #[serde(default)]
    session_id: String,
    #[serde(default)]
    socket_path: String,
    #[serde(default)]
    status: String,
    #[serde(default)]
    model: String,
    #[serde(default)]
    prompt: String,
    #[serde(default)]
    updated_at: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    cwd: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(tag = "cmd", rename_all = "snake_case")]
enum WorkerCommand {
    Message { text: String },
    Pause,
    Resume,
    Stop,
}

#[derive(Debug)]
pub struct WorkerBackend {
    agents_dir: PathBuf,
}

impl WorkerBackend {
    #[must_use]
    pub fn new(state_dir: impl Into<PathBuf>) -> Self {
        Self {
            agents_dir: state_dir.into().join(AGENTS_SUBDIR),
        }
    }

    fn validate_id(id: &str) -> ControlResult<()> {
        if id.is_empty() || id.len() > MAX_AGENT_ID_LEN {
            return Err(ControlError::InvalidId(id.to_owned()));
        }
        if id.contains("..") || id.contains('/') || id.contains('\\') || id.contains('\0') {
            return Err(ControlError::InvalidId(id.to_owned()));
        }
        if !id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.')
        {
            return Err(ControlError::InvalidId(id.to_owned()));
        }
        Ok(())
    }

    fn state_path(&self, id: &str) -> ControlResult<PathBuf> {
        Self::validate_id(id)?;
        Ok(self.agents_dir.join(id).join(STATE_FILE))
    }

    fn read_state(&self, id: &str) -> ControlResult<WorkerStateFile> {
        let path = self.state_path(id)?;
        let data = fs::read_to_string(&path).map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                ControlError::NotFound(id.to_owned())
            } else {
                ControlError::io(e)
            }
        })?;
        sonic_rs::from_str(&data).map_err(ControlError::protocol)
    }

    fn to_record(state: &WorkerStateFile) -> AgentRecord {
        AgentRecord {
            id: state.id.clone(),
            backend: BackendKind::Worker,
            session_id: if state.session_id.is_empty() {
                None
            } else {
                Some(state.session_id.clone())
            },
            status: if state.status.is_empty() {
                "unknown".into()
            } else {
                state.status.clone()
            },
            title: if state.prompt.is_empty() {
                None
            } else {
                Some(state.prompt.chars().take(40).collect())
            },
            model: if state.model.is_empty() {
                None
            } else {
                Some(state.model.clone())
            },
            output: None,
            cwd: state.cwd.clone(),
        }
    }

    #[cfg(unix)]
    fn send_command(&self, id: &str, command: &WorkerCommand) -> ControlResult<sonic_rs::Value> {
        use futures_lite::{AsyncBufReadExt, AsyncWriteExt, io::BufReader};
        use smol::net::unix::UnixStream;

        let state = self.read_state(id)?;
        if state.socket_path.is_empty() {
            return Err(ControlError::Unavailable(format!(
                "worker {id} has no socket_path"
            )));
        }
        let cmd_line = sonic_rs::to_string(command).map_err(ControlError::protocol)?;
        smol::block_on(async {
            let stream = UnixStream::connect(Path::new(&state.socket_path))
                .await
                .map_err(ControlError::io)?;
            let (reader, mut writer) = futures_lite::io::split(stream);
            writer
                .write_all(cmd_line.as_bytes())
                .await
                .map_err(ControlError::io)?;
            writer.write_all(b"\n").await.map_err(ControlError::io)?;
            writer.flush().await.map_err(ControlError::io)?;
            let mut reader = BufReader::new(reader);
            let mut line = String::new();
            reader
                .read_line(&mut line)
                .await
                .map_err(ControlError::io)?;
            if line.trim().is_empty() {
                return Ok(sonic_rs::json!({"ok": true}));
            }
            sonic_rs::from_str(line.trim()).map_err(ControlError::protocol)
        })
    }

    #[cfg(not(unix))]
    fn send_command(&self, id: &str, _command: &WorkerCommand) -> ControlResult<sonic_rs::Value> {
        let _ = self.read_state(id)?;
        Err(ControlError::Unavailable(
            "worker control sockets require unix".into(),
        ))
    }
}

impl ControlBackend for WorkerBackend {
    fn list(&self) -> ControlResult<Vec<AgentRecord>> {
        let read_dir = match fs::read_dir(&self.agents_dir) {
            Ok(rd) => rd,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(ControlError::io(e)),
        };
        let mut out = Vec::new();
        for entry in read_dir {
            let entry = entry.map_err(ControlError::io)?;
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
                continue;
            };
            if Self::validate_id(&name).is_err() {
                continue;
            }
            match self.read_state(&name) {
                Ok(state) => out.push(Self::to_record(&state)),
                Err(ControlError::NotFound(_) | ControlError::Protocol(_)) => {}
                Err(e) => return Err(e),
            }
        }
        out.sort_by(|a, b| a.id.cmp(&b.id));
        Ok(out)
    }

    fn status(&self, id: &str) -> ControlResult<AgentRecord> {
        Ok(Self::to_record(&self.read_state(id)?))
    }

    fn message(&self, id: &str, text: &str, _opts: &MessageOpts) -> ControlResult<sonic_rs::Value> {
        self.send_command(
            id,
            &WorkerCommand::Message {
                text: text.to_owned(),
            },
        )
    }

    fn pause(&self, id: &str) -> ControlResult<()> {
        let _ = self.send_command(id, &WorkerCommand::Pause)?;
        Ok(())
    }

    fn resume(&self, id: &str) -> ControlResult<()> {
        let _ = self.send_command(id, &WorkerCommand::Resume)?;
        Ok(())
    }

    fn stop(&self, id: &str) -> ControlResult<()> {
        let _ = self.send_command(id, &WorkerCommand::Stop)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn write_fixture(dir: &Path, id: &str, status: &str) -> Result<(), String> {
        let agent_dir = dir.join(AGENTS_SUBDIR).join(id);
        fs::create_dir_all(&agent_dir).map_err(|e| e.to_string())?;
        let state = WorkerStateFile {
            id: id.to_owned(),
            session_id: "sess".into(),
            socket_path: "/tmp/missing.sock".into(),
            status: status.to_owned(),
            model: "test/model".into(),
            prompt: "hello world".into(),
            updated_at: 1,
            cwd: None,
        };
        let encoded = sonic_rs::to_string_pretty(&state).map_err(|e| e.to_string())?;
        fs::write(agent_dir.join(STATE_FILE), encoded).map_err(|e| e.to_string())?;
        Ok(())
    }

    #[test]
    fn list_empty_when_agents_dir_missing() -> Result<(), String> {
        let tmp = TempDir::new().map_err(|e| e.to_string())?;
        let backend = WorkerBackend::new(tmp.path());
        let agents = backend.list().map_err(|e| e.to_string())?;
        assert!(agents.is_empty());
        Ok(())
    }

    #[test]
    fn list_reads_fixtures() -> Result<(), String> {
        let tmp = TempDir::new().map_err(|e| e.to_string())?;
        write_fixture(tmp.path(), "agent-a", "running")?;
        write_fixture(tmp.path(), "agent-b", "paused")?;
        let backend = WorkerBackend::new(tmp.path());
        let agents = backend.list().map_err(|e| e.to_string())?;
        assert_eq!(agents.len(), 2);
        assert_eq!(agents[0].backend, BackendKind::Worker);
        assert_eq!(agents[0].id, "agent-a");
        assert_eq!(agents[1].status, "paused");
        Ok(())
    }

    #[test]
    fn rejects_path_traversal_id() -> Result<(), String> {
        let tmp = TempDir::new().map_err(|e| e.to_string())?;
        let backend = WorkerBackend::new(tmp.path());
        match backend.status("../etc") {
            Err(ControlError::InvalidId(_)) => Ok(()),
            other => Err(format!("expected InvalidId, got {other:?}")),
        }
    }

    #[cfg(unix)]
    #[test]
    fn pause_roundtrips_over_control_sock() -> Result<(), String> {
        use futures_lite::{AsyncBufReadExt, AsyncWriteExt, io::BufReader};
        use smol::net::unix::UnixListener;
        use std::time::Duration;

        let tmp = TempDir::new().map_err(|e| e.to_string())?;
        let sock_path = tmp.path().join("control.sock");
        let sock_serve = sock_path.clone();
        let server = std::thread::spawn(move || {
            smol::block_on(async {
                let listener = UnixListener::bind(&sock_serve).map_err(|e| e.to_string())?;
                let (stream, _) = listener.accept().await.map_err(|e| e.to_string())?;
                let (reader, mut writer) = futures_lite::io::split(stream);
                let mut reader = BufReader::new(reader);
                let mut line = String::new();
                reader
                    .read_line(&mut line)
                    .await
                    .map_err(|e| e.to_string())?;
                if !line.contains("\"cmd\":\"pause\"") {
                    return Err(format!("unexpected command line: {line}"));
                }
                writer
                    .write_all(b"{\"ok\":true}\n")
                    .await
                    .map_err(|e| e.to_string())?;
                writer.flush().await.map_err(|e| e.to_string())?;
                Ok::<(), String>(())
            })
        });

        std::thread::sleep(Duration::from_millis(20));
        write_fixture_with_socket(tmp.path(), "worker-1", "running", &sock_path)?;

        let backend = WorkerBackend::new(tmp.path());
        backend.pause("worker-1").map_err(|e| e.to_string())?;

        match server.join() {
            Ok(Ok(())) => Ok(()),
            Ok(Err(e)) => Err(e),
            Err(_) => Err("mock worker server panicked".into()),
        }
    }

    fn write_fixture_with_socket(
        dir: &Path,
        id: &str,
        status: &str,
        socket_path: &Path,
    ) -> Result<(), String> {
        let agent_dir = dir.join(AGENTS_SUBDIR).join(id);
        fs::create_dir_all(&agent_dir).map_err(|e| e.to_string())?;
        let state = WorkerStateFile {
            id: id.to_owned(),
            session_id: "sess".into(),
            socket_path: socket_path.to_string_lossy().into_owned(),
            status: status.to_owned(),
            model: "test/model".into(),
            prompt: "hello world".into(),
            updated_at: 1,
            cwd: None,
        };
        let encoded = sonic_rs::to_string_pretty(&state).map_err(|e| e.to_string())?;
        fs::write(agent_dir.join(STATE_FILE), encoded).map_err(|e| e.to_string())?;
        Ok(())
    }
}

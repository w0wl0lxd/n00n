use std::sync::{Arc, Mutex};

use super::AgentCommand;
use super::cancel_map::CancelMap;

pub(super) fn spawn_command_router(
    cmd_rx: flume::Receiver<AgentCommand>,
    toggle_tx: flume::Sender<(String, bool)>,
    cancel_map: Arc<Mutex<CancelMap>>,
) {
    smol::spawn(async move {
        while let Ok(cmd) = cmd_rx.recv_async().await {
            match cmd {
                AgentCommand::Cancel { run_id } => {
                    cancel_map
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .cancel(run_id);
                }
                AgentCommand::CancelAll => {
                    cancel_map
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .cancel_all();
                }
                AgentCommand::ToggleMcp(name, enabled) => {
                    let _ = toggle_tx.try_send((name, enabled));
                }
            }
        }
    })
    .detach();
}

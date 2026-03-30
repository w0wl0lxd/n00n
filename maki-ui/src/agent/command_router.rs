use std::sync::{Arc, Mutex};

use maki_agent::CancelTrigger;

use super::AgentCommand;

pub(super) fn spawn_command_router(
    cmd_rx: flume::Receiver<AgentCommand>,
    toggle_tx: flume::Sender<(String, bool)>,
    cancel_trigger: Arc<Mutex<Option<CancelTrigger>>>,
) {
    smol::spawn(async move {
        while let Ok(cmd) = cmd_rx.recv_async().await {
            match cmd {
                AgentCommand::Cancel => {
                    if let Some(trigger) = cancel_trigger
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .take()
                    {
                        trigger.cancel();
                    }
                }
                AgentCommand::ToggleMcp(name, enabled) => {
                    let _ = toggle_tx.try_send((name, enabled));
                }
            }
        }
    })
    .detach();
}

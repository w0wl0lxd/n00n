use std::sync::{Arc, Mutex};

use maki_agent::{CancelTrigger, ExtractedCommand};

use super::AgentCommand;

pub(super) fn spawn_command_router(
    cmd_rx: flume::Receiver<AgentCommand>,
    ecmd_tx: flume::Sender<ExtractedCommand>,
    toggle_tx: flume::Sender<(String, bool)>,
    cancel_trigger: Arc<Mutex<Option<CancelTrigger>>>,
) {
    smol::spawn(async move {
        while let Ok(cmd) = cmd_rx.recv_async().await {
            match cmd {
                AgentCommand::Run(input, run_id) => {
                    if ecmd_tx
                        .try_send(ExtractedCommand::Interrupt(input, run_id))
                        .is_err()
                    {
                        break;
                    }
                }
                AgentCommand::Cancel => {
                    if let Some(trigger) = cancel_trigger
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .take()
                    {
                        trigger.cancel();
                    }
                    if ecmd_tx.try_send(ExtractedCommand::Cancel).is_err() {
                        break;
                    }
                }
                AgentCommand::Compact(run_id) => {
                    if ecmd_tx.try_send(ExtractedCommand::Compact(run_id)).is_err() {
                        break;
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

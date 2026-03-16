//! Message queue for input typed while the agent is busy. The front item is sent to
//! the agent immediately via `cmd_tx`; the next is sent only after `QueueItemConsumed`
//! is received, so messages are delivered one at a time in order.

use crate::components::queue_panel::QueueEntry;
use crate::theme;
use maki_agent::{AgentInput, AgentMode};

use super::{App, format_with_images};

const COMPACT_LABEL: &str = "/compact";

pub(crate) enum QueuedItem {
    Message(AgentInput),
    Compact,
}

impl QueuedItem {
    pub(super) fn as_queue_entry(&self) -> QueueEntry<'_> {
        match self {
            Self::Message(input) => QueueEntry {
                text: &input.message,
                color: theme::current().foreground,
            },
            Self::Compact => QueueEntry {
                text: COMPACT_LABEL,
                color: theme::current()
                    .queue_compact
                    .fg
                    .unwrap_or(theme::current().foreground),
            },
        }
    }

    pub(super) fn to_agent_command(
        &self,
        run_id: u64,
        mode: AgentMode,
        pending_plan: Option<String>,
    ) -> crate::AgentCommand {
        match self {
            Self::Message(input) => crate::AgentCommand::Run(
                AgentInput {
                    message: input.message.clone(),
                    mode,
                    pending_plan,
                    images: input.images.clone(),
                },
                run_id,
            ),
            Self::Compact => crate::AgentCommand::Compact(run_id),
        }
    }
}

impl App {
    pub(super) fn queue_entries(&self) -> Vec<QueueEntry<'_>> {
        self.queue
            .iter()
            .map(|item| item.as_queue_entry())
            .collect()
    }

    pub(super) fn queue_and_notify(&mut self, item: QueuedItem) {
        self.queue.push_back(item);
        if self.queue.len() == 1 {
            self.send_front_to_agent();
        }
    }

    pub(super) fn send_front_to_agent(&self) {
        if let Some(front) = self.queue.front()
            && let Some(tx) = &self.cmd_tx
        {
            let cmd = front.to_agent_command(
                self.run_id,
                self.agent_mode(),
                self.pending_plan().map(String::from),
            );
            let _ = tx.try_send(cmd);
        }
    }

    pub(super) fn drain_consumed_item(&mut self) {
        let Some(item) = self.queue.pop_front() else {
            return;
        };
        if let QueuedItem::Message(ref input) = item {
            self.main_chat().flush();
            self.main_chat()
                .push_user_message(&format_with_images(&input.message, input.images.len()));
            self.main_chat().enable_auto_scroll();
        }
        self.clamp_queue_focus();
        self.send_front_to_agent();
    }

    pub(super) fn clear_queue(&mut self) {
        self.queue.clear();
        self.queue_focus = None;
    }

    pub(super) fn remove_queue_item(&mut self, index: usize) {
        if index >= self.queue.len() {
            return;
        }
        self.queue.remove(index);
        self.clamp_queue_focus();
    }

    pub(super) fn clamp_queue_focus(&mut self) {
        match self.queue_focus {
            Some(sel) if sel >= self.queue.len() && !self.queue.is_empty() => {
                self.queue_focus = Some(self.queue.len() - 1);
            }
            Some(_) if self.queue.is_empty() => self.queue_focus = None,
            _ => {}
        }
    }

    pub(super) fn focus_queue(&mut self) {
        if !self.queue.is_empty() {
            self.queue_focus = Some(0);
        }
    }
}

//! Message queue for input typed while the agent is busy. The front item is
//! sent eagerly to the agent via `queue_and_notify`; subsequent items are sent
//! one at a time as the agent signals `QueueItemConsumed` or `Done`.
//!
//! All queue + focus state is encapsulated in [`MessageQueue`] so the two
//! cannot drift out of sync.

use std::collections::VecDeque;
use std::ops::Index;

use crate::components::input::Submission;
use crate::components::queue_panel::QueueEntry;
use crate::theme;
use maki_agent::ImageSource;

use super::{App, format_with_images};

const COMPACT_LABEL: &str = "/compact";

pub(crate) struct QueuedMessage {
    pub(crate) text: String,
    pub(crate) images: Vec<ImageSource>,
}

impl From<Submission> for QueuedMessage {
    fn from(sub: Submission) -> Self {
        Self {
            text: sub.text,
            images: sub.images,
        }
    }
}

pub(crate) enum QueuedItem {
    Message(QueuedMessage),
    Compact,
}

impl QueuedItem {
    fn as_queue_entry(&self) -> QueueEntry<'_> {
        match self {
            Self::Message(msg) => QueueEntry {
                text: &msg.text,
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
}

#[derive(Default)]
pub(crate) struct MessageQueue {
    items: VecDeque<QueuedItem>,
    focus: Option<usize>,
    in_flight: bool,
}

impl MessageQueue {
    pub(crate) fn push(&mut self, item: QueuedItem) {
        self.items.push_back(item);
    }

    pub(crate) fn pop_front(&mut self) -> Option<QueuedItem> {
        let item = self.items.pop_front()?;
        self.in_flight = false;
        self.clamp_focus();
        Some(item)
    }

    pub(crate) fn remove(&mut self, index: usize) {
        if index >= self.items.len() {
            return;
        }
        if index == 0 && self.in_flight {
            self.in_flight = false;
        }
        self.items.remove(index);
        self.clamp_focus();
    }

    pub(crate) fn clear(&mut self) {
        self.items.clear();
        self.focus = None;
        self.in_flight = false;
    }

    pub(crate) fn in_flight(&self) -> bool {
        self.in_flight
    }

    pub(crate) fn mark_in_flight(&mut self) {
        self.in_flight = true;
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    pub(crate) fn len(&self) -> usize {
        self.items.len()
    }

    pub(crate) fn focus(&self) -> Option<usize> {
        self.focus
    }

    pub(crate) fn set_focus(&mut self) {
        self.set_focus_at(0);
    }

    pub(crate) fn unfocus(&mut self) {
        self.focus = None;
    }

    pub(crate) fn move_focus_up(&mut self) {
        if let Some(sel) = self.focus
            && sel > 0
        {
            self.focus = Some(sel - 1);
        }
    }

    pub(crate) fn move_focus_down(&mut self) {
        if let Some(sel) = self.focus
            && sel + 1 < self.items.len()
        {
            self.focus = Some(sel + 1);
        }
    }

    pub(crate) fn remove_focused(&mut self) {
        if let Some(sel) = self.focus {
            self.remove(sel);
        }
    }

    pub(crate) fn entries(&self) -> Vec<QueueEntry<'_>> {
        self.items
            .iter()
            .map(|item| item.as_queue_entry())
            .collect()
    }

    fn clamp_focus(&mut self) {
        self.focus = match self.focus {
            Some(_) if self.items.is_empty() => None,
            Some(sel) if sel >= self.items.len() => Some(self.items.len() - 1),
            other => other,
        };
    }

    pub(crate) fn set_focus_at(&mut self, index: usize) {
        if index < self.items.len() {
            self.focus = Some(index);
        }
    }
}

impl Index<usize> for MessageQueue {
    type Output = QueuedItem;

    fn index(&self, index: usize) -> &Self::Output {
        &self.items[index]
    }
}

impl App {
    pub(super) fn queue_and_notify(&mut self, item: QueuedItem) {
        self.queue.push(item);
        self.send_front_to_agent();
    }

    fn send_front_to_agent(&mut self) {
        if self.queue.in_flight() || self.queue.is_empty() {
            return;
        }
        if let Some(tx) = &self.cmd_tx {
            let cmd = match &self.queue[0] {
                QueuedItem::Message(msg) => {
                    crate::AgentCommand::Run(self.build_agent_input(msg), self.run_id)
                }
                QueuedItem::Compact => crate::AgentCommand::Compact(self.run_id),
            };
            let _ = tx.try_send(cmd);
            self.queue.mark_in_flight();
        }
    }

    pub(super) fn drain_consumed_item(&mut self) {
        if !self.queue.in_flight() {
            return;
        }
        let Some(item) = self.queue.pop_front() else {
            return;
        };
        if let QueuedItem::Message(ref msg) = item {
            self.display_queued_msg(msg);
        }
        self.send_front_to_agent();
    }

    fn display_queued_msg(&mut self, msg: &QueuedMessage) {
        self.main_chat().flush();
        self.main_chat()
            .push_user_message(&format_with_images(&msg.text, msg.images.len()));
        self.main_chat().enable_auto_scroll();
    }

    pub(super) fn start_from_queue(&mut self, msg: &QueuedMessage) -> Vec<super::Action> {
        self.display_queued_msg(msg);
        self.status = super::Status::Streaming;
        vec![super::Action::SendMessage(self.build_agent_input(msg))]
    }

    pub(super) fn drain_next_queued(&mut self) -> Option<Vec<super::Action>> {
        debug_assert!(!self.queue.in_flight(), "in_flight should be false on Done");
        let item = self.queue.pop_front()?;
        Some(match item {
            QueuedItem::Message(msg) => self.start_from_queue(&msg),
            QueuedItem::Compact => vec![super::Action::Compact],
        })
    }
}

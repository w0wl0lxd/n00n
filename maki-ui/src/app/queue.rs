//! Queue for messages typed while the agent is busy.

use std::collections::VecDeque;
use std::ops::Index;

use super::{App, format_with_images};

use crate::components::input::Submission;
use crate::components::queue_panel::QueueEntry;
use crate::theme;
use maki_agent::ImageSource;

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
    dispatched: bool,
}

impl MessageQueue {
    pub(crate) fn push(&mut self, item: QueuedItem) {
        self.items.push_back(item);
    }

    pub(crate) fn pop_front(&mut self) -> Option<QueuedItem> {
        let item = self.items.pop_front()?;
        self.dispatched = false;
        self.clamp_focus();
        Some(item)
    }

    pub(crate) fn dispatched(&self) -> bool {
        self.dispatched
    }

    pub(crate) fn mark_dispatched(&mut self) {
        self.dispatched = true;
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    pub(crate) fn remove(&mut self, index: usize) {
        if index >= self.items.len() {
            return;
        }
        if index == 0 && self.dispatched {
            self.dispatched = false;
        }
        self.items.remove(index);
        self.clamp_focus();
    }

    pub(crate) fn clear(&mut self) {
        self.items.clear();
        self.focus = None;
        self.dispatched = false;
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
        if self.queue.dispatched() || self.queue.is_empty() {
            return;
        }
        let Some(tx) = &self.cmd_tx else { return };
        let cmd = match &self.queue[0] {
            QueuedItem::Message(msg) => {
                crate::AgentCommand::Run(self.build_agent_input(msg), self.run_id)
            }
            QueuedItem::Compact => crate::AgentCommand::Compact(self.run_id),
        };
        let _ = tx.try_send(cmd);
        self.queue.mark_dispatched();
    }

    pub(super) fn on_queue_item_consumed(&mut self) {
        if !self.queue.dispatched() {
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

    pub(super) fn on_agent_done(&mut self) -> Option<Vec<super::Action>> {
        if self.queue.dispatched() {
            return Some(vec![]);
        }
        let item = self.queue.pop_front()?;
        Some(match item {
            QueuedItem::Message(msg) => self.start_from_queue(&msg),
            QueuedItem::Compact => vec![super::Action::Compact],
        })
    }

    pub(super) fn start_from_queue(&mut self, msg: &QueuedMessage) -> Vec<super::Action> {
        self.display_queued_msg(msg);
        self.status = super::Status::Streaming;
        vec![super::Action::SendMessage(Box::new(
            self.build_agent_input(msg),
        ))]
    }

    fn display_queued_msg(&mut self, msg: &QueuedMessage) {
        self.main_chat().flush();
        self.main_chat()
            .push_user_message(&format_with_images(&msg.text, msg.images.len()));
        self.main_chat().enable_auto_scroll();
    }
}

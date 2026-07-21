//! Queue of work handed from the UI to the agent loop.
//!
//! Shutdown rides on `Drop`: when the last [`QueueSender`] goes away, flume
//! closes the notify channel, so the receiver's `recv_notify` wakes with an
//! `Err` and the agent loop falls out of its main loop on its own. That way
//! nobody needs a separate "please stop" flag, and callers can't forget to
//! set it.

use std::borrow::Cow;
use std::collections::VecDeque;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use n00n_agent::{AgentInput, ExtractedCommand, ImageSource, InterruptPoint, InterruptSource};

use crate::components::input::Submission;
use crate::components::queue_panel::QueueEntry;
use crate::theme;

const COMPACT_LABEL: &str = "/compact";
const STEERING_PREFIX: &str = "↪ ";
const IMMEDIATE_PREFIX: &str = "↯ ";

type Items = Arc<Mutex<VecDeque<QueueItem>>>;

#[derive(Clone)]
pub(crate) struct QueuedMessage {
    pub(crate) text: String,
    pub(crate) images: Vec<ImageSource>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Delivery {
    TurnEnd,
    Steering,
    Immediate,
}

impl From<Submission> for QueuedMessage {
    fn from(sub: Submission) -> Self {
        Self {
            text: sub.text,
            images: sub.images,
        }
    }
}

pub(crate) enum QueueItem {
    Message {
        text: String,
        image_count: usize,
        input: AgentInput,
        run_id: u64,
        /// `true` when the UI already drew the bubble (immediate dispatch).
        /// The agent then skips `QueueItemConsumed` so we don't draw it twice.
        /// `false` when the user typed while the agent was busy: the UI waits
        /// for `QueueItemConsumed` before drawing.
        displayed: bool,
        delivery: Delivery,
    },
    Compact {
        run_id: u64,
    },
}

impl QueueItem {
    pub(crate) fn run_id(&self) -> u64 {
        match self {
            Self::Message { run_id, .. } | Self::Compact { run_id } => *run_id,
        }
    }

    fn as_queue_entry(&self) -> QueueEntry<'static> {
        match self {
            Self::Message { text, delivery, .. } => QueueEntry {
                text: Cow::Owned(match delivery {
                    Delivery::TurnEnd => text.clone(),
                    Delivery::Steering => format!("{STEERING_PREFIX}{text}"),
                    Delivery::Immediate => format!("{IMMEDIATE_PREFIX}{text}"),
                }),
                color: theme::current().foreground,
            },
            Self::Compact { .. } => QueueEntry {
                text: Cow::Borrowed(COMPACT_LABEL),
                color: theme::current()
                    .queue
                    .fg
                    .unwrap_or(theme::current().foreground),
            },
        }
    }

    fn into_extracted_command(self) -> ExtractedCommand {
        match self {
            Self::Message { input, run_id, .. } => ExtractedCommand::Interrupt(input, run_id),
            Self::Compact { run_id } => ExtractedCommand::Compact(run_id),
        }
    }

    /// Immediate-dispatch messages already sit in the chat, so hiding them
    /// here stops the panel from reserving a row the agent is about to free,
    /// which used to make the bubble hop up by one frame.
    fn visible_in_panel(&self) -> bool {
        match self {
            Self::Message { displayed, .. } => !displayed,
            Self::Compact { .. } => true,
        }
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

#[derive(Clone)]
pub(crate) struct QueueSender {
    items: Items,
    notify_tx: flume::Sender<()>,
}

pub(crate) struct QueueReceiver {
    items: Items,
    notify_rx: flume::Receiver<()>,
}

pub(crate) fn queue() -> (QueueSender, QueueReceiver) {
    let (notify_tx, notify_rx) = flume::bounded(1);
    let items: Items = Arc::new(Mutex::new(VecDeque::new()));
    (
        QueueSender {
            items: Arc::clone(&items),
            notify_tx,
        },
        QueueReceiver { items, notify_rx },
    )
}

impl QueueSender {
    pub(crate) fn push(&self, entry: QueueItem) {
        lock(&self.items).push_back(entry);
        let _ = self.notify_tx.try_send(());
    }

    fn panel_index(items: &VecDeque<QueueItem>, index: usize) -> Option<usize> {
        items
            .iter()
            .enumerate()
            .filter(|(_, item)| item.visible_in_panel())
            .nth(index)
            .map(|(item_index, _)| item_index)
    }

    pub(crate) fn remove_panel(&self, index: usize) -> Option<QueueItem> {
        let mut items = lock(&self.items);
        let item_index = Self::panel_index(&items, index)?;
        items.remove(item_index)
    }

    pub(crate) fn insert_panel(&self, index: usize, entry: QueueItem) {
        let mut items = lock(&self.items);
        let item_index = Self::panel_index(&items, index).unwrap_or(items.len());
        items.insert(item_index, entry);
    }

    pub(crate) fn promote_latest_steering(&self) -> bool {
        let mut items = lock(&self.items);
        let Some(QueueItem::Message { delivery, .. }) = items.iter_mut().rev().find(|item| {
            matches!(
                item,
                QueueItem::Message {
                    delivery: Delivery::Steering,
                    ..
                }
            )
        }) else {
            return false;
        };
        *delivery = Delivery::Immediate;
        true
    }

    #[cfg(test)]
    pub(crate) fn is_empty(&self) -> bool {
        lock(&self.items).is_empty()
    }

    pub(crate) fn clear(&self) {
        lock(&self.items).clear();
    }

    pub(crate) fn text_messages(&self) -> Vec<String> {
        lock(&self.items)
            .iter()
            .filter(|item| item.visible_in_panel())
            .filter_map(|item| match item {
                QueueItem::Message { text, .. } => Some(text.clone()),
                QueueItem::Compact { .. } => None,
            })
            .collect()
    }

    pub(crate) fn panel_len(&self) -> usize {
        lock(&self.items)
            .iter()
            .filter(|item| item.visible_in_panel())
            .count()
    }

    pub(crate) fn panel_entries(&self) -> Vec<QueueEntry<'static>> {
        lock(&self.items)
            .iter()
            .filter(|item| item.visible_in_panel())
            .map(QueueItem::as_queue_entry)
            .collect()
    }
}

impl QueueReceiver {
    pub(crate) fn pop(&self) -> Option<QueueItem> {
        let mut items = lock(&self.items);
        let index = items.iter().position(|item| {
            matches!(
                item,
                QueueItem::Message {
                    delivery: Delivery::TurnEnd,
                    ..
                } | QueueItem::Compact { .. }
            )
        })?;
        items.remove(index)
    }

    pub(crate) async fn recv_notify(&self) -> Result<(), flume::RecvError> {
        self.notify_rx.recv_async().await
    }
}

impl InterruptSource for QueueReceiver {
    fn poll(&self, point: InterruptPoint) -> Option<ExtractedCommand> {
        let mut items = lock(&self.items);
        let index = items.iter().position(|item| match item {
            QueueItem::Message { delivery, .. } => match delivery {
                Delivery::TurnEnd => false,
                Delivery::Steering => point == InterruptPoint::ToolComplete,
                Delivery::Immediate => true,
            },
            QueueItem::Compact { .. } => point == InterruptPoint::Safe,
        })?;
        items.remove(index).map(QueueItem::into_extracted_command)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use test_case::test_case;

    fn msg(displayed: bool) -> QueueItem {
        QueueItem::Message {
            text: "t".into(),
            image_count: 0,
            input: AgentInput {
                message: String::new(),
                mode: Default::default(),
                images: Vec::new(),
                preamble: Vec::new(),
                thinking: Default::default(),
                fast: false,
                workflow: false,
                prompt: None,
            },
            run_id: 0,
            displayed,
            delivery: Delivery::TurnEnd,
        }
    }

    #[test_case(msg(false),                       true  ; "deferred_message_visible")]
    #[test_case(msg(true),                        false ; "displayed_message_hidden")]
    #[test_case(QueueItem::Compact { run_id: 0 }, true  ; "compact_visible")]
    fn panel_visibility(item: QueueItem, visible: bool) {
        let (tx, _rx) = queue();
        tx.push(item);
        let expected = usize::from(visible);
        assert_eq!(tx.panel_len(), expected);
        assert_eq!(tx.panel_entries().len(), expected);
    }
    #[test]
    fn poll_respects_delivery_points_and_fifo_within_ready_messages() {
        let (tx, rx) = queue();
        let queued = |text: &str, delivery| QueueItem::Message {
            text: text.into(),
            image_count: 0,
            input: AgentInput {
                message: text.into(),
                mode: Default::default(),
                images: Vec::new(),
                preamble: Vec::new(),
                thinking: Default::default(),
                fast: false,
                workflow: false,
                prompt: None,
            },
            run_id: 0,
            displayed: false,
            delivery,
        };
        tx.push(queued("normal", Delivery::TurnEnd));
        tx.push(queued("steer one", Delivery::Steering));
        tx.push(queued("steer two", Delivery::Steering));

        let ExtractedCommand::Interrupt(first, _) = rx.poll(InterruptPoint::ToolComplete).unwrap()
        else {
            panic!("expected steering interrupt");
        };
        let ExtractedCommand::Interrupt(second, _) = rx.poll(InterruptPoint::ToolComplete).unwrap()
        else {
            panic!("expected steering interrupt");
        };
        assert!(rx.poll(InterruptPoint::Safe).is_none());
        let QueueItem::Message { input: normal, .. } = rx.pop().unwrap() else {
            panic!("expected normal queue item");
        };
        assert_eq!(
            (first.message, second.message, normal.message),
            ("steer one".into(), "steer two".into(), "normal".into())
        );
    }

    #[test]
    fn promoted_steering_is_available_at_safe_point() {
        let (tx, rx) = queue();
        tx.push(msg(false));
        if let Some(QueueItem::Message { delivery, .. }) = lock(&tx.items).back_mut() {
            *delivery = Delivery::Steering;
        }
        assert!(tx.promote_latest_steering());
        assert!(rx.poll(InterruptPoint::Safe).is_some());
    }
}

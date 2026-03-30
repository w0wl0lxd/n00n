use std::borrow::Cow;
use std::collections::VecDeque;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use maki_agent::{AgentInput, ExtractedCommand, ImageSource, InterruptSource};

use crate::components::input::Submission;
use crate::components::queue_panel::QueueEntry;
use crate::theme;

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

pub(crate) enum QueueItem {
    Message {
        text: String,
        image_count: usize,
        input: AgentInput,
        run_id: u64,
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
            Self::Message { text, .. } => QueueEntry {
                text: Cow::Owned(text.clone()),
                color: theme::current().foreground,
            },
            Self::Compact { .. } => QueueEntry {
                text: Cow::Borrowed(COMPACT_LABEL),
                color: theme::current()
                    .queue_compact
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
}

pub(crate) struct SharedQueue {
    inner: Mutex<VecDeque<QueueItem>>,
    notify_tx: flume::Sender<()>,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

impl SharedQueue {
    pub(crate) fn new() -> (Arc<Self>, flume::Receiver<()>) {
        let (tx, rx) = flume::bounded(1);
        (
            Arc::new(Self {
                inner: Mutex::new(VecDeque::new()),
                notify_tx: tx,
            }),
            rx,
        )
    }

    pub(crate) fn push(&self, entry: QueueItem) {
        lock(&self.inner).push_back(entry);
        let _ = self.notify_tx.try_send(());
    }

    pub(crate) fn pop(&self) -> Option<QueueItem> {
        lock(&self.inner).pop_front()
    }

    pub(crate) fn remove(&self, index: usize) -> Option<QueueItem> {
        let mut inner = lock(&self.inner);
        if index < inner.len() {
            inner.remove(index)
        } else {
            None
        }
    }

    pub(crate) fn len(&self) -> usize {
        lock(&self.inner).len()
    }

    #[cfg(test)]
    pub(crate) fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub(crate) fn clear(&self) {
        lock(&self.inner).clear();
    }

    pub(crate) fn text_messages(&self) -> Vec<String> {
        lock(&self.inner)
            .iter()
            .filter_map(|item| match item {
                QueueItem::Message { text, .. } => Some(text.clone()),
                QueueItem::Compact { .. } => None,
            })
            .collect()
    }

    pub(crate) fn entries(&self) -> Vec<QueueEntry<'static>> {
        lock(&self.inner)
            .iter()
            .map(QueueItem::as_queue_entry)
            .collect()
    }
}

impl InterruptSource for SharedQueue {
    fn poll(&self) -> Option<ExtractedCommand> {
        self.pop().map(QueueItem::into_extracted_command)
    }
}

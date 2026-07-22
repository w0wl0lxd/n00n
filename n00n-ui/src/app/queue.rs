//! Queue for messages typed while the agent is busy.

use super::{Action, App, Status};

use crate::agent::shared_queue::{Delivery, QueueItem, QueueSender};
use crate::components::queue_panel::QueueEntry;
use n00n_agent::ImageSource;

pub(crate) use crate::agent::shared_queue::QueuedMessage;

pub(crate) const EMPTY_PROMPT_ERR: &str = "prompt is empty";
pub(crate) const NO_QUEUE_ERR: &str = "session cannot queue messages";

pub(crate) enum SubmitOutcome {
    Started(Vec<Action>),
    Queued,
    Rejected(&'static str),
}

#[derive(Default)]
pub(crate) struct MessageQueue {
    shared: Option<QueueSender>,
    focus: Option<usize>,
    editing: Option<(usize, Delivery)>,
}

impl MessageQueue {
    pub(crate) fn set_shared(&mut self, shared: QueueSender) {
        self.shared = Some(shared);
    }

    #[cfg(test)]
    pub(crate) fn is_empty(&self) -> bool {
        self.shared
            .as_ref()
            .is_none_or(super::super::agent::shared_queue::QueueSender::is_empty)
    }

    pub(crate) fn len(&self) -> usize {
        self.shared
            .as_ref()
            .map_or(0, super::super::agent::shared_queue::QueueSender::panel_len)
    }

    pub(crate) fn remove(&mut self, index: usize) {
        if let Some(ref shared) = self.shared
            && shared.remove_panel(index).is_some()
        {
            self.clamp_focus();
        }
    }

    pub(crate) fn clear(&mut self) {
        if let Some(ref shared) = self.shared {
            shared.clear();
        }
        self.focus = None;
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

    pub(crate) fn take_focused_for_edit(&mut self) -> Option<(usize, QueuedMessage, Delivery)> {
        let index = self.focus?;
        let QueueItem::Message {
            text,
            input,
            delivery,
            ..
        } = self.shared.as_ref()?.remove_panel(index)?
        else {
            return None;
        };
        self.focus = None;
        self.editing = Some((index, delivery));
        Some((
            index,
            QueuedMessage {
                text,
                images: input.images,
            },
            delivery,
        ))
    }

    pub(crate) fn editing(&self) -> Option<Delivery> {
        self.editing.map(|(_, delivery)| delivery)
    }

    pub(crate) fn promote_latest_steering(&self) -> bool {
        self.shared
            .as_ref()
            .is_some_and(QueueSender::promote_latest_steering)
    }

    pub(crate) fn move_focus_up(&mut self) {
        if let Some(sel) = self.focus
            && sel > 0
        {
            self.focus = Some(sel - 1);
        }
    }

    pub(crate) fn move_focus_down(&mut self) {
        if let Some(sel) = self.focus {
            let len = self.len();
            if sel + 1 < len {
                self.focus = Some(sel + 1);
            }
        }
    }

    pub(crate) fn replace_editing(&mut self, entry: QueueItem) -> bool {
        let Some((index, _)) = self.editing.take() else {
            return false;
        };
        let Some(shared) = self.shared.as_ref() else {
            return false;
        };
        shared.insert_panel(index, entry);
        true
    }

    pub(crate) fn remove_focused(&mut self) {
        if let Some(sel) = self.focus {
            self.remove(sel);
        }
    }

    pub(crate) fn panel_len(&self) -> usize {
        self.len()
    }

    pub(crate) fn panel_entries(&self) -> Vec<QueueEntry<'static>> {
        self.shared.as_ref().map_or(
            vec![],
            super::super::agent::shared_queue::QueueSender::panel_entries,
        )
    }

    pub(crate) fn text_messages(&self) -> Vec<String> {
        self.shared.as_ref().map_or(
            vec![],
            super::super::agent::shared_queue::QueueSender::text_messages,
        )
    }

    fn clamp_focus(&mut self) {
        let len = self.len();
        self.focus = match self.focus {
            Some(_) if len == 0 => None,
            Some(sel) if sel >= len => Some(len - 1),
            other => other,
        };
    }

    pub(crate) fn set_focus_at(&mut self, index: usize) {
        if index < self.len() {
            self.focus = Some(index);
        }
    }
}

impl App {
    /// The one queue-or-start decision, shared by the keyboard and Lua
    /// paths so they cannot drift. Expects raw text: interpretation (slash
    /// commands, `exit`, `!`) is the caller's job, or skipped on purpose.
    pub(crate) fn submit_prompt(&mut self, msg: QueuedMessage) -> SubmitOutcome {
        if msg.text.trim().is_empty() && msg.images.is_empty() {
            return SubmitOutcome::Rejected(EMPTY_PROMPT_ERR);
        }
        if self.status == Status::Streaming {
            if self.queue_and_notify(msg) {
                SubmitOutcome::Queued
            } else {
                SubmitOutcome::Rejected(NO_QUEUE_ERR)
            }
        } else {
            self.run_id += 1;
            SubmitOutcome::Started(self.start_from_queue(&msg))
        }
    }

    /// Keyboard path: nobody is around to receive an `Err`, so
    /// rejections flash on screen instead.
    pub(super) fn submit_or_queue(&mut self, msg: QueuedMessage) -> Vec<Action> {
        match self.submit_prompt(msg) {
            SubmitOutcome::Started(actions) => actions,
            SubmitOutcome::Queued => vec![],
            SubmitOutcome::Rejected(e) => {
                self.flash(e.into());
                vec![]
            }
        }
    }

    /// Deferred path: the agent is busy, so park the message and let
    /// `QueueItemConsumed` draw it once the agent picks it up. Returns
    /// false when there is no shared queue, meaning the message was dropped.
    pub(super) fn queue_and_notify(&mut self, msg: QueuedMessage) -> bool {
        let Some(shared) = self.queue.shared.clone() else {
            return false;
        };
        let input = self.build_agent_input(&msg);
        shared.push(QueueItem::Message {
            text: msg.text,
            image_count: msg.images.len(),
            input,
            run_id: self.run_id,
            displayed: false,
            delivery: Delivery::TurnEnd,
        });
        true
    }

    pub(super) fn queue_steering(&mut self, msg: QueuedMessage) -> bool {
        self.queue_with_delivery(msg, Delivery::Steering)
    }

    pub(super) fn replace_edited(&mut self, msg: QueuedMessage) -> bool {
        let Some(delivery) = self.queue.editing() else {
            return false;
        };
        self.queue_with_delivery(msg, delivery)
    }

    fn queue_with_delivery(&mut self, msg: QueuedMessage, delivery: Delivery) -> bool {
        let Some(shared) = self.queue.shared.clone() else {
            return false;
        };
        let input = self.build_agent_input(&msg);
        let item = QueueItem::Message {
            text: msg.text,
            image_count: msg.images.len(),
            input,
            run_id: self.run_id,
            displayed: false,
            delivery,
        };
        if self.queue.editing().is_some() {
            self.queue.replace_editing(item);
        } else {
            shared.push(item);
        }
        true
    }

    pub(super) fn queue_compact(&mut self) {
        let Some(ref shared) = self.queue.shared else {
            return;
        };
        shared.push(QueueItem::Compact {
            run_id: self.run_id,
        });
    }

    /// Agent reached a deferred message: time to draw the bubble.
    /// Immediate-dispatch items skip this event, so no dedup needed.
    pub(super) fn on_queue_item_consumed(
        &mut self,
        text: &str,
        image_count: usize,
        images: Vec<ImageSource>,
    ) {
        let _ = image_count;
        self.main_chat().show_user_message_with_images(text, images);
    }

    /// Immediate path: kick off the agent and draw the bubble in the same
    /// frame, so the user sees their message land where it will stay.
    pub(super) fn start_from_queue(&mut self, msg: &QueuedMessage) -> Vec<Action> {
        self.status = Status::Streaming;
        self.fire_session_autocmd("TurnStart", serde_json::json!({}));
        self.main_chat()
            .show_user_message_with_images(msg.text.clone(), msg.images.clone());
        vec![Action::SendMessage(Box::new(self.build_agent_input(msg)))]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::shared_queue;
    use n00n_agent::{AgentInput, AgentMode, ThinkingConfig};

    fn displayed_message(text: &str) -> QueueItem {
        QueueItem::Message {
            text: text.into(),
            image_count: 0,
            input: AgentInput {
                message: String::new(),
                mode: AgentMode::default(),
                images: Vec::new(),
                preamble: Vec::new(),
                thinking: ThinkingConfig::default(),
                fast: false,
                workflow: false,
                prompt: None,
            },
            run_id: 0,
            displayed: true,
            delivery: Delivery::TurnEnd,
        }
    }

    fn queue_with_interspersed_hidden_items() -> MessageQueue {
        let (shared, _receiver) = shared_queue::queue();
        shared.push(displayed_message("hidden-first"));
        shared.push(QueueItem::Compact { run_id: 0 });
        shared.push(displayed_message("hidden-middle"));
        shared.push(QueueItem::Compact { run_id: 1 });

        let mut queue = MessageQueue::default();
        queue.set_shared(shared);
        queue
    }

    #[test]
    fn focus_traverses_visible_panel_entries() {
        let mut queue = queue_with_interspersed_hidden_items();

        queue.set_focus();
        assert_eq!(queue.focus(), Some(0));
        queue.move_focus_down();
        assert_eq!(queue.focus(), Some(1));
        queue.move_focus_down();
        assert_eq!(queue.focus(), Some(1));
    }

    #[test]
    fn remove_focused_removes_visible_entry_not_interspersed_hidden_item() {
        let mut queue = queue_with_interspersed_hidden_items();
        queue.set_focus();
        queue.move_focus_down();

        queue.remove_focused();

        assert_eq!(queue.panel_len(), 1);
        assert_eq!(queue.panel_entries()[0].text, "/compact");
        assert_eq!(queue.focus(), Some(0));
    }
}

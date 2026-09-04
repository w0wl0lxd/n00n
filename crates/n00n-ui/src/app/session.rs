#[cfg(test)]
use n00n_storage::sessions::SessionError;
use std::borrow::Cow;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

use crate::chat::{Chat, DONE_TEXT, RESTORE_BATCH_SIZE, history_to_display, transcript_to_display};
use crate::components::DisplayRole;
use crate::components::Status;
use crate::components::rewind_picker::RewindEntry;
use crate::components::{Action, LoadedSession};
use n00n_agent::tools::SessionIdentity;
use n00n_agent::{AgentInput, AgentMode, McpPromptRef, ToolOutput};
use n00n_providers::{Message, Model, TokenUsage};
use n00n_storage::id::{SessionRef, n00nId};
use n00n_storage::sessions::{
    CompactionStateError, SESSIONS_DIR, StoredDelivery, StoredDirectTool, StoredImageMediaType,
    StoredImageSource, StoredMcpPrompt, StoredMode, StoredQueuedMessage, StoredSessionLifecycle,
    StoredSessionStateSnapshot, StoredSubagent, StoredThinking, TranscriptEntry,
};

use crate::AppSession;

use super::session_state::{SessionState, stored_to_rules};
use super::{App, Mode, PendingInput, PlanState};
use crate::agent::shared_queue::QueueItem;
use crate::agent::{Delivery, QueuedMessage};

pub(super) struct SubagentDisplaySource<'a> {
    pub(super) messages: Cow<'a, [Message]>,
    pub(super) tool_outputs: Cow<'a, HashMap<String, ToolOutput>>,
}

const INITIAL_STATE_REVISION: u64 = 0;
/// Floor between full session snapshots. Snapshotting walks the whole session,
/// so an unthrottled save per tool completion is quadratic over a long run.
pub(crate) const SAVE_COALESCE_INTERVAL: Duration = Duration::from_millis(250);

/// Every `tool_use_id` a message calls. The storage layer takes this as a
/// closure so it does not have to know the provider message type.
pub(crate) fn message_tool_use_ids(message: &Message) -> Vec<String> {
    message
        .tool_uses()
        .map(|(id, _, _)| id.to_owned())
        .collect()
}

pub(super) fn plugin_state_identity(session: &AppSession) -> SessionIdentity {
    let root_id = session.meta.root_session_id.unwrap_or_else(|| session.id);
    let session_id = SessionRef::from_id(session.id);
    if root_id == session.id {
        SessionIdentity::root(session_id)
    } else {
        SessionIdentity::child(session_id, SessionRef::from_id(root_id))
    }
}

fn state_revision_or_initial(snapshot: Option<&StoredSessionStateSnapshot>) -> u64 {
    let Some(snapshot) = snapshot else {
        return INITIAL_STATE_REVISION;
    };
    let Some(revision) = snapshot.state_revision() else {
        return INITIAL_STATE_REVISION;
    };
    revision
}

fn outer_compaction_revision(transcript: &[TranscriptEntry<Message>]) -> Option<u64> {
    match transcript.first() {
        Some(TranscriptEntry::Compaction { state_revision, .. }) => *state_revision,
        _ => None,
    }
}

fn selected_plugin_snapshot(
    session: &AppSession,
) -> Result<Option<StoredSessionStateSnapshot>, CompactionStateError> {
    let latest = session.meta.state_snapshot.clone();
    let Some(revision) = outer_compaction_revision(&session.transcript) else {
        return Ok(latest);
    };
    if state_revision_or_initial(latest.as_ref()) >= revision {
        return Ok(latest);
    }
    match session.meta.compaction_state_at(revision) {
        Ok(snapshot) => Ok(Some(snapshot.clone())),
        Err(
            error @ (CompactionStateError::MissingRevision { .. }
            | CompactionStateError::FutureRevision { .. }),
        ) => {
            tracing::warn!(
                session_id = %session.id,
                checkpoint_revision = revision,
                %error,
                "compaction checkpoint unavailable while loading plugin state; using latest snapshot"
            );
            Ok(latest)
        }
        Err(error) => Err(error),
    }
}

/// The single content predicate: `App::save_session` persists a session
/// iff this holds, and the shutdown path reuses it to tell which tabs were
/// saved, so the report and the disk can never disagree. Sync the session
/// first (`save_session` does).
pub(crate) fn session_has_content(session: &AppSession) -> bool {
    !session.messages.is_empty()
        || !session.subagent_messages.is_empty()
        || !session.meta.subagents.is_empty()
        || session.meta.input_draft.is_some()
        || !session.meta.queued_messages.is_empty()
        || !session.meta.queued_submissions.is_empty()
        || !session.meta.queued_direct_tools.is_empty()
        || session.meta.direct_output.is_some()
        || session.meta.lifecycle == StoredSessionLifecycle::Cancelled
        || session.meta.mode != Some(n00n_storage::sessions::StoredMode::Build)
        || session.meta.plan_path.is_some()
        || session.meta.plan_written
        || !session.meta.session_rules.is_empty()
        || session.meta.context_size != 0
        || !matches!(session.meta.thinking, None | Some(StoredThinking::Off))
        || session.meta.fast
        || session.meta.workflow
        || !session.meta.usage_by_model.is_empty()
        || !session.transcript.is_empty()
        || !session.tool_outputs.is_empty()
        || session.token_usage != TokenUsage::default()
}

fn stored_image(image: &n00n_agent::ImageSource) -> StoredImageSource {
    StoredImageSource {
        media_type: match image.media_type {
            n00n_agent::ImageMediaType::Png => StoredImageMediaType::Png,
            n00n_agent::ImageMediaType::Jpeg => StoredImageMediaType::Jpeg,
            n00n_agent::ImageMediaType::Gif => StoredImageMediaType::Gif,
            n00n_agent::ImageMediaType::Webp => StoredImageMediaType::Webp,
        },
        data: image.data.to_string(),
    }
}

fn restored_image(image: StoredImageSource) -> n00n_agent::ImageSource {
    let media_type = match image.media_type {
        StoredImageMediaType::Png => n00n_agent::ImageMediaType::Png,
        StoredImageMediaType::Jpeg => n00n_agent::ImageMediaType::Jpeg,
        StoredImageMediaType::Gif => n00n_agent::ImageMediaType::Gif,
        StoredImageMediaType::Webp => n00n_agent::ImageMediaType::Webp,
    };
    n00n_agent::ImageSource::new(media_type, Arc::from(image.data))
}

fn stored_delivery(delivery: Delivery) -> StoredDelivery {
    match delivery {
        Delivery::TurnEnd => StoredDelivery::TurnEnd,
        Delivery::Steering => StoredDelivery::Steering,
        Delivery::Immediate => StoredDelivery::Immediate,
    }
}

fn restored_delivery(delivery: StoredDelivery) -> Delivery {
    match delivery {
        StoredDelivery::TurnEnd => Delivery::TurnEnd,
        StoredDelivery::Steering => Delivery::Steering,
        StoredDelivery::Immediate => Delivery::Immediate,
    }
}

fn stored_message(
    input: AgentInput,
    delivery: Delivery,
    run_delivery: Option<n00n_agent::ControlDeliveryMetadata>,
) -> StoredQueuedMessage {
    // Preamble contains live shell results and may include transient secrets.
    let (mode, plan_path) = match input.mode {
        AgentMode::Build => (Some(StoredMode::Build), None),
        AgentMode::Plan(path) => (
            Some(StoredMode::Plan),
            Some(path.to_string_lossy().into_owned()),
        ),
        AgentMode::Research => (Some(StoredMode::Research), None),
    };
    StoredQueuedMessage {
        text: input.message,
        images: input.images.iter().map(stored_image).collect(),
        mode,
        plan_path,
        thinking: Some(input.thinking.into()),
        fast: input.fast,
        workflow: input.workflow,
        control: input.control,
        delivery: stored_delivery(delivery),
        prompt: input.prompt.map(|prompt| StoredMcpPrompt {
            qualified_name: prompt.qualified_name,
            arguments: prompt.arguments,
        }),
        run_delivery: run_delivery.map(|delivery| n00n_storage::sessions::StoredControlDelivery {
            delivery_id: delivery.delivery_id,
            child_run_id: delivery.child_run_id,
            source_revision: delivery.source_revision,
        }),
    }
}

fn restored_submission(
    app: &App,
    message: StoredQueuedMessage,
) -> (QueuedMessage, AgentInput, Delivery) {
    let delivery = restored_delivery(message.delivery);
    let queued = QueuedMessage {
        text: message.text,
        images: message.images.into_iter().map(restored_image).collect(),
        control: message.control,
        run_delivery: message
            .run_delivery
            .map(|delivery| n00n_agent::ControlDeliveryMetadata {
                delivery_id: delivery.delivery_id,
                child_run_id: delivery.child_run_id,
                source_revision: delivery.source_revision,
            }),
    };
    let mut input = app.build_agent_input(&queued);
    if let Some(mode) = message.mode {
        input.mode = match mode {
            StoredMode::Build => AgentMode::Build,
            StoredMode::Plan => message
                .plan_path
                .map_or(input.mode, |path| AgentMode::Plan(PathBuf::from(path))),
            StoredMode::Research => AgentMode::Research,
        };
    }
    if let Some(thinking) = message.thinking {
        input.thinking = thinking.into();
    }
    input.fast = message.fast;
    input.workflow = message.workflow;
    input.prompt = message.prompt.map(|prompt| {
        Box::new(McpPromptRef {
            qualified_name: prompt.qualified_name,
            arguments: prompt.arguments,
        })
    });
    (queued, input, delivery)
}

impl App {
    #[allow(dead_code)]
    pub(crate) fn has_content(&self) -> bool {
        session_has_content(&self.state.session)
    }
    #[cfg(test)]
    pub(crate) fn save_session(&mut self) {
        if self.plugin_state_capture_safe() {
            self.save_session_with_plugin_state_capture();
        } else {
            self.save_session_without_plugin_state_capture();
        }
    }

    #[cfg(test)]
    pub(super) fn save_session_with_plugin_state_capture(&mut self) {
        let snapshot = self.session_snapshot_with_plugin_state();
        self.save_snapshot(snapshot);
    }

    pub(crate) fn save_session_without_plugin_state_capture(&mut self) {
        let snapshot = self.session_snapshot();
        self.save_snapshot(snapshot);
    }

    fn save_snapshot(&mut self, snapshot: AppSession) {
        self.pending_save = false;
        self.last_save_flush = Some(Instant::now());
        #[cfg(test)]
        {
            self.session_saves += 1;
        }
        if !session_has_content(&snapshot) {
            return;
        }
        self.storage_writer.send(Box::new(snapshot));
        self.enforce_retention_budget();
    }

    /// Persists completion state at most once per [`SAVE_COALESCE_INTERVAL`].
    pub(crate) fn save_session_coalesced(&mut self) {
        if self.save_window_elapsed() {
            self.save_session_without_plugin_state_capture();
        } else {
            self.pending_save = true;
        }
    }

    /// Driven by the event loop so a deferred save still lands once the burst
    /// stops.
    pub(crate) fn tick_pending_save(&mut self) {
        if self.pending_save && self.save_window_elapsed() {
            self.save_session_without_plugin_state_capture();
        }
    }

    fn save_window_elapsed(&self) -> bool {
        self.last_save_flush
            .is_none_or(|flushed| flushed.elapsed() >= SAVE_COALESCE_INTERVAL)
    }

    pub(crate) fn plugin_state_capture_safe(&self) -> bool {
        self.status != Status::Streaming
            && !self.chats.iter().any(Chat::is_working)
            && !self.chats.first().is_some_and(Chat::has_pending_compaction)
    }

    /// Drops the oldest tool outputs and subagent histories past the budget.
    /// Only records the writer already put on disk are dropped, so the log
    /// stays the complete history.
    fn enforce_retention_budget(&mut self) {
        let mut eviction = self
            .state
            .session
            .retention_eviction_candidates(self.retention_budget, message_tool_use_ids);
        if eviction.is_empty() {
            return;
        }
        self.storage_writer
            .retain_durable(self.state.session.id, &mut eviction);
        if eviction.is_empty() {
            return;
        }
        if let Some(outputs) = &self.shared_tool_outputs {
            let mut outputs = outputs
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            for tool_use_id in &eviction.tool_outputs {
                outputs.remove(tool_use_id);
            }
        }
        tracing::debug!(
            session_id = %self.state.session.id,
            tool_outputs = eviction.tool_outputs.len(),
            subagent_messages = eviction.subagent_messages.len(),
            "evicted live-session records past the retention budget",
        );
        self.state.session.evict_retained(&eviction);
    }

    #[cfg(test)]
    pub(crate) fn checkpoint_session(&mut self, timeout: Duration) -> Result<(), SessionError> {
        self.pending_save = false;
        self.last_save_flush = Some(Instant::now());
        let snapshot = self.session_snapshot_with_plugin_state();
        if !session_has_content(&snapshot) {
            return Ok(());
        }
        let result = self
            .storage_writer
            .persist_and_wait(Box::new(snapshot), timeout);
        if result.is_ok() {
            self.enforce_retention_budget();
        }
        result
    }

    pub(crate) fn session_snapshot(&mut self) -> AppSession {
        self.state.sync_session(
            self.shared_history.as_ref(),
            self.shared_transcript.as_ref(),
            self.shared_tool_outputs.as_ref(),
            &self.permissions,
        );
        self.sync_ephemeral_state();
        self.state.finish_snapshot();
        self.state.session.clone()
    }

    #[cfg(test)]
    fn session_snapshot_with_plugin_state(&mut self) -> AppSession {
        let mut snapshot = self.session_snapshot();
        self.capture_plugin_state();
        snapshot
            .meta
            .state_snapshot
            .clone_from(&self.state.session.meta.state_snapshot);
        snapshot
    }

    pub(crate) fn fire_session_focus_autocmd(&mut self) {
        let state_snapshot = match self.state.session.meta.state_snapshot.as_ref() {
            Some(snapshot) => match serde_json::to_value(snapshot) {
                Ok(value) => value,
                Err(error) => {
                    tracing::warn!(%error, "failed to encode focused plugin session state");
                    serde_json::Value::Null
                }
            },
            None => serde_json::Value::Null,
        };
        self.fire_session_autocmd(
            "SessionFocus",
            serde_json::json!({ "state_snapshot": state_snapshot }),
        );
    }

    pub(crate) fn hydrate_plugin_state(&mut self) -> bool {
        let snapshot = match selected_plugin_snapshot(&self.state.session) {
            Ok(snapshot) => snapshot,
            Err(error) => {
                tracing::warn!(session_id = %self.state.session.id, %error, "refusing unusable compaction checkpoint while loading plugin state");
                return false;
            }
        };
        self.hydrate_plugin_snapshot(snapshot)
    }

    fn hydrate_plugin_snapshot(&mut self, snapshot: Option<StoredSessionStateSnapshot>) -> bool {
        let Some(handle) = &self.lua_event_handle else {
            return false;
        };
        let session_id = self.state.session.id;
        let identity = plugin_state_identity(&self.state.session);
        match handle.hydrate_state_background(&identity, snapshot) {
            Ok(()) => true,
            Err(error) => {
                tracing::warn!(%session_id, %error, "failed to restore plugin session state");
                false
            }
        }
    }

    #[cfg(test)]
    fn capture_plugin_state(&mut self) {
        if self.main_chat().has_pending_compaction() {
            return;
        }
        let Some(handle) = &self.lua_event_handle else {
            return;
        };
        let session_id = self.state.session.id;
        let identity = plugin_state_identity(&self.state.session);
        let revision = if let Some(allocator) = &self.revision_allocator {
            match allocator.allocate() {
                Ok(revision) => revision,
                Err(error) => {
                    tracing::warn!(%session_id, %error, "failed to allocate plugin state revision");
                    return;
                }
            }
        } else {
            let persisted_revision =
                state_revision_or_initial(self.state.session.meta.state_snapshot.as_ref());
            self.state
                .session
                .meta
                .revision
                .max(persisted_revision.saturating_add(1))
        };
        match handle.capture_state(&identity, revision) {
            Ok(snapshot) => {
                if let Some(allocator) = &self.revision_allocator {
                    allocator.observe(state_revision_or_initial(Some(&snapshot)));
                }
                self.state.session.meta.state_snapshot = Some(snapshot);
            }
            Err(error) => {
                tracing::warn!(%session_id, %error, "failed to capture plugin session state");
            }
        }
    }

    pub(crate) fn drop_plugin_state(&self, session_id: n00nId) {
        let Some(handle) = &self.lua_event_handle else {
            return;
        };
        if let Err(error) = handle.drop_state_owner_background(session_id) {
            tracing::warn!(%session_id, %error, "failed to drop plugin session state");
        }
    }

    fn sync_ephemeral_state(&mut self) {
        let draft = self.input_box.buffer.value();
        self.state.session.meta.input_draft = if draft.is_empty() { None } else { Some(draft) };

        let queued = self.queue.queued_inputs();
        self.state.session.meta.queued_messages = self.queue.text_messages();
        self.state.session.meta.queued_submissions = queued
            .into_iter()
            .map(|(input, delivery, run_delivery)| stored_message(input, delivery, run_delivery))
            .collect();
        let queued_direct_tools: Vec<_> = self
            .queue
            .direct_tools()
            .into_iter()
            .map(|(tool, input)| StoredDirectTool { tool, input })
            .collect();
        if !queued_direct_tools.is_empty() {
            self.state.session.meta.queued_direct_tools = queued_direct_tools;
        } else if !self.state.session.meta.lifecycle.is_active() {
            self.state.session.meta.queued_direct_tools.clear();
        }

        self.state.session.meta.subagents = self
            .chats
            .iter()
            .skip(1)
            .filter_map(|chat| {
                chat.tool_use_id.as_ref().map(|tool_use_id| StoredSubagent {
                    tool_use_id: tool_use_id.clone(),
                    name: chat.name.clone(),
                    prompt: None,
                    model: chat.model_id.clone(),
                })
            })
            .collect();
    }

    pub(super) fn save_input_history(&self) {
        if let Err(e) = self.input_box.history().save(&self.storage) {
            tracing::warn!(error = %e, "input history save failed");
        }
    }

    pub(super) fn enqueue_save(&mut self) {
        let snapshot = self.session_snapshot();
        if session_has_content(&snapshot) {
            self.storage_writer.send(Box::new(snapshot));
        }
    }

    pub(super) fn reset_ui_chrome(&mut self) {
        self.chats.clear();
        let mut main = Chat::new(
            "Main".into(),
            self.ui_config.clone(),
            Arc::clone(&self.picker),
        );
        main.set_restore_channel(self.lua_event_handle.clone(), self.restore_event_tx.clone());
        self.chats.push(main);
        self.active_chat = 0;
        self.chat_index.clear();
        self.status = super::Status::Idle;
        self.queue.clear();
        self.close_all_overlays();
        self.pending_input = PendingInput::None;
        self.status_bar.clear_flash();
        self.task_picker_original = None;
        self.last_esc = None;
        self.restoring = Arc::new(AtomicBool::new(false));
        self.plan_form.reset();
    }

    pub(crate) fn restore_display(&mut self) {
        let restoring = Arc::new(AtomicBool::new(true));
        self.restoring = Arc::clone(&restoring);

        // A rewind redraws the live session, whose oldest outputs the retention
        // budget may have evicted. Read those back before rendering, or the
        // restored transcript shows empty tool results.
        let referenced = self
            .state
            .session
            .displayed_tool_use_ids(&message_tool_use_ids);
        let tool_outputs = self.display_tool_outputs(referenced.into_iter());
        let (display_msgs, restore_items) = if self.state.session.transcript.is_empty() {
            history_to_display(
                &self.state.session.messages,
                &tool_outputs,
                &self.ui_config.tool_output_lines,
            )
        } else {
            transcript_to_display(
                &self.state.session.transcript,
                &tool_outputs,
                &self.ui_config.tool_output_lines,
            )
        };
        self.main_chat()
            .begin_restore(display_msgs, RESTORE_BATCH_SIZE);
        self.main_chat().token_usage = self.state.token_usage;
        self.main_chat().context_size = self.state.context_size;
        if let Some(draft) = self.state.session.meta.input_draft.take() {
            self.input_box.set_input(&draft);
            self.input_box.buffer.move_to_end();
        }

        let queued: Vec<(QueuedMessage, AgentInput, Delivery)> =
            if self.state.session.meta.queued_submissions.is_empty() {
                std::mem::take(&mut self.state.session.meta.queued_messages)
                    .into_iter()
                    .map(|text| {
                        let msg = QueuedMessage {
                            text,
                            images: Vec::new(),
                            control: false,
                            run_delivery: None,
                        };
                        let input = self.build_agent_input(&msg);
                        (msg, input, Delivery::TurnEnd)
                    })
                    .collect()
            } else {
                std::mem::take(&mut self.state.session.meta.queued_submissions)
                    .into_iter()
                    .map(|message| restored_submission(self, message))
                    .collect()
            };
        self.state.session.meta.queued_messages.clear();
        for (msg, input, delivery) in queued {
            self.queue_restored_submission(msg, input, delivery);
        }
        for bootstrap in self.state.session.meta.queued_direct_tools.clone() {
            self.run_id += 1;
            self.status = super::Status::Streaming;
            self.queue.push_direct_tool(QueueItem::DirectTool {
                run_id: self.run_id,
                tool: bootstrap.tool,
                input: bootstrap.input,
            });
        }

        self.fire_restore_items(restore_items);

        for sa in std::mem::take(&mut self.state.session.meta.subagents) {
            let idx = self.chats.len();
            self.chat_index.insert(sa.tool_use_id.clone(), idx);
            let mut chat = Chat::new(sa.name, self.ui_config.clone(), Arc::clone(&self.picker));
            chat.set_restore_channel(self.lua_event_handle.clone(), self.restore_event_tx.clone());
            chat.tool_use_id = Some(sa.tool_use_id.clone());
            chat.model_id = sa.model;
            if let Some(source) = self.subagent_display_source(&sa.tool_use_id) {
                let (display, items) = history_to_display(
                    &source.messages,
                    &source.tool_outputs,
                    &self.ui_config.tool_output_lines,
                );
                chat.begin_restore(display, RESTORE_BATCH_SIZE);
                chat.mark_finished(DisplayRole::Done, DONE_TEXT);
                self.fire_restore_items(items);
            }
            self.chats.push(chat);
        }

        if let Some(eh) = &self.lua_event_handle {
            eh.send_restore_complete(restoring);
        } else {
            self.restoring
                .store(false, std::sync::atomic::Ordering::Relaxed);
        }
    }

    /// A rewind rebuilds every subagent tab from the *live* session, which may
    /// have evicted older histories under its retention budget. Those come
    /// back off the log, along with the tool outputs they render.
    pub(super) fn subagent_display_source(
        &self,
        tool_use_id: &str,
    ) -> Option<SubagentDisplaySource<'_>> {
        if let Some(messages) = self.state.session.subagent_messages.get(tool_use_id) {
            return Some(SubagentDisplaySource {
                messages: Cow::Borrowed(messages),
                tool_outputs: Cow::Borrowed(&self.state.session.tool_outputs),
            });
        }
        if !self
            .state
            .session
            .evicted_subagent_messages()
            .contains(tool_use_id)
        {
            return None;
        }
        let session_id = self.state.session.id;
        let dir = self.sessions_dir()?;
        let messages = match AppSession::load_subagent_messages_from(session_id, &dir, tool_use_id)
        {
            Ok(messages) => messages,
            Err(error) => {
                tracing::warn!(%session_id, %error, "failed to load an evicted subagent history");
                return None;
            }
        };
        let tool_outputs =
            self.display_tool_outputs(messages.iter().flat_map(message_tool_use_ids));
        Some(SubagentDisplaySource {
            messages: Cow::Owned(messages),
            tool_outputs,
        })
    }

    /// The tool outputs a display pass needs, with any the retention budget
    /// evicted read back from the log. Borrowed whenever nothing is missing, so
    /// the common path stays allocation-free.
    pub(super) fn display_tool_outputs(
        &self,
        referenced: impl Iterator<Item = String>,
    ) -> Cow<'_, HashMap<String, ToolOutput>> {
        let resident = &self.state.session.tool_outputs;
        if self.state.session.evicted_tool_outputs().is_empty() {
            return Cow::Borrowed(resident);
        }
        let missing: HashSet<String> = referenced
            .filter(|id| !resident.contains_key(id))
            .filter(|id| self.state.session.evicted_tool_outputs().contains(id))
            .collect();
        if missing.is_empty() {
            return Cow::Borrowed(resident);
        }
        let Some(dir) = self.sessions_dir() else {
            return Cow::Borrowed(resident);
        };
        let session_id = self.state.session.id;
        match AppSession::load_tool_outputs_from(session_id, &dir, &missing) {
            Ok(loaded) => {
                let mut outputs = resident.clone();
                outputs.extend(loaded);
                Cow::Owned(outputs)
            }
            Err(error) => {
                tracing::warn!(%session_id, %error, "failed to load evicted tool outputs");
                Cow::Borrowed(resident)
            }
        }
    }

    fn sessions_dir(&self) -> Option<PathBuf> {
        match self.storage.ensure_subdir(SESSIONS_DIR) {
            Ok(dir) => Some(dir),
            Err(error) => {
                tracing::warn!(
                    session_id = %self.state.session.id,
                    %error,
                    "cannot reach the session log to recover evicted history",
                );
                None
            }
        }
    }

    fn fire_restore_items(&self, items: Vec<n00n_lua::RestoreItem>) {
        let (Some(eh), Some(tx)) = (&self.lua_event_handle, &self.restore_event_tx) else {
            return;
        };
        let theme_gen = crate::theme::generation();
        for mut item in items {
            item.theme_gen = Some(theme_gen);
            eh.request_restore(item, tx.clone());
        }
    }

    fn loaded_session_snapshot(&self) -> LoadedSession {
        LoadedSession {
            messages: self.state.session.messages.clone(),
            transcript: self.state.session.transcript.clone(),
            tool_outputs: self.state.session.tool_outputs.clone(),
            model_spec: self.state.session.model.clone(),
            plugin_state_hydrated: false,
        }
    }

    pub(super) fn reset_session(&mut self) -> Vec<Action> {
        self.save_session_without_plugin_state_capture();
        let previous_id = self.state.session.id;
        self.drop_plugin_state(previous_id);
        self.reset_ui_chrome();
        self.state.token_usage = TokenUsage::default();
        self.state.context_size = 0;
        self.state.plan = PlanState::None;
        if self.state.mode == Mode::Plan {
            self.enter_plan();
        }
        self.state.session = AppSession::new(&self.state.session.model, &self.state.session.cwd);
        self.hydrate_plugin_state();
        self.fire_session_autocmd("SessionReset", serde_json::json!({}));
        self.fire_session_focus_autocmd();
        vec![Action::NewSession { previous_id }]
    }

    pub(super) fn open_rewind_picker(&mut self) -> Vec<Action> {
        self.save_session_without_plugin_state_capture();
        match self.rewind_picker.open(&self.state.session.messages) {
            Ok(()) => vec![],
            Err(msg) => {
                self.status_bar.flash(msg);
                vec![]
            }
        }
    }

    pub(super) fn rewind_to(&mut self, entry: &RewindEntry) -> Vec<Action> {
        self.run_id += 1;

        self.state.session.messages.truncate(entry.turn_index);
        n00n_agent::agent::rebuild_transcript(
            &mut self.state.session.transcript,
            &self.state.session.messages,
        );
        if let Some(revision) = outer_compaction_revision(&self.state.session.transcript) {
            match self.state.session.meta.compaction_state_at(revision) {
                Ok(snapshot) => {
                    let snapshot = snapshot.clone();
                    self.state.session.meta.state_snapshot = Some(snapshot.clone());
                    self.hydrate_plugin_snapshot(Some(snapshot));
                }
                Err(error) => tracing::warn!(
                    session_id = %self.state.session.id,
                    checkpoint_revision = revision,
                    %error,
                    "failed to select rewound plugin session state"
                ),
            }
        }
        self.state
            .session
            .prune_orphans(|m| m.tool_uses().map(|(id, _, _)| id.to_owned()).collect());
        self.state.context_size = n00n_agent::agent::estimate_message_tokens(
            &self.state.session.messages,
            &self.state.model.id,
        );

        self.reset_ui_chrome();
        self.restore_display();

        self.input_box.set_input(&entry.prompt_text);
        self.input_box.buffer.move_to_end();

        self.state.session.update_title_if_default();
        self.enqueue_save();

        vec![Action::LoadSession(Box::new(
            self.loaded_session_snapshot(),
        ))]
    }

    #[allow(dead_code)]
    pub(crate) fn apply_loaded_session(
        &mut self,
        session: AppSession,
        fallback_model: &Model,
    ) -> LoadedSession {
        self.permissions
            .load_session_rules(stored_to_rules(&session.meta.session_rules));
        let previous_session_id = self.state.session.id;
        self.storage_writer.register_loaded(&session);
        self.state = SessionState::from_session(session, fallback_model, &self.storage);
        if previous_session_id != self.state.session.id {
            self.drop_plugin_state(previous_session_id);
        }
        let plugin_state_hydrated = self.hydrate_plugin_state();
        self.state
            .session
            .prune_orphans(|m| m.tool_uses().map(|(id, _, _)| id.to_owned()).collect());
        for w in self.state.warnings.drain(..) {
            self.status_bar.flash(w);
        }
        self.reset_ui_chrome();
        self.restore_display();
        self.fire_session_focus_autocmd();

        self.enqueue_save();
        let mut loaded = self.loaded_session_snapshot();
        loaded.plugin_state_hydrated = plugin_state_hydrated;
        loaded
    }

    #[allow(dead_code)]
    pub(crate) fn load_session(&mut self, session_id: n00nId) -> Vec<Action> {
        let mut session = match AppSession::load_with_retention(
            session_id,
            &self.storage,
            self.retention_budget,
            message_tool_use_ids,
        ) {
            Ok(s) => s,
            Err(e) => {
                self.status_bar
                    .flash(format!("Failed to load session: {e}"));
                return vec![];
            }
        };
        self.save_session_without_plugin_state_capture();
        session.meta.revision = session.meta.revision.max(self.state.session.meta.revision);
        let loaded = self.apply_loaded_session(session, &self.state.model.clone());
        vec![Action::LoadSession(Box::new(loaded))]
    }
}

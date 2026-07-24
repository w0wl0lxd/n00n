use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use crate::chat::{Chat, DONE_TEXT, RESTORE_BATCH_SIZE, history_to_display, transcript_to_display};
use crate::components::DisplayRole;
use crate::components::rewind_picker::RewindEntry;
use crate::components::{Action, LoadedSession};
use n00n_agent::{AgentInput, AgentMode, McpPromptRef};
use n00n_providers::{Model, TokenUsage};
use n00n_storage::id::N00nId;
use n00n_storage::sessions::{
    StoredImageMediaType, StoredImageSource, StoredMcpPrompt, StoredMode, StoredQueuedMessage,
    StoredSubagent, StoredThinking,
};

use crate::AppSession;

use super::session_state::{SessionState, stored_to_rules};
use super::{App, Mode, PendingInput, PlanState};
use crate::agent::QueuedMessage;

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

fn stored_message(input: AgentInput) -> StoredQueuedMessage {
    // Preamble contains live shell results and may include transient secrets.
    let (mode, plan_path) = match input.mode {
        AgentMode::Build => (Some(StoredMode::Build), None),
        AgentMode::Plan(path) => (
            Some(StoredMode::Plan),
            Some(path.to_string_lossy().into_owned()),
        ),
    };
    StoredQueuedMessage {
        text: input.message,
        images: input.images.iter().map(stored_image).collect(),
        mode,
        plan_path,
        thinking: Some(input.thinking.into()),
        fast: input.fast,
        workflow: input.workflow,
        prompt: input.prompt.map(|prompt| StoredMcpPrompt {
            qualified_name: prompt.qualified_name,
            arguments: prompt.arguments,
        }),
    }
}

fn restored_submission(app: &App, message: StoredQueuedMessage) -> (QueuedMessage, AgentInput) {
    let queued = QueuedMessage {
        text: message.text,
        images: message.images.into_iter().map(restored_image).collect(),
    };
    let mut input = app.build_agent_input(&queued);
    if let Some(mode) = message.mode {
        input.mode = match mode {
            StoredMode::Build => AgentMode::Build,
            StoredMode::Plan => message
                .plan_path
                .map_or(input.mode, |path| AgentMode::Plan(PathBuf::from(path))),
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
    (queued, input)
}

impl App {
    pub(crate) fn has_content(&self) -> bool {
        session_has_content(&self.state.session)
    }

    pub(crate) fn save_session(&mut self) {
        let snapshot = self.session_snapshot();
        if !session_has_content(&snapshot) {
            return;
        }
        self.storage_writer.send(Box::new(snapshot));
    }

    pub(crate) fn session_snapshot(&mut self) -> AppSession {
        self.state.sync_session(
            self.shared_history.as_ref(),
            self.shared_transcript.as_ref(),
            self.shared_tool_outputs.as_ref(),
            &self.permissions,
        );
        self.sync_ephemeral_state();
        self.state.session.clone()
    }

    fn sync_ephemeral_state(&mut self) {
        let draft = self.input_box.buffer.value();
        self.state.session.meta.input_draft = if draft.is_empty() { None } else { Some(draft) };

        let queued = self.queue.queued_inputs();
        self.state.session.meta.queued_messages = self.queue.text_messages();
        self.state.session.meta.queued_submissions =
            queued.into_iter().map(stored_message).collect();

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

        let (display_msgs, restore_items) = if self.state.session.transcript.is_empty() {
            history_to_display(
                &self.state.session.messages,
                &self.state.session.tool_outputs,
                &self.ui_config.tool_output_lines,
            )
        } else {
            transcript_to_display(
                &self.state.session.transcript,
                &self.state.session.tool_outputs,
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

        let queued: Vec<(QueuedMessage, AgentInput)> =
            if self.state.session.meta.queued_submissions.is_empty() {
                std::mem::take(&mut self.state.session.meta.queued_messages)
                    .into_iter()
                    .map(|text| {
                        let msg = QueuedMessage {
                            text,
                            images: Vec::new(),
                        };
                        let input = self.build_agent_input(&msg);
                        (msg, input)
                    })
                    .collect()
            } else {
                std::mem::take(&mut self.state.session.meta.queued_submissions)
                    .into_iter()
                    .map(|message| restored_submission(self, message))
                    .collect()
            };
        self.state.session.meta.queued_messages.clear();
        for (msg, input) in queued {
            self.queue_restored_submission(msg, input);
        }

        self.fire_restore_items(restore_items);

        for sa in std::mem::take(&mut self.state.session.meta.subagents) {
            let idx = self.chats.len();
            self.chat_index.insert(sa.tool_use_id.clone(), idx);
            let mut chat = Chat::new(sa.name, self.ui_config.clone(), Arc::clone(&self.picker));
            chat.set_restore_channel(self.lua_event_handle.clone(), self.restore_event_tx.clone());
            chat.tool_use_id = Some(sa.tool_use_id.clone());
            chat.model_id = sa.model;
            if let Some(messages) = self.state.session.subagent_messages.get(&sa.tool_use_id) {
                let (display, items) = history_to_display(
                    messages,
                    &self.state.session.tool_outputs,
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
        }
    }

    pub(super) fn reset_session(&mut self) -> Vec<Action> {
        self.reset_ui_chrome();
        self.state.token_usage = TokenUsage::default();
        self.state.context_size = 0;
        self.state.plan = PlanState::None;
        if self.state.mode == Mode::Plan {
            self.enter_plan();
        }
        self.state.session = AppSession::new(&self.state.session.model, &self.state.session.cwd);
        self.fire_session_autocmd("SessionReset", serde_json::json!({}));
        vec![Action::NewSession]
    }

    pub(super) fn open_rewind_picker(&mut self) -> Vec<Action> {
        self.save_session();
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
        self.state
            .session
            .prune_orphans(|m| m.tool_uses().map(|(id, _, _)| id.to_owned()).collect());
        self.state.context_size =
            n00n_agent::agent::estimate_message_tokens(&self.state.session.messages);

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

    pub(crate) fn apply_loaded_session(
        &mut self,
        session: AppSession,
        fallback_model: &Model,
    ) -> LoadedSession {
        self.permissions
            .load_session_rules(stored_to_rules(&session.meta.session_rules));
        self.state = SessionState::from_session(session, fallback_model, &self.storage);
        for w in self.state.warnings.drain(..) {
            self.status_bar.flash(w);
        }
        self.reset_ui_chrome();
        self.restore_display();

        self.enqueue_save();
        self.loaded_session_snapshot()
    }

    pub(crate) fn load_session(&mut self, session_id: N00nId) -> Vec<Action> {
        let mut session = match AppSession::load(session_id, &self.storage) {
            Ok(s) => s,
            Err(e) => {
                self.status_bar
                    .flash(format!("Failed to load session: {e}"));
                return vec![];
            }
        };
        self.save_session();
        session.meta.revision = session.meta.revision.max(self.state.session.meta.revision);
        let loaded = self.apply_loaded_session(session, &self.state.model.clone());
        vec![Action::LoadSession(Box::new(loaded))]
    }
}

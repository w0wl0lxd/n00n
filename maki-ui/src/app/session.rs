use crate::chat::{Chat, history_to_display};
use crate::components::rewind_picker::RewindEntry;
use crate::components::{Action, LoadedSession};
use maki_providers::TokenUsage;

use crate::AppSession;

use super::App;

impl App {
    pub(super) fn save_session(&mut self) {
        if let Some(ref history) = self.shared_history {
            self.session.messages = history.lock().unwrap().clone();
        }
        if let Some(ref outputs) = self.shared_tool_outputs {
            self.session.tool_outputs = outputs.lock().unwrap().clone();
        }
        if self.session.messages.is_empty() {
            return;
        }
        self.session.token_usage = self.token_usage;
        self.session.update_title_if_default();
        self.enqueue_save();
    }

    pub(super) fn save_input_history(&self) {
        if let Err(e) = self.input_box.history().save(&self.storage) {
            tracing::warn!(error = %e, "input history save failed");
        }
    }

    pub(super) fn enqueue_save(&self) {
        self.storage_writer.send(Box::new(self.session.clone()));
    }

    pub(super) fn reset_session_state(&mut self) {
        self.chats.clear();
        self.chats.push(Chat::new("Main".into()));
        self.active_chat = 0;
        self.chat_index.clear();
        self.status = super::Status::Idle;
        self.token_usage = TokenUsage::default();
        self.clear_queue();
        #[cfg(feature = "demo")]
        {
            self.demo_questions = None;
        }
        self.question_form.close();
        self.help_modal.close();
        self.theme_picker.close();
        self.model_picker.close();
        self.session_picker.close();
        self.rewind_picker.close();
        self.pending_question = false;
        self.status_bar.clear_flash();
        self.chat_picker.close();
        self.last_esc = None;
    }

    pub(super) fn reset_session(&mut self) -> Vec<Action> {
        self.reset_session_state();
        self.session = AppSession::new(&self.session.model, &self.session.cwd);
        vec![Action::NewSession]
    }

    pub(super) fn open_rewind_picker(&mut self) -> Vec<Action> {
        self.save_session();
        match self.rewind_picker.open(&self.session.messages) {
            Ok(()) => vec![],
            Err(msg) => {
                self.status_bar.flash(msg);
                vec![]
            }
        }
    }

    pub(super) fn rewind_to(&mut self, entry: RewindEntry) -> Vec<Action> {
        self.run_id += 1;

        let stale_ids: Vec<String> = self.session.messages[entry.turn_index..]
            .iter()
            .flat_map(|m| m.tool_uses())
            .map(|(id, _, _)| id.to_owned())
            .collect();
        self.session.messages.truncate(entry.turn_index);
        for id in &stale_ids {
            self.session.tool_outputs.remove(id);
        }
        self.session.token_usage = self.token_usage;

        self.reset_session_state();
        let display_msgs = history_to_display(&self.session.messages, &self.session.tool_outputs);
        self.main_chat().load_messages(display_msgs);
        self.token_usage = self.session.token_usage;

        self.input_box.set_input(entry.prompt_text);

        self.session.update_title_if_default();
        self.enqueue_save();

        vec![Action::LoadSession(LoadedSession {
            messages: self.session.messages.clone(),
            tool_outputs: self.session.tool_outputs.clone(),
        })]
    }

    pub(super) fn open_session_picker(&mut self) -> Vec<Action> {
        match self
            .session_picker
            .open(&self.session.cwd, &self.session.id, &self.storage)
        {
            Ok(()) => vec![],
            Err(msg) => {
                self.status_bar.flash(msg);
                vec![]
            }
        }
    }

    pub(super) fn load_session(&mut self, session_id: String) -> Vec<Action> {
        let session = match AppSession::load(&session_id, &self.storage) {
            Ok(s) => s,
            Err(e) => {
                self.status_bar
                    .flash(format!("Failed to load session: {e}"));
                return vec![];
            }
        };
        self.save_session();
        self.reset_session_state();
        self.session = session;
        let display_msgs = history_to_display(&self.session.messages, &self.session.tool_outputs);
        self.main_chat().load_messages(display_msgs);
        self.token_usage = self.session.token_usage;
        vec![Action::LoadSession(LoadedSession {
            messages: self.session.messages.clone(),
            tool_outputs: self.session.tool_outputs.clone(),
        })]
    }

    pub(super) fn delete_session(&mut self, session_id: String) -> Vec<Action> {
        if let Err(e) = AppSession::delete(&session_id, &self.storage) {
            self.status_bar
                .flash(format!("Failed to delete session: {e}"));
            return vec![];
        }
        self.session_picker.remove_entry(&session_id);
        self.status_bar.flash("Session deleted".into());
        vec![]
    }
}

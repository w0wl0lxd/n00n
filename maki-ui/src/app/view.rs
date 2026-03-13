use crate::components::keybindings::KeybindContext;
use crate::components::queue_panel;
use crate::components::status_bar::{StatusBarContext, UsageStats};
use crate::selection::{self, SelectableZone, SelectionZone};
use crate::theme;
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::widgets::{Block, Widget};

use super::{App, Status};

impl App {
    pub fn view(&mut self, frame: &mut Frame) {
        self.status_bar.clear_expired_hint();

        let bg =
            Block::default().style(ratatui::style::Style::new().bg(theme::current().background));
        bg.render(frame.area(), frame.buffer_mut());

        let form_visible = self.question_form.is_visible();
        let max_form_height = frame.area().height.saturating_sub(3);
        let bottom_height = if form_visible {
            self.question_form
                .height(frame.area().width)
                .min(max_form_height)
        } else {
            queue_panel::height(self.queue.len()) + self.input_box.height(frame.area().width)
        };
        let [msg_area, bottom_area, status_area] = Layout::vertical([
            Constraint::Min(1),
            Constraint::Length(bottom_height),
            Constraint::Length(1),
        ])
        .areas(frame.area());
        self.zones[SelectionZone::Messages.idx()] = Some(SelectableZone {
            area: msg_area,
            highlight_area: Rect::new(
                msg_area.x,
                msg_area.y,
                msg_area.width.saturating_sub(1),
                msg_area.height,
            ),
            zone: SelectionZone::Messages,
        });
        let picker_open = self.task_picker.is_open();
        let render_chat = if picker_open {
            self.task_picker
                .selected_index()
                .unwrap_or(self.active_chat)
        } else {
            self.active_chat
        };
        self.chats[render_chat].view(frame, msg_area, self.selection_state.is_some());

        let queue_height = queue_panel::height(self.queue.len());
        let input_height = bottom_area.height.saturating_sub(queue_height);
        let [queue_area, input_area] = Layout::vertical([
            Constraint::Length(queue_height),
            Constraint::Length(input_height),
        ])
        .areas(bottom_area);
        let input_inner = selection::inset_border(input_area);
        self.zones[SelectionZone::Input.idx()] = Some(SelectableZone {
            area: input_inner,
            highlight_area: input_inner,
            zone: SelectionZone::Input,
        });

        if form_visible {
            self.question_form.view(frame, bottom_area);
        } else {
            let queue_entries = self.queue_entries();
            queue_panel::view(frame, queue_area, &queue_entries, self.queue_focus);
            self.input_box.view(
                frame,
                input_area,
                self.status == Status::Streaming,
                self.mode.color(),
            );
            self.command_palette.view(frame, input_area);
        }

        if picker_open {
            let full_area = frame.area();
            self.task_picker.view(frame, full_area);
        }

        if self.session_picker.is_open() {
            self.session_picker.view(frame, frame.area());
        }

        if self.rewind_picker.is_open() {
            self.rewind_picker.view(frame, frame.area());
        }

        if self.theme_picker.is_open() {
            self.theme_picker.view(frame, frame.area());
        }

        if self.model_picker.is_open() {
            self.model_picker.view(frame, frame.area());
        }

        let chat = &self.chats[render_chat];
        let chat_name = (self.chats.len() > 1).then_some(chat.name.as_str());
        let (mode_label, mode_style) = self.mode_label();
        let ctx = StatusBarContext {
            status: &self.status,
            mode_label,
            mode_style,
            model_id: chat.model_id.as_deref().unwrap_or(&self.model_id),
            stats: UsageStats {
                usage: &chat.token_usage,
                global_usage: &self.token_usage,
                context_size: chat.context_size,
                pricing: &self.pricing,
                context_window: self.context_window,
                show_global: self.chats.len() > 1,
            },
            auto_scroll: chat.auto_scroll(),
            chat_name,
            retry_info: self.retry_info.as_ref(),
        };
        self.status_bar.view(frame, status_area, &ctx);

        self.zones[SelectionZone::StatusBar.idx()] = Some(SelectableZone {
            area: status_area,
            highlight_area: status_area,
            zone: SelectionZone::StatusBar,
        });

        let help_contexts = self.active_keybind_contexts();
        self.help_modal.view(frame, frame.area(), &help_contexts);

        if let Some(ref state) = self.selection_state {
            let zone = state.sel.zone;
            let scroll = self.scroll_offset(zone);
            if let Some(screen_sel) = state.sel.to_screen(scroll) {
                let highlight_area = self.zones[zone.idx()]
                    .map(|z| z.highlight_area)
                    .unwrap_or_default();
                selection::apply_highlight(frame.buffer_mut(), highlight_area, &screen_sel);
            }
            if state.copy_on_release {
                let sel = state.sel;
                self.copy_selection(frame.buffer_mut(), &sel, render_chat);
            }
        }
    }

    pub(super) fn active_keybind_contexts(&self) -> Vec<KeybindContext> {
        let mut contexts = vec![KeybindContext::General];
        if self.question_form.is_visible() {
            contexts.push(KeybindContext::QuestionForm);
        } else if self.queue_focus.is_some() {
            contexts.push(KeybindContext::QueueFocus);
        } else if self.session_picker.is_open() {
            contexts.push(KeybindContext::SessionPicker);
        } else if self.rewind_picker.is_open() {
            contexts.push(KeybindContext::RewindPicker);
        } else if self.task_picker.is_open() {
            contexts.push(KeybindContext::TaskPicker);
        } else if self.theme_picker.is_open() {
            contexts.push(KeybindContext::ThemePicker);
        } else if self.model_picker.is_open() {
            contexts.push(KeybindContext::ModelPicker);
        } else if self.command_palette.is_active() {
            contexts.push(KeybindContext::CommandPalette);
        } else {
            if self.status == Status::Streaming {
                contexts.push(KeybindContext::Streaming);
            }
            contexts.push(KeybindContext::Editing);
        }
        contexts
    }
}

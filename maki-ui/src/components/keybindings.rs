use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Bind {
    pub code: KeyCode,
    pub modifiers: KeyModifiers,
}

impl Bind {
    pub const fn ctrl(c: char) -> Self {
        Self {
            code: KeyCode::Char(c),
            modifiers: KeyModifiers::CONTROL,
        }
    }

    pub fn matches(&self, key: KeyEvent) -> bool {
        key.code == self.code && key.modifiers.contains(self.modifiers)
    }

    #[cfg(test)]
    pub const fn to_key_event(self) -> KeyEvent {
        KeyEvent {
            code: self.code,
            modifiers: self.modifiers,
            kind: crossterm::event::KeyEventKind::Press,
            state: crossterm::event::KeyEventState::NONE,
        }
    }
}

pub mod key {
    use super::Bind;

    pub const QUIT: Bind = Bind::ctrl('c');
    pub const HELP: Bind = Bind::ctrl('h');
    pub const PREV_CHAT: Bind = Bind::ctrl('p');
    pub const NEXT_CHAT: Bind = Bind::ctrl('n');
    pub const SCROLL_HALF_UP: Bind = Bind::ctrl('u');
    pub const SCROLL_HALF_DOWN: Bind = Bind::ctrl('d');
    pub const SCROLL_LINE_UP: Bind = Bind::ctrl('y');
    pub const SCROLL_LINE_DOWN: Bind = Bind::ctrl('e');
    pub const SCROLL_TOP: Bind = Bind::ctrl('g');
    pub const SCROLL_BOTTOM: Bind = Bind::ctrl('b');
    pub const POP_QUEUE: Bind = Bind::ctrl('q');
    pub const DELETE_WORD: Bind = Bind::ctrl('w');
    pub const SEARCH: Bind = Bind::ctrl('f');
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeybindContext {
    General,
    Editing,
    Streaming,
    QuestionForm,
    TaskPicker,
    SessionPicker,
    RewindPicker,
    ThemePicker,
    ModelPicker,
    QueueFocus,
    CommandPalette,
    Search,
}

impl KeybindContext {
    pub const fn label(self) -> &'static str {
        match self {
            Self::General => "General",
            Self::Editing => "Editing",
            Self::Streaming => "While Streaming",
            Self::QuestionForm => "Question Form",
            Self::TaskPicker => "Task Picker",
            Self::SessionPicker => "Session Picker",
            Self::RewindPicker => "Rewind Picker",
            Self::ThemePicker => "Theme Picker",
            Self::ModelPicker => "Model Picker",
            Self::QueueFocus => "Queue Focus",
            Self::CommandPalette => "Command Palette",
            Self::Search => "Search",
        }
    }
}

pub struct Keybind {
    pub key: &'static str,
    pub description: &'static str,
    pub context: KeybindContext,
}

const KEYBINDS: &[Keybind] = &[
    Keybind {
        key: "Ctrl+C",
        description: "Quit / clear input",
        context: KeybindContext::General,
    },
    Keybind {
        key: "Ctrl+H",
        description: "Toggle keybindings",
        context: KeybindContext::General,
    },
    Keybind {
        key: "Ctrl+P/N",
        description: "Previous/next chat",
        context: KeybindContext::General,
    },
    Keybind {
        key: "Ctrl+F",
        description: "Search messages",
        context: KeybindContext::General,
    },
    Keybind {
        key: "Enter",
        description: "Submit prompt",
        context: KeybindContext::Editing,
    },
    Keybind {
        key: "\\+Enter",
        description: "Continue on next line",
        context: KeybindContext::Editing,
    },
    Keybind {
        key: "Tab",
        description: "Toggle mode (Build/Plan)",
        context: KeybindContext::Editing,
    },
    Keybind {
        key: "/command",
        description: "Open command palette",
        context: KeybindContext::Editing,
    },
    Keybind {
        key: "Ctrl+W",
        description: "Delete word before cursor",
        context: KeybindContext::Editing,
    },
    Keybind {
        key: "Ctrl+←/→",
        description: "Move word left/right",
        context: KeybindContext::Editing,
    },
    Keybind {
        key: "Ctrl+Del",
        description: "Delete word forward",
        context: KeybindContext::Editing,
    },
    Keybind {
        key: "Ctrl+K",
        description: "Kill to end of line",
        context: KeybindContext::Editing,
    },
    Keybind {
        key: "Ctrl+A",
        description: "Move to start of line",
        context: KeybindContext::Editing,
    },
    Keybind {
        key: "Ctrl+U/D",
        description: "Scroll half page up/down",
        context: KeybindContext::Editing,
    },
    Keybind {
        key: "Ctrl+Y/E",
        description: "Scroll one line up/down",
        context: KeybindContext::Editing,
    },
    Keybind {
        key: "Ctrl+G",
        description: "Scroll to top",
        context: KeybindContext::Editing,
    },
    Keybind {
        key: "Ctrl+B",
        description: "Scroll to bottom",
        context: KeybindContext::Editing,
    },
    Keybind {
        key: "Ctrl+Q",
        description: "Pop front of queue",
        context: KeybindContext::Editing,
    },
    Keybind {
        key: "↑/↓",
        description: "Scroll messages",
        context: KeybindContext::Streaming,
    },
    Keybind {
        key: "Esc Esc",
        description: "Cancel agent",
        context: KeybindContext::Streaming,
    },
    Keybind {
        key: "Esc Esc",
        description: "Rewind",
        context: KeybindContext::Editing,
    },
    Keybind {
        key: "↑/↓",
        description: "Select option",
        context: KeybindContext::QuestionForm,
    },
    Keybind {
        key: "Enter",
        description: "Confirm selection",
        context: KeybindContext::QuestionForm,
    },
    Keybind {
        key: "Esc",
        description: "Dismiss",
        context: KeybindContext::QuestionForm,
    },
    Keybind {
        key: "↑/↓",
        description: "Navigate tasks",
        context: KeybindContext::TaskPicker,
    },
    Keybind {
        key: "Enter",
        description: "Select task",
        context: KeybindContext::TaskPicker,
    },
    Keybind {
        key: "Esc",
        description: "Cancel",
        context: KeybindContext::TaskPicker,
    },
    Keybind {
        key: "Type",
        description: "Filter tasks",
        context: KeybindContext::TaskPicker,
    },
    Keybind {
        key: "\u{2191}/\u{2193}",
        description: "Navigate sessions",
        context: KeybindContext::SessionPicker,
    },
    Keybind {
        key: "Enter",
        description: "Load session",
        context: KeybindContext::SessionPicker,
    },
    Keybind {
        key: "Esc",
        description: "Close",
        context: KeybindContext::SessionPicker,
    },
    Keybind {
        key: "Type",
        description: "Filter sessions",
        context: KeybindContext::SessionPicker,
    },
    Keybind {
        key: "Ctrl+D",
        description: "Delete session",
        context: KeybindContext::SessionPicker,
    },
    Keybind {
        key: "\u{2191}/\u{2193}",
        description: "Navigate turns",
        context: KeybindContext::RewindPicker,
    },
    Keybind {
        key: "Enter",
        description: "Rewind to turn",
        context: KeybindContext::RewindPicker,
    },
    Keybind {
        key: "Esc",
        description: "Close",
        context: KeybindContext::RewindPicker,
    },
    Keybind {
        key: "Type",
        description: "Filter turns",
        context: KeybindContext::RewindPicker,
    },
    Keybind {
        key: "↑/↓",
        description: "Navigate themes",
        context: KeybindContext::ThemePicker,
    },
    Keybind {
        key: "Enter",
        description: "Select theme",
        context: KeybindContext::ThemePicker,
    },
    Keybind {
        key: "Esc",
        description: "Cancel",
        context: KeybindContext::ThemePicker,
    },
    Keybind {
        key: "Type",
        description: "Filter themes",
        context: KeybindContext::ThemePicker,
    },
    Keybind {
        key: "↑/↓",
        description: "Navigate models",
        context: KeybindContext::ModelPicker,
    },
    Keybind {
        key: "Enter",
        description: "Select model",
        context: KeybindContext::ModelPicker,
    },
    Keybind {
        key: "Esc",
        description: "Cancel",
        context: KeybindContext::ModelPicker,
    },
    Keybind {
        key: "Type",
        description: "Filter models",
        context: KeybindContext::ModelPicker,
    },
    Keybind {
        key: "↑/↓",
        description: "Navigate queue",
        context: KeybindContext::QueueFocus,
    },
    Keybind {
        key: "Enter",
        description: "Remove item",
        context: KeybindContext::QueueFocus,
    },
    Keybind {
        key: "Esc",
        description: "Exit queue focus",
        context: KeybindContext::QueueFocus,
    },
    Keybind {
        key: "↑/↓",
        description: "Navigate commands",
        context: KeybindContext::CommandPalette,
    },
    Keybind {
        key: "Enter",
        description: "Execute command",
        context: KeybindContext::CommandPalette,
    },
    Keybind {
        key: "Esc",
        description: "Close palette",
        context: KeybindContext::CommandPalette,
    },
    Keybind {
        key: "Tab",
        description: "Close and toggle mode",
        context: KeybindContext::CommandPalette,
    },
    Keybind {
        key: "Esc",
        description: "Close search",
        context: KeybindContext::Search,
    },
    Keybind {
        key: "↑/↓",
        description: "Navigate",
        context: KeybindContext::Search,
    },
    Keybind {
        key: "Enter",
        description: "Go to match",
        context: KeybindContext::Search,
    },
];

pub fn active_keybinds(contexts: &[KeybindContext]) -> Vec<&'static Keybind> {
    let mut binds: Vec<&'static Keybind> = KEYBINDS
        .iter()
        .filter(|kb| contexts.contains(&kb.context))
        .collect();
    binds.sort_by_key(|kb| contexts.iter().position(|c| *c == kb.context));
    binds
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn multiple_contexts_returns_union() {
        let binds = active_keybinds(&[KeybindContext::General, KeybindContext::Streaming]);
        let has_general = binds.iter().any(|kb| kb.context == KeybindContext::General);
        let has_streaming = binds
            .iter()
            .any(|kb| kb.context == KeybindContext::Streaming);
        assert!(has_general);
        assert!(has_streaming);
    }

    #[test]
    fn every_context_has_at_least_one_keybind() {
        let all_contexts = [
            KeybindContext::General,
            KeybindContext::Editing,
            KeybindContext::Streaming,
            KeybindContext::QuestionForm,
            KeybindContext::TaskPicker,
            KeybindContext::SessionPicker,
            KeybindContext::RewindPicker,
            KeybindContext::ThemePicker,
            KeybindContext::ModelPicker,
            KeybindContext::QueueFocus,
            KeybindContext::CommandPalette,
            KeybindContext::Search,
        ];
        for ctx in all_contexts {
            let binds = active_keybinds(&[ctx]);
            assert!(!binds.is_empty(), "context {:?} has no keybinds", ctx);
        }
    }

    #[test]
    fn no_duplicate_entries() {
        for (i, a) in KEYBINDS.iter().enumerate() {
            for (j, b) in KEYBINDS.iter().enumerate() {
                if i != j && a.context == b.context {
                    assert!(
                        a.key != b.key || a.description != b.description,
                        "duplicate keybind: {} - {} in {:?}",
                        a.key,
                        a.description,
                        a.context,
                    );
                }
            }
        }
    }
}

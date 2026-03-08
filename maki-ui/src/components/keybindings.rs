/// Central keybinding registry.
///
/// Keep in sync with: `App::handle_key`, `App::active_keybind_contexts`.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeybindContext {
    General,
    Editing,
    Streaming,
    QuestionForm,
    ChatPicker,
    QueueFocus,
    CommandPalette,
}

impl KeybindContext {
    pub const fn label(self) -> &'static str {
        match self {
            Self::General => "General",
            Self::Editing => "Editing",
            Self::Streaming => "While Streaming",
            Self::QuestionForm => "Question Form",
            Self::ChatPicker => "Chat Picker",
            Self::QueueFocus => "Queue Focus",
            Self::CommandPalette => "Command Palette",
        }
    }
}

pub struct Keybind {
    pub key: &'static str,
    pub description: &'static str,
    pub context: KeybindContext,
}

pub const KEYBINDS: &[Keybind] = &[
    // General
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
    // Editing
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
    // Streaming
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
    // QuestionForm
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
    // ChatPicker
    Keybind {
        key: "↑/↓",
        description: "Navigate chats",
        context: KeybindContext::ChatPicker,
    },
    Keybind {
        key: "Enter",
        description: "Select chat",
        context: KeybindContext::ChatPicker,
    },
    Keybind {
        key: "Esc",
        description: "Cancel",
        context: KeybindContext::ChatPicker,
    },
    Keybind {
        key: "Type",
        description: "Filter chats",
        context: KeybindContext::ChatPicker,
    },
    // QueueFocus
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
    // CommandPalette
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
];

pub fn active_keybinds(contexts: &[KeybindContext]) -> Vec<&'static Keybind> {
    KEYBINDS
        .iter()
        .filter(|kb| contexts.contains(&kb.context))
        .collect()
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
            KeybindContext::ChatPicker,
            KeybindContext::QueueFocus,
            KeybindContext::CommandPalette,
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

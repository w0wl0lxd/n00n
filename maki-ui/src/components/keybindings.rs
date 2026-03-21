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
    pub const OPEN_EDITOR: Bind = Bind::ctrl('o');
    pub const TODO_PANEL: Bind = Bind::ctrl('t');
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeybindContext {
    General,
    Editing,
    Streaming,
    Picker,
    FormInput,
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
            Self::Picker => "Pickers",
            Self::FormInput => "Form",
            Self::TaskPicker => "Task Picker",
            Self::SessionPicker => "Session Picker",
            Self::RewindPicker => "Rewind Picker",
            Self::ThemePicker => "Theme Picker",
            Self::ModelPicker => "Model Picker",
            Self::QueueFocus => "Queue",
            Self::CommandPalette => "Commands",
            Self::Search => "Search",
        }
    }

    pub const fn parent(self) -> Option<KeybindContext> {
        match self {
            Self::TaskPicker
            | Self::SessionPicker
            | Self::RewindPicker
            | Self::ThemePicker
            | Self::ModelPicker
            | Self::QueueFocus
            | Self::CommandPalette
            | Self::Search => Some(Self::Picker),
            _ => None,
        }
    }
}

pub struct Keybind {
    pub key: &'static str,
    pub description: &'static str,
    pub context: KeybindContext,
}

pub const KEYBINDS: &[Keybind] = &[
    Keybind {
        key: "Ctrl+C",
        description: "Quit / clear input",
        context: KeybindContext::General,
    },
    Keybind {
        key: "Ctrl+H",
        description: "Show keybindings",
        context: KeybindContext::General,
    },
    Keybind {
        key: "Ctrl+N/P",
        description: "Next / previous task chat",
        context: KeybindContext::General,
    },
    Keybind {
        key: "Ctrl+F",
        description: "Search messages",
        context: KeybindContext::General,
    },
    Keybind {
        key: "Ctrl+O",
        description: "Open plan in editor",
        context: KeybindContext::General,
    },
    Keybind {
        key: "Ctrl+T",
        description: "Toggle todo panel",
        context: KeybindContext::General,
    },
    Keybind {
        key: "Enter",
        description: "Submit prompt",
        context: KeybindContext::Editing,
    },
    Keybind {
        key: "\\+Enter",
        description: "Newline",
        context: KeybindContext::Editing,
    },
    Keybind {
        key: "Tab",
        description: "Toggle mode",
        context: KeybindContext::Editing,
    },
    Keybind {
        key: "/command",
        description: "Open command palette",
        context: KeybindContext::Editing,
    },
    Keybind {
        key: "Ctrl+W",
        description: "Delete word backward",
        context: KeybindContext::Editing,
    },
    Keybind {
        key: "Ctrl+←/→",
        description: "Move word left / right",
        context: KeybindContext::Editing,
    },
    Keybind {
        key: "Ctrl+Del",
        description: "Delete word forward",
        context: KeybindContext::Editing,
    },
    Keybind {
        key: "Ctrl+K",
        description: "Delete to end of line",
        context: KeybindContext::Editing,
    },
    Keybind {
        key: "Ctrl+A",
        description: "Jump to start of line",
        context: KeybindContext::Editing,
    },
    Keybind {
        key: "Ctrl+U/D",
        description: "Scroll half page up / down",
        context: KeybindContext::Editing,
    },
    Keybind {
        key: "Ctrl+Y/E",
        description: "Scroll one line up / down",
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
        description: "Pop queue",
        context: KeybindContext::Editing,
    },
    Keybind {
        key: "Esc Esc",
        description: "Rewind",
        context: KeybindContext::Editing,
    },
    Keybind {
        key: "↑/↓",
        description: "Navigate messages",
        context: KeybindContext::Streaming,
    },
    Keybind {
        key: "Esc Esc",
        description: "Cancel agent",
        context: KeybindContext::Streaming,
    },
    Keybind {
        key: "↑/↓",
        description: "Navigate options",
        context: KeybindContext::FormInput,
    },
    Keybind {
        key: "Enter",
        description: "Select option",
        context: KeybindContext::FormInput,
    },
    Keybind {
        key: "Esc",
        description: "Close",
        context: KeybindContext::FormInput,
    },
    Keybind {
        key: "↑/↓",
        description: "Navigate",
        context: KeybindContext::Picker,
    },
    Keybind {
        key: "Enter",
        description: "Select",
        context: KeybindContext::Picker,
    },
    Keybind {
        key: "Esc",
        description: "Close",
        context: KeybindContext::Picker,
    },
    Keybind {
        key: "Type",
        description: "Filter",
        context: KeybindContext::Picker,
    },
    Keybind {
        key: "Ctrl+D",
        description: "Delete session",
        context: KeybindContext::SessionPicker,
    },
    Keybind {
        key: "Enter",
        description: "Remove item",
        context: KeybindContext::QueueFocus,
    },
    Keybind {
        key: "Tab",
        description: "Toggle mode",
        context: KeybindContext::CommandPalette,
    },
];

pub const ALL_CONTEXTS: &[KeybindContext] = &[
    KeybindContext::General,
    KeybindContext::Editing,
    KeybindContext::Streaming,
    KeybindContext::Picker,
    KeybindContext::TaskPicker,
    KeybindContext::SessionPicker,
    KeybindContext::RewindPicker,
    KeybindContext::ThemePicker,
    KeybindContext::ModelPicker,
    KeybindContext::QueueFocus,
    KeybindContext::CommandPalette,
    KeybindContext::Search,
    KeybindContext::FormInput,
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_context_has_at_least_one_keybind() {
        for &ctx in ALL_CONTEXTS {
            let has_own = KEYBINDS.iter().any(|kb| kb.context == ctx);
            let has_parent = ctx
                .parent()
                .is_some_and(|p| KEYBINDS.iter().any(|kb| kb.context == p));
            assert!(
                has_own || has_parent,
                "context {:?} has no keybinds and no parent with keybinds",
                ctx,
            );
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

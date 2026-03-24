use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

macro_rules! mod_key {
    ($suffix:expr) => {
        if cfg!(target_os = "macos") {
            concat!("⌘", $suffix)
        } else {
            concat!("Ctrl+", $suffix)
        }
    };
}

macro_rules! upper {
    ('a') => {
        "A"
    };
    ('b') => {
        "B"
    };
    ('c') => {
        "C"
    };
    ('d') => {
        "D"
    };
    ('e') => {
        "E"
    };
    ('f') => {
        "F"
    };
    ('g') => {
        "G"
    };
    ('h') => {
        "H"
    };
    ('i') => {
        "I"
    };
    ('j') => {
        "J"
    };
    ('k') => {
        "K"
    };
    ('l') => {
        "L"
    };
    ('m') => {
        "M"
    };
    ('n') => {
        "N"
    };
    ('o') => {
        "O"
    };
    ('p') => {
        "P"
    };
    ('q') => {
        "Q"
    };
    ('r') => {
        "R"
    };
    ('s') => {
        "S"
    };
    ('t') => {
        "T"
    };
    ('u') => {
        "U"
    };
    ('v') => {
        "V"
    };
    ('w') => {
        "W"
    };
    ('x') => {
        "X"
    };
    ('y') => {
        "Y"
    };
    ('z') => {
        "Z"
    };
}

macro_rules! ctrl_bind {
    ($char:tt) => {
        Bind {
            code: KeyCode::Char($char),
            modifiers: KeyModifiers::CONTROL,
            label: mod_key!(upper!($char)),
        }
    };
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Bind {
    pub code: KeyCode,
    pub modifiers: KeyModifiers,
    pub label: &'static str,
}

impl Bind {
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
    use crossterm::event::{KeyCode, KeyModifiers};

    pub const QUIT: Bind = ctrl_bind!('c');
    pub const HELP: Bind = ctrl_bind!('h');
    pub const PREV_CHAT: Bind = ctrl_bind!('p');
    pub const NEXT_CHAT: Bind = ctrl_bind!('n');
    pub const SCROLL_HALF_UP: Bind = ctrl_bind!('u');
    pub const SCROLL_HALF_DOWN: Bind = ctrl_bind!('d');
    pub const SCROLL_LINE_UP: Bind = ctrl_bind!('y');
    pub const SCROLL_LINE_DOWN: Bind = ctrl_bind!('e');
    pub const SCROLL_TOP: Bind = ctrl_bind!('g');
    pub const SCROLL_BOTTOM: Bind = ctrl_bind!('b');
    pub const POP_QUEUE: Bind = ctrl_bind!('q');
    pub const DELETE_WORD: Bind = ctrl_bind!('w');
    pub const SEARCH: Bind = ctrl_bind!('f');
    pub const OPEN_EDITOR: Bind = ctrl_bind!('o');
    pub const TODO_PANEL: Bind = ctrl_bind!('t');
    pub const DELETE: Bind = ctrl_bind!('d');
    pub const KILL_LINE: Bind = ctrl_bind!('k');
    pub const LINE_START: Bind = ctrl_bind!('a');

    pub const NEXT_PREV_CHAT_LABEL: &str = mod_key!("N/P");
    pub const SCROLL_HALF_LABEL: &str = mod_key!("U/D");
    pub const SCROLL_LINE_LABEL: &str = mod_key!("Y/E");
    pub const WORD_ARROWS_LABEL: &str = mod_key!("←/→");
    pub const WORD_DEL_LABEL: &str = mod_key!("Del");
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
        key: key::QUIT.label,
        description: "Quit / clear input",
        context: KeybindContext::General,
    },
    Keybind {
        key: key::HELP.label,
        description: "Show keybindings",
        context: KeybindContext::General,
    },
    Keybind {
        key: key::NEXT_PREV_CHAT_LABEL,
        description: "Next / previous task chat",
        context: KeybindContext::General,
    },
    Keybind {
        key: key::SEARCH.label,
        description: "Search messages",
        context: KeybindContext::General,
    },
    Keybind {
        key: key::OPEN_EDITOR.label,
        description: "Open plan in editor",
        context: KeybindContext::General,
    },
    Keybind {
        key: key::TODO_PANEL.label,
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
        key: key::DELETE_WORD.label,
        description: "Delete word backward",
        context: KeybindContext::Editing,
    },
    Keybind {
        key: key::WORD_ARROWS_LABEL,
        description: "Move word left / right",
        context: KeybindContext::Editing,
    },
    Keybind {
        key: key::WORD_DEL_LABEL,
        description: "Delete word forward",
        context: KeybindContext::Editing,
    },
    Keybind {
        key: key::KILL_LINE.label,
        description: "Delete to end of line",
        context: KeybindContext::Editing,
    },
    Keybind {
        key: key::LINE_START.label,
        description: "Jump to start of line",
        context: KeybindContext::Editing,
    },
    Keybind {
        key: key::SCROLL_HALF_LABEL,
        description: "Scroll half page up / down",
        context: KeybindContext::Editing,
    },
    Keybind {
        key: key::SCROLL_LINE_LABEL,
        description: "Scroll one line up / down",
        context: KeybindContext::Editing,
    },
    Keybind {
        key: key::SCROLL_TOP.label,
        description: "Scroll to top",
        context: KeybindContext::Editing,
    },
    Keybind {
        key: key::SCROLL_BOTTOM.label,
        description: "Scroll to bottom",
        context: KeybindContext::Editing,
    },
    Keybind {
        key: key::POP_QUEUE.label,
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
        description: "Navigate input history",
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
        key: key::DELETE.label,
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

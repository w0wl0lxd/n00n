use crate::theme;

use crossterm::event::{KeyCode, KeyEvent};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Clear, Paragraph};

struct Command {
    name: &'static str,
    description: &'static str,
}

const COMMANDS: &[Command] = &[
    Command {
        name: "/chats",
        description: "Browse and search chats",
    },
    Command {
        name: "/compact",
        description: "Summarize and compact conversation history",
    },
    Command {
        name: "/new",
        description: "Start a new session",
    },
    Command {
        name: "/help",
        description: "Show keybindings",
    },
    Command {
        name: "/queue",
        description: "Remove items from queue",
    },
    Command {
        name: "/sessions",
        description: "Browse and switch sessions",
    },
    Command {
        name: "/model",
        description: "Switch model",
    },
    Command {
        name: "/theme",
        description: "Switch color theme",
    },
    Command {
        name: "/exit",
        description: "Exit the application",
    },
];

pub enum CommandAction {
    Consumed,
    Execute(&'static str),
    Close,
    Passthrough,
}

pub struct CommandPalette {
    selected: usize,
    filtered: Vec<usize>,
}

impl CommandPalette {
    pub fn new() -> Self {
        Self {
            selected: 0,
            filtered: Vec::new(),
        }
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> CommandAction {
        if !self.is_active() {
            return CommandAction::Passthrough;
        }
        match key.code {
            KeyCode::Up => {
                self.move_up();
                CommandAction::Consumed
            }
            KeyCode::Down => {
                self.move_down();
                CommandAction::Consumed
            }
            KeyCode::Esc => {
                self.close();
                CommandAction::Consumed
            }
            KeyCode::Enter => match self.confirm() {
                Some(name) => {
                    self.close();
                    CommandAction::Execute(name)
                }
                None => CommandAction::Consumed,
            },
            KeyCode::Tab => {
                self.close();
                CommandAction::Close
            }
            _ => CommandAction::Passthrough,
        }
    }

    pub fn is_active(&self) -> bool {
        !self.filtered.is_empty()
    }

    pub fn sync(&mut self, input: &str) {
        let Some(prefix) = input.strip_prefix('/') else {
            self.filtered.clear();
            return;
        };
        let prefix_lower = prefix.to_ascii_lowercase();
        self.filtered = COMMANDS
            .iter()
            .enumerate()
            .filter(|(_, cmd)| {
                cmd.name[1..]
                    .to_ascii_lowercase()
                    .starts_with(&prefix_lower)
            })
            .map(|(i, _)| i)
            .collect();
        self.selected = self.selected.min(self.filtered.len().saturating_sub(1));
    }

    pub fn close(&mut self) {
        self.filtered.clear();
    }

    pub fn move_up(&mut self) {
        if self.filtered.is_empty() {
            return;
        }
        self.selected = if self.selected == 0 {
            self.filtered.len() - 1
        } else {
            self.selected - 1
        };
    }

    pub fn move_down(&mut self) {
        if self.filtered.is_empty() {
            return;
        }
        self.selected = if self.selected == self.filtered.len() - 1 {
            0
        } else {
            self.selected + 1
        };
    }

    pub fn confirm(&self) -> Option<&'static str> {
        self.filtered.get(self.selected).map(|&i| COMMANDS[i].name)
    }

    pub fn view(&self, frame: &mut Frame, input_area: Rect) -> Option<Rect> {
        let filtered = &self.filtered;
        if filtered.is_empty() {
            return None;
        }

        let popup_height = (filtered.len() as u16).min(input_area.y);
        if popup_height == 0 {
            return None;
        }

        const GAP: usize = 2;
        let max_name = filtered
            .iter()
            .map(|&i| COMMANDS[i].name.len())
            .max()
            .unwrap_or(0);
        let max_desc = filtered
            .iter()
            .map(|&i| COMMANDS[i].description.len())
            .max()
            .unwrap_or(0);
        const PAD: usize = 1;
        let popup_width = (PAD + max_name + GAP + max_desc + PAD) as u16;

        let popup = Rect {
            x: input_area.x,
            y: input_area.y.saturating_sub(popup_height),
            width: popup_width.min(input_area.width),
            height: popup_height,
        };

        let lines: Vec<Line> = filtered
            .iter()
            .enumerate()
            .map(|(i, &cmd_idx)| {
                let cmd = &COMMANDS[cmd_idx];
                let selected = i == self.selected;
                let name_pad = max_name - cmd.name.len() + GAP;
                if selected {
                    let s = theme::current().cmd_selected;
                    Line::from(vec![
                        Span::styled(" ".repeat(PAD), s),
                        Span::styled(cmd.name, s),
                        Span::styled(" ".repeat(name_pad), s),
                        Span::styled(cmd.description, s),
                        Span::styled(" ".repeat(PAD), s),
                    ])
                } else {
                    Line::from(vec![
                        Span::raw(" ".repeat(PAD)),
                        Span::styled(cmd.name, theme::current().cmd_name),
                        Span::raw(" ".repeat(name_pad)),
                        Span::styled(cmd.description, theme::current().cmd_desc),
                        Span::raw(" ".repeat(PAD)),
                    ])
                }
            })
            .collect();

        frame.render_widget(Clear, popup);
        frame.render_widget(
            Paragraph::new(lines).style(Style::new().bg(theme::current().background)),
            popup,
        );

        Some(popup)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use test_case::test_case;

    fn synced(input: &str) -> CommandPalette {
        let mut p = CommandPalette::new();
        p.sync(input);
        p
    }

    #[test]
    fn slash_shows_all_commands() {
        let p = synced("/");
        assert!(p.is_active());
        assert_eq!(p.filtered.len(), COMMANDS.len());
    }

    #[test]
    fn close_deactivates() {
        let mut p = synced("/");
        p.close();
        assert!(!p.is_active());
    }

    #[test_case("/co", true ; "compact_prefix")]
    #[test_case("/ne", true ; "lowercase_prefix")]
    #[test_case("/NE", true ; "uppercase_prefix")]
    #[test_case("/zzz", false ; "no_match")]
    fn filter_by_prefix(input: &str, expect_active: bool) {
        let p = synced(input);
        assert_eq!(p.is_active(), expect_active);
    }

    #[test]
    fn navigation_wraps() {
        let mut p = synced("/");
        p.move_up();
        assert_eq!(p.selected, p.filtered.len() - 1);
        p.move_down();
        assert_eq!(p.selected, 0);
    }

    #[test]
    fn confirm_when_inactive_returns_none() {
        let p = CommandPalette::new();
        assert_eq!(p.confirm(), None);
    }

    #[test]
    fn sync_clamps_selected() {
        let mut p = synced("/");
        p.selected = 100;
        p.sync("/");
        assert_eq!(p.selected, p.filtered.len() - 1);
    }
}

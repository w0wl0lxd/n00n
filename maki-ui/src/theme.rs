use ratatui::style::{Color, Modifier, Style};

pub const BACKGROUND: Color = Color::Rgb(0x28, 0x2a, 0x36);
pub const BACKGROUND_2: Color = Color::Rgb(0x22, 0x24, 0x30);
pub const FOREGROUND: Color = Color::Rgb(0xf8, 0xf8, 0xf2);
pub const COMMENT: Color = Color::Rgb(0x62, 0x72, 0xa4);
pub const CYAN: Color = Color::Rgb(0x8b, 0xe9, 0xfd);
pub const GREEN: Color = Color::Rgb(0x50, 0xfa, 0x7b);
pub const ORANGE: Color = Color::Rgb(0xff, 0xb8, 0x6c);
pub const PINK: Color = Color::Rgb(0xff, 0x79, 0xc6);
pub const PURPLE: Color = Color::Rgb(0xbd, 0x93, 0xf9);
pub const RED: Color = Color::Rgb(0xff, 0x55, 0x55);
pub const YELLOW: Color = Color::Rgb(0xf1, 0xfa, 0x8c);

pub const USER: Style = Style::new().fg(CYAN);
pub const ASSISTANT: Style = Style::new().fg(FOREGROUND);
pub const THINKING: Style = Style::new().fg(COMMENT).add_modifier(Modifier::ITALIC);
pub const TOOL_BG: Style = Style::new().bg(BACKGROUND_2);
pub const TOOL: Style = Style::new().fg(FOREGROUND);
pub const TOOL_IN_PROGRESS: Style = Style::new().fg(FOREGROUND);
pub const TOOL_SUCCESS: Style = Style::new().fg(GREEN);
pub const TOOL_ERROR: Style = Style::new().fg(RED);
pub const CURSOR: Style = Style::new()
    .fg(FOREGROUND)
    .add_modifier(Modifier::SLOW_BLINK);
pub const ERROR: Style = Style::new().fg(RED);

pub const STATUS_IDLE: Style = Style::new().fg(COMMENT);
pub const STATUS_STREAMING: Style = Style::new().fg(YELLOW);
pub const MODE_BUILD: Style = Style::new().fg(GREEN).add_modifier(Modifier::BOLD);
pub const MODE_PLAN: Style = Style::new().fg(PURPLE).add_modifier(Modifier::BOLD);
pub const CANCEL_HINT: Style = Style::new().fg(ORANGE);

pub const BOLD: Style = Style::new().fg(CYAN).add_modifier(Modifier::BOLD);
pub const INLINE_CODE: Style = Style::new().fg(PINK);
pub const CODE_FALLBACK: Style = Style::new().fg(PURPLE);

pub const INPUT_BORDER: Color = COMMENT;

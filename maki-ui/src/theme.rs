use ratatui::style::{Color, Modifier, Style};
use ratatui::text::Line;

pub const BACKGROUND: Color = Color::Rgb(0x28, 0x2a, 0x36);
pub const BACKGROUND_2: Color = Color::Rgb(0x22, 0x24, 0x30);
pub const FOREGROUND: Color = Color::Rgb(0xf8, 0xf8, 0xf2);
pub const FOREGROUND_STYLE: Style = Style::new().fg(FOREGROUND);
pub const COMMENT: Color = Color::Rgb(0x62, 0x72, 0xa4);
pub const COMMENT_LIGHTER: Color = Color::Rgb(0x91, 0x9c, 0xbf);
pub const COMMENT_STYLE: Style = Style::new().fg(COMMENT);
pub const CYAN: Color = Color::Rgb(0x8b, 0xe9, 0xfd);
pub const GREEN: Color = Color::Rgb(0x50, 0xfa, 0x7b);
pub const ORANGE: Color = Color::Rgb(0xff, 0xb8, 0x6c);
pub const PINK: Color = Color::Rgb(0xff, 0x79, 0xc6);
pub const PURPLE: Color = Color::Rgb(0xbd, 0x93, 0xf9);
pub const RED: Color = Color::Rgb(0xff, 0x55, 0x55);
pub const YELLOW: Color = Color::Rgb(0xf1, 0xfa, 0x8c);

pub const USER: Style = Style::new().fg(CYAN);
pub const ASSISTANT: Style = Style::new().fg(FOREGROUND);
pub const ASSISTANT_PREFIX: Style = Style::new().fg(PINK);
pub const THINKING: Style = Style::new()
    .fg(COMMENT_LIGHTER)
    .add_modifier(Modifier::ITALIC);
pub const TOOL_BG: Style = Style::new().bg(BACKGROUND_2);
pub const TOOL: Style = Style::new().fg(FOREGROUND);
pub const TOOL_PATH: Style = Style::new().fg(ORANGE);
pub const TOOL_ANNOTATION: Style = Style::new().fg(COMMENT);
pub const TOOL_PREFIX: Style = Style::new().fg(FOREGROUND).add_modifier(Modifier::BOLD);
pub const TOOL_IN_PROGRESS: Style = Style::new().fg(FOREGROUND);
pub const TOOL_SUCCESS: Style = Style::new().fg(GREEN);
pub const TOOL_ERROR: Style = Style::new().fg(RED);
pub const TOOL_DIM: Style = Style::new().fg(COMMENT);
pub const ERROR: Style = Style::new().fg(RED);

pub const STATUS_IDLE: Style = Style::new().fg(COMMENT);
pub const STATUS_GLOBAL_COST: Style = Style::new().fg(FOREGROUND).add_modifier(Modifier::BOLD);
pub const STATUS_CONTEXT: Style = Style::new().fg(FOREGROUND);
pub const STATUS_STREAMING: Style = Style::new().fg(YELLOW);
pub const CANCEL_HINT: Style = Style::new().fg(ORANGE);

pub const BOLD: Style = Style::new().fg(ORANGE).add_modifier(Modifier::BOLD);
pub const INLINE_CODE: Style = Style::new().fg(GREEN);
pub const CODE_FALLBACK: Style = Style::new().fg(FOREGROUND);
pub const CODE_BAR_STYLE: Style = Style::new().fg(ORANGE);
pub const STRIKETHROUGH: Style = Style::new().fg(COMMENT).add_modifier(Modifier::CROSSED_OUT);
pub const HEADING: Style = Style::new().fg(CYAN).add_modifier(Modifier::BOLD);
pub const LIST_MARKER: Style = Style::new().fg(CYAN);
pub const HORIZONTAL_RULE: Style = Style::new().fg(COMMENT);
pub const PLAN_RULE: Style = Style::new().fg(PINK).add_modifier(Modifier::BOLD);
pub const TABLE_BORDER: Style = Style::new().fg(COMMENT);

const DIFF_OLD_BG: Color = Color::Rgb(0x4D, 0x1F, 0x1F);
const DIFF_NEW_BG: Color = Color::Rgb(0x1F, 0x3D, 0x1F);
const DIFF_OLD_EMPHASIS_BG: Color = Color::Rgb(0x6B, 0x2E, 0x2E);
const DIFF_NEW_EMPHASIS_BG: Color = Color::Rgb(0x2E, 0x5C, 0x2E);

pub const DIFF_OLD: Style = Style::new().bg(DIFF_OLD_BG);
pub const DIFF_NEW: Style = Style::new().bg(DIFF_NEW_BG);
pub const DIFF_OLD_EMPHASIS: Style = Style::new().bg(DIFF_OLD_EMPHASIS_BG);
pub const DIFF_NEW_EMPHASIS: Style = Style::new().bg(DIFF_NEW_EMPHASIS_BG);

pub const DIFF_LINE_NR: Style = Style::new().fg(COMMENT);
pub const TODO_COMPLETED: Style = Style::new().fg(GREEN);
pub const TODO_IN_PROGRESS: Style = Style::new().fg(YELLOW);
pub const TODO_PENDING: Style = Style::new().fg(FOREGROUND);
pub const TODO_CANCELLED: Style = Style::new().fg(COMMENT);
pub const QUESTION_LABEL: Style = Style::new().fg(CYAN);
pub const QUESTION_ANSWER: Style = Style::new().fg(GREEN);

pub const CMD_SELECTED: Style = Style::new().fg(BACKGROUND).bg(PURPLE);
pub const CMD_NAME: Style = Style::new().fg(FOREGROUND);
pub const CMD_DESC: Style = Style::new().fg(COMMENT_LIGHTER);

pub const PANEL_BORDER: Style = Style::new().fg(PURPLE);
pub const PANEL_TITLE: Style = Style::new().fg(PURPLE).add_modifier(Modifier::BOLD);
pub const PICKER_SEARCH_PREFIX: Style = Style::new().fg(COMMENT_LIGHTER);
pub const CURSOR: Style = Style::new().fg(BACKGROUND).bg(FOREGROUND);

pub const INPUT_BORDER: Color = COMMENT;

pub const KEYBIND_KEY: Style = Style::new().fg(CYAN).add_modifier(Modifier::BOLD);
pub const KEYBIND_SECTION: Style = Style::new().fg(COMMENT_LIGHTER);

const fn midpoint(a: u8, b: u8) -> u8 {
    (a as u16 / 2 + b as u16 / 2) as u8
}

fn dim_style(style: Style) -> Style {
    let Color::Rgb(br, bg, bb) = BACKGROUND else {
        return style;
    };
    match style.fg {
        Some(Color::Rgb(r, g, b)) => style.fg(Color::Rgb(
            midpoint(r, br),
            midpoint(g, bg),
            midpoint(b, bb),
        )),
        _ => style,
    }
}

pub fn dim_lines(lines: &mut [Line<'_>]) {
    for line in lines {
        for span in &mut line.spans {
            span.style = dim_style(span.style);
        }
    }
}

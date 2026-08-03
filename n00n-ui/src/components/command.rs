use std::borrow::Cow;
use std::collections::HashSet;
use std::mem;
use std::sync::Arc;

use crossterm::event::{KeyCode, KeyEvent};
use n00n_agent::command::CustomCommand;
use n00n_agent::{McpPromptInfo, McpSnapshotReader};
use n00n_lua::{LuaCommandInfo, LuaCommandReader};
use nucleo::pattern::{CaseMatching, Normalization};
use nucleo::{Config, Matcher, Nucleo, Utf32String};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Clear, Paragraph};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::cast;
use crate::theme;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CommandCategory {
    Session,
    Model,
    View,
    Settings,
    Mode,
    Action,
}

impl CommandCategory {
    pub const ALL: [Self; 6] = [
        Self::Session,
        Self::Model,
        Self::View,
        Self::Settings,
        Self::Mode,
        Self::Action,
    ];

    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Session => "Session",
            Self::Model => "Model",
            Self::View => "View",
            Self::Settings => "Settings",
            Self::Mode => "Mode",
            Self::Action => "Action",
        }
    }
}

pub struct BuiltinCommand {
    pub name: &'static str,
    pub dispatch_name: &'static str,
    pub aliases: &'static [&'static str],
    pub category: CommandCategory,
    pub description: &'static str,
    pub max_args: usize,
}

macro_rules! command {
    ($name:literal, $dispatch:literal, [$($alias:literal),*], $category:ident, $description:literal, $max:expr) => {
        BuiltinCommand {
            name: $name,
            dispatch_name: $dispatch,
            aliases: &[$($alias),*],
            category: CommandCategory::$category,
            description: $description,
            max_args: $max,
        }
    };
}

pub const BUILTIN_COMMANDS: &[BuiltinCommand] = &[
    command!(
        "/session:new",
        "/new",
        ["/new"],
        Session,
        "Start a new session",
        0
    ),
    command!(
        "/session:list",
        "/sessions",
        ["/sessions"],
        Session,
        "Browse and switch sessions",
        0
    ),
    command!(
        "/session:rename",
        "/rename",
        ["/rename"],
        Session,
        "Rename the current session",
        1
    ),
    command!(
        "/session:fork",
        "/fork",
        ["/fork"],
        Session,
        "Fork the current session",
        0
    ),
    command!(
        "/model:pick",
        "/model",
        ["/model"],
        Model,
        "Switch model",
        0
    ),
    command!(
        "/view:tasks",
        "/tasks",
        ["/tasks"],
        View,
        "View running and completed work",
        0
    ),
    command!(
        "/view:usage",
        "/usage",
        ["/usage"],
        View,
        "View token usage",
        0
    ),
    command!(
        "/view:memory",
        "/memory",
        ["/memory"],
        View,
        "View and edit persistent notes",
        0
    ),
    command!(
        "/settings:theme",
        "/theme",
        ["/theme"],
        Settings,
        "Switch color theme",
        0
    ),
    command!(
        "/settings:mcp",
        "/mcp",
        ["/mcp"],
        Settings,
        "Configure MCP servers",
        0
    ),
    command!(
        "/settings:login",
        "/login",
        ["/login"],
        Settings,
        "Authenticate with a provider",
        0
    ),
    command!(
        "/mode:no-confirm",
        "/yolo",
        ["/yolo"],
        Mode,
        "Toggle permission confirmations",
        0
    ),
    command!(
        "/mode:fast",
        "/fast",
        ["/fast"],
        Mode,
        "Toggle fast mode when supported",
        0
    ),
    command!(
        "/mode:workflow",
        "/workflow",
        ["/workflow"],
        Mode,
        "Toggle workflow mode",
        0
    ),
    command!(
        "/mode:thinking",
        "/thinking",
        ["/thinking"],
        Mode,
        "Set thinking level",
        1
    ),
    command!(
        "/action:compact",
        "/compact",
        ["/compact", "/session:compact"],
        Action,
        "Compact conversation history",
        0
    ),
    command!(
        "/action:queue",
        "/queue",
        ["/queue"],
        Action,
        "Manage queued prompts",
        0
    ),
    command!(
        "/action:cd",
        "/cd",
        ["/cd"],
        Action,
        "Change working directory",
        1
    ),
    command!(
        "/action:ask",
        "/btw",
        ["/btw"],
        Action,
        "Ask a quick question without tools",
        usize::MAX
    ),
    command!(
        "/action:help",
        "/help",
        ["/help"],
        Action,
        "Show context-aware help",
        0
    ),
    command!(
        "/action:reload",
        "/reload",
        ["/reload", "/session:reload"],
        Action,
        "Reload plugins and configuration",
        0
    ),
    command!(
        "/action:exit",
        "/exit",
        ["/exit", "/session:exit"],
        Action,
        "Exit n00n",
        0
    ),
    command!(
        "/welcome",
        "/welcome",
        [],
        Action,
        "Show the welcome guide",
        0
    ),
];

fn builtin_command(name: &str) -> Option<&'static BuiltinCommand> {
    BUILTIN_COMMANDS.iter().find(|command| {
        command.name == name || command.dispatch_name == name || command.aliases.contains(&name)
    })
}

pub(crate) fn builtin_dispatch_name(name: &str) -> Option<&'static str> {
    builtin_command(name).map(|command| command.dispatch_name)
}

pub(crate) fn builtin_canonical_name(name: &str) -> Option<&'static str> {
    builtin_command(name).map(|command| command.name)
}

fn conflicts_with_builtin(name: &str) -> bool {
    builtin_dispatch_name(name).is_some()
}

pub struct ParsedCommand {
    pub name: String,
    pub args: String,
}

pub enum CommandAction {
    Consumed,
    Execute(ParsedCommand),
    Complete(String),
    Passthrough,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CommandGroup {
    Session,
    Model,
    View,
    Settings,
    Mode,
    Action,
    Custom,
    Mcp,
    Lua,
}

impl CommandGroup {
    const fn label(self) -> &'static str {
        match self {
            Self::Session => "Session",
            Self::Model => "Model",
            Self::View => "View",
            Self::Settings => "Settings",
            Self::Mode => "Mode",
            Self::Action => "Action",
            Self::Custom => "Custom",
            Self::Mcp => "MCP prompts",
            Self::Lua => "Plugins",
        }
    }
}

#[derive(Clone)]
enum CommandType {
    Builtin(&'static BuiltinCommand),
    Custom(usize),
    McpPrompt(usize),
    Lua(usize),
}

impl CommandType {
    const fn group(&self) -> CommandGroup {
        match self {
            Self::Builtin(command) => match command.category {
                CommandCategory::Session => CommandGroup::Session,
                CommandCategory::Model => CommandGroup::Model,
                CommandCategory::View => CommandGroup::View,
                CommandCategory::Settings => CommandGroup::Settings,
                CommandCategory::Mode => CommandGroup::Mode,
                CommandCategory::Action => CommandGroup::Action,
            },
            Self::Custom(_) => CommandGroup::Custom,
            Self::McpPrompt(_) => CommandGroup::Mcp,
            Self::Lua(_) => CommandGroup::Lua,
        }
    }
}

struct CommandItem {
    search_text: String,
    max_args: usize,
    command_type: CommandType,
}

struct Match {
    command_type: CommandType,
    indices: Vec<u32>,
}

enum PaletteRow {
    Header(CommandGroup),
    Item(usize),
}

pub struct CommandPalette {
    selected: usize,
    filtered: Vec<Match>,
    custom: Arc<[CustomCommand]>,
    mcp_reader: McpSnapshotReader,
    mcp_prompts: Vec<McpPromptInfo>,
    mcp_generation: u64,
    lua_reader: LuaCommandReader,
    lua_commands: Vec<LuaCommandInfo>,
    lua_generation: u64,
    nucleo: Nucleo<CommandItem>,
    matcher: Matcher,
    current_arg_count: usize,
}

fn span_style(kind: Option<(bool, bool)>, base: Style, alias_base: Style) -> Style {
    let (is_alias, matched) = kind.unwrap_or_else(|| (false, false));
    let style = if is_alias { alias_base } else { base };
    if matched {
        let t = theme::current();
        style
            .fg(t.accent.fg.or(style.fg).unwrap_or_else(|| Color::Reset))
            .add_modifier(Modifier::BOLD)
    } else {
        style
    }
}

fn truncate_to_width(text: &str, max_width: usize) -> String {
    let width = UnicodeWidthStr::width(text);
    if width <= max_width {
        return text.to_owned();
    }
    if max_width == 0 {
        return String::new();
    }

    let limit = max_width.saturating_sub(1);
    let mut output = String::new();
    let mut used = 0;
    for ch in text.chars() {
        let char_width = ch.width().unwrap_or_else(|| 0);
        if used + char_width > limit {
            break;
        }
        used += char_width;
        output.push(ch);
    }
    output.push('…');
    output
}

impl CommandPalette {
    pub fn new(
        custom_commands: Arc<[CustomCommand]>,
        mcp_reader: McpSnapshotReader,
        lua_reader: LuaCommandReader,
    ) -> Self {
        let snap = mcp_reader.load();
        let mcp_generation = snap.generation;
        let prompts = snap.prompts.clone();

        let lua_snap = lua_reader.load();
        let lua_generation = lua_snap.generation;
        let lua_commands = lua_snap.commands.clone();

        let nucleo = Self::build_nucleo(&custom_commands, &prompts, &lua_commands);
        Self {
            selected: 0,
            filtered: Vec::new(),
            custom: custom_commands,
            mcp_reader,
            mcp_prompts: prompts,
            mcp_generation,
            lua_reader,
            lua_commands,
            lua_generation,
            nucleo,
            matcher: Matcher::new(Config::DEFAULT),
            current_arg_count: 0,
        }
    }

    fn build_nucleo(
        custom_commands: &[CustomCommand],
        mcp_prompts: &[McpPromptInfo],
        lua_commands: &[LuaCommandInfo],
    ) -> Nucleo<CommandItem> {
        let nucleo = Nucleo::new(Config::DEFAULT, Arc::new(|| {}), None, 1);
        let injector = nucleo.injector();
        let mut reserved = HashSet::new();

        for cmd in BUILTIN_COMMANDS {
            let aliases = cmd.aliases.join(" ");
            let search_text = if aliases.is_empty() {
                cmd.name.to_owned()
            } else {
                format!("{} {aliases}", cmd.name)
            };
            let item = CommandItem {
                search_text,
                max_args: cmd.max_args,
                command_type: CommandType::Builtin(cmd),
            };
            injector.push(item, |item, cols| {
                cols[0] = Utf32String::from(item.search_text.as_str());
            });
        }

        for (i, cmd) in custom_commands.iter().enumerate() {
            let name = cmd.display_name();
            if conflicts_with_builtin(&name) || !reserved.insert(name.clone()) {
                continue;
            }
            let item = CommandItem {
                search_text: name,
                max_args: if cmd.has_args() { usize::MAX } else { 0 },
                command_type: CommandType::Custom(i),
            };
            injector.push(item, |item, cols| {
                cols[0] = Utf32String::from(item.search_text.as_str());
            });
        }

        for (i, prompt) in mcp_prompts.iter().enumerate() {
            let name = format!("/{}", prompt.display_name);
            if conflicts_with_builtin(&name) || !reserved.insert(name.clone()) {
                continue;
            }
            let item = CommandItem {
                search_text: name,
                max_args: if prompt.arguments.is_empty() {
                    0
                } else {
                    usize::MAX
                },
                command_type: CommandType::McpPrompt(i),
            };
            injector.push(item, |item, cols| {
                cols[0] = Utf32String::from(item.search_text.as_str());
            });
        }

        for (i, cmd) in lua_commands.iter().enumerate() {
            if conflicts_with_builtin(&cmd.name) || !reserved.insert(cmd.name.to_string()) {
                continue;
            }
            let item = CommandItem {
                search_text: cmd.name.to_string(),
                max_args: cmd.max_args,
                command_type: CommandType::Lua(i),
            };
            injector.push(item, |item, cols| {
                cols[0] = Utf32String::from(item.search_text.as_str());
            });
        }

        nucleo
    }

    pub fn handle_key(&mut self, key: KeyEvent, input: &str) -> CommandAction {
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
            KeyCode::Enter => match self.confirm(input) {
                Some(cmd) => {
                    self.close();
                    CommandAction::Execute(cmd)
                }
                None => CommandAction::Consumed,
            },
            KeyCode::Tab => {
                if let Some(item) = self.filtered.get(self.selected) {
                    let name = self.item_name(item);
                    let text = if self.item_has_args(item) {
                        format!("{name} ")
                    } else {
                        name
                    };
                    CommandAction::Complete(text)
                } else {
                    CommandAction::Consumed
                }
            }
            _ => CommandAction::Passthrough,
        }
    }

    pub fn is_active(&self) -> bool {
        !self.filtered.is_empty()
    }

    pub fn sync(&mut self, input: &str) {
        let mcp_snap = self.mcp_reader.load();
        let lua_snap = self.lua_reader.load();
        if mcp_snap.generation != self.mcp_generation || lua_snap.generation != self.lua_generation
        {
            self.mcp_generation = mcp_snap.generation;
            self.mcp_prompts.clone_from(&mcp_snap.prompts);
            self.lua_generation = lua_snap.generation;
            self.lua_commands.clone_from(&lua_snap.commands);
            self.nucleo = Self::build_nucleo(&self.custom, &self.mcp_prompts, &self.lua_commands);
        }
        let Some(stripped) = input.strip_prefix('/') else {
            self.filtered.clear();
            self.current_arg_count = 0;
            return;
        };

        let parts: Vec<&str> = stripped.split_whitespace().collect();
        let cmd_word = parts.first().copied().map_or_else(|| stripped, |w| w);
        let trailing_space = stripped.ends_with(char::is_whitespace);

        self.current_arg_count = if trailing_space {
            parts.len()
        } else {
            parts.len().saturating_sub(1)
        };

        self.nucleo.pattern.reparse(
            0,
            cmd_word,
            CaseMatching::Ignore,
            Normalization::Smart,
            false,
        );

        // Tick to get matches
        self.tick();
    }

    fn tick(&mut self) {
        let status = self.nucleo.tick(100);
        if status.changed {
            self.refresh_matches();
        }
    }

    fn refresh_matches(&mut self) {
        let snapshot = self.nucleo.snapshot();
        let pattern = snapshot.pattern();
        let has_pattern = !pattern.column_pattern(0).atoms.is_empty();

        self.filtered.clear();
        let count = snapshot.matched_item_count();
        for item in snapshot.matched_items(0..count) {
            let cmd_item = &item.data;
            let col = &item.matcher_columns[0];

            if self.current_arg_count > cmd_item.max_args {
                continue;
            }

            let indices = if has_pattern {
                let mut indices_buf = vec![];
                pattern.column_pattern(0).indices(
                    col.slice(..),
                    &mut self.matcher,
                    &mut indices_buf,
                );
                indices_buf
            } else {
                Vec::new()
            };

            self.filtered.push(Match {
                command_type: cmd_item.command_type.clone(),
                indices,
            });
        }

        self.selected = self.selected.min(self.filtered.len().saturating_sub(1));
    }

    pub fn close(&mut self) {
        self.filtered.clear();
        self.current_arg_count = 0;
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

    fn item_name(&self, m: &Match) -> String {
        match &m.command_type {
            CommandType::Builtin(cmd) => cmd.name.to_string(),
            CommandType::Custom(i) => self.custom[*i].display_name(),
            CommandType::McpPrompt(i) => format!("/{}", self.mcp_prompts[*i].display_name),
            CommandType::Lua(i) => self.lua_commands[*i].name.to_string(),
        }
    }

    fn item_has_args(&self, m: &Match) -> bool {
        match &m.command_type {
            CommandType::Builtin(cmd) => cmd.max_args > 0,
            CommandType::Custom(i) => self.custom[*i].has_args(),
            CommandType::McpPrompt(i) => !self.mcp_prompts[*i].arguments.is_empty(),
            CommandType::Lua(i) => self.lua_commands[*i].max_args > 0,
        }
    }

    fn item_description<'a>(&'a self, m: &'a Match) -> Cow<'a, str> {
        match &m.command_type {
            CommandType::Builtin(cmd) => Cow::Borrowed(cmd.description),
            CommandType::Custom(i) => Cow::Borrowed(&self.custom[*i].description),
            CommandType::McpPrompt(i) => Cow::Borrowed(&self.mcp_prompts[*i].description),
            CommandType::Lua(i) => Cow::Borrowed(&self.lua_commands[*i].description),
        }
    }

    fn item_aliases(m: &Match) -> &'static [&'static str] {
        match &m.command_type {
            CommandType::Builtin(cmd) => cmd.aliases,
            CommandType::Custom(_) | CommandType::McpPrompt(_) | CommandType::Lua(_) => &[],
        }
    }

    fn item_display_name(&self, m: &Match) -> String {
        let aliases = Self::item_aliases(m);
        if aliases.is_empty() {
            return self.item_name(m);
        }
        format!("{}  {}", self.item_name(m), aliases.join("  "))
    }

    pub fn confirm(&self, input: &str) -> Option<ParsedCommand> {
        let item = self.filtered.get(self.selected)?;
        let typed_name = input.split_whitespace().next()?;
        let name = match &item.command_type {
            CommandType::Builtin(command)
                if command.dispatch_name == typed_name || command.aliases.contains(&typed_name) =>
            {
                typed_name.to_owned()
            }
            CommandType::Builtin(command) => command.name.to_owned(),
            _ => self.item_name(item),
        };
        let args = input
            .strip_prefix('/')
            .and_then(|s| s.split_once(char::is_whitespace))
            .map_or("", |(_, a)| a.trim());
        Some(ParsedCommand {
            name,
            args: args.to_string(),
        })
    }

    pub fn find_custom_command(&self, display_name: &str) -> Option<&CustomCommand> {
        self.custom
            .iter()
            .find(|c| c.display_name() == display_name)
    }

    pub fn find_mcp_prompt(&self, slash_name: &str) -> Option<&McpPromptInfo> {
        let name = slash_name.strip_prefix('/')?;
        self.mcp_prompts.iter().find(|p| p.display_name == name)
    }

    pub fn find_lua_command(&self, name: &str) -> Option<&LuaCommandInfo> {
        self.lua_commands.iter().find(|c| c.name.as_ref() == name)
    }

    pub fn view(&self, frame: &mut Frame, input_area: Rect) -> Option<Rect> {
        const GAP: usize = 2;
        const PAD: usize = 1;

        if self.filtered.is_empty() || input_area.width == 0 || input_area.y == 0 {
            return None;
        }

        let rows = self.display_rows();
        let selected_row = rows
            .iter()
            .position(|row| matches!(row, PaletteRow::Item(i) if *i == self.selected))
            .unwrap_or_else(|| 0);
        let popup_height = rows.len().min(usize::from(input_area.y));
        if popup_height == 0 {
            return None;
        }
        let first_row = selected_row
            .saturating_add(1)
            .saturating_sub(popup_height)
            .min(selected_row);
        let last_row = first_row + popup_height;

        let max_name = self
            .filtered
            .iter()
            .map(|item| UnicodeWidthStr::width(self.item_display_name(item).as_str()))
            .max()
            .unwrap_or_else(|| 0);
        let max_desc = self
            .filtered
            .iter()
            .map(|item| UnicodeWidthStr::width(self.item_description(item).as_ref()))
            .max()
            .unwrap_or_else(|| 0);
        let available = usize::from(input_area.width).saturating_sub(PAD * 2);
        let name_width = max_name.min(available.saturating_sub(GAP));
        let gap = GAP.min(available.saturating_sub(name_width));
        let desc_width = available.saturating_sub(name_width + gap);
        let desired_width = PAD + max_name + GAP + max_desc + PAD;
        let popup_width = cast::usize_to_u16(desired_width.max(PAD * 2)).min(input_area.width);
        let popup = Rect {
            x: input_area.x,
            y: input_area
                .y
                .saturating_sub(cast::usize_to_u16(popup_height)),
            width: popup_width,
            height: cast::usize_to_u16(popup_height),
        };

        let t = theme::current();
        let lines: Vec<Line> = rows[first_row..last_row]
            .iter()
            .map(|row| match row {
                PaletteRow::Header(group) => Line::from(Span::styled(
                    group.label(),
                    t.item_desc.add_modifier(Modifier::BOLD),
                )),
                PaletteRow::Item(i) => {
                    let item = &self.filtered[*i];
                    let selected = *i == self.selected;
                    let row_style = if selected { t.item_selected } else { t.item };
                    let alias_style = if selected {
                        t.item_desc.bg(row_style.bg.unwrap_or_else(|| t.background))
                    } else {
                        t.item_desc
                    };
                    let name = self.item_display_name(item);
                    let clipped_name = truncate_to_width(&name, name_width);
                    let name_pad = name_width
                        .saturating_sub(UnicodeWidthStr::width(clipped_name.as_str()))
                        + gap;
                    let desc = truncate_to_width(self.item_description(item).as_ref(), desc_width);
                    let mut spans = vec![Span::styled(" ".repeat(PAD), row_style)];
                    spans.extend(self.name_spans(item, name_width, row_style, alias_style));
                    spans.push(Span::styled(" ".repeat(name_pad), row_style));
                    spans.push(Span::styled(desc, row_style));
                    spans.push(Span::styled(" ".repeat(PAD), row_style));
                    Line::from(spans)
                }
            })
            .collect();

        frame.render_widget(Clear, popup);
        frame.render_widget(
            Paragraph::new(lines).style(Style::new().bg(t.background)),
            popup,
        );

        Some(popup)
    }

    fn display_rows(&self) -> Vec<PaletteRow> {
        let mut rows = Vec::with_capacity(self.filtered.len() * 2);
        let mut group = None;
        for (index, item) in self.filtered.iter().enumerate() {
            let item_group = item.command_type.group();
            if group != Some(item_group) {
                rows.push(PaletteRow::Header(item_group));
                group = Some(item_group);
            }
            rows.push(PaletteRow::Item(index));
        }
        rows
    }

    fn name_spans(
        &self,
        m: &Match,
        max_width: usize,
        base: Style,
        alias_base: Style,
    ) -> Vec<Span<'static>> {
        let name = self.item_name(m);
        let aliases = Self::item_aliases(m);
        let mut chars = Vec::new();
        let mut search_index = 0_usize;
        for ch in name.chars() {
            let matched = m
                .indices
                .binary_search(&cast::usize_to_u32(search_index))
                .is_ok();
            chars.push((ch, false, matched));
            search_index += 1;
        }
        for alias in aliases {
            chars.extend([(' ', true, false), (' ', true, false)]);
            search_index += 1;
            for ch in alias.chars() {
                let matched = m
                    .indices
                    .binary_search(&cast::usize_to_u32(search_index))
                    .is_ok();
                chars.push((ch, true, matched));
                search_index += 1;
            }
        }

        let source_width: usize = chars
            .iter()
            .map(|(ch, _, _)| ch.width().unwrap_or_else(|| 0))
            .sum();
        let truncated = source_width > max_width;
        let limit = max_width.saturating_sub(usize::from(truncated));
        let mut spans = Vec::new();
        let mut run = String::new();
        let mut run_kind = None;
        let mut width = 0;
        for (ch, is_alias, matched) in chars {
            let char_width = ch.width().unwrap_or_else(|| 0);
            if width + char_width > limit {
                break;
            }
            width += char_width;
            let kind = (is_alias, matched);
            if run_kind != Some(kind) && !run.is_empty() {
                spans.push(Span::styled(
                    mem::take(&mut run),
                    span_style(run_kind, base, alias_base),
                ));
            }
            run_kind = Some(kind);
            run.push(ch);
        }
        if !run.is_empty() {
            spans.push(Span::styled(run, span_style(run_kind, base, alias_base)));
        }
        if truncated {
            spans.push(Span::styled("…", alias_base));
        }
        spans
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use n00n_agent::{McpPromptArg, McpSnapshot};
    use ratatui::{Terminal, backend::TestBackend};
    use test_case::test_case;

    fn empty_snapshot() -> McpSnapshotReader {
        McpSnapshotReader::empty()
    }

    fn synced(input: &str) -> CommandPalette {
        let mut p = CommandPalette::new(Arc::from([]), empty_snapshot(), LuaCommandReader::empty());
        p.sync(input);
        p
    }

    fn synced_with_custom(input: &str, custom: Arc<[CustomCommand]>) -> CommandPalette {
        let mut p = CommandPalette::new(custom, empty_snapshot(), LuaCommandReader::empty());
        p.sync(input);
        p
    }

    fn sample_custom() -> Arc<[CustomCommand]> {
        Arc::from([
            CustomCommand {
                name: "review".into(),
                description: "Code review".into(),
                content: "Review $ARGUMENTS".into(),
                scope: n00n_agent::command::CommandScope::Project,
                accepts_args: true,
            },
            CustomCommand {
                name: "fix".into(),
                description: "Quick fix".into(),
                content: "Fix the code".into(),
                scope: n00n_agent::command::CommandScope::User,
                accepts_args: false,
            },
        ])
    }

    #[test_case("/action:compact", "/compact"; "compact")]
    #[test_case("/action:reload", "/reload"; "reload")]
    #[test_case("/action:exit", "/exit"; "exit")]
    #[test_case("/session:fork", "/fork"; "fork")]
    fn canonical_commands_keep_legacy_dispatch(name: &str, dispatch_name: &str) {
        let command = BUILTIN_COMMANDS
            .iter()
            .find(|command| command.name == name)
            .expect("canonical command");
        assert_eq!(command.dispatch_name, dispatch_name);
        assert!(command.aliases.contains(&dispatch_name));
    }

    #[test_case("/action:compact", "/compact"; "compact")]
    #[test_case("/session:compact", "/compact"; "legacy_compact")]
    #[test_case("/action:reload", "/reload"; "reload")]
    #[test_case("/session:reload", "/reload"; "legacy_reload")]
    #[test_case("/action:exit", "/exit"; "exit")]
    #[test_case("/session:exit", "/exit"; "legacy_exit")]
    #[test_case("/session:fork", "/fork"; "fork")]
    fn builtin_dispatch_resolves_canonical_and_legacy_names(name: &str, expected: &str) {
        assert_eq!(builtin_dispatch_name(name), Some(expected));
    }

    #[test]
    fn builtin_command_names_and_aliases_are_unique() {
        let mut names = std::collections::HashSet::new();
        for command in BUILTIN_COMMANDS {
            assert!(
                names.insert(command.name),
                "duplicate command: {}",
                command.name
            );
            for alias in command.aliases {
                assert!(names.insert(*alias), "duplicate command alias: {alias}");
            }
            assert!(
                command.dispatch_name == command.name
                    || command.aliases.contains(&command.dispatch_name),
                "dispatch target is not registered for {}",
                command.name
            );
        }
    }

    #[test]
    fn builtin_commands_reserve_canonical_names_and_aliases() {
        assert!(conflicts_with_builtin("/action:help"));
        assert!(conflicts_with_builtin("/help"));
        assert!(!conflicts_with_builtin("/project:review"));
    }

    #[test]
    fn slash_shows_builtins_plus_extras() {
        let builtin_count = synced("/").filtered.len();
        assert!(builtin_count > 0);

        let with_custom = synced_with_custom("/", sample_custom());
        assert_eq!(with_custom.filtered.len(), builtin_count + 2);

        let with_prompts = synced_with_prompts("/");
        assert_eq!(with_prompts.filtered.len(), builtin_count + 2);
    }

    #[test]
    fn close_deactivates() {
        let mut p = synced("/");
        p.close();
        assert!(!p.is_active());
    }

    #[test_case("/mp", true ; "compact_substring")]
    #[test_case("/ew", true ; "lowercase_substring")]
    #[test_case("/EW", true ; "uppercase_substring")]
    #[test_case("/zzz", false ; "no_match")]
    fn filter_by_substring(input: &str, expect_active: bool) {
        let p = synced(input);
        assert_eq!(p.is_active(), expect_active);
    }

    #[test]
    fn filter_custom_by_substring() {
        let p = synced_with_custom("/review", sample_custom());
        assert!(p.is_active());
        assert_eq!(p.filtered.len(), 1);
        assert!(matches!(p.filtered[0].command_type, CommandType::Custom(0)));
    }
    #[test]
    fn palette_uses_canonical_group_headers() {
        let p = synced("/");
        let headers: Vec<_> = p
            .display_rows()
            .into_iter()
            .filter_map(|row| match row {
                PaletteRow::Header(group) => Some(group.label()),
                PaletteRow::Item(_) => None,
            })
            .collect();
        assert_eq!(
            headers,
            vec!["Session", "Model", "View", "Settings", "Mode", "Action"]
        );
    }

    #[test]
    fn aliases_are_searchable_and_visible_without_category_prefix() {
        let p = synced("/new");
        let item = p
            .filtered
            .iter()
            .find(|item| p.item_name(item) == "/session:new")
            .expect("alias match");
        assert!(p.item_display_name(item).contains("/new"));
        assert!(item.indices.iter().any(|index| {
            usize::try_from(*index).unwrap_or_else(|_| usize::MAX) > "/session:new".chars().count()
        }));
        assert_eq!(p.item_description(item), "Start a new session");
    }

    #[test]
    fn tab_completes_canonical_name_after_alias_search() {
        let mut p = synced("/new");
        assert!(matches!(
            p.handle_key(KeyEvent::from(KeyCode::Tab), "/new"),
            CommandAction::Complete(text) if text == "/session:new"
        ));
    }

    #[test]
    fn confirming_typed_alias_keeps_alias_dispatch_name() {
        let p = synced("/new");
        let command = p.confirm("/new").expect("alias match");
        assert_eq!(command.name, "/new");
    }

    #[test]
    fn selected_item_stays_visible_below_headers() {
        let mut p = synced("/");
        p.selected = p.filtered.len() - 1;
        let mut terminal = Terminal::new(TestBackend::new(80, 5)).expect("terminal");
        terminal
            .draw(|frame| {
                p.view(frame, Rect::new(0, 4, 80, 1));
            })
            .expect("draw palette");
        let rendered: String = (0..5)
            .flat_map(|y| (0..80).map(move |x| (x, y)))
            .filter_map(|position| terminal.backend().buffer().cell(position))
            .map(ratatui::buffer::Cell::symbol)
            .collect();
        assert!(rendered.contains("/welcome"));
        assert!(matches!(
            p.display_rows().last(),
            Some(PaletteRow::Item(index)) if *index == p.selected
        ));
    }

    #[test_case("é漢字", 1, "…"; "narrow_unicode")]
    #[test_case("é漢字", 3, "é…"; "mixed_width")]
    #[test_case("hello", 0, ""; "zero_width")]
    fn truncates_by_terminal_width(text: &str, width: usize, expected: &str) {
        assert_eq!(truncate_to_width(text, width), expected);
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
        let p = CommandPalette::new(Arc::from([]), empty_snapshot(), LuaCommandReader::empty());
        assert!(p.confirm("").is_none());
    }

    #[test]
    fn sync_clamps_selected() {
        let mut p = synced("/");
        p.selected = 100;
        p.sync("/");
        assert_eq!(p.selected, p.filtered.len() - 1);
    }

    #[test]
    fn sync_filters_on_first_word_only() {
        let p = synced("/cd ~/foo");
        assert!(p.is_active());
        assert_eq!(p.filtered.len(), 1);
        let name = p.item_name(&p.filtered[0]);
        assert_eq!(name, "/action:cd");
    }

    #[test_case("/compact ", false ; "zero_arg_cmd_with_space")]
    #[test_case("/tasks ", false   ; "zero_arg_tasks_with_space")]
    #[test_case("/cd ", true        ; "one_arg_cmd_with_space")]
    #[test_case("/cd ~/foo", true   ; "one_arg_cmd_mid_arg")]
    #[test_case("/cd  ~/foo", true  ; "one_arg_cmd_double_space")]
    #[test_case("/cd ~/foo ", false ; "one_arg_cmd_second_space")]
    #[test_case("/btw hello world", true ; "btw_stays_active_with_many_args")]
    fn sync_respects_max_args(input: &str, expect_active: bool) {
        let p = synced(input);
        assert_eq!(p.is_active(), expect_active);
    }

    #[test]
    fn custom_command_with_args_stays_active() {
        let p = synced_with_custom("/project:review some args", sample_custom());
        assert!(p.is_active());
    }

    #[test]
    fn custom_command_without_args_hides_on_space() {
        let p = synced_with_custom("/user:fix ", sample_custom());
        assert!(!p.is_active());
    }

    #[test_case("/cd", "/cd", ""              ; "legacy_no_args")]
    #[test_case("/action:cd", "/action:cd", ""       ; "canonical_no_args")]
    #[test_case("/cd ~/foo", "/cd", "~/foo"   ; "legacy_with_args")]
    #[test_case("/action:cd ~/foo", "/action:cd", "~/foo" ; "canonical_with_args")]
    #[test_case("/CD ~/foo", "/action:cd", "~/foo"   ; "case_insensitive")]
    #[test_case("/compact", "/compact", ""    ; "other_command")]
    #[cfg_attr(not(target_os = "windows"), test_case("/cmp", "/action:compact", ""    ; "fuzzy-match-1"))]
    #[cfg_attr(not(target_os = "windows"), test_case("/pct", "/action:compact", ""    ; "fuzzy-match-2"))]
    #[test_case("/session:fork", "/session:fork", ""       ; "canonical_fork")]
    #[test_case("/fork", "/fork", ""               ; "legacy_fork")]
    #[test_case("/btw hello world", "/btw", "hello world" ; "btw_multi_word")]
    fn confirm_parses_args(input: &str, expected_name: &str, expected_args: &str) {
        let mut p = CommandPalette::new(Arc::from([]), empty_snapshot(), LuaCommandReader::empty());
        p.sync(input);
        let cmd = p.confirm(input).unwrap();
        assert_eq!(cmd.name, expected_name);
        assert_eq!(cmd.args, expected_args);
    }

    #[test]
    fn confirm_custom_command() {
        let custom = sample_custom();
        let mut p = CommandPalette::new(custom, empty_snapshot(), LuaCommandReader::empty());
        p.sync("/project:review");
        assert!(p.is_active());
        let cmd = p.confirm("/project:review some-file.rs").unwrap();
        assert_eq!(cmd.name, "/project:review");
        assert_eq!(cmd.args, "some-file.rs");
    }

    #[test]
    fn find_custom_command_lookup() {
        let custom = sample_custom();
        let p = CommandPalette::new(custom, empty_snapshot(), LuaCommandReader::empty());
        let found = p.find_custom_command("/project:review");
        assert!(found.is_some());
        assert_eq!(found.unwrap().content, "Review $ARGUMENTS");
        assert!(p.find_custom_command("/nonexistent").is_none());
    }

    fn sample_prompts() -> McpSnapshotReader {
        McpSnapshotReader::from_snapshot(McpSnapshot {
            infos: vec![],
            prompts: vec![
                McpPromptInfo {
                    display_name: "myserver:code-review".into(),
                    qualified_name: "myserver/code-review".into(),
                    description: "Review code changes".into(),
                    arguments: vec![McpPromptArg {
                        name: "diff".into(),
                        description: "The diff".into(),
                        required: true,
                    }],
                },
                McpPromptInfo {
                    display_name: "myserver:summarize".into(),
                    qualified_name: "myserver/summarize".into(),
                    description: "Summarize text".into(),
                    arguments: vec![],
                },
            ],
            pids: vec![],
            generation: 0,
        })
    }

    fn synced_with_prompts(input: &str) -> CommandPalette {
        let mut p = CommandPalette::new(Arc::from([]), sample_prompts(), LuaCommandReader::empty());
        p.sync(input);
        p
    }

    #[test]
    fn filter_mcp_prompt_by_substring() {
        let p = synced_with_prompts("/code");
        assert!(p.is_active());
        assert!(
            p.filtered
                .iter()
                .any(|item| matches!(item.command_type, CommandType::McpPrompt(0)))
        );
    }

    #[test]
    fn mcp_prompt_with_args_stays_active() {
        let p = synced_with_prompts("/myserver:code-review some diff");
        assert!(p.is_active());
    }

    #[test]
    fn mcp_prompt_without_args_hides_on_space() {
        let p = synced_with_prompts("/myserver:summarize ");
        assert!(
            !p.filtered
                .iter()
                .any(|f| matches!(f.command_type, CommandType::McpPrompt(1)))
        );
    }

    #[test]
    fn find_mcp_prompt_lookup() {
        let p = synced_with_prompts("/");
        let found = p.find_mcp_prompt("/myserver:code-review");
        assert!(found.is_some());
        assert_eq!(found.unwrap().qualified_name, "myserver/code-review");
        assert!(p.find_mcp_prompt("/nonexistent").is_none());
    }

    #[test]
    fn confirm_mcp_prompt_parses_args() {
        let input = "/myserver:code-review my-diff-content";
        let mut p = synced_with_prompts(input);
        p.selected = p
            .filtered
            .iter()
            .position(|f| matches!(f.command_type, CommandType::McpPrompt(0)))
            .unwrap();
        let cmd = p.confirm(input).unwrap();
        assert_eq!(cmd.name, "/myserver:code-review");
        assert_eq!(cmd.args, "my-diff-content");
    }

    #[test]
    fn mcp_update_clears_old_prompts() {
        let reader = sample_prompts();
        let mut p = CommandPalette::new(Arc::from([]), reader, LuaCommandReader::empty());

        p.sync("/");
        let initial_count = p
            .filtered
            .iter()
            .filter(|f| matches!(f.command_type, CommandType::McpPrompt(_)))
            .count();
        assert_eq!(initial_count, 2, "Should have 2 MCP prompts initially");

        let updated_reader = McpSnapshotReader::from_snapshot(McpSnapshot {
            infos: vec![],
            prompts: vec![McpPromptInfo {
                display_name: "myserver:new-prompt".into(),
                qualified_name: "myserver/new-prompt".into(),
                description: "A new prompt".into(),
                arguments: vec![],
            }],
            pids: vec![],
            generation: 1,
        });

        p.mcp_reader = updated_reader;
        p.sync("/");

        let updated_count = p
            .filtered
            .iter()
            .filter(|f| matches!(f.command_type, CommandType::McpPrompt(_)))
            .count();
        assert_eq!(
            updated_count, 1,
            "Should have only 1 MCP prompt after update"
        );

        assert!(!p.filtered.is_empty(), "Should have filtered results");
        let prompt = &p
            .filtered
            .iter()
            .find(|f| matches!(f.command_type, CommandType::McpPrompt(_)))
            .expect("Should have at least one MCP prompt");
        match &prompt.command_type {
            CommandType::McpPrompt(i) => {
                assert_eq!(p.mcp_prompts[*i].display_name, "myserver:new-prompt");
            }
            _ => panic!("Should have MCP prompt"),
        }
    }

    #[test_case("/cmp", "/action:compact" ; "compact_fuzzy")]
    #[test_case("/new", "/session:new" ; "new_alias")]
    #[test_case("/tsk", "/view:tasks" ; "tasks_fuzzy")]
    fn nucleo_highlights_matching_indices(input: &str, expected_cmd: &str) {
        let p = synced(input);
        assert!(p.is_active(), "Input '{input}' should activate palette");
        // Find the expected match
        let matched = p
            .filtered
            .iter()
            .find(|m| p.item_name(m) == expected_cmd)
            .unwrap_or_else(|| panic!("Should find {expected_cmd} for input {input}"));
        // Should have some highlight indices
        assert!(
            !matched.indices.is_empty(),
            "Match should have highlight indices"
        );
    }

    fn sample_lua_commands() -> LuaCommandReader {
        LuaCommandReader::from_commands(vec![
            LuaCommandInfo {
                name: Arc::from("/memory"),
                description: Arc::from("View memory files"),
                plugin: Arc::from("memory"),
                max_args: 0,
            },
            LuaCommandInfo {
                name: Arc::from("/deploy"),
                description: Arc::from("Deploy the project"),
                plugin: Arc::from("deploy_plugin"),
                max_args: 0,
            },
        ])
    }

    fn synced_with_lua(input: &str) -> CommandPalette {
        let mut p = CommandPalette::new(Arc::from([]), empty_snapshot(), sample_lua_commands());
        p.sync(input);
        p
    }

    #[test]
    fn lua_commands_appear_in_unfiltered_list() {
        let p = synced_with_lua("/");
        let lua_count = p
            .filtered
            .iter()
            .filter(|f| matches!(f.command_type, CommandType::Lua(_)))
            .count();
        assert_eq!(lua_count, 1);
    }

    #[test]
    fn builtin_command_wins_over_matching_lua_command() {
        let p = synced_with_lua("/mem");
        assert!(p.is_active());
        let found = p.filtered.iter().any(|item| {
            matches!(item.command_type, CommandType::Builtin(_))
                && p.item_name(item) == "/view:memory"
        });
        assert!(found);
    }

    #[test]
    fn find_lua_command_returns_matching_entry() {
        let p = synced_with_lua("/");
        let found = p.find_lua_command("/memory");
        assert!(found.is_some());
        assert_eq!(found.unwrap().plugin.as_ref(), "memory");
        assert!(p.find_lua_command("/nonexistent").is_none());
    }

    #[test]
    fn confirm_lua_command_parses_args() {
        let mut p = CommandPalette::new(Arc::from([]), empty_snapshot(), sample_lua_commands());
        p.sync("/memory");
        let cmd = p.confirm("/memory some-arg").unwrap();
        assert_eq!(cmd.name, "/memory");
        assert_eq!(cmd.args, "some-arg");
    }

    #[test]
    fn lua_commands_update_on_generation_change() {
        let (writer, reader) = n00n_lua::test_support::lua_command_writer_pair();
        writer.publish(vec![LuaCommandInfo {
            name: Arc::from("/old"),
            description: Arc::from("old command"),
            plugin: Arc::from("p"),
            max_args: 0,
        }]);
        let mut p = CommandPalette::new(Arc::from([]), empty_snapshot(), reader);
        p.sync("/");
        let initial_lua = p
            .filtered
            .iter()
            .filter(|f| matches!(f.command_type, CommandType::Lua(_)))
            .count();
        assert_eq!(initial_lua, 1);

        writer.publish(vec![
            LuaCommandInfo {
                name: Arc::from("/new1"),
                description: Arc::from("new"),
                plugin: Arc::from("p"),
                max_args: 0,
            },
            LuaCommandInfo {
                name: Arc::from("/new2"),
                description: Arc::from("new2"),
                plugin: Arc::from("p"),
                max_args: 0,
            },
        ]);
        p.sync("/");
        let updated_lua = p
            .filtered
            .iter()
            .filter(|f| matches!(f.command_type, CommandType::Lua(_)))
            .count();
        assert_eq!(updated_lua, 2);
        assert!(p.find_lua_command("/old").is_none());
        assert!(p.find_lua_command("/new1").is_some());
    }

    #[test]
    fn lua_command_respects_max_args_zero() {
        let (writer, reader) = n00n_lua::test_support::lua_command_writer_pair();
        writer.publish(vec![LuaCommandInfo {
            name: Arc::from("/noargs"),
            description: Arc::from("no args"),
            plugin: Arc::from("p"),
            max_args: 0,
        }]);
        let mut p = CommandPalette::new(Arc::from([]), empty_snapshot(), reader);
        p.sync("/noargs");
        assert!(p.is_active());

        p.sync("/noargs arg");
        assert!(!p.is_active(), "should not show when args exceed max_args");
    }

    #[test]
    fn lua_command_respects_max_args_one() {
        let (writer, reader) = n00n_lua::test_support::lua_command_writer_pair();
        writer.publish(vec![LuaCommandInfo {
            name: Arc::from("/onearg"),
            description: Arc::from("one arg"),
            plugin: Arc::from("p"),
            max_args: 1,
        }]);
        let mut p = CommandPalette::new(Arc::from([]), empty_snapshot(), reader);
        p.sync("/onearg");
        assert!(p.is_active());

        p.sync("/onearg first");
        assert!(p.is_active(), "should show with one arg");

        p.sync("/onearg first second");
        assert!(!p.is_active(), "should not show when args exceed max_args");
    }
}

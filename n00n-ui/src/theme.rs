use std::collections::HashMap;
use std::env;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, LazyLock};

use arc_swap::{ArcSwap, Guard};
use n00n_storage::StateDir;
use ratatui::style::{Color, Modifier, Style};
use serde::Deserialize;
use syntect::highlighting::{
    Color as SynColor, FontStyle, ScopeSelectors, StyleModifier, ThemeItem, ThemeSettings,
};

const DEFAULT_THEME: &str = "dracula";
const SYN_BLACK: SynColor = SynColor {
    r: 0,
    g: 0,
    b: 0,
    a: 0xff,
};
const SYN_WHITE: SynColor = SynColor {
    r: 0xff,
    g: 0xff,
    b: 0xff,
    a: 0xff,
};
const RESERVED_KEYS: &[&str] = &["palette", "ui", "inherits"];

#[cfg(test)]
pub(crate) static THEME_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

const HELIX_TO_TEXTMATE: &[(&str, &str)] = &[
    ("comment", "comment, comment punctuation.definition.comment"),
    (
        "comment.line",
        "comment.line, comment.line punctuation.definition.comment",
    ),
    (
        "comment.block",
        "comment.block, comment.block punctuation.definition.comment",
    ),
    (
        "comment.line.documentation",
        "comment.line.documentation, comment.line.documentation punctuation.definition.comment",
    ),
    (
        "comment.block.documentation",
        "comment.block.documentation, comment.block.documentation punctuation.definition.comment",
    ),
    ("string", "string, string punctuation.definition.string"),
    (
        "string.regexp",
        "string.regexp, string.regexp punctuation.definition.string",
    ),
    (
        "string.special",
        "string.special, string.quoted.single punctuation.definition.string, string.quoted.double.raw punctuation.definition.string",
    ),
    ("function", "entity.name.function, variable.function"),
    ("function.builtin", "support.function"),
    (
        "function.call",
        "entity.name.function, variable.function, support.function",
    ),
    (
        "function.macro",
        "entity.name.function.macro, support.macro",
    ),
    (
        "function.method",
        "entity.name.function, meta.function-call",
    ),
    ("constructor", "entity.name.function.constructor"),
    (
        "type",
        "entity.name.type, entity.name.class, entity.name.struct, entity.name.enum, entity.name.trait, entity.name.union, entity.name.impl, support.type, support.class, meta.generic",
    ),
    ("type.builtin", "support.type, storage.type.primitive"),
    ("type.enum.variant", "entity.name.type.enum"),
    ("tag", "entity.name.tag"),
    ("tag.attribute", "entity.other.attribute-name"),
    ("tag.delimiter", "punctuation.definition.tag"),
    ("variable", "variable.other"),
    ("variable.builtin", "variable.language"),
    ("variable.parameter", "variable.parameter"),
    (
        "variable.other.member",
        "variable.other.member, variable.other.property",
    ),
    (
        "constant",
        "constant, variable.other.constant, entity.name.constant",
    ),
    ("constant.builtin", "constant.language"),
    (
        "constant.builtin.boolean",
        "constant.language.boolean, constant.language",
    ),
    (
        "constant.character.escape",
        "constant.character.escape, constant.character.escaped",
    ),
    (
        "keyword.storage.type",
        "storage.type, keyword.declaration, keyword.declaration.function, keyword.declaration.class, keyword.declaration.struct, keyword.declaration.enum, keyword.declaration.trait, keyword.declaration.impl",
    ),
    ("keyword.storage.modifier", "storage.modifier"),
    (
        "keyword.function",
        "keyword.declaration.function, storage.type.function",
    ),
    (
        "keyword.control.import",
        "keyword.control.import, keyword.other",
    ),
    ("keyword.return", "keyword.control.return, keyword.control"),
    ("keyword.directive", "meta.preprocessor"),
    ("keyword.control.exception", "keyword.control.exception"),
    ("punctuation", "punctuation, punctuation.accessor.dot"),
    (
        "punctuation.special",
        "punctuation.section.embedded, punctuation.section.interpolation, punctuation.separator.namespace, punctuation.accessor",
    ),
    ("label", "entity.name.label, storage.modifier.lifetime"),
    (
        "attribute",
        "entity.other.attribute-name, meta.annotation, variable.annotation, meta.annotation punctuation.definition.annotation, meta.annotation punctuation.section.group",
    ),
    (
        "namespace",
        "entity.name.namespace, entity.name.module, meta.path",
    ),
    (
        "markup.raw",
        "markup.raw, markup.raw.inline, markup.raw.block",
    ),
    ("markup.link.url", "markup.underline.link"),
    ("operator", "keyword.operator"),
];

pub struct ThemeEntry {
    pub name: &'static str,
    pub toml: &'static str,
}

pub static BUNDLED_THEMES: &[ThemeEntry] = &[
    ThemeEntry {
        name: "ayu_dark",
        toml: include_str!("themes/ayu_dark.toml"),
    },
    ThemeEntry {
        name: "ayu_light",
        toml: include_str!("themes/ayu_light.toml"),
    },
    ThemeEntry {
        name: "ayu_mirage",
        toml: include_str!("themes/ayu_mirage.toml"),
    },
    ThemeEntry {
        name: "carbonfox",
        toml: include_str!("themes/carbonfox.toml"),
    },
    ThemeEntry {
        name: "catppuccin_frappe",
        toml: include_str!("themes/catppuccin_frappe.toml"),
    },
    ThemeEntry {
        name: "catppuccin_latte",
        toml: include_str!("themes/catppuccin_latte.toml"),
    },
    ThemeEntry {
        name: "catppuccin_macchiato",
        toml: include_str!("themes/catppuccin_macchiato.toml"),
    },
    ThemeEntry {
        name: "catppuccin_mocha",
        toml: include_str!("themes/catppuccin_mocha.toml"),
    },
    ThemeEntry {
        name: "dracula",
        toml: include_str!("themes/dracula.toml"),
    },
    ThemeEntry {
        name: "everforest_dark",
        toml: include_str!("themes/everforest_dark.toml"),
    },
    ThemeEntry {
        name: "fleet_dark",
        toml: include_str!("themes/fleet_dark.toml"),
    },
    ThemeEntry {
        name: "github_dark",
        toml: include_str!("themes/github_dark.toml"),
    },
    ThemeEntry {
        name: "gruvbox",
        toml: include_str!("themes/gruvbox.toml"),
    },
    ThemeEntry {
        name: "gruvbox_light",
        toml: include_str!("themes/gruvbox_light.toml"),
    },
    ThemeEntry {
        name: "kanagawa",
        toml: include_str!("themes/kanagawa.toml"),
    },
    ThemeEntry {
        name: "material_darker",
        toml: include_str!("themes/material_darker.toml"),
    },
    ThemeEntry {
        name: "monokai_pro",
        toml: include_str!("themes/monokai_pro.toml"),
    },
    ThemeEntry {
        name: "night_owl",
        toml: include_str!("themes/night_owl.toml"),
    },
    ThemeEntry {
        name: "nightfox",
        toml: include_str!("themes/nightfox.toml"),
    },
    ThemeEntry {
        name: "nord",
        toml: include_str!("themes/nord.toml"),
    },
    ThemeEntry {
        name: "onedark",
        toml: include_str!("themes/onedark.toml"),
    },
    ThemeEntry {
        name: "rose_pine",
        toml: include_str!("themes/rose_pine.toml"),
    },
    ThemeEntry {
        name: "rose_pine_dawn",
        toml: include_str!("themes/rose_pine_dawn.toml"),
    },
    ThemeEntry {
        name: "rose_pine_moon",
        toml: include_str!("themes/rose_pine_moon.toml"),
    },
    ThemeEntry {
        name: "solarized_dark",
        toml: include_str!("themes/solarized_dark.toml"),
    },
    ThemeEntry {
        name: "solarized_light",
        toml: include_str!("themes/solarized_light.toml"),
    },
    ThemeEntry {
        name: "tokyonight",
        toml: include_str!("themes/tokyonight.toml"),
    },
    ThemeEntry {
        name: "vscode_dark_plus",
        toml: include_str!("themes/vscode_dark_plus.toml"),
    },
    ThemeEntry {
        name: "zenburn",
        toml: include_str!("themes/zenburn.toml"),
    },
];

static THEME: LazyLock<ArcSwap<Theme>> =
    LazyLock::new(|| ArcSwap::from_pointee(Theme::load_or_bundled()));

static GENERATION: AtomicU64 = AtomicU64::new(0);
static NO_COLOR: LazyLock<bool> = LazyLock::new(|| env::var_os("NO_COLOR").is_some());
static HIGH_CONTRAST: LazyLock<bool> =
    LazyLock::new(|| env::var_os("N00N_HIGH_CONTRAST").is_some_and(|value| value != "0"));

pub fn current() -> Guard<Arc<Theme>> {
    THEME.load()
}

pub fn set(theme: Theme) {
    // Order matters: install colors before bumping the counter, otherwise a
    // reader could see the new generation but bake with the old palette.
    THEME.store(Arc::new(theme));
    crate::highlight::refresh_syntax_theme();
    GENERATION.fetch_add(1, Ordering::Release);
}

pub fn generation() -> u64 {
    GENERATION.load(Ordering::Acquire)
}

pub fn load_by_name(name: &str) -> Result<Theme, String> {
    BUNDLED_THEMES.iter().find(|e| e.name == name).map_or_else(
        || Err(format!("unknown theme: {name}")),
        |e| Theme::from_toml(e.toml).map(Theme::with_current_accessibility),
    )
}

pub fn persist_theme(name: &str) {
    if let Ok(dir) = StateDir::resolve() {
        n00n_storage::theme::persist_theme_name(&dir, name);
    }
}

fn read_theme_name() -> Option<String> {
    let dir = StateDir::resolve().ok()?;
    n00n_storage::theme::read_theme_name(&dir)
}

pub fn current_theme_name() -> String {
    read_theme_name().unwrap_or_else(|| DEFAULT_THEME.to_owned())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SemanticRole {
    Text,
    Muted,
    Accent,
    Success,
    Error,
    Activity,
}

#[must_use]
pub fn no_color() -> bool {
    *NO_COLOR
}

#[must_use]
pub fn high_contrast() -> bool {
    *HIGH_CONTRAST
}

#[must_use]
pub fn semantic_style(role: SemanticRole) -> Style {
    if no_color() {
        return Style::default();
    }
    let theme = current();
    let style = match role {
        SemanticRole::Text => Style::new().fg(theme.foreground),
        SemanticRole::Muted => theme.status_dim,
        SemanticRole::Accent => theme.accent,
        SemanticRole::Success => theme.tool_success,
        SemanticRole::Error => theme.tool_error,
        SemanticRole::Activity => theme.status_notice,
    };
    if high_contrast() {
        style.add_modifier(Modifier::BOLD)
    } else {
        style
    }
}

pub fn style_by_name(name: &str) -> Style {
    let t = current();
    match name {
        "dim" | "tool_dim" => t.tool_dim,
        "path" | "tool_path" => t.tool_path,
        "tool" => t.tool,
        "tool_prefix" => t.tool_prefix,
        "tool_success" => t.tool_success,
        "tool_error" => t.tool_error,
        "tool_annotation" => t.tool_annotation,
        "spinner" => t.spinner,
        "control" => t.control,
        "error" => t.error,
        "bold" => t.bold,
        "italic" => t.italic,
        "bold_italic" => t.bold_italic,
        "inline_code" => t.inline_code,
        "strikethrough" => t.strikethrough,
        "heading" => t.heading,
        "list_marker" => t.list_marker,
        "horizontal_rule" => t.horizontal_rule,
        "code_gutter" => t.code_gutter,
        "table_border" => t.table_border,
        "keyword" | "index_keyword" => t.index_keyword,
        "section" | "index_section" => t.index_section,
        "line_nr" | "index_line_nr" => t.index_line_nr,
        "diff_old" => t.diff_old,
        "diff_new" => t.diff_new,
        "item" => t.item,
        "item_desc" => t.item_desc,
        "item_selected" | "selected" => t.item_selected,
        "item_match" | "match" => t.item_match,
        "item_match_selected" | "match_selected" => t.item_match_selected,
        "cursor" => t.cursor,
        "foreground" => Style::new().fg(t.foreground),
        "accent" => t.accent,
        "active" => t.active,
        "keybind_key" => t.keybind_key,
        "keybind_desc" => t.keybind_desc,
        "success" | "todo_completed" => t.todo_completed,
        "warning" | "todo_in_progress" => t.todo_in_progress,
        "todo_pending" | "pending" => t.todo_pending,
        "todo_cancelled" | "cancelled" => t.todo_cancelled,
        _ => Style::default(),
    }
}

#[derive(Debug)]
pub struct Theme {
    pub background: Color,
    pub foreground: Color,

    pub user: Style,
    pub control: Style,
    pub assistant: Style,
    pub assistant_prefix: Style,
    pub thinking: Style,
    pub tool_bg: Style,
    pub tool: Style,
    pub tool_path: Style,
    pub tool_annotation: Style,
    pub tool_prefix: Style,
    pub tool_success: Style,
    pub tool_error: Style,
    pub tool_dim: Style,
    pub error: Style,
    pub status_dim: Style,
    pub bold: Style,
    pub italic: Style,
    pub bold_italic: Style,
    pub inline_code: Style,
    pub code_block: Style,
    pub code_gutter: Style,
    pub strikethrough: Style,
    pub heading: Style,
    pub list_marker: Style,
    pub horizontal_rule: Style,
    pub plan_rule: Style,
    pub table_border: Style,
    pub diff_old: Style,
    pub diff_new: Style,
    pub diff_old_emphasis: Style,
    pub diff_new_emphasis: Style,
    pub diff_line_nr: Style,
    pub todo_completed: Style,
    pub todo_in_progress: Style,
    pub todo_pending: Style,
    pub todo_cancelled: Style,
    pub item_selected: Style,
    pub item: Style,
    pub item_desc: Style,
    pub item_match: Style,
    pub item_match_selected: Style,
    pub panel_border: Style,
    pub panel_title: Style,
    pub cursor: Style,
    pub input_border: Style,
    pub accent: Style,
    pub active: Style,
    pub keybind_key: Style,
    pub keybind_desc: Style,
    pub keybind_section: Style,
    pub mode_build: Color,
    pub mode_plan: Color,
    pub mode_bash: Color,
    pub queue: Style,
    pub plan_path: Style,
    pub status_notice: Style,
    pub status_retry_error: Style,
    pub status_retry_info: Style,
    pub input_placeholder: Style,
    pub queue_delete: Style,
    pub timestamp: Style,
    pub spinner: Style,
    pub index_section: Style,
    pub index_line_nr: Style,
    pub index_keyword: Style,
    pub shell_prefix: Style,
    pub progress_bar: Style,

    pub syntax: syntect::highlighting::Theme,
}

#[derive(Deserialize)]
struct StyleDef {
    fg: Option<String>,
    bg: Option<String>,
    #[serde(default)]
    modifiers: Vec<String>,
}

fn helix_to_textmate_scope(key: &str) -> &str {
    for &(helix, tm) in HELIX_TO_TEXTMATE {
        if key == helix {
            return tm;
        }
    }
    key
}

fn parse_hex_rgb(s: &str) -> Option<(u8, u8, u8)> {
    let hex = s.strip_prefix('#')?;
    if hex.len() != 6 {
        return None;
    }
    let r = u8::from_str_radix(&hex[0..2], 16).ok()?;
    let g = u8::from_str_radix(&hex[2..4], 16).ok()?;
    let b = u8::from_str_radix(&hex[4..6], 16).ok()?;
    Some((r, g, b))
}

fn parse_hex(s: &str) -> Option<Color> {
    let (r, g, b) = parse_hex_rgb(s)?;
    Some(Color::Rgb(r, g, b))
}

fn parse_syn_color(s: &str, palette: &HashMap<String, String>) -> Option<SynColor> {
    let resolved = if s.starts_with('#') {
        s
    } else {
        palette.get(s)?.as_str()
    };
    let (r, g, b) = parse_hex_rgb(resolved)?;
    Some(SynColor { r, g, b, a: 0xFF })
}

fn resolve_color(name: &str, palette: &HashMap<String, Color>) -> Option<Color> {
    if name.starts_with('#') {
        parse_hex(name)
    } else {
        palette.get(name).copied()
    }
}

fn resolve_modifier(name: &str) -> Modifier {
    match name {
        "bold" => Modifier::BOLD,
        "italic" => Modifier::ITALIC,
        "underlined" => Modifier::UNDERLINED,
        "crossed_out" => Modifier::CROSSED_OUT,
        "dim" => Modifier::DIM,
        "reversed" => Modifier::REVERSED,
        _ => Modifier::empty(),
    }
}

fn resolve_style(def: &StyleDef, palette: &HashMap<String, Color>) -> Style {
    let mut style = Style::new();
    if let Some(fg) = def.fg.as_ref().and_then(|n| resolve_color(n, palette)) {
        style = style.fg(fg);
    }
    if let Some(bg) = def.bg.as_ref().and_then(|n| resolve_color(n, palette)) {
        style = style.bg(bg);
    }
    for m in &def.modifiers {
        style = style.add_modifier(resolve_modifier(m));
    }
    style
}

fn scope_fg(
    full_table: &toml::Table,
    palette: &HashMap<String, Color>,
    raw_palette: &HashMap<String, String>,
    scope: &str,
) -> Option<Color> {
    let table = full_table.get(scope)?.as_table()?;
    let fg_val = table.get("fg")?.as_str()?;
    resolve_color(fg_val, palette).or_else(|| {
        let resolved = raw_palette.get(fg_val)?;
        parse_hex(resolved)
    })
}

fn resolve_font_style(modifiers: &[String]) -> FontStyle {
    let mut fs = FontStyle::empty();
    for m in modifiers {
        match m.as_str() {
            "bold" => fs |= FontStyle::BOLD,
            "italic" => fs |= FontStyle::ITALIC,
            "underlined" => fs |= FontStyle::UNDERLINE,
            _ => {}
        }
    }
    fs
}

fn style_def_to_syn(def: &StyleDef, raw_palette: &HashMap<String, String>) -> StyleModifier {
    let has_color = def.fg.is_some() || def.bg.is_some();
    StyleModifier {
        foreground: def
            .fg
            .as_ref()
            .and_then(|n| parse_syn_color(n, raw_palette)),
        background: def
            .bg
            .as_ref()
            .and_then(|n| parse_syn_color(n, raw_palette)),
        font_style: if def.modifiers.is_empty() {
            if has_color {
                Some(FontStyle::empty())
            } else {
                None
            }
        } else {
            Some(resolve_font_style(&def.modifiers))
        },
    }
}

fn build_syntax_theme(
    toml_table: &toml::Table,
    raw_palette: &HashMap<String, String>,
) -> syntect::highlighting::Theme {
    let fg = parse_syn_color("foreground", raw_palette);
    let bg = parse_syn_color("background", raw_palette);

    let settings = ThemeSettings {
        foreground: fg,
        background: bg,
        caret: fg,
        line_highlight: parse_syn_color("current_line", raw_palette)
            .or_else(|| parse_syn_color("selection", raw_palette)),
        selection: parse_syn_color("selection", raw_palette)
            .or_else(|| parse_syn_color("current_line", raw_palette)),
        ..Default::default()
    };

    let mut scopes = Vec::new();

    for (key, value) in toml_table {
        if RESERVED_KEYS.contains(&key.as_str()) || key.starts_with("ui.") {
            continue;
        }

        let Some(table) = value.as_table() else {
            continue;
        };

        let def: StyleDef = match toml::Value::Table(table.clone()).try_into() {
            Ok(d) => d,
            Err(_) => continue,
        };

        let tm_scope = helix_to_textmate_scope(key);

        let Ok(scope) = tm_scope.parse::<ScopeSelectors>() else {
            continue;
        };

        scopes.push(ThemeItem {
            scope,
            style: style_def_to_syn(&def, raw_palette),
        });
    }

    syntect::highlighting::Theme {
        name: None,
        author: None,
        settings,
        scopes,
    }
}

impl Theme {
    fn from_toml(toml_str: &str) -> Result<Self, String> {
        let full_table: toml::Table = toml::from_str(toml_str).map_err(|e| e.to_string())?;

        let raw_palette = Self::parse_raw_palette(&full_table);
        let palette = Self::parse_palette(&raw_palette);
        let ui = Self::parse_ui(&full_table);
        let syntax = build_syntax_theme(&full_table, &raw_palette);

        let style = |key: &str| -> Style {
            ui.get(key)
                .map(|d| resolve_style(d, &palette))
                .map_or(Style::default(), |s| s)
        };

        let derived_color = |ui_key: &str, scopes: &[&str]| -> Color {
            if let Some(c) = palette.get(ui_key) {
                return *c;
            }
            for scope in scopes {
                if let Some(c) = scope_fg(&full_table, &palette, &raw_palette, scope) {
                    return c;
                }
            }
            Color::Reset
        };

        let derived_style = |ui_key: &str, scopes: &[&str], mods: Modifier| -> Style {
            if let Some(d) = ui.get(ui_key) {
                return resolve_style(d, &palette);
            }
            for scope in scopes {
                if let Some(c) = scope_fg(&full_table, &palette, &raw_palette, scope) {
                    return Style::new().fg(c).add_modifier(mods);
                }
            }
            Style::default()
        };

        let color =
            |key: &str| -> Color { palette.get(key).copied().unwrap_or_else(|| Color::Reset) };

        let bold_style = derived_style(
            "bold",
            &["markup.bold", "variable.parameter"],
            Modifier::BOLD,
        );

        Ok(Self::build_theme(
            &style,
            &derived_color,
            &derived_style,
            &color,
            bold_style,
            &ui,
            &palette,
            syntax,
        ))
    }

    fn with_current_accessibility(mut self) -> Self {
        self.apply_accessibility(no_color(), high_contrast());
        self
    }

    fn styles_mut(&mut self) -> [&mut Style; 64] {
        [
            &mut self.user,
            &mut self.control,
            &mut self.assistant,
            &mut self.assistant_prefix,
            &mut self.thinking,
            &mut self.tool_bg,
            &mut self.tool,
            &mut self.tool_path,
            &mut self.tool_annotation,
            &mut self.tool_prefix,
            &mut self.tool_success,
            &mut self.tool_error,
            &mut self.tool_dim,
            &mut self.error,
            &mut self.status_dim,
            &mut self.bold,
            &mut self.italic,
            &mut self.bold_italic,
            &mut self.inline_code,
            &mut self.code_block,
            &mut self.code_gutter,
            &mut self.strikethrough,
            &mut self.heading,
            &mut self.list_marker,
            &mut self.horizontal_rule,
            &mut self.plan_rule,
            &mut self.table_border,
            &mut self.diff_old,
            &mut self.diff_new,
            &mut self.diff_old_emphasis,
            &mut self.diff_new_emphasis,
            &mut self.diff_line_nr,
            &mut self.todo_completed,
            &mut self.todo_in_progress,
            &mut self.todo_pending,
            &mut self.todo_cancelled,
            &mut self.item_selected,
            &mut self.item,
            &mut self.item_desc,
            &mut self.item_match,
            &mut self.item_match_selected,
            &mut self.panel_border,
            &mut self.panel_title,
            &mut self.cursor,
            &mut self.input_border,
            &mut self.accent,
            &mut self.active,
            &mut self.keybind_key,
            &mut self.keybind_desc,
            &mut self.keybind_section,
            &mut self.queue,
            &mut self.plan_path,
            &mut self.status_notice,
            &mut self.status_retry_error,
            &mut self.status_retry_info,
            &mut self.input_placeholder,
            &mut self.queue_delete,
            &mut self.timestamp,
            &mut self.spinner,
            &mut self.index_section,
            &mut self.index_line_nr,
            &mut self.index_keyword,
            &mut self.shell_prefix,
            &mut self.progress_bar,
        ]
    }

    fn apply_accessibility(&mut self, no_color: bool, high_contrast: bool) {
        if !no_color && !high_contrast {
            return;
        }
        for style in self.styles_mut() {
            if no_color {
                style.fg = None;
                style.bg = None;
                style.underline_color = None;
            } else {
                *style = style.add_modifier(Modifier::BOLD);
                style.fg = Some(Color::White);
                style.bg = None;
            }
        }
        if no_color {
            self.background = Color::Reset;
            self.foreground = Color::Reset;
            self.mode_build = Color::Reset;
            self.mode_plan = Color::Reset;
            self.mode_bash = Color::Reset;
        } else if high_contrast {
            self.background = Color::Black;
            self.foreground = Color::White;
            self.syntax.settings.foreground = Some(SYN_WHITE);
            self.syntax.settings.background = Some(SYN_BLACK);
            self.syntax.settings.caret = Some(SYN_WHITE);
            self.syntax.settings.line_highlight = None;
            self.syntax.settings.selection = None;
            for item in &mut self.syntax.scopes {
                item.style.foreground = Some(SYN_WHITE);
                item.style.background = None;
            }
            self.accent.fg = Some(Color::Yellow);
            self.tool_success.fg = Some(Color::Green);
            self.tool_error.fg = Some(Color::LightRed);
            self.error.fg = Some(Color::LightRed);
            self.diff_old.fg = Some(Color::LightRed);
            self.diff_new.fg = Some(Color::LightGreen);
            self.diff_old_emphasis.fg = Some(Color::Red);
            self.diff_new_emphasis.fg = Some(Color::Green);
            self.todo_completed.fg = Some(Color::Green);
            self.todo_in_progress.fg = Some(Color::Yellow);
            self.todo_pending.fg = Some(Color::Cyan);
            self.todo_cancelled.fg = Some(Color::LightRed);
            self.mode_build = Color::Yellow;
            self.mode_plan = Color::Cyan;
            self.mode_bash = Color::Magenta;
        }
        self.item_selected = self.item_selected.add_modifier(Modifier::REVERSED);
        self.item_match_selected = self.item_match_selected.add_modifier(Modifier::REVERSED);
    }

    fn parse_raw_palette(full_table: &toml::Table) -> HashMap<String, String> {
        full_table
            .get("palette")
            .and_then(|v| v.as_table())
            .map_or_else(HashMap::default, |t| {
                t.iter()
                    .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_owned())))
                    .collect()
            })
    }

    fn parse_palette(raw_palette: &HashMap<String, String>) -> HashMap<String, Color> {
        raw_palette
            .iter()
            .filter_map(|(k, v)| parse_hex(v).map(|c| (k.clone(), c)))
            .collect()
    }

    fn parse_ui(full_table: &toml::Table) -> HashMap<String, StyleDef> {
        full_table
            .get("ui")
            .and_then(|v| v.as_table())
            .map_or_else(HashMap::default, |t| {
                t.iter()
                    .filter_map(|(k, v)| {
                        let def: StyleDef = v.clone().try_into().ok()?;
                        Some((k.clone(), def))
                    })
                    .collect()
            })
    }

    fn load_or_bundled() -> Self {
        if let Some(name) = read_theme_name()
            && let Ok(theme) = load_by_name(&name)
        {
            return theme;
        }
        let mut last_error = String::new();
        for entry in BUNDLED_THEMES {
            match Self::from_toml(entry.toml) {
                Ok(theme) => return theme.with_current_accessibility(),
                Err(e) => last_error = e,
            }
        }
        eprintln!("Failed to load any bundled theme. Last error: {last_error}");
        std::process::exit(1)
    }

    fn build_theme(
        style: &impl Fn(&str) -> Style,
        derived_color: &impl Fn(&str, &[&str]) -> Color,
        derived_style: &impl Fn(&str, &[&str], Modifier) -> Style,
        color: &impl Fn(&str) -> Color,
        bold_style: Style,
        ui: &HashMap<String, StyleDef>,
        palette: &HashMap<String, Color>,
        syntax: syntect::highlighting::Theme,
    ) -> Self {
        let mut theme = Self::build_chat_theme(
            style,
            derived_color,
            derived_style,
            color,
            bold_style,
            ui,
            palette,
        );
        theme.background = color("background");
        theme.foreground = color("foreground");
        theme.syntax = syntax;
        theme
    }

    fn build_chat_theme(
        style: &impl Fn(&str) -> Style,
        derived_color: &impl Fn(&str, &[&str]) -> Color,
        derived_style: &impl Fn(&str, &[&str], Modifier) -> Style,
        color: &impl Fn(&str) -> Color,
        bold_style: Style,
        ui: &HashMap<String, StyleDef>,
        palette: &HashMap<String, Color>,
    ) -> Self {
        Self {
            background: Color::default(),
            foreground: Color::default(),
            user: style("user"),
            control: Self::build_simple_fallback(style, "control", "user"),
            assistant: style("assistant"),
            assistant_prefix: style("assistant_prefix"),
            thinking: brighten_toward(
                style("thinking"),
                color("comment"),
                color("foreground"),
                0.3,
            ),
            tool_bg: style("tool_bg"),
            tool: style("tool"),
            tool_path: style("tool_path"),
            tool_annotation: style("tool_annotation"),
            tool_prefix: style("tool_prefix"),
            tool_success: style("tool_success"),
            tool_error: style("tool_error"),
            tool_dim: style("tool_dim"),
            error: style("error"),
            status_dim: style("status_dim"),
            bold: bold_style,
            italic: ui.get("italic").map_or_else(
                || Style::default().add_modifier(Modifier::ITALIC),
                |d| resolve_style(d, palette),
            ),
            bold_italic: ui.get("bold_italic").map_or_else(
                || bold_style.add_modifier(Modifier::ITALIC),
                |d| resolve_style(d, palette),
            ),
            inline_code: derived_style(
                "inline_code",
                &["function.call", "function"],
                Modifier::empty(),
            ),
            code_block: style("code_block"),
            code_gutter: derived_style(
                "code_gutter",
                &["variable.parameter", "string"],
                Modifier::empty(),
            ),
            strikethrough: style("strikethrough"),
            heading: derived_style(
                "heading",
                &["keyword.storage.type", "keyword"],
                Modifier::BOLD,
            ),
            list_marker: derived_style(
                "list_marker",
                &["keyword.storage.type", "keyword"],
                Modifier::empty(),
            ),
            horizontal_rule: style("horizontal_rule"),
            plan_rule: style("plan_rule"),
            table_border: style("table_border"),
            diff_old: style("diff_old"),
            diff_new: style("diff_new"),
            diff_old_emphasis: style("diff_old_emphasis"),
            diff_new_emphasis: style("diff_new_emphasis"),
            diff_line_nr: style("diff_line_nr"),
            todo_completed: style("todo_completed"),
            todo_in_progress: style("todo_in_progress"),
            todo_pending: style("todo_pending"),
            todo_cancelled: style("todo_cancelled"),
            item_selected: style("item_selected"),
            item: style("item"),
            item_desc: style("item_desc"),
            item_match: Self::build_fallback_style(style, "item_match", "item", "accent"),
            item_match_selected: Self::build_fallback_style(
                style,
                "item_match_selected",
                "item_selected",
                "accent",
            ),
            panel_border: style("panel_border"),
            panel_title: style("panel_title"),
            cursor: style("cursor"),
            input_border: style("input_border"),
            accent: style("accent"),
            active: Self::build_simple_fallback(style, "active", "accent"),
            keybind_key: style("keybind_key"),
            keybind_desc: style("keybind_desc"),
            keybind_section: style("keybind_section"),
            mode_build: derived_color("mode_build", &["keyword.storage.type", "keyword"]),
            mode_plan: derived_color("mode_plan", &["keyword", "keyword.storage.type"]),
            mode_bash: derived_color("mode_bash", &["function.builtin", "function"]),
            queue: style("queue"),
            plan_path: style("plan_path"),
            status_notice: style("status_notice"),
            status_retry_error: style("status_retry_error"),
            status_retry_info: style("status_retry_info"),
            input_placeholder: style("input_placeholder"),
            queue_delete: style("queue_delete"),
            timestamp: style("timestamp"),
            spinner: style("spinner"),
            index_section: derived_style(
                "index_section",
                &["keyword.storage.type", "keyword"],
                Modifier::BOLD,
            ),
            index_line_nr: derived_style("index_line_nr", &["comment"], Modifier::empty()),
            index_keyword: derived_style("index_keyword", &["keyword"], Modifier::empty()),
            shell_prefix: derived_style("shell_prefix", &["string"], Modifier::BOLD),
            progress_bar: Self::build_simple_fallback(style, "progress_bar", "accent"),
            syntax: syntect::highlighting::Theme::default(),
        }
    }

    fn build_fallback_style(
        style: &impl Fn(&str) -> Style,
        primary: &str,
        fallback_base: &str,
        accent: &str,
    ) -> Style {
        let s = style(primary);
        if s == Style::default() {
            style(fallback_base)
                .fg(style(accent).fg.map_or(Color::default(), |c| c))
                .add_modifier(Modifier::BOLD)
        } else {
            s
        }
    }

    fn build_simple_fallback(
        style: &impl Fn(&str) -> Style,
        primary: &str,
        fallback: &str,
    ) -> Style {
        let s = style(primary);
        if s == Style::default() {
            style(fallback)
        } else {
            s
        }
    }
}

pub(crate) fn lerp_u8(a: u8, b: u8, t: f32) -> u8 {
    crate::cast::f32_to_u8(f32::from(a) + (f32::from(b) - f32::from(a)) * t.clamp(0.0, 1.0))
}

pub(crate) fn dim_style(style: Style, factor: f32) -> Style {
    match (style.fg, current().background) {
        (Some(Color::Rgb(fr, fg, fb)), Color::Rgb(br, bg, bb)) => style.fg(Color::Rgb(
            lerp_u8(fr, br, factor),
            lerp_u8(fg, bg, factor),
            lerp_u8(fb, bb, factor),
        )),
        _ => style,
    }
}

fn brighten_toward(style: Style, from: Color, to: Color, t: f32) -> Style {
    match (from, to) {
        (Color::Rgb(fr, fg, fb), Color::Rgb(tr, tg, tb)) => style.fg(Color::Rgb(
            lerp_u8(fr, tr, t),
            lerp_u8(fg, tg, t),
            lerp_u8(fb, tb, t),
        )),
        _ => style,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use test_case::test_case;

    fn dracula_toml() -> &'static str {
        BUNDLED_THEMES
            .iter()
            .find(|e| e.name == "dracula")
            .expect("dracula theme must exist")
            .toml
    }

    fn dracula() -> Theme {
        Theme::from_toml(dracula_toml()).unwrap()
    }

    #[test]
    fn no_color_accessibility_strips_semantic_colors() {
        let mut theme = dracula();
        theme.apply_accessibility(true, false);

        assert_eq!(theme.background, Color::Reset);
        assert_eq!(theme.foreground, Color::Reset);
        assert_eq!(theme.tool_success.fg, None);
        assert_eq!(theme.tool_error.fg, None);
        assert_eq!(theme.accent.fg, None);
        assert_eq!(theme.progress_bar.fg, None);
    }

    #[test]
    fn high_contrast_accessibility_strengthens_semantic_styles() {
        let mut theme = dracula();
        theme.apply_accessibility(false, true);

        assert!(theme.tool_success.add_modifier.contains(Modifier::BOLD));
        assert!(theme.tool_error.add_modifier.contains(Modifier::BOLD));
        assert!(theme.status_dim.add_modifier.contains(Modifier::BOLD));
        assert_eq!(theme.background, Color::Black);
        assert_eq!(theme.foreground, Color::White);
        assert_eq!(theme.accent.fg, Some(Color::Yellow));
        assert_eq!(theme.tool_error.fg, Some(Color::LightRed));
        assert_eq!(theme.diff_old.fg, Some(Color::LightRed));
        assert_eq!(theme.diff_new.fg, Some(Color::LightGreen));
        assert_eq!(theme.todo_completed.fg, Some(Color::Green));
        assert_eq!(theme.todo_in_progress.fg, Some(Color::Yellow));
        assert_eq!(theme.todo_pending.fg, Some(Color::Cyan));
        assert_eq!(theme.todo_cancelled.fg, Some(Color::LightRed));
        assert_eq!(theme.syntax.settings.foreground, Some(SYN_WHITE));
        assert_eq!(theme.syntax.settings.background, Some(SYN_BLACK));
        assert_eq!(theme.syntax.settings.caret, Some(SYN_WHITE));
        assert_eq!(theme.syntax.settings.line_highlight, None);
        assert_eq!(theme.syntax.settings.selection, None);
        assert!(theme.syntax.scopes.iter().all(|item| {
            item.style.foreground == Some(SYN_WHITE) && item.style.background.is_none()
        }));
    }

    #[test]
    fn accessibility_style_list_covers_every_theme_style_field() {
        let mut theme = dracula();
        let listed = theme.styles_mut().len();
        let declared = include_str!("theme.rs")
            .lines()
            .skip_while(|line| !line.starts_with("pub struct Theme {"))
            .skip(1)
            .take_while(|line| *line != "}")
            .filter(|line| {
                line.trim_start().starts_with("pub ") && line.trim_end().ends_with(": Style,")
            })
            .count();

        assert_eq!(listed, declared);
    }

    #[test]
    fn dracula_theme_fields() {
        let t = dracula();
        assert_eq!(t.background, Color::Rgb(0x28, 0x2a, 0x36));
        assert_eq!(t.foreground, Color::Rgb(0xf8, 0xf8, 0xf2));
        assert_eq!(t.user.fg, Some(Color::Rgb(0x8b, 0xe9, 0xfd)));
        assert_eq!(t.error.fg, Some(Color::Rgb(0xff, 0x55, 0x55)));
        assert!(t.bold.add_modifier.contains(Modifier::BOLD));
        assert!(t.thinking.add_modifier.contains(Modifier::ITALIC));
        assert!(t.strikethrough.add_modifier.contains(Modifier::CROSSED_OUT));
        assert_eq!(t.diff_old.bg, Some(Color::Rgb(0x4D, 0x1F, 0x1F)));
        assert_eq!(t.diff_new.bg, Some(Color::Rgb(0x1F, 0x3D, 0x1F)));
        assert_eq!(t.input_border.fg, Some(Color::Rgb(0x62, 0x72, 0xa4)));
    }

    #[test]
    fn dracula_derivations() {
        let t = dracula();
        assert_eq!(t.mode_build, Color::Rgb(0x8b, 0xe9, 0xfd));
        assert_eq!(t.mode_plan, Color::Rgb(0xff, 0x79, 0xc6));
        assert_eq!(t.heading.fg, Some(Color::Rgb(0x8b, 0xe9, 0xfd)));
        assert!(t.heading.add_modifier.contains(Modifier::BOLD));
        assert_eq!(t.inline_code.fg, Some(Color::Rgb(0x50, 0xfa, 0x7b)));
        assert_eq!(t.code_gutter.fg, Some(Color::Rgb(0xff, 0xb8, 0x6c)));
        assert_eq!(t.list_marker.fg, Some(Color::Rgb(0x8b, 0xe9, 0xfd)));
        assert_eq!(t.bold.fg, Some(Color::Rgb(0xff, 0xb8, 0x6c)));
    }

    #[test]
    fn dracula_syntax_scopes() {
        let t = dracula();
        assert!(!t.syntax.scopes.is_empty());
        assert!(t.syntax.settings.foreground.is_some());
        assert!(t.syntax.settings.background.is_some());
    }

    const COMMENT_COLOR: SynColor = SynColor {
        r: 0x62,
        g: 0x72,
        b: 0xa4,
        a: 0xFF,
    };
    const STRING_COLOR: SynColor = SynColor {
        r: 0xf1,
        g: 0xfa,
        b: 0x8c,
        a: 0xFF,
    };
    const PINK_COLOR: SynColor = SynColor {
        r: 0xff,
        g: 0x79,
        b: 0xc6,
        a: 0xFF,
    };
    const CYAN_COLOR: SynColor = SynColor {
        r: 0x8b,
        g: 0xe9,
        b: 0xfd,
        a: 0xFF,
    };

    fn resolve_color_for_scope(
        theme: &syntect::highlighting::Theme,
        scope_str: &str,
    ) -> Option<SynColor> {
        use syntect::parsing::ScopeStack;

        let stack: ScopeStack = scope_str.parse().unwrap();
        let mut best_item: Option<&ThemeItem> = None;
        let mut best_score: f64 = 0.0;
        for item in &theme.scopes {
            if let Some(score) = item.scope.does_match(stack.as_slice())
                && score.0 > best_score
            {
                best_score = score.0;
                best_item = Some(item);
            }
        }
        best_item.and_then(|item| item.style.foreground)
    }

    #[test]
    fn scope_resolution_maps_helix_to_textmate() {
        let t = dracula();
        let cases: &[(&str, SynColor)] = &[
            (
                "source.rust comment.line.double-slash.rust punctuation.definition.comment.rust",
                COMMENT_COLOR,
            ),
            ("source.rust comment.line.double-slash.rust", COMMENT_COLOR),
            (
                "source.rust string.quoted.double.rust punctuation.definition.string.begin.rust",
                STRING_COLOR,
            ),
            ("source.rust meta.generic.rust", CYAN_COLOR),
            (
                "source.rust meta.path.rust punctuation.accessor.rust",
                PINK_COLOR,
            ),
        ];
        for (scope, expected) in cases {
            assert_eq!(
                resolve_color_for_scope(&t.syntax, scope),
                Some(*expected),
                "scope {scope} should resolve correctly"
            );
        }
    }

    #[test]
    fn missing_ui_key_defaults_to_empty_style() {
        let toml = r"
[palette]
[ui]
";
        let theme = Theme::from_toml(toml).unwrap();
        assert_eq!(theme.user, Style::default());
    }

    #[test]
    fn invalid_toml_returns_error() {
        assert!(Theme::from_toml("not valid {{{{").is_err());
    }

    #[test]
    fn all_bundled_themes_parse() {
        for entry in BUNDLED_THEMES {
            let result = Theme::from_toml(entry.toml);
            assert!(
                result.is_ok(),
                "theme '{}' failed to parse: {}",
                entry.name,
                result.unwrap_err()
            );
        }
    }

    #[test]
    fn load_by_name_unknown() {
        assert!(load_by_name("nonexistent").is_err());
    }

    #[test]
    fn helix_theme_loads_without_ui_section() {
        let toml = r##"
"keyword" = { fg = "pink" }
"string" = { fg = "yellow" }
"comment" = { fg = "comment" }

[palette]
foreground = "#f8f8f2"
background = "#282a36"
pink = "#ff79c6"
yellow = "#f1fa8c"
comment = "#6272a4"
"##;
        let theme = Theme::from_toml(toml).unwrap();
        assert!(!theme.syntax.scopes.is_empty());
        assert_eq!(theme.background, Color::Rgb(0x28, 0x2a, 0x36));
    }

    #[test]
    fn ui_override_takes_precedence_over_derivation() {
        let toml = r##"
"keyword.storage.type" = { fg = "cyan" }
"keyword" = { fg = "pink" }
"function.call" = { fg = "green" }

[palette]
foreground = "#f8f8f2"
background = "#282a36"
cyan = "#8be9fd"
pink = "#ff79c6"
green = "#50fa7b"
custom = "#aabbcc"

[ui]
heading = { fg = "custom", modifiers = ["bold"] }
"##;
        let theme = Theme::from_toml(toml).unwrap();
        assert_eq!(theme.heading.fg, Some(Color::Rgb(0xaa, 0xbb, 0xcc)));
        assert_eq!(theme.mode_build, Color::Rgb(0x8b, 0xe9, 0xfd));
    }

    #[test]
    fn derivation_without_ui_section() {
        let toml = r##"
"keyword.storage.type" = { fg = "#8be9fd" }
"keyword" = { fg = "#ff79c6" }
"constant" = { fg = "#bd93f9" }
"function.call" = { fg = "#50fa7b" }
"variable.parameter" = { fg = "#ffb86c" }
"markup.bold" = { fg = "#ffb86c" }

[palette]
foreground = "#f8f8f2"
background = "#282a36"
"##;
        let theme = Theme::from_toml(toml).unwrap();
        assert_eq!(theme.mode_build, Color::Rgb(0x8b, 0xe9, 0xfd));
        assert_eq!(theme.mode_plan, Color::Rgb(0xff, 0x79, 0xc6));
        assert_eq!(theme.heading.fg, Some(Color::Rgb(0x8b, 0xe9, 0xfd)));
        assert!(theme.heading.add_modifier.contains(Modifier::BOLD));
        assert_eq!(theme.inline_code.fg, Some(Color::Rgb(0x50, 0xfa, 0x7b)));
        assert_eq!(theme.code_gutter.fg, Some(Color::Rgb(0xff, 0xb8, 0x6c)));
    }

    #[test]
    fn palette_override_takes_precedence_for_color() {
        let toml = r##"
"keyword.storage.type" = { fg = "#8be9fd" }

[palette]
foreground = "#f8f8f2"
background = "#282a36"
mode_build = "#112233"
"##;
        let theme = Theme::from_toml(toml).unwrap();
        assert_eq!(theme.mode_build, Color::Rgb(0x11, 0x22, 0x33));
    }

    #[test]
    fn style_by_name_resolves() {
        let _guard = THEME_TEST_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        set(dracula());
        let t = current();
        assert_eq!(style_by_name("dim"), t.tool_dim);
        assert_eq!(style_by_name("tool_dim"), t.tool_dim);
        assert_eq!(style_by_name("path"), t.tool_path);
        assert_eq!(style_by_name("tool_path"), t.tool_path);
        assert_eq!(style_by_name("keyword"), t.index_keyword);
        assert_eq!(style_by_name("index_keyword"), t.index_keyword);
        assert_eq!(style_by_name("section"), t.index_section);
        assert_eq!(style_by_name("index_section"), t.index_section);
        assert_eq!(style_by_name("line_nr"), t.index_line_nr);
        assert_eq!(style_by_name("index_line_nr"), t.index_line_nr);
        assert_eq!(style_by_name("tool"), t.tool);
        assert_eq!(style_by_name("error"), t.error);
        assert_eq!(style_by_name("bold"), t.bold);
        assert_eq!(style_by_name("italic"), t.italic);
        assert_eq!(style_by_name("bold_italic"), t.bold_italic);
        assert_eq!(style_by_name("diff_old"), t.diff_old);
        assert_eq!(style_by_name("diff_new"), t.diff_new);
        assert_eq!(style_by_name("item_selected"), t.item_selected);
        assert_eq!(style_by_name("item"), t.item);
        assert_eq!(style_by_name("item_desc"), t.item_desc);
        assert_eq!(style_by_name("cursor"), t.cursor);
        assert_eq!(style_by_name("accent"), t.accent);
        assert_eq!(style_by_name("active"), t.active);
        assert_eq!(style_by_name("foreground"), Style::new().fg(t.foreground));
        assert_eq!(style_by_name("keybind_key"), t.keybind_key);
        assert_eq!(style_by_name("keybind_desc"), t.keybind_desc);
        assert_eq!(style_by_name("selected"), t.item_selected);
        assert_eq!(style_by_name("success"), t.todo_completed);
        assert_eq!(style_by_name("warning"), t.todo_in_progress);
        assert_eq!(style_by_name("match"), t.item_match);
        assert_eq!(style_by_name("match_selected"), t.item_match_selected);
    }

    #[test_case("nonexistent_style")]
    #[test_case("")]
    #[test_case("typo_keyword")]
    fn style_by_name_unknown_returns_default(name: &str) {
        assert_eq!(style_by_name(name), Style::default());
    }

    const DRACULA_BG: Color = Color::Rgb(0x28, 0x2a, 0x36);
    const TOKYONIGHT_BG: Color = Color::Rgb(0x1a, 0x1b, 0x26);

    fn tokyonight() -> Theme {
        load_by_name("tokyonight").expect("tokyonight theme must exist")
    }

    #[test]
    fn set_advances_generation() {
        let _guard = THEME_TEST_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let before = generation();
        set(dracula());
        assert!(generation() > before);
    }

    #[test]
    fn set_installs_theme_before_generation_observed() {
        let _guard = THEME_TEST_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let theme = tokyonight();
        let expected_syntax_bg = theme.syntax.settings.background;
        let before = generation();

        set(theme);

        let observed = generation();
        assert!(observed > before);
        assert_eq!(current().background, TOKYONIGHT_BG);
        assert_eq!(
            n00n_highlight::theme().settings.background,
            expected_syntax_bg,
            "syntax palette must reflect the new theme once generation advances",
        );
    }

    #[test]
    fn set_generation_is_monotonic_across_switches() {
        let _guard = THEME_TEST_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let g0 = generation();
        set(dracula());
        let g1 = generation();
        assert!(g1 > g0);
        assert_eq!(current().background, DRACULA_BG);

        set(tokyonight());
        let g2 = generation();
        assert!(g2 > g1);
        assert_eq!(current().background, TOKYONIGHT_BG);
        assert_eq!(
            n00n_highlight::theme().settings.background,
            tokyonight().syntax.settings.background,
        );
    }
}

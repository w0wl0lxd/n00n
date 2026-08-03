use crate::components::ModalScroll;
use crate::components::Overlay;
use crate::components::keybindings::{
    ALT_SEP, KEYBINDS, KeybindContext, ResolvedLabel, all_contexts, key,
};
use crate::components::modal::Modal;
use crate::components::scrollbar::render_vertical_scrollbar;
use crate::theme;

use crossterm::event::{KeyCode, KeyEvent};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use unicode_width::UnicodeWidthStr;

const TITLE: &str = " Keybindings ";
const KEY_COL_GAP: usize = 2;
const PREFIX_TOP: &str = "  ";
const PREFIX_CHILD: &str = "    ";

const INPUT_PREFIXES: &[(&str, &str)] = &[
    ("!", "Run shell command (visible to agent)"),
    ("!!", "Run shell command (hidden from agent)"),
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum HelpTab {
    All,
    Chat,
    Editing,
    Pickers,
}

impl HelpTab {
    const ALL: [Self; 4] = [Self::All, Self::Chat, Self::Editing, Self::Pickers];

    fn label(self) -> &'static str {
        match self {
            Self::All => "All",
            Self::Chat => "Chat",
            Self::Editing => "Editing",
            Self::Pickers => "Pickers",
        }
    }

    fn includes(self, context: KeybindContext) -> bool {
        match self {
            Self::All => true,
            Self::Chat => matches!(context, KeybindContext::General | KeybindContext::Streaming),
            Self::Editing => context == KeybindContext::Editing,
            Self::Pickers => {
                context == KeybindContext::Picker
                    || context.parent() == Some(KeybindContext::Picker)
            }
        }
    }
}

pub struct HelpModal {
    open: bool,
    scroll: ModalScroll,
    tab: HelpTab,
    search: String,
    searching: bool,
}

fn matches_help(tab: HelpTab, query: &str, context: KeybindContext, description: &str) -> bool {
    if !tab.includes(context) {
        return false;
    }
    let query = query.trim().to_lowercase();
    query.is_empty()
        || context.label().to_lowercase().contains(&query)
        || description.to_lowercase().contains(&query)
}

fn key_spans(label: ResolvedLabel, pad: usize, prefix: &str) -> Vec<Span<'static>> {
    let theme = theme::current();
    match label {
        ResolvedLabel::Single(s) => {
            let w = UnicodeWidthStr::width(s);
            let trailing = pad.saturating_sub(w);
            vec![Span::styled(
                format!("{prefix}{s}{:trailing$}", ""),
                theme.keybind_key,
            )]
        }
        ResolvedLabel::Alt(a, b) => multi_key_spans(&[a, b], pad, prefix, &theme),
        ResolvedLabel::Multi(keys) => multi_key_spans(keys, pad, prefix, &theme),
    }
}

fn multi_key_spans(
    keys: &[&'static str],
    pad: usize,
    prefix: &str,
    theme: &crate::theme::Theme,
) -> Vec<Span<'static>> {
    let sep_w = UnicodeWidthStr::width(ALT_SEP);
    let content_w: usize = keys
        .iter()
        .map(|k| UnicodeWidthStr::width(*k))
        .sum::<usize>()
        + sep_w * keys.len().saturating_sub(1);
    let trailing = pad.saturating_sub(content_w);
    let mut spans = Vec::with_capacity(keys.len() * 2);
    for (i, k) in keys.iter().enumerate() {
        if i > 0 {
            spans.push(Span::styled(ALT_SEP, theme.keybind_desc));
        }
        let text = if i == 0 && i == keys.len() - 1 {
            format!("{prefix}{k}{:trailing$}", "")
        } else if i == 0 {
            format!("{prefix}{k}")
        } else if i == keys.len() - 1 {
            format!("{k}{:trailing$}", "")
        } else {
            (*k).to_string()
        };
        spans.push(Span::styled(text, theme.keybind_key));
    }
    spans
}

impl HelpModal {
    pub fn new() -> Self {
        Self {
            open: false,
            scroll: ModalScroll::new_top(),
            tab: HelpTab::All,
            search: String::new(),
            searching: false,
        }
    }

    pub fn is_open(&self) -> bool {
        self.open
    }

    pub fn toggle(&mut self) {
        self.open = !self.open;
        self.reset_view();
    }

    pub fn close(&mut self) {
        self.open = false;
        self.reset_view();
    }

    fn reset_view(&mut self) {
        self.scroll.reset();
        self.tab = HelpTab::All;
        self.search.clear();
        self.searching = false;
    }

    pub fn scroll(&mut self, delta: i32) {
        self.scroll.scroll(delta);
    }

    pub fn handle_key(&mut self, key_event: KeyEvent) -> bool {
        if self.searching {
            match key_event.code {
                KeyCode::Esc => {
                    self.search.clear();
                    self.searching = false;
                    self.scroll.reset();
                    return true;
                }
                KeyCode::Enter => {
                    self.searching = false;
                    self.scroll.reset();
                    return true;
                }
                KeyCode::Backspace => {
                    self.search.pop();
                    self.scroll.reset();
                    return true;
                }
                KeyCode::Char(c)
                    if !key_event.modifiers.intersects(
                        crossterm::event::KeyModifiers::CONTROL
                            | crossterm::event::KeyModifiers::ALT,
                    ) =>
                {
                    self.search.push(c);
                    self.scroll.reset();
                    return true;
                }
                _ => return true,
            }
        }

        if key_event.code == KeyCode::Char('/') {
            self.searching = true;
            return true;
        }
        if key_event.code == KeyCode::Tab {
            let current = HelpTab::ALL
                .iter()
                .position(|tab| *tab == self.tab)
                .unwrap_or_else(|| 0);
            self.tab = HelpTab::ALL[(current + 1) % HelpTab::ALL.len()];
            self.scroll.reset();
            return true;
        }
        if let KeyCode::Char(c @ '1'..='4') = key_event.code {
            self.tab = HelpTab::ALL[usize::from(c as u8 - b'1')];
            self.scroll.reset();
            return true;
        }
        let close = key_event.code == KeyCode::Esc
            || key::HELP.matches(key_event)
            || key::QUIT.matches(key_event);
        if close {
            self.close();
            return true;
        }
        self.scroll.handle_key(key_event);
        true
    }

    pub fn view(&mut self, frame: &mut Frame, area: Rect) -> Rect {
        if !self.open {
            return Rect::default();
        }

        let mut lines: Vec<Line> = Vec::new();
        let theme = theme::current();
        let query = self.search.as_str();

        let tabs: Vec<Span<'static>> = HelpTab::ALL
            .iter()
            .enumerate()
            .map(|(i, tab)| {
                let marker = if *tab == self.tab { "[" } else { " " };
                let end = if *tab == self.tab { "]" } else { " " };
                Span::styled(
                    format!("{marker}{} {}{end} ", i + 1, tab.label()),
                    if *tab == self.tab {
                        theme.keybind_key
                    } else {
                        theme.tool_dim
                    },
                )
            })
            .collect();
        lines.push(Line::from(tabs));
        let search_label = if self.searching {
            format!(
                "  Search (type, Enter to keep, Esc to clear): {}",
                self.search
            )
        } else if query.is_empty() {
            "  Search: / to filter keybindings".to_string()
        } else {
            format!("  Search: {query} (press / to edit)")
        };
        lines.push(Line::from(Span::styled(search_label, theme.tool_dim)));

        let key_col_width = KEYBINDS
            .iter()
            .filter(|kb| {
                kb.platform.is_visible()
                    && matches_help(self.tab, query, kb.context, kb.description)
            })
            .map(|kb| kb.label.resolve().display_width())
            .max()
            .unwrap_or_else(|| 0)
            + KEY_COL_GAP;
        let mut first = true;
        for ctx in all_contexts() {
            if ctx.parent().is_some() {
                continue;
            }
            if !KEYBINDS.iter().any(|kb| {
                kb.platform.is_visible() && matches_help(self.tab, query, ctx, kb.description)
            }) {
                continue;
            }
            if !first {
                lines.push(Line::default());
            }
            first = false;

            lines.push(Line::from(Span::styled(
                format!("  {}", ctx.label()),
                theme.keybind_section,
            )));

            for kb in KEYBINDS.iter().filter(|kb| {
                kb.context == ctx
                    && kb.platform.is_visible()
                    && matches_help(self.tab, query, ctx, kb.description)
            }) {
                let mut spans = key_spans(kb.label.resolve(), key_col_width, PREFIX_TOP);
                spans.push(Span::styled(kb.description, theme.keybind_desc));
                lines.push(Line::from(spans));
            }

            for child in all_contexts() {
                if child.parent() != Some(ctx) {
                    continue;
                }
                let child_binds: Vec<_> = KEYBINDS
                    .iter()
                    .filter(|kb| {
                        kb.context == child
                            && kb.platform.is_visible()
                            && matches_help(self.tab, query, child, kb.description)
                    })
                    .collect();
                if child_binds.is_empty() {
                    continue;
                }
                lines.push(Line::default());
                lines.push(Line::from(Span::styled(
                    format!("    {}", child.label()),
                    theme.keybind_section,
                )));
                for kb in child_binds {
                    let mut spans = key_spans(
                        kb.label.resolve(),
                        key_col_width - KEY_COL_GAP,
                        PREFIX_CHILD,
                    );
                    spans.push(Span::styled(kb.description, theme.keybind_desc));
                    lines.push(Line::from(spans));
                }
            }

            if ctx == KeybindContext::Editing {
                lines.push(Line::default());
                lines.push(Line::from(Span::styled(
                    "    Input Prefixes",
                    theme.keybind_section,
                )));
                for &(pfx, desc) in INPUT_PREFIXES {
                    let mut spans = key_spans(
                        ResolvedLabel::Single(pfx),
                        key_col_width - KEY_COL_GAP,
                        PREFIX_CHILD,
                    );
                    spans.push(Span::styled(desc, theme.keybind_desc));
                    lines.push(Line::from(spans));
                }
            }
        }

        let total = u16::try_from(lines.len()).unwrap_or_else(|_| u16::MAX);
        let modal = Modal {
            title: TITLE,
            width_percent: 50,
            max_height_percent: 80,
        };
        let (popup, inner) = modal.render(frame, area, total);
        let viewport_h = inner.height;
        self.scroll.update_dimensions(total, viewport_h);
        let scroll = self.scroll.offset();

        let paragraph = Paragraph::new(lines).scroll((scroll, 0));
        frame.render_widget(paragraph, inner);

        if total > viewport_h {
            render_vertical_scrollbar(frame, inner, total, scroll, None);
        }

        popup
    }
}

impl Overlay for HelpModal {
    fn is_open(&self) -> bool {
        self.is_open()
    }

    fn close(&mut self) {
        self.close();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::components::key as key_ev;
    use crossterm::event::KeyCode;
    use test_case::test_case;

    #[test_case(key_ev(KeyCode::Esc)       ; "esc_closes")]
    #[test_case(key::QUIT.to_key_event()    ; "ctrl_c_closes")]
    #[test_case(key::HELP.to_key_event()    ; "ctrl_h_closes")]
    fn handle_key_closes(k: KeyEvent) {
        let mut modal = HelpModal::new();
        modal.toggle();
        assert!(modal.handle_key(k));
        assert!(!modal.is_open());
    }

    #[test]
    fn handle_key_consumes_all() {
        let mut modal = HelpModal::new();
        modal.toggle();
        assert!(modal.handle_key(key_ev(KeyCode::Char('a'))));
        assert!(modal.is_open());
    }

    #[test]
    fn tabs_and_search_are_keyboard_accessible() {
        let mut modal = HelpModal::new();
        modal.toggle();
        assert!(modal.handle_key(key_ev(KeyCode::Char('3'))));
        assert_eq!(modal.tab, HelpTab::Editing);
        assert!(modal.handle_key(key_ev(KeyCode::Char('/'))));
        assert!(modal.handle_key(key_ev(KeyCode::Char('s'))));
        assert_eq!(modal.search, "s");
        assert!(modal.handle_key(key_ev(KeyCode::Enter)));
        assert!(!modal.searching);
        assert!(modal.is_open());
    }
}

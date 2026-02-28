use crate::text_buffer::TextBuffer;
use crate::theme;

use crossterm::event::{KeyCode, KeyEvent};
use maki_providers::QuestionInfo;
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Paragraph, Wrap};

const FORM_LABEL: &str = " Questions ";
const CUSTOM_OPTION: &str = "Type your own answer";
const HINT_BAR: &str = "↑↓ select  Enter confirm  Tab next  Esc dismiss";

pub enum QuestionFormAction {
    Consumed,
    Submit(String),
    Dismiss,
}

pub struct QuestionForm {
    questions: Vec<QuestionInfo>,
    current_tab: usize,
    selected: usize,
    answers: Vec<Vec<String>>,
    editing_custom: bool,
    buffer: TextBuffer,
    visible: bool,
}

impl QuestionForm {
    pub fn new() -> Self {
        Self {
            questions: Vec::new(),
            current_tab: 0,
            selected: 0,
            answers: Vec::new(),
            editing_custom: false,
            buffer: TextBuffer::new(String::new()),
            visible: false,
        }
    }

    pub fn is_visible(&self) -> bool {
        self.visible
    }

    pub fn open(&mut self, questions: Vec<QuestionInfo>) {
        let n = questions.len();
        self.answers = vec![Vec::new(); n];
        self.questions = questions;
        self.current_tab = 0;
        self.selected = 0;
        self.editing_custom = false;
        self.buffer = TextBuffer::new(String::new());
        self.visible = true;
    }

    pub fn close(&mut self) {
        self.visible = false;
        self.questions.clear();
    }

    fn is_multi(&self) -> bool {
        self.questions.len() > 1
    }

    fn on_confirm_tab(&self) -> bool {
        self.is_multi() && self.current_tab == self.questions.len()
    }

    fn option_count(&self) -> usize {
        if self.on_confirm_tab() {
            return 0;
        }
        self.questions[self.current_tab].options.len() + 1
    }

    fn total_tabs(&self) -> usize {
        if self.is_multi() {
            self.questions.len() + 1
        } else {
            1
        }
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> QuestionFormAction {
        if !self.visible {
            return QuestionFormAction::Consumed;
        }

        if self.editing_custom {
            return self.handle_custom_key(key);
        }

        if super::is_ctrl(&key) {
            return QuestionFormAction::Consumed;
        }

        match key.code {
            KeyCode::Up => {
                if self.selected > 0 {
                    self.selected -= 1;
                }
                QuestionFormAction::Consumed
            }
            KeyCode::Down => {
                if self.selected + 1 < self.option_count() {
                    self.selected += 1;
                }
                QuestionFormAction::Consumed
            }
            KeyCode::Tab | KeyCode::Right if self.is_multi() => {
                self.next_tab();
                QuestionFormAction::Consumed
            }
            KeyCode::BackTab | KeyCode::Left if self.is_multi() => {
                self.prev_tab();
                QuestionFormAction::Consumed
            }
            KeyCode::Enter => self.handle_enter(),
            KeyCode::Esc => QuestionFormAction::Dismiss,
            _ => QuestionFormAction::Consumed,
        }
    }

    fn handle_custom_key(&mut self, key: KeyEvent) -> QuestionFormAction {
        if super::is_ctrl(&key) {
            if key.code == KeyCode::Char('w') {
                self.buffer.remove_word_before_cursor();
            }
            return QuestionFormAction::Consumed;
        }

        match key.code {
            KeyCode::Enter => {
                let text = self.buffer.value().trim().to_string();
                if !text.is_empty() {
                    self.answers[self.current_tab] = vec![text];
                }
                self.editing_custom = false;
                if !self.is_multi() {
                    return self.build_submit();
                }
                self.next_tab();
                QuestionFormAction::Consumed
            }
            KeyCode::Esc => {
                self.editing_custom = false;
                QuestionFormAction::Consumed
            }
            KeyCode::Char(c) => self.buffer_key(|b| b.push_char(c)),
            KeyCode::Backspace => self.buffer_key(|b| b.remove_char()),
            KeyCode::Delete => self.buffer_key(|b| b.delete_char()),
            KeyCode::Left => self.buffer_key(|b| b.move_left()),
            KeyCode::Right => self.buffer_key(|b| b.move_right()),
            KeyCode::Home => self.buffer_key(|b| b.move_home()),
            KeyCode::End => self.buffer_key(|b| b.move_end()),
            _ => QuestionFormAction::Consumed,
        }
    }

    fn buffer_key(&mut self, f: impl FnOnce(&mut TextBuffer)) -> QuestionFormAction {
        f(&mut self.buffer);
        QuestionFormAction::Consumed
    }

    fn handle_enter(&mut self) -> QuestionFormAction {
        if self.on_confirm_tab() {
            return self.build_submit();
        }

        let q = &self.questions[self.current_tab];
        let custom_idx = q.options.len();

        if self.selected == custom_idx {
            let existing = self.answers[self.current_tab]
                .first()
                .cloned()
                .unwrap_or_default();
            self.buffer = TextBuffer::new(existing);
            self.editing_custom = true;
            return QuestionFormAction::Consumed;
        }

        let label = q.options[self.selected].label.clone();
        let answers = &mut self.answers[self.current_tab];

        if q.multiple {
            if let Some(pos) = answers.iter().position(|a| a == &label) {
                answers.remove(pos);
            } else {
                answers.push(label);
            }
            QuestionFormAction::Consumed
        } else {
            *answers = vec![label];
            if !self.is_multi() {
                return self.build_submit();
            }
            self.next_tab();
            QuestionFormAction::Consumed
        }
    }

    fn build_submit(&self) -> QuestionFormAction {
        let json = serde_json::to_string(&self.answers).unwrap_or_default();
        QuestionFormAction::Submit(json)
    }

    fn next_tab(&mut self) {
        if self.current_tab + 1 < self.total_tabs() {
            self.current_tab += 1;
            self.selected = 0;
        }
    }

    fn prev_tab(&mut self) {
        if self.current_tab > 0 {
            self.current_tab -= 1;
            self.selected = 0;
        }
    }

    pub fn view(&self, frame: &mut Frame, area: Rect) {
        if !self.visible {
            return;
        }

        let mut lines: Vec<Line> = Vec::new();

        if self.is_multi() {
            lines.push(self.render_tab_bar());
            lines.push(Line::default());
        }

        if self.on_confirm_tab() {
            self.render_confirm(&mut lines);
        } else {
            self.render_question(&mut lines);
        }

        lines.push(Line::default());
        lines.push(Line::from(Span::styled(
            HINT_BAR,
            Style::new().fg(theme::COMMENT),
        )));

        let block = Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::new().fg(theme::INPUT_BORDER))
            .title_top(Line::from(FORM_LABEL).left_aligned());

        let paragraph = Paragraph::new(lines)
            .style(Style::new().fg(theme::FOREGROUND))
            .wrap(Wrap { trim: false })
            .block(block);

        frame.render_widget(paragraph, area);
    }

    fn render_tab_bar(&self) -> Line<'static> {
        let mut spans = Vec::new();
        for (i, q) in self.questions.iter().enumerate() {
            if i > 0 {
                spans.push(Span::styled(" │ ", Style::new().fg(theme::COMMENT)));
            }
            let label = if q.header.is_empty() {
                format!("Q{}", i + 1)
            } else {
                q.header.clone()
            };
            let has_answer = !self.answers[i].is_empty();
            let style = if i == self.current_tab {
                Style::new().fg(theme::CYAN)
            } else if has_answer {
                Style::new().fg(theme::GREEN)
            } else {
                Style::new().fg(theme::COMMENT)
            };
            spans.push(Span::styled(label, style));
        }
        spans.push(Span::styled(" │ ", Style::new().fg(theme::COMMENT)));
        let confirm_style = if self.on_confirm_tab() {
            Style::new().fg(theme::CYAN)
        } else {
            Style::new().fg(theme::COMMENT)
        };
        spans.push(Span::styled("Confirm", confirm_style));
        Line::from(spans)
    }

    fn render_question(&self, lines: &mut Vec<Line<'static>>) {
        let q = &self.questions[self.current_tab];
        lines.push(Line::from(Span::styled(
            q.question.clone(),
            Style::new().fg(theme::FOREGROUND),
        )));
        lines.push(Line::default());

        let answers = &self.answers[self.current_tab];

        for (i, opt) in q.options.iter().enumerate() {
            let is_selected = i == self.selected;
            let is_picked = answers.contains(&opt.label);
            let marker = if is_picked { "✓ " } else { "  " };
            let prefix = if is_selected { "▸ " } else { "  " };

            let style = if is_selected {
                Style::new().fg(theme::CYAN)
            } else if is_picked {
                Style::new().fg(theme::GREEN)
            } else {
                Style::new().fg(theme::FOREGROUND)
            };

            let mut spans = vec![
                Span::styled(prefix.to_string(), style),
                Span::styled(marker.to_string(), Style::new().fg(theme::GREEN)),
                Span::styled(opt.label.clone(), style),
            ];

            if !opt.description.is_empty() {
                spans.push(Span::styled(
                    format!(" — {}", opt.description),
                    Style::new().fg(theme::COMMENT),
                ));
            }
            lines.push(Line::from(spans));
        }

        let custom_idx = q.options.len();
        let is_custom_selected = self.selected == custom_idx;
        let custom_style = if is_custom_selected {
            Style::new().fg(theme::CYAN)
        } else {
            Style::new().fg(theme::COMMENT)
        };
        let prefix = if is_custom_selected { "▸ " } else { "  " };
        lines.push(Line::from(vec![
            Span::styled(prefix.to_string(), custom_style),
            Span::styled(format!("  {CUSTOM_OPTION}"), custom_style),
        ]));

        if self.editing_custom {
            self.render_text_input(lines);
        }
    }

    fn render_text_input(&self, lines: &mut Vec<Line<'static>>) {
        let val = self.buffer.value();
        let byte_x = TextBuffer::char_to_byte(&val, self.buffer.x());
        let (before, after) = val.split_at(byte_x);
        let mut chars = after.chars();
        let cursor_ch = chars.next().map_or(" ".to_string(), |c| c.to_string());
        let mut spans = vec![
            Span::styled("  → ", Style::new().fg(theme::COMMENT)),
            Span::raw(before.to_string()),
            Span::styled(cursor_ch, Style::new().reversed()),
        ];
        let rest: String = chars.collect();
        if !rest.is_empty() {
            spans.push(Span::raw(rest));
        }
        lines.push(Line::from(spans));
    }

    fn render_confirm(&self, lines: &mut Vec<Line<'static>>) {
        lines.push(Line::from(Span::styled(
            "Review your answers:",
            Style::new().fg(theme::FOREGROUND),
        )));
        lines.push(Line::default());

        for (i, q) in self.questions.iter().enumerate() {
            let answer_text = if self.answers[i].is_empty() {
                "(no answer)".to_string()
            } else {
                self.answers[i].join(", ")
            };
            lines.push(Line::from(vec![
                Span::styled(format!("{}. ", i + 1), Style::new().fg(theme::COMMENT)),
                Span::styled(q.question.clone(), Style::new().fg(theme::FOREGROUND)),
                Span::styled(" → ", Style::new().fg(theme::COMMENT)),
                Span::styled(answer_text, Style::new().fg(theme::GREEN)),
            ]));
        }

        lines.push(Line::default());
        lines.push(Line::from(Span::styled(
            "Press Enter to submit, or navigate back to edit.",
            Style::new().fg(theme::COMMENT),
        )));
    }

    pub fn height(&self) -> u16 {
        if !self.visible {
            return 0;
        }

        let chrome = 2 + 1 + 1; // border(2) + empty line before hint + hint line

        if self.on_confirm_tab() {
            let review_lines = 1 + 1 + self.questions.len() + 1 + 1; // header + empty + questions + empty + instruction
            let tabs = if self.is_multi() { 2 } else { 0 };
            return (chrome + review_lines + tabs) as u16;
        }

        let q = &self.questions[self.current_tab];
        let option_lines = q.options.len() + 1; // +1 for custom option
        let question_lines = 1 + 1; // question text + empty line
        let tabs = if self.is_multi() { 2 } else { 0 };
        let custom_input = if self.editing_custom { 1 } else { 0 };

        (chrome + question_lines + option_lines + tabs + custom_input) as u16
    }
}

#[cfg(test)]
mod tests {
    use maki_providers::{QuestionInfo, QuestionOption};

    use super::*;
    use crate::components::key;

    fn single_q_with_options() -> Vec<QuestionInfo> {
        vec![QuestionInfo {
            question: "Pick a DB".into(),
            header: "DB".into(),
            options: vec![
                QuestionOption {
                    label: "PostgreSQL".into(),
                    description: "Relational".into(),
                },
                QuestionOption {
                    label: "Redis".into(),
                    description: "Key-value".into(),
                },
            ],
            multiple: false,
        }]
    }

    fn multi_q() -> Vec<QuestionInfo> {
        vec![
            QuestionInfo {
                question: "Language?".into(),
                header: "Lang".into(),
                options: vec![
                    QuestionOption {
                        label: "Rust".into(),
                        description: String::new(),
                    },
                    QuestionOption {
                        label: "Go".into(),
                        description: String::new(),
                    },
                ],
                multiple: false,
            },
            QuestionInfo {
                question: "Framework?".into(),
                header: "FW".into(),
                options: vec![
                    QuestionOption {
                        label: "Axum".into(),
                        description: String::new(),
                    },
                    QuestionOption {
                        label: "Actix".into(),
                        description: String::new(),
                    },
                ],
                multiple: false,
            },
        ]
    }

    fn q_no_options() -> Vec<QuestionInfo> {
        vec![QuestionInfo {
            question: "What's your name?".into(),
            header: String::new(),
            options: vec![],
            multiple: false,
        }]
    }

    #[test]
    fn single_question_select_option_immediately_submits() {
        let mut form = QuestionForm::new();
        form.open(single_q_with_options());

        let action = form.handle_key(key(KeyCode::Enter));
        match action {
            QuestionFormAction::Submit(json) => {
                let parsed: Vec<Vec<String>> = serde_json::from_str(&json).unwrap();
                assert_eq!(parsed, vec![vec!["PostgreSQL"]]);
            }
            _ => panic!("expected Submit"),
        }
    }

    #[test]
    fn navigate_down_and_select_second_option() {
        let mut form = QuestionForm::new();
        form.open(single_q_with_options());
        form.handle_key(key(KeyCode::Down));

        let action = form.handle_key(key(KeyCode::Enter));
        match action {
            QuestionFormAction::Submit(json) => {
                let parsed: Vec<Vec<String>> = serde_json::from_str(&json).unwrap();
                assert_eq!(parsed, vec![vec!["Redis"]]);
            }
            _ => panic!("expected Submit"),
        }
    }

    #[test]
    fn custom_input_flow() {
        let mut form = QuestionForm::new();
        form.open(single_q_with_options());

        form.handle_key(key(KeyCode::Down));
        form.handle_key(key(KeyCode::Down));
        form.handle_key(key(KeyCode::Enter));
        assert!(form.editing_custom);

        for c in "MongoDB".chars() {
            form.handle_key(key(KeyCode::Char(c)));
        }
        let action = form.handle_key(key(KeyCode::Enter));
        match action {
            QuestionFormAction::Submit(json) => {
                let parsed: Vec<Vec<String>> = serde_json::from_str(&json).unwrap();
                assert_eq!(parsed, vec![vec!["MongoDB"]]);
            }
            _ => panic!("expected Submit"),
        }
    }

    #[test]
    fn esc_dismisses() {
        let mut form = QuestionForm::new();
        form.open(single_q_with_options());
        let action = form.handle_key(key(KeyCode::Esc));
        assert!(matches!(action, QuestionFormAction::Dismiss));
    }

    #[test]
    fn esc_in_custom_mode_exits_edit_not_form() {
        let mut form = QuestionForm::new();
        form.open(single_q_with_options());
        form.handle_key(key(KeyCode::Down));
        form.handle_key(key(KeyCode::Down));
        form.handle_key(key(KeyCode::Enter));
        assert!(form.editing_custom);

        let action = form.handle_key(key(KeyCode::Esc));
        assert!(matches!(action, QuestionFormAction::Consumed));
        assert!(!form.editing_custom);
        assert!(form.visible);
    }

    #[test]
    fn multi_question_tab_navigation_and_confirm() {
        let mut form = QuestionForm::new();
        form.open(multi_q());
        assert_eq!(form.current_tab, 0);

        form.handle_key(key(KeyCode::Enter));
        assert_eq!(form.current_tab, 1);
        assert_eq!(form.answers[0], vec!["Rust"]);

        form.handle_key(key(KeyCode::Down));
        form.handle_key(key(KeyCode::Enter));
        assert_eq!(form.current_tab, 2);
        assert!(form.on_confirm_tab());
        assert_eq!(form.answers[1], vec!["Actix"]);

        let action = form.handle_key(key(KeyCode::Enter));
        match action {
            QuestionFormAction::Submit(json) => {
                let parsed: Vec<Vec<String>> = serde_json::from_str(&json).unwrap();
                assert_eq!(parsed, vec![vec!["Rust"], vec!["Actix"]]);
            }
            _ => panic!("expected Submit"),
        }
    }

    #[test]
    fn back_tab_navigates_backward() {
        let mut form = QuestionForm::new();
        form.open(multi_q());

        form.handle_key(key(KeyCode::Tab));
        assert_eq!(form.current_tab, 1);

        form.handle_key(key(KeyCode::BackTab));
        assert_eq!(form.current_tab, 0);

        form.handle_key(key(KeyCode::BackTab));
        assert_eq!(form.current_tab, 0);
    }

    #[test]
    fn up_down_clamped() {
        let mut form = QuestionForm::new();
        form.open(single_q_with_options());

        form.handle_key(key(KeyCode::Up));
        assert_eq!(form.selected, 0);

        form.handle_key(key(KeyCode::Down));
        form.handle_key(key(KeyCode::Down));
        form.handle_key(key(KeyCode::Down));
        assert_eq!(form.selected, 2);
    }

    #[test]
    fn no_options_shows_only_custom() {
        let mut form = QuestionForm::new();
        form.open(q_no_options());
        assert_eq!(form.option_count(), 1);
        assert_eq!(form.selected, 0);

        form.handle_key(key(KeyCode::Enter));
        assert!(form.editing_custom);
    }

    #[test]
    fn multiple_selection_toggles() {
        let mut form = QuestionForm::new();
        form.open(vec![QuestionInfo {
            question: "Pick features".into(),
            header: String::new(),
            options: vec![
                QuestionOption {
                    label: "A".into(),
                    description: String::new(),
                },
                QuestionOption {
                    label: "B".into(),
                    description: String::new(),
                },
            ],
            multiple: true,
        }]);

        form.handle_key(key(KeyCode::Enter));
        assert_eq!(form.answers[0], vec!["A"]);

        form.handle_key(key(KeyCode::Enter));
        assert!(form.answers[0].is_empty());

        form.handle_key(key(KeyCode::Enter));
        form.handle_key(key(KeyCode::Down));
        form.handle_key(key(KeyCode::Enter));
        assert_eq!(form.answers[0], vec!["A", "B"]);
    }

    #[test]
    fn height_changes_with_editing_custom() {
        let mut form = QuestionForm::new();
        form.open(single_q_with_options());
        let h1 = form.height();

        form.handle_key(key(KeyCode::Down));
        form.handle_key(key(KeyCode::Down));
        form.handle_key(key(KeyCode::Enter));
        let h2 = form.height();

        assert!(h2 > h1);
    }

    #[test]
    fn empty_custom_input_not_stored() {
        let mut form = QuestionForm::new();
        form.open(single_q_with_options());

        form.handle_key(key(KeyCode::Down));
        form.handle_key(key(KeyCode::Down));
        form.handle_key(key(KeyCode::Enter));
        assert!(form.editing_custom);

        let action = form.handle_key(key(KeyCode::Enter));
        match action {
            QuestionFormAction::Submit(json) => {
                let parsed: Vec<Vec<String>> = serde_json::from_str(&json).unwrap();
                assert!(parsed[0].is_empty());
            }
            _ => panic!("expected Submit"),
        }
    }
}

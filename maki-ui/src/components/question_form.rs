use crate::components::form::{overlay_impl, render_form, selected_prefix};
use crate::components::hint_line;
use crate::components::keybindings::key;
use crate::markdown::text_to_lines;
use crate::text_buffer::TextBuffer;
use crate::theme;

use crossterm::event::{KeyCode, KeyEvent};
use maki_agent::QuestionInfo;
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Wrap};

const FORM_LABEL: &str = " Questions ";
const CUSTOM_OPTION: &str = "Type your own answer";
const HINT_PAIRS: &[(&str, &str)] = &[("↑↓", "select"), ("Enter", "confirm"), ("Esc", "dismiss")];
const HINT_PAIRS_TOGGLE: &[(&str, &str)] = &[
    ("↑↓", "select"),
    ("Enter", "toggle"),
    ("Tab", "submit"),
    ("Esc", "dismiss"),
];
const NO_ANSWER: &str = "(no answer)";
const MAX_QUESTION_LINES_NO_OPTIONS: usize = 10;
const SEPARATOR: &str = "─";

pub enum QuestionFormAction {
    Consumed,
    Submit(String),
    Dismiss,
}

fn format_answer(answers: &[String]) -> String {
    if answers.is_empty() {
        NO_ANSWER.to_string()
    } else {
        answers.join(", ")
    }
}

pub struct QuestionForm {
    questions: Vec<QuestionInfo>,
    current_tab: usize,
    selected: usize,
    answers: Vec<Vec<String>>,
    editing_custom: bool,
    buffer: TextBuffer,
    visible: bool,
    scroll_offset: u16,
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
            scroll_offset: 0,
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
        self.scroll_offset = 0;
        self.visible = true;
    }

    pub fn close(&mut self) {
        self.visible = false;
        self.questions.clear();
    }

    pub fn format_answers_display(&self) -> String {
        self.questions
            .iter()
            .zip(self.answers.iter())
            .map(|(q, a)| format!("{}: {}", q.question, format_answer(a)))
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn is_multi(&self) -> bool {
        self.questions.len() > 1
    }

    fn has_confirm_tab(&self) -> bool {
        self.is_multi() || self.questions.iter().any(|q| q.multiple)
    }

    fn on_confirm_tab(&self) -> bool {
        self.has_confirm_tab() && self.current_tab == self.questions.len()
    }

    fn current_question_is_multi(&self) -> bool {
        self.current_tab < self.questions.len() && self.questions[self.current_tab].multiple
    }

    fn toggle_selected_option(&mut self) {
        let q = &self.questions[self.current_tab];
        let custom_idx = q.options.len();
        if self.selected == custom_idx {
            return;
        }
        let label = q.options[self.selected].label.clone();
        let answers = &mut self.answers[self.current_tab];
        if let Some(pos) = answers.iter().position(|a| a == &label) {
            answers.remove(pos);
        } else {
            answers.push(label);
        }
    }

    fn option_count(&self) -> usize {
        if self.on_confirm_tab() {
            return 0;
        }
        self.questions[self.current_tab].options.len() + 1
    }

    fn total_tabs(&self) -> usize {
        if self.has_confirm_tab() {
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

        if key::QUIT.matches(key) {
            return QuestionFormAction::Dismiss;
        }
        if super::is_ctrl(&key) {
            return QuestionFormAction::Consumed;
        }

        match key.code {
            KeyCode::Up => {
                if self.selected > 0 {
                    self.selected -= 1;
                } else {
                    self.scroll_offset = self.scroll_offset.saturating_sub(1);
                }
                QuestionFormAction::Consumed
            }
            KeyCode::Down => {
                if self.selected + 1 < self.option_count() {
                    self.selected += 1;
                } else {
                    self.scroll_offset = self.scroll_offset.saturating_add(1);
                }
                QuestionFormAction::Consumed
            }
            KeyCode::Tab | KeyCode::Right if self.has_confirm_tab() => {
                self.next_tab();
                QuestionFormAction::Consumed
            }
            KeyCode::BackTab | KeyCode::Left if self.has_confirm_tab() => {
                self.prev_tab();
                QuestionFormAction::Consumed
            }
            KeyCode::Enter => self.handle_enter(),
            KeyCode::Esc => QuestionFormAction::Dismiss,
            _ => QuestionFormAction::Consumed,
        }
    }

    fn handle_custom_key(&mut self, key: KeyEvent) -> QuestionFormAction {
        if key::QUIT.matches(key) {
            return QuestionFormAction::Dismiss;
        }

        match key.code {
            KeyCode::Enter => {
                let text = self.buffer.value().trim().to_string();
                if !text.is_empty() {
                    self.answers[self.current_tab] = vec![text];
                }
                self.editing_custom = false;
                if !self.has_confirm_tab() {
                    return self.build_submit();
                }
                self.next_tab();
                QuestionFormAction::Consumed
            }
            KeyCode::Esc => {
                self.editing_custom = false;
                QuestionFormAction::Consumed
            }
            _ => {
                self.buffer.handle_key(key);
                QuestionFormAction::Consumed
            }
        }
    }

    pub fn handle_paste(&mut self, text: &str) -> bool {
        if self.visible && self.editing_custom {
            self.buffer.insert_text(text);
            return true;
        }
        false
    }

    fn handle_enter(&mut self) -> QuestionFormAction {
        if self.on_confirm_tab() {
            return self.build_submit();
        }

        let q = &self.questions[self.current_tab];
        let custom_idx = q.options.len();

        if self.selected == custom_idx {
            self.buffer = TextBuffer::new(String::new());
            self.editing_custom = true;
            return QuestionFormAction::Consumed;
        }

        if q.multiple {
            self.toggle_selected_option();
            return QuestionFormAction::Consumed;
        }

        self.answers[self.current_tab] = vec![q.options[self.selected].label.clone()];

        if !self.has_confirm_tab() {
            return self.build_submit();
        }
        self.next_tab();
        QuestionFormAction::Consumed
    }

    fn build_submit(&self) -> QuestionFormAction {
        let json = serde_json::to_string(&self.answers).unwrap_or_default();
        QuestionFormAction::Submit(json)
    }

    fn next_tab(&mut self) {
        if self.current_tab + 1 < self.total_tabs() {
            self.current_tab += 1;
            self.selected = 0;
            self.scroll_offset = 0;
        }
    }

    fn prev_tab(&mut self) {
        if self.current_tab > 0 {
            self.current_tab -= 1;
            self.selected = 0;
            self.scroll_offset = 0;
        }
    }

    fn build_lines(&self, inner_width: u16) -> Vec<Line<'static>> {
        let mut lines: Vec<Line> = Vec::new();

        if self.has_confirm_tab() {
            lines.push(self.render_tab_bar());
            lines.push(Line::default());
        }

        if self.on_confirm_tab() {
            self.render_confirm(&mut lines, inner_width);
        } else {
            self.render_question(&mut lines, inner_width);
        }

        lines.push(Line::default());
        let pairs = if !self.on_confirm_tab() && self.current_question_is_multi() {
            HINT_PAIRS_TOGGLE
        } else {
            HINT_PAIRS
        };
        lines.push(hint_line(pairs));

        lines
    }

    pub fn view(&self, frame: &mut Frame, area: Rect) {
        if !self.visible {
            return;
        }

        let inner_width = area.width.saturating_sub(2);
        let lines = self.build_lines(inner_width);

        let t = theme::current();
        render_form(&t, FORM_LABEL, frame, area, lines, (self.scroll_offset, 0));
    }

    fn render_tab_bar(&self) -> Line<'static> {
        let mut spans = Vec::new();
        for (i, q) in self.questions.iter().enumerate() {
            if i > 0 {
                spans.push(Span::styled(" │ ", theme::current().form_separator));
            }
            let label = if q.header.is_empty() {
                format!("Q{}", i + 1)
            } else {
                q.header.clone()
            };
            let has_answer = !self.answers[i].is_empty();
            let style = if i == self.current_tab {
                theme::current().form_active
            } else if has_answer {
                theme::current().form_answered
            } else {
                theme::current().form_inactive
            };
            spans.push(Span::styled(label, style));
        }
        spans.push(Span::styled(" │ ", theme::current().form_separator));
        let confirm_style = if self.on_confirm_tab() {
            theme::current().form_active
        } else {
            theme::current().form_inactive
        };
        spans.push(Span::styled("Confirm", confirm_style));
        Line::from(spans)
    }

    fn question_text_lines(&self, width: u16) -> Vec<Line<'static>> {
        let base = Style::new().fg(theme::current().foreground);
        text_to_lines(
            &self.questions[self.current_tab].question,
            "",
            base,
            base,
            None,
            width,
        )
    }

    fn separator_line(width: u16) -> Line<'static> {
        Line::from(Span::styled(
            SEPARATOR.repeat(width as usize),
            theme::current().form_separator,
        ))
    }

    fn render_question(&self, lines: &mut Vec<Line<'static>>, width: u16) {
        lines.extend(self.question_text_lines(width));
        lines.push(Line::default());

        let q = &self.questions[self.current_tab];
        let answers = &self.answers[self.current_tab];
        let separator = Self::separator_line(width);
        let t = theme::current();

        for (i, opt) in q.options.iter().enumerate() {
            if i > 0 {
                lines.push(separator.clone());
            }
            let is_selected = i == self.selected;
            let is_picked = answers.contains(&opt.label);
            let marker = if is_picked { "✓ " } else { "  " };
            let prefix = if is_selected { "▸ " } else { "  " };

            let style = if is_selected {
                t.form_active
            } else if is_picked {
                t.form_answered
            } else {
                Style::new().fg(t.foreground)
            };

            let mut spans = vec![
                Span::styled(prefix, style),
                Span::styled(marker, t.form_check),
                Span::styled(opt.label.clone(), style),
            ];

            if !opt.description.is_empty() {
                spans.push(Span::styled(
                    format!(" — {}", opt.description),
                    t.form_description,
                ));
            }
            lines.push(Line::from(spans));
        }

        if !q.options.is_empty() {
            lines.push(separator);
        }
        let custom_idx = q.options.len();
        let is_custom_selected = self.selected == custom_idx;
        let (prefix, custom_style) = selected_prefix(&t, is_custom_selected);
        lines.push(Line::from(vec![
            Span::styled(prefix, custom_style),
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
            Span::styled("  → ", theme::current().form_arrow),
            Span::raw(before.to_string()),
            Span::styled(cursor_ch, Style::new().reversed()),
        ];
        let rest: String = chars.collect();
        if !rest.is_empty() {
            spans.push(Span::raw(rest));
        }
        lines.push(Line::from(spans));
    }

    fn render_confirm(&self, lines: &mut Vec<Line<'static>>, inner_width: u16) {
        lines.push(Line::from(Span::styled(
            "Review your answers:",
            Style::new().fg(theme::current().foreground),
        )));
        lines.push(Line::default());

        let separator = Self::separator_line(inner_width);

        for (i, q) in self.questions.iter().enumerate() {
            if i > 0 {
                lines.push(separator.clone());
            }
            let answer_text = format_answer(&self.answers[i]);
            lines.push(Line::from(vec![
                Span::styled(format!("{}. ", i + 1), theme::current().form_inactive),
                Span::styled(
                    q.question.clone(),
                    Style::new().fg(theme::current().foreground),
                ),
                Span::styled(" → ", theme::current().form_arrow),
                Span::styled(answer_text, theme::current().form_answer),
            ]));
        }

        lines.push(Line::default());
        lines.push(hint_line(&[("Enter", "submit, or navigate back to edit.")]));
    }

    pub fn is_form_suitable(questions: &[QuestionInfo]) -> bool {
        if questions.len() != 1 {
            return true;
        }
        let q = &questions[0];
        if !q.options.is_empty() {
            return true;
        }
        q.question.split('\n').count() <= MAX_QUESTION_LINES_NO_OPTIONS
    }

    pub fn format_questions_as_text(questions: &[QuestionInfo]) -> String {
        questions
            .iter()
            .enumerate()
            .map(|(i, q)| {
                let mut line = format!("{}. {}", i + 1, q.question);
                for opt in &q.options {
                    line.push_str(&format!("\n   - {}", opt.label));
                    if !opt.description.is_empty() {
                        line.push_str(&format!(" — {}", opt.description));
                    }
                }
                line
            })
            .collect::<Vec<_>>()
            .join("\n\n")
    }

    pub fn height(&self, width: u16) -> u16 {
        if !self.visible {
            return 0;
        }
        let inner_width = width.saturating_sub(2);
        let lines = self.build_lines(inner_width);
        let para = Paragraph::new(lines).wrap(Wrap { trim: false });
        para.line_count(inner_width) as u16 + 2
    }
}

overlay_impl!(QuestionForm);

#[cfg(test)]
mod tests {
    use maki_agent::{QuestionInfo, QuestionOption};

    use test_case::test_case;

    use super::*;
    use crate::components::key;
    use crate::components::keybindings::key as kb;

    fn assert_submit(action: QuestionFormAction) -> Vec<Vec<String>> {
        match action {
            QuestionFormAction::Submit(json) => serde_json::from_str(&json).unwrap(),
            _ => panic!("expected Submit"),
        }
    }

    fn enter_custom_mode(form: &mut QuestionForm) {
        form.handle_key(key(KeyCode::Down));
        form.handle_key(key(KeyCode::Down));
        form.handle_key(key(KeyCode::Enter));
    }

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

    #[test_case(0, "PostgreSQL" ; "first_option")]
    #[test_case(1, "Redis"      ; "second_option")]
    fn select_option_submits(downs: usize, expected: &str) {
        let mut form = QuestionForm::new();
        form.open(single_q_with_options());
        for _ in 0..downs {
            form.handle_key(key(KeyCode::Down));
        }
        let action = form.handle_key(key(KeyCode::Enter));
        assert_eq!(assert_submit(action), vec![vec![expected]]);
    }

    #[test]
    fn custom_input_flow() {
        let mut form = QuestionForm::new();
        form.open(single_q_with_options());
        enter_custom_mode(&mut form);

        for c in "MongoDB".chars() {
            form.handle_key(key(KeyCode::Char(c)));
        }
        let action = form.handle_key(key(KeyCode::Enter));
        assert_eq!(assert_submit(action), vec![vec!["MongoDB"]]);
    }

    #[test_case(key(KeyCode::Esc) ; "esc_in_normal_mode")]
    #[test_case(kb::QUIT.to_key_event() ; "ctrl_c_in_normal_mode")]
    fn dismiss_from_normal_mode(dismiss_key: KeyEvent) {
        let mut form = QuestionForm::new();
        form.open(single_q_with_options());
        let action = form.handle_key(dismiss_key);
        assert!(matches!(action, QuestionFormAction::Dismiss));
    }

    #[test]
    fn ctrl_c_in_custom_mode_dismisses_form() {
        let mut form = QuestionForm::new();
        form.open(single_q_with_options());
        enter_custom_mode(&mut form);

        let action = form.handle_key(kb::QUIT.to_key_event());
        assert!(matches!(action, QuestionFormAction::Dismiss));
    }

    #[test]
    fn esc_in_custom_mode_exits_edit_not_form() {
        let mut form = QuestionForm::new();
        form.open(single_q_with_options());
        enter_custom_mode(&mut form);
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
        assert_eq!(assert_submit(action), vec![vec!["Rust"], vec!["Actix"]]);
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
        let max = form.option_count() - 1;

        form.handle_key(key(KeyCode::Up));
        assert_eq!(form.selected, 0);

        for _ in 0..max + 2 {
            form.handle_key(key(KeyCode::Down));
        }
        assert_eq!(form.selected, max);
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

    fn single_multi_select_q() -> Vec<QuestionInfo> {
        vec![QuestionInfo {
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
        }]
    }

    #[test]
    fn enter_toggles_multi_select() {
        let mut form = QuestionForm::new();
        form.open(single_multi_select_q());

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
    fn single_multi_select_confirm_flow() {
        let mut form = QuestionForm::new();
        form.open(single_multi_select_q());
        assert!(form.has_confirm_tab());

        form.handle_key(key(KeyCode::Enter));
        form.handle_key(key(KeyCode::Down));
        form.handle_key(key(KeyCode::Enter));

        form.handle_key(key(KeyCode::Tab));
        assert!(form.on_confirm_tab());

        let action = form.handle_key(key(KeyCode::Enter));
        assert_eq!(assert_submit(action), vec![vec!["A", "B"]]);
    }

    #[test]
    fn enter_on_custom_in_multi_select_goes_to_confirm() {
        let mut form = QuestionForm::new();
        form.open(single_multi_select_q());

        form.handle_key(key(KeyCode::Down));
        form.handle_key(key(KeyCode::Down));
        form.handle_key(key(KeyCode::Enter));
        assert!(form.editing_custom);

        for c in "custom".chars() {
            form.handle_key(key(KeyCode::Char(c)));
        }
        form.handle_key(key(KeyCode::Enter));
        assert!(form.on_confirm_tab());
    }

    #[test]
    fn height_changes_with_editing_custom() {
        let mut form = QuestionForm::new();
        form.open(single_q_with_options());
        let h1 = form.height(80);

        enter_custom_mode(&mut form);
        let h2 = form.height(80);

        assert!(h2 > h1);
    }

    #[test]
    fn empty_custom_input_not_stored() {
        let mut form = QuestionForm::new();
        form.open(single_q_with_options());
        enter_custom_mode(&mut form);

        let action = form.handle_key(key(KeyCode::Enter));
        assert!(assert_submit(action)[0].is_empty());
    }

    #[test]
    fn height_accounts_for_wrapping() {
        let long_text = "a".repeat(100);
        let mut form = QuestionForm::new();
        form.open(vec![QuestionInfo {
            question: long_text,
            header: String::new(),
            options: vec![],
            multiple: false,
        }]);
        let h_narrow = form.height(30);
        let h_wide = form.height(200);
        assert!(h_narrow > h_wide);
    }

    #[test_case(single_q_with_options() ; "with_options")]
    #[test_case(q_no_options() ; "short_no_options")]
    #[test_case(multi_q() ; "multi_question")]
    fn is_form_suitable_positive(qs: Vec<QuestionInfo>) {
        assert!(QuestionForm::is_form_suitable(&qs));
    }

    #[test]
    fn is_form_unsuitable_long_no_options() {
        let long = (0..20)
            .map(|i| format!("line {i}"))
            .collect::<Vec<_>>()
            .join("\n");
        let qs = vec![QuestionInfo {
            question: long,
            header: String::new(),
            options: vec![],
            multiple: false,
        }];
        assert!(!QuestionForm::is_form_suitable(&qs));
    }

    #[test]
    fn format_questions_as_text_with_options() {
        let qs = single_q_with_options();
        let text = QuestionForm::format_questions_as_text(&qs);
        assert!(text.contains("1. Pick a DB"));
        assert!(text.contains("- PostgreSQL — Relational"));
        assert!(text.contains("- Redis — Key-value"));
    }

    #[test]
    fn scroll_offset_resets_on_tab_change() {
        let mut form = QuestionForm::new();
        form.open(multi_q());
        form.scroll_offset = 5;
        form.handle_key(key(KeyCode::Tab));
        assert_eq!(form.scroll_offset, 0);
    }

    #[test]
    fn scroll_at_boundary_adjusts_offset() {
        let mut form = QuestionForm::new();
        form.open(q_no_options());

        form.handle_key(key(KeyCode::Down));
        assert_eq!(form.scroll_offset, 1);

        form.handle_key(key(KeyCode::Up));
        assert_eq!(form.scroll_offset, 0);
    }

    #[test]
    fn paste_only_works_in_custom_mode() {
        let mut form = QuestionForm::new();
        form.open(single_q_with_options());
        assert!(!form.handle_paste("ignored"));

        enter_custom_mode(&mut form);
        assert!(form.handle_paste("pasted text"));

        let action = form.handle_key(key(KeyCode::Enter));
        assert_eq!(assert_submit(action), vec![vec!["pasted text"]]);
    }

    #[test]
    fn format_answers_display() {
        let mut form = QuestionForm::new();
        form.open(single_q_with_options());
        assert_eq!(
            form.format_answers_display(),
            format!("Pick a DB: {NO_ANSWER}")
        );

        form.handle_key(key(KeyCode::Enter));
        assert_eq!(form.format_answers_display(), "Pick a DB: PostgreSQL");

        let mut form = QuestionForm::new();
        form.open(multi_q());
        form.handle_key(key(KeyCode::Enter));
        form.handle_key(key(KeyCode::Down));
        form.handle_key(key(KeyCode::Enter));
        assert_eq!(
            form.format_answers_display(),
            "Language?: Rust\nFramework?: Actix"
        );
    }
}

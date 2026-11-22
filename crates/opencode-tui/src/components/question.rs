use std::cell::Cell;

use ratatui::{
    layout::Rect,
    style::Style,
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Wrap},
    Frame,
};

use crate::theme::Theme;

use super::permission::{wrapped_row_count, wrapped_text_rows};

#[derive(Clone, Debug, PartialEq)]
pub enum QuestionType {
    Text,
    MultipleChoice,
    SingleChoice,
}

#[derive(Clone, Debug)]
pub struct QuestionOption {
    pub id: String,
    pub label: String,
}

#[derive(Clone, Debug)]
pub struct QuestionRequest {
    pub id: String,
    pub question: String,
    pub question_type: QuestionType,
    pub options: Vec<QuestionOption>,
}

/// 選択式 question のキーヒント。未選択時は Enter を受け付けないことを明示する。
pub const CHOICE_KEY_HINT: &str =
    "Up/Down to navigate, Space to toggle, Enter to confirm (after selection), Esc to cancel";

/// テキスト question のキーヒント。Esc でキャンセルできることを明示する。
pub const TEXT_KEY_HINT: &str = "Type your answer, Enter to confirm, Esc to cancel";

/// 選択肢のキーヒントを返す。
///
/// 先頭 26 件は `a`..`z` の 1 文字（既存表示を不変に保つ）、27 件目以降は
/// 1 始まりの番号（例: 27 件目は `"27"`）。`u8` 加算は `index < 26` の
/// 範囲でのみ行うため、159 件目を超えてもオーバーフローせず、
/// `{` や制御文字も現れない。
/// 選択肢のキーヒントを返す。
///
/// 0..=25 は対応する小文字キー（a..z）で直接選択できる。26 個目以降は
/// 番号を表示するが、キーボードのダイレクト選択は a..z のみなので、
/// 这些选择肢は Up/Down / Space / クリックで選ぶ必要がある（R4）。
fn option_key_hint(index: usize) -> String {
    if index < 26 {
        ((b'a' + index as u8) as char).to_string()
    } else {
        (index + 1).to_string()
    }
}

pub struct QuestionPrompt {
    current_question: Option<QuestionRequest>,
    pub is_open: bool,
    selected_index: usize,
    selected_options: Vec<bool>,
    text_input: String,
    last_rendered_area: Cell<Option<Rect>>,
    /// 直近の描画で question 行が占めた行数。クリックの当たり判定で
    /// オプションの開始行を算出するために使う。
    last_rendered_question_rows: Cell<usize>,
}

impl QuestionPrompt {
    pub fn new() -> Self {
        Self {
            current_question: None,
            is_open: false,
            selected_index: 0,
            selected_options: Vec::new(),
            text_input: String::new(),
            last_rendered_area: Cell::new(None),
            last_rendered_question_rows: Cell::new(1),
        }
    }

    pub fn ask(&mut self, question: QuestionRequest) {
        let option_count = question.options.len();
        self.current_question = Some(question);
        self.is_open = true;
        self.selected_index = 0;
        self.selected_options = vec![false; option_count];
        self.text_input.clear();
    }

    pub fn current(&self) -> Option<&QuestionRequest> {
        self.current_question.as_ref()
    }

    pub fn close(&mut self) {
        self.current_question = None;
        self.is_open = false;
        self.selected_index = 0;
        self.selected_options.clear();
        self.text_input.clear();
    }

    pub fn move_up(&mut self) {
        if self.selected_index > 0 {
            self.selected_index -= 1;
        }
    }

    pub fn move_down(&mut self) {
        if let Some(q) = &self.current_question {
            if !q.options.is_empty() && self.selected_index + 1 < q.options.len() {
                self.selected_index += 1;
            }
        }
    }

    pub fn toggle_selected(&mut self) {
        if let Some(q) = &self.current_question {
            if q.question_type == QuestionType::MultipleChoice
                && self.selected_index < self.selected_options.len()
            {
                self.selected_options[self.selected_index] =
                    !self.selected_options[self.selected_index];
            } else if q.question_type == QuestionType::SingleChoice {
                // For single choice, clear all and select current
                for opt in self.selected_options.iter_mut() {
                    *opt = false;
                }
                if self.selected_index < self.selected_options.len() {
                    self.selected_options[self.selected_index] = true;
                }
            }
        }
    }

    pub fn type_char(&mut self, c: char) {
        if let Some(q) = &self.current_question {
            if q.question_type == QuestionType::Text {
                self.text_input.push(c);
            } else if !q.options.is_empty() {
                // Letter key selects an option by index
                let idx = (c as u8).wrapping_sub(b'a') as usize;
                if idx < q.options.len() {
                    if q.question_type == QuestionType::SingleChoice {
                        for opt in self.selected_options.iter_mut() {
                            *opt = false;
                        }
                    }
                    if idx < self.selected_options.len() {
                        self.selected_options[idx] = !self.selected_options[idx];
                    }
                    self.selected_index = idx;
                }
            }
        }
    }

    pub fn backspace(&mut self) {
        if let Some(q) = &self.current_question {
            if q.question_type == QuestionType::Text {
                self.text_input.pop();
            }
        }
    }

    /// Confirm the current question and return the answer values.
    ///
    /// Text questions return the typed text, choice questions return the
    /// labels of the selected options. Single choice questions fall back to
    /// the highlighted option when nothing was explicitly toggled.
    pub fn confirm(&mut self) -> Option<Vec<String>> {
        let q = self.current_question.as_ref()?;
        let answers = if q.question_type == QuestionType::Text {
            let text = self.text_input.trim().to_string();
            if text.is_empty() {
                Vec::new()
            } else {
                vec![text]
            }
        } else {
            let mut selected: Vec<String> = q
                .options
                .iter()
                .enumerate()
                .filter(|(i, _)| self.selected_options.get(*i).copied().unwrap_or(false))
                .map(|(_, opt)| opt.label.clone())
                .collect();

            if selected.is_empty() && q.question_type == QuestionType::SingleChoice {
                if let Some(option) = q.options.get(self.selected_index) {
                    selected.push(option.label.clone());
                }
            }

            selected
        };
        self.current_question.take()?;
        self.is_open = false;
        self.selected_index = 0;
        self.selected_options.clear();
        self.text_input.clear();
        Some(answers)
    }

    /// 明示的な回答が存在するか。選択式ではいずれかの選択、テキストでは
    /// 空白以外の入力を要求する（未選択時の Enter を無効化するため）。
    pub fn has_explicit_answer(&self) -> bool {
        let Some(question) = &self.current_question else {
            return false;
        };
        if question.question_type == QuestionType::Text {
            !self.text_input.trim().is_empty()
        } else {
            self.selected_options.iter().any(|selected| *selected)
        }
    }

    /// 未選択時に Enter を無効化する `confirm`。
    ///
    /// 回答が無い場合は prompt を開いたまま `None` を返すため、呼び出し側は
    /// 回答を送信せず待機する。キャンセルは Esc（呼び出し側の `close`）で行う。
    pub fn confirm_selected(&mut self) -> Option<Vec<String>> {
        if !self.has_explicit_answer() {
            return None;
        }
        self.confirm()
    }

    pub fn handle_click(&mut self, col: u16, row: u16) {
        if !self.is_open {
            return;
        }
        if let Some(area) = self.last_rendered_area.get() {
            if row < area.y
                || row >= area.y + area.height
                || col < area.x
                || col >= area.x + area.width
            {
                return;
            }
            // オプションは枠(1) + "Question:"(1) + 空行(1) + question 行(N) +
            // 空行(1) の後から始まる。折り返しで N が増えても描画と同じ位置を指す。
            let question_rows = self.last_rendered_question_rows.get();
            let options_start = area
                .y
                .saturating_add(4)
                .saturating_add(u16::try_from(question_rows).unwrap_or(u16::MAX));
            if row >= options_start {
                let idx = (row - options_start) as usize;
                if let Some(q) = &self.current_question {
                    if idx < q.options.len() {
                        self.selected_index = idx;
                        self.toggle_selected();
                    }
                }
            }
        }
    }

    pub fn render(&self, frame: &mut Frame, area: Rect, theme: &Theme) {
        if !self.is_open {
            return;
        }

        let question = match &self.current_question {
            Some(q) => q,
            None => return,
        };

        let mut content = vec![
            Line::from(Span::styled(
                "Question:",
                Style::default().fg(theme.primary).bold(),
            )),
            Line::from(""),
            Line::from(Span::styled(
                &question.question,
                Style::default().fg(theme.text),
            )),
            Line::from(""),
        ];

        if !question.options.is_empty() {
            for (i, opt) in question.options.iter().enumerate() {
                let is_selected = self.selected_options.get(i).copied().unwrap_or(false);
                let is_highlighted = i == self.selected_index;
                let marker = if is_selected { "[x]" } else { "[ ]" };
                let key = option_key_hint(i);
                let label_style = if is_highlighted {
                    Style::default().fg(theme.primary).bold()
                } else {
                    Style::default().fg(theme.text)
                };
                content.push(Line::from(vec![
                    Span::styled(
                        format!("{} ({}) ", marker, key),
                        Style::default().fg(theme.primary),
                    ),
                    Span::styled(&opt.label, label_style),
                ]));
            }
            content.push(Line::from(""));
            content.push(Line::from(Span::styled(
                CHOICE_KEY_HINT,
                Style::default().fg(theme.text_muted),
            )));
        } else {
            // Text input mode
            let input_display = if self.text_input.is_empty() {
                "Type your answer...".to_string()
            } else {
                format!("> {}", self.text_input)
            };
            let input_style = if self.text_input.is_empty() {
                Style::default().fg(theme.text_muted)
            } else {
                Style::default().fg(theme.text)
            };
            content.push(Line::from(Span::styled(input_display, input_style)));
            content.push(Line::from(""));
            content.push(Line::from(Span::styled(
                TEXT_KEY_HINT,
                Style::default().fg(theme.text_muted),
            )));
        }

        let width = area.width.saturating_sub(2).min(80);
        // 枠 2 行を除いた幅で折り返した内容行数から高さを算出する。
        let content_width = width.saturating_sub(2).max(1);
        // question 行は単一 span なので、その行だけの折り返し行数を記録する。
        // 高さ算出には全行の行数を使うが、クリック座標は question 行の
        // 実際の行数に一致させる必要がある。
        self.last_rendered_question_rows
            .set(wrapped_text_rows(&question.question, content_width));
        let wrapped_rows = wrapped_row_count(&content, content_width);
        let max_height = area.height.saturating_sub(2).max(1);
        let height = (wrapped_rows as u16)
            .saturating_add(2)
            .min(max_height)
            .max(1);

        // Render inline at the bottom of the area
        let popup_area = Rect::new(
            area.x + 1,
            area.y + area.height.saturating_sub(height + 1),
            width,
            height,
        );

        // Track rendered area for click handling
        self.last_rendered_area.set(Some(popup_area));

        let paragraph = Paragraph::new(content)
            .block(
                Block::default()
                    .title(" Question ")
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(theme.primary)),
            )
            .style(Style::default().bg(theme.background_panel))
            .wrap(Wrap { trim: false });

        frame.render_widget(paragraph, popup_area);
    }
}

impl Default for QuestionPrompt {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::{backend::TestBackend, Terminal};

    /// TestBackend に描画し、バッファの文本内容を文字列として返すヘルパー。
    fn render_to_string(
        width: u16,
        height: u16,
        draw: impl for<'a> FnOnce(&mut Frame<'a>),
    ) -> String {
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).expect("TestBackend should initialize");
        terminal.draw(draw).expect("draw should succeed");

        let buffer = terminal.backend().buffer();
        let area = buffer.area();
        let mut text = String::new();
        for y in area.top()..area.bottom() {
            for x in area.left()..area.right() {
                text.push_str(buffer.cell((x, y)).map(|cell| cell.symbol()).unwrap_or(" "));
            }
            text.push('\n');
        }
        text
    }

    fn choice_question(question_type: QuestionType) -> QuestionRequest {
        QuestionRequest {
            id: "qst_1".to_string(),
            question: "Continue?".to_string(),
            question_type,
            options: vec![
                QuestionOption {
                    id: "Yes".to_string(),
                    label: "Yes".to_string(),
                },
                QuestionOption {
                    id: "No".to_string(),
                    label: "No".to_string(),
                },
            ],
        }
    }

    #[test]
    fn confirm_returns_typed_text_for_text_questions() {
        let mut prompt = QuestionPrompt::new();
        prompt.ask(QuestionRequest {
            id: "qst_text".to_string(),
            question: "Name?".to_string(),
            question_type: QuestionType::Text,
            options: Vec::new(),
        });

        for c in "hello".chars() {
            prompt.type_char(c);
        }

        assert_eq!(prompt.confirm(), Some(vec!["hello".to_string()]));
        assert!(!prompt.is_open);
        assert!(prompt.current().is_none());
    }

    #[test]
    fn confirm_returns_highlighted_option_for_single_choice() {
        let mut prompt = QuestionPrompt::new();
        prompt.ask(choice_question(QuestionType::SingleChoice));
        assert_eq!(prompt.confirm(), Some(vec!["Yes".to_string()]));

        let mut prompt = QuestionPrompt::new();
        prompt.ask(choice_question(QuestionType::SingleChoice));
        prompt.move_down();
        assert_eq!(prompt.confirm(), Some(vec!["No".to_string()]));
    }

    #[test]
    fn confirm_collects_multiple_selections() {
        let mut prompt = QuestionPrompt::new();
        prompt.ask(choice_question(QuestionType::MultipleChoice));
        prompt.toggle_selected();
        prompt.move_down();
        prompt.toggle_selected();

        assert_eq!(
            prompt.confirm(),
            Some(vec!["Yes".to_string(), "No".to_string()])
        );
    }

    #[test]
    fn confirm_returns_empty_answer_for_unselected_multiple_choice() {
        let mut prompt = QuestionPrompt::new();
        prompt.ask(choice_question(QuestionType::MultipleChoice));
        assert_eq!(prompt.confirm(), Some(Vec::<String>::new()));
    }

    #[test]
    fn confirm_returns_none_when_no_question_is_pending() {
        let mut prompt = QuestionPrompt::new();
        assert_eq!(prompt.confirm(), None);
    }

    #[test]
    fn confirm_selected_ignores_enter_until_a_choice_is_made() {
        let mut prompt = QuestionPrompt::new();
        prompt.ask(choice_question(QuestionType::SingleChoice));
        assert!(!prompt.has_explicit_answer());
        assert_eq!(prompt.confirm_selected(), None);
        // Enter が無効でも prompt は開いたまま（Esc でキャンセルできる）。
        assert!(prompt.is_open);
        assert!(prompt.current().is_some());

        prompt.toggle_selected();
        assert!(prompt.has_explicit_answer());
        assert_eq!(prompt.confirm_selected(), Some(vec!["Yes".to_string()]));
        assert!(!prompt.is_open);
    }

    #[test]
    fn confirm_selected_ignores_unselected_multiple_choice() {
        let mut prompt = QuestionPrompt::new();
        prompt.ask(choice_question(QuestionType::MultipleChoice));
        assert_eq!(prompt.confirm_selected(), None);
        assert!(prompt.is_open);

        prompt.move_down();
        prompt.toggle_selected();
        assert_eq!(prompt.confirm_selected(), Some(vec!["No".to_string()]));
    }

    #[test]
    fn confirm_selected_ignores_blank_text_input() {
        let mut prompt = QuestionPrompt::new();
        prompt.ask(QuestionRequest {
            id: "qst_text".to_string(),
            question: "Name?".to_string(),
            question_type: QuestionType::Text,
            options: Vec::new(),
        });
        assert_eq!(prompt.confirm_selected(), None);
        prompt.type_char(' ');
        assert_eq!(prompt.confirm_selected(), None);
        assert!(prompt.is_open);

        for c in "answer".chars() {
            prompt.type_char(c);
        }
        assert_eq!(prompt.confirm_selected(), Some(vec!["answer".to_string()]));
        assert!(!prompt.is_open);
    }

    #[test]
    fn render_wraps_long_question_and_keeps_options_and_hint_visible() {
        let mut prompt = QuestionPrompt::new();
        prompt.ask(QuestionRequest {
            id: "qst_long".to_string(),
            question: format!("LONGQ-{} -ENDQ", "q".repeat(180)),
            question_type: QuestionType::SingleChoice,
            options: vec![
                QuestionOption {
                    id: "Yes".to_string(),
                    label: "Yes".to_string(),
                },
                QuestionOption {
                    id: "No".to_string(),
                    label: "No".to_string(),
                },
            ],
        });
        let theme = Theme::default();

        let text = render_to_string(80, 24, |frame| {
            prompt.render(frame, Rect::new(0, 0, 80, 24), &theme);
        });

        assert!(
            text.contains("-ENDQ"),
            "the end of a long question must be wrapped and visible"
        );
        assert!(text.contains("[ ] (a) Yes"), "first option must be visible");
        assert!(text.contains("[ ] (b) No"), "last option must be visible");
        assert!(text.contains("Esc to cancel"), "hint must be visible");
    }

    #[test]
    fn handle_click_selects_options_after_question_wraps() {
        let mut prompt = QuestionPrompt::new();
        // 単語境界で複数行に折り返す long question（描画幅 76）。
        prompt.ask(QuestionRequest {
            id: "qst_wrapped".to_string(),
            question: "please answer yes or no ".repeat(12),
            question_type: QuestionType::SingleChoice,
            options: vec![
                QuestionOption {
                    id: "Yes".to_string(),
                    label: "Yes".to_string(),
                },
                QuestionOption {
                    id: "No".to_string(),
                    label: "No".to_string(),
                },
            ],
        });
        let theme = Theme::default();

        let text = render_to_string(80, 24, |frame| {
            prompt.render(frame, Rect::new(0, 0, 80, 24), &theme);
        });

        let question_rows = prompt.last_rendered_question_rows.get();
        assert!(
            question_rows >= 2,
            "the long question must wrap into multiple rows"
        );
        let area = prompt
            .last_rendered_area
            .get()
            .expect("render records the popup area");
        let options_start = area.y + 4 + question_rows as u16;

        let rendered_rows: Vec<&str> = text.lines().collect();
        let first_option_row = rendered_rows
            .iter()
            .position(|line| line.contains("[ ] (a) Yes"))
            .expect("first option must be rendered") as u16;
        let second_option_row = rendered_rows
            .iter()
            .position(|line| line.contains("[ ] (b) No"))
            .expect("second option must be rendered") as u16;
        assert_eq!(
            options_start, first_option_row,
            "recorded question rows must match the rendered option position"
        );

        // 折り返しが発生してもオプションをクリックできる。
        prompt.handle_click(area.x + 3, first_option_row);
        assert_eq!(prompt.selected_index, 0);
        assert!(prompt.selected_options[0]);

        prompt.handle_click(area.x + 3, second_option_row);
        assert_eq!(prompt.selected_index, 1);
        assert!(!prompt.selected_options[0]);
        assert!(prompt.selected_options[1]);
    }

    #[test]
    fn question_key_hints_advertise_enter_after_selection_and_esc_cancel() {
        assert!(CHOICE_KEY_HINT.contains("Enter to confirm (after selection)"));
        assert!(CHOICE_KEY_HINT.contains("Esc to cancel"));
        assert!(TEXT_KEY_HINT.contains("Enter to confirm"));
        assert!(TEXT_KEY_HINT.contains("Esc to cancel"));
    }

    #[test]
    fn option_key_hint_keeps_letters_and_switches_to_numbers() {
        // 先頭 26 件は既存表示のまま（1 文字の小文字）。
        assert_eq!(option_key_hint(0), "a");
        assert_eq!(option_key_hint(25), "z");
        // 27 件目以降は 1 始まりの番号。
        assert_eq!(option_key_hint(26), "27");
        assert_eq!(option_key_hint(158), "159");
        assert_eq!(option_key_hint(159), "160");
        assert_eq!(option_key_hint(999), "1000");
    }

    #[test]
    fn option_key_hint_never_emits_braces_or_control_chars() {
        // 旧実装では 27 件目が '{' になり、160 件目で u8 加算がオーバーフローした。
        for index in 0..2000 {
            let key = option_key_hint(index);
            assert!(
                key.chars().all(|c| c.is_ascii_alphanumeric()),
                "key for option {index} was {key:?}"
            );
        }
    }

    #[test]
    fn render_many_options_shows_numbers_for_27th_onward() {
        let options: Vec<QuestionOption> = (0..30)
            .map(|i| QuestionOption {
                id: format!("opt_{i}"),
                label: format!("Option {i}"),
            })
            .collect();
        let mut prompt = QuestionPrompt::new();
        prompt.ask(QuestionRequest {
            id: "qst_many".to_string(),
            question: "Pick one".to_string(),
            question_type: QuestionType::SingleChoice,
            options,
        });
        let theme = Theme::default();

        // 30 件すべてが収まる高さで描画し、文字→番号の境界を検証する。
        let text = render_to_string(80, 40, |frame| {
            prompt.render(frame, Rect::new(0, 0, 80, 40), &theme);
        });

        assert!(
            text.contains("(a) Option 0"),
            "first option must stay a letter"
        );
        assert!(text.contains("(z) Option 25"), "26th option must stay z");
        assert!(
            text.contains("(27) Option 26"),
            "27th option must be numbered"
        );
        assert!(
            text.contains("(30) Option 29"),
            "last option must be numbered"
        );
        assert!(!text.contains("{"), "no brace key hint must be rendered");
    }

    #[test]
    fn render_beyond_159_options_does_not_panic() {
        // 旧実装は i=159 で b'a' + 159 > 255 となり debug ビルドでパニックした。
        // 表示領域は小さくても content 構築時に全選択肢のキーが計算されるため、
        // このテストはオーバーフロー回帰を確実に検出できる。
        let options: Vec<QuestionOption> = (0..160)
            .map(|i| QuestionOption {
                id: format!("opt_{i}"),
                label: format!("Option {i}"),
            })
            .collect();
        let mut prompt = QuestionPrompt::new();
        prompt.ask(QuestionRequest {
            id: "qst_overflow".to_string(),
            question: "Pick one".to_string(),
            question_type: QuestionType::SingleChoice,
            options,
        });
        let theme = Theme::default();

        let text = render_to_string(80, 24, |frame| {
            prompt.render(frame, Rect::new(0, 0, 80, 24), &theme);
        });

        // 160 件は 24 行の枠からあふれるためヒント行は描画されない。
        // ここでは render が完了してパニックしないことと、括弧キーが出ないことを確認する。
        assert!(!text.is_empty(), "render must complete without panic");
        // 最大 26 個目まで文字キー、27 個目以降は番号。'{'+ や制御文字は出ない。
        assert!(
            !text.contains('{'),
            "brace must not be rendered as a key hint"
        );
        assert!(
            text.bytes().all(|b| b >= 0x20 || b == b'\n'),
            "control characters must not be rendered"
        );
    }
}

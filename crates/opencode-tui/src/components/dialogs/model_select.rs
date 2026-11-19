use ratatui::{
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph},
    Frame,
};

use crate::theme::Theme;

use super::DialogClickMap;

#[derive(Clone, Debug)]
pub struct Model {
    pub id: String,
    pub name: String,
    pub provider: String,
    pub context_window: u64,
}

pub struct ModelSelectDialog {
    models: Vec<Model>,
    filtered: Vec<usize>,
    query: String,
    state: ListState,
    open: bool,
    click_map: DialogClickMap,
}

impl ModelSelectDialog {
    pub fn new() -> Self {
        let models = vec![
            Model {
                id: "claude-sonnet-4".into(),
                name: "Claude Sonnet 4".into(),
                provider: "anthropic".into(),
                context_window: 200000,
            },
            Model {
                id: "claude-3-5-sonnet".into(),
                name: "Claude 3.5 Sonnet".into(),
                provider: "anthropic".into(),
                context_window: 200000,
            },
            Model {
                id: "claude-3-opus".into(),
                name: "Claude 3 Opus".into(),
                provider: "anthropic".into(),
                context_window: 200000,
            },
            Model {
                id: "gpt-4o".into(),
                name: "GPT-4o".into(),
                provider: "openai".into(),
                context_window: 128000,
            },
            Model {
                id: "gpt-4-turbo".into(),
                name: "GPT-4 Turbo".into(),
                provider: "openai".into(),
                context_window: 128000,
            },
            Model {
                id: "o1".into(),
                name: "o1".into(),
                provider: "openai".into(),
                context_window: 200000,
            },
            Model {
                id: "gemini-2.0-flash".into(),
                name: "Gemini 2.0 Flash".into(),
                provider: "google".into(),
                context_window: 1000000,
            },
            Model {
                id: "gemini-1.5-pro".into(),
                name: "Gemini 1.5 Pro".into(),
                provider: "google".into(),
                context_window: 1000000,
            },
            Model {
                id: "deepseek-v3".into(),
                name: "DeepSeek V3".into(),
                provider: "deepseek".into(),
                context_window: 64000,
            },
            Model {
                id: "llama-3.1-70b".into(),
                name: "Llama 3.1 70B".into(),
                provider: "openrouter".into(),
                context_window: 128000,
            },
        ];

        let filtered = (0..models.len()).collect();
        let mut state = ListState::default();
        state.select(Some(0));

        Self {
            models,
            filtered,
            query: String::new(),
            state,
            open: false,
            click_map: DialogClickMap::default(),
        }
    }

    pub fn set_models(&mut self, models: Vec<Model>) {
        self.models = models;
        self.filtered = (0..self.models.len()).collect();
    }

    pub fn open(&mut self) {
        self.open = true;
        self.query.clear();
        self.filtered = (0..self.models.len()).collect();
        self.state.select(Some(0));
    }

    pub fn close(&mut self) {
        self.open = false;
    }

    pub fn is_open(&self) -> bool {
        self.open
    }

    pub fn handle_input(&mut self, c: char) {
        self.query.push(c);
        self.filter();
    }

    pub fn handle_backspace(&mut self) {
        self.query.pop();
        self.filter();
    }

    pub fn move_up(&mut self) {
        if let Some(selected) = self.state.selected() {
            if selected > 0 {
                self.state.select(Some(selected - 1));
            }
        }
    }

    pub fn move_down(&mut self) {
        if let Some(selected) = self.state.selected() {
            if selected < self.filtered.len().saturating_sub(1) {
                self.state.select(Some(selected + 1));
            }
        }
    }

    pub fn selected_model(&self) -> Option<&Model> {
        self.state
            .selected()
            .and_then(|i| self.filtered.get(i))
            .and_then(|&idx| self.models.get(idx))
    }

    /// Select the item under the pointer. Returns `true` when the click landed
    /// on the dialog frame (so the app must not close it).
    pub fn handle_click(&mut self, col: u16, row: u16) -> bool {
        if let Some(index) = self.click_map.item_at(col, row) {
            self.state.select(Some(index));
        }
        self.click_map.contains(col, row)
    }

    fn filter(&mut self) {
        let query_lower = self.query.to_lowercase();
        self.filtered = self
            .models
            .iter()
            .enumerate()
            .filter(|(_, m)| {
                m.name.to_lowercase().contains(&query_lower)
                    || m.provider.to_lowercase().contains(&query_lower)
                    || m.id.to_lowercase().contains(&query_lower)
            })
            .map(|(i, _)| i)
            .collect();

        if self.filtered.is_empty() {
            self.state.select(None);
        } else {
            self.state.select(Some(0));
        }
    }

    pub fn render(&self, frame: &mut Frame, area: Rect, theme: &Theme) {
        if !self.open {
            return;
        }

        let dialog_width = 50;
        let dialog_height = (self.filtered.len() + 4).min(15) as u16;
        let dialog_area = centered_rect(dialog_width, dialog_height, area);
        self.click_map.set_area(dialog_area);

        frame.render_widget(Clear, dialog_area);

        let block = Block::default()
            .title(Span::styled(
                " Select Model ",
                Style::default()
                    .fg(theme.primary)
                    .add_modifier(Modifier::BOLD),
            ))
            .borders(Borders::ALL)
            .border_style(Style::default().fg(theme.border))
            .style(Style::default().bg(theme.background_panel));

        let inner_area = super::dialog_inner(block.inner(dialog_area));
        frame.render_widget(block, dialog_area);

        let search_line = Line::from(vec![
            Span::styled("> ", Style::default().fg(theme.primary)),
            Span::styled(&self.query, Style::default().fg(theme.text)),
            Span::styled("▏", Style::default().fg(theme.primary)),
        ]);

        frame.render_widget(
            Paragraph::new(search_line),
            Rect {
                x: inner_area.x,
                y: inner_area.y,
                width: inner_area.width,
                height: 1,
            },
        );

        let items: Vec<ListItem> = self
            .filtered
            .iter()
            .filter_map(|&idx| {
                self.models.get(idx).map(|m| {
                    let is_selected = self
                        .state
                        .selected()
                        .and_then(|s| self.filtered.get(s))
                        .map(|&i| i == idx)
                        .unwrap_or(false);

                    let style = if is_selected {
                        Style::default().fg(theme.text).bg(theme.background_element)
                    } else {
                        Style::default().fg(theme.text)
                    };

                    ListItem::new(Line::from(vec![
                        Span::styled(&m.name, style),
                        Span::styled("  ", style),
                        Span::styled(&m.provider, Style::default().fg(theme.text_muted)),
                    ]))
                })
            })
            .collect();

        let list = List::new(items);
        let list_area = Rect {
            x: inner_area.x,
            y: inner_area.y + 2,
            width: inner_area.width,
            height: inner_area.height.saturating_sub(2),
        };

        let mut state = self.state;
        frame.render_stateful_widget(list, list_area, &mut state);
        self.click_map
            .set_list(list_area, state.offset(), self.filtered.len());
    }
}

impl Default for ModelSelectDialog {
    fn default() -> Self {
        Self::new()
    }
}

fn centered_rect(width: u16, height: u16, area: Rect) -> Rect {
    super::centered_rect(width, height, area)
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

        // ratatui-core の Buffer は Display を実装しないため cell() で走査する。
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

    /// テスト用 Model を構築するヘルパー。
    fn model(id: &str, name: &str, provider: &str, context_window: u64) -> Model {
        Model {
            id: id.to_string(),
            name: name.to_string(),
            provider: provider.to_string(),
            context_window,
        }
    }

    /// 確実に一致しないクエリを順に入力するヘルパー。
    fn type_no_match(dialog: &mut ModelSelectDialog) {
        for c in "zzz_qqq_xxx".chars() {
            dialog.handle_input(c);
        }
    }

    // --- Model データ保持 ---

    #[test]
    fn model_stores_all_fields() {
        let m = model("custom-id", "Custom Model", "custom", 32_000);

        assert_eq!(m.id, "custom-id");
        assert_eq!(m.name, "Custom Model");
        assert_eq!(m.provider, "custom");
        assert_eq!(m.context_window, 32_000);
    }

    #[test]
    fn model_clone_preserves_all_fields() {
        let original = model("clone-id", "Clone Model", "provider", 16_000);

        let cloned = original.clone();

        assert_eq!(cloned.id, original.id);
        assert_eq!(cloned.name, original.name);
        assert_eq!(cloned.provider, original.provider);
        assert_eq!(cloned.context_window, original.context_window);
    }

    #[test]
    fn new_populates_expected_default_models() {
        let dialog = ModelSelectDialog::new();

        assert_eq!(dialog.models.len(), 10);
        assert!(dialog
            .models
            .iter()
            .any(|m| m.id == "claude-sonnet-4" && m.name == "Claude Sonnet 4"));
        assert!(dialog
            .models
            .iter()
            .any(|m| m.id == "gpt-4o" && m.provider == "openai"));
        assert!(dialog
            .models
            .iter()
            .any(|m| m.id == "gemini-2.0-flash" && m.context_window == 1_000_000));
        assert!(dialog.models.iter().any(|m| m.id == "deepseek-v3"));
        assert!(dialog.models.iter().any(|m| m.id == "llama-3.1-70b"));
    }

    // --- new / Default ---

    #[test]
    fn new_starts_with_full_filter_and_first_selected_and_closed() {
        let dialog = ModelSelectDialog::new();

        assert!(dialog.query.is_empty());
        assert_eq!(dialog.filtered.len(), dialog.models.len());
        assert_eq!(dialog.filtered.first().copied(), Some(0));
        assert_eq!(dialog.state.selected(), Some(0));
        assert!(!dialog.is_open());
    }

    #[test]
    fn default_matches_new_state() {
        let dialog = ModelSelectDialog::default();

        assert!(dialog.query.is_empty());
        assert_eq!(dialog.filtered.len(), dialog.models.len());
        assert_eq!(dialog.state.selected(), Some(0));
        assert!(!dialog.is_open());
    }

    // --- open / close / is_open ---

    #[test]
    fn open_sets_open_state_and_resets_query_filter_and_selection() {
        let mut dialog = ModelSelectDialog::new();
        for c in "openai".chars() {
            dialog.handle_input(c);
        }
        dialog.move_down();
        assert!(dialog.filtered.len() < dialog.models.len());
        assert!(!dialog.is_open());

        dialog.open();

        assert!(dialog.is_open());
        assert!(dialog.query.is_empty());
        assert_eq!(dialog.filtered.len(), dialog.models.len());
        assert_eq!(dialog.state.selected(), Some(0));
    }

    #[test]
    fn close_clears_open_state_and_preserves_data() {
        let mut dialog = ModelSelectDialog::new();
        dialog.open();
        dialog.move_down();
        assert!(dialog.is_open());
        assert_eq!(dialog.state.selected(), Some(1));

        dialog.close();

        assert!(!dialog.is_open());
        assert_eq!(dialog.state.selected(), Some(1));
        assert_eq!(dialog.filtered.len(), dialog.models.len());
    }

    #[test]
    fn open_close_cycles_toggle_state() {
        let mut dialog = ModelSelectDialog::new();

        for _ in 0..3 {
            dialog.open();
            assert!(dialog.is_open());
            dialog.close();
            assert!(!dialog.is_open());
        }
    }

    #[test]
    fn close_is_idempotent_from_new() {
        let mut dialog = ModelSelectDialog::new();

        dialog.close();
        dialog.close();

        assert!(!dialog.is_open());
        assert!(dialog.query.is_empty());
    }

    #[test]
    fn open_is_idempotent() {
        let mut dialog = ModelSelectDialog::new();

        dialog.open();
        dialog.open();

        assert!(dialog.is_open());
        assert_eq!(dialog.state.selected(), Some(0));
        assert!(dialog.query.is_empty());
    }

    // --- set_models ---

    #[test]
    fn set_models_replaces_models_and_filter() {
        let mut dialog = ModelSelectDialog::new();
        assert_eq!(dialog.models.len(), 10);

        dialog.set_models(vec![
            model("a", "Alpha", "p1", 1_000),
            model("b", "Beta", "p2", 2_000),
        ]);

        assert_eq!(dialog.models.len(), 2);
        assert_eq!(dialog.filtered.len(), 2);
        assert_eq!(dialog.filtered, vec![0usize, 1]);
        let selected = dialog
            .selected_model()
            .expect("first model should be selected");
        assert_eq!(selected.id, "a");
        assert_eq!(selected.name, "Alpha");
    }

    #[test]
    fn set_models_preserves_open_and_selection_state() {
        let mut dialog = ModelSelectDialog::new();
        dialog.open();
        dialog.move_down();
        assert_eq!(dialog.state.selected(), Some(1));

        dialog.set_models(vec![
            model("x", "X Model", "px", 100),
            model("y", "Y Model", "py", 200),
        ]);

        assert!(dialog.is_open());
        assert_eq!(dialog.state.selected(), Some(1));
        let selected = dialog
            .selected_model()
            .expect("selection index should be preserved");
        assert_eq!(selected.id, "y");
        assert_eq!(selected.name, "Y Model");
    }

    #[test]
    fn set_models_can_clear_models() {
        let mut dialog = ModelSelectDialog::new();
        assert_eq!(dialog.models.len(), 10);

        dialog.set_models(Vec::new());

        assert!(dialog.models.is_empty());
        assert!(dialog.filtered.is_empty());
        assert!(dialog.selected_model().is_none());
    }

    // --- クエリ入力とフィルタ (handle_input / handle_backspace) ---

    #[test]
    fn handle_input_appends_chars_to_query() {
        let mut dialog = ModelSelectDialog::new();

        for c in "gpt".chars() {
            dialog.handle_input(c);
        }

        assert_eq!(dialog.query, "gpt");
    }

    #[test]
    fn handle_input_filters_by_name_case_insensitively() {
        let mut dialog = ModelSelectDialog::new();

        for c in "CLAUDE".chars() {
            dialog.handle_input(c);
        }

        assert_eq!(dialog.query, "CLAUDE");
        assert_eq!(dialog.filtered.len(), 3);
        let selected = dialog.selected_model().expect("a model should match");
        assert_eq!(selected.id, "claude-sonnet-4");
        assert_eq!(selected.provider, "anthropic");
    }

    #[test]
    fn handle_input_filters_by_provider() {
        let mut dialog = ModelSelectDialog::new();

        for c in "openai".chars() {
            dialog.handle_input(c);
        }

        assert_eq!(dialog.filtered.len(), 3);
        let first_id = dialog
            .selected_model()
            .expect("first match should be selected")
            .id
            .clone();
        assert_eq!(first_id, "gpt-4o");
        dialog.move_down();
        let second_id = dialog
            .selected_model()
            .expect("second match should be selected")
            .id
            .clone();
        assert_eq!(second_id, "gpt-4-turbo");
        dialog.move_down();
        let third_id = dialog
            .selected_model()
            .expect("third match should be selected")
            .id
            .clone();
        assert_eq!(third_id, "o1");
    }

    #[test]
    fn handle_input_filters_by_id() {
        let mut dialog = ModelSelectDialog::new();

        for c in "gemini-2.0-flash".chars() {
            dialog.handle_input(c);
        }

        assert_eq!(dialog.filtered.len(), 1);
        let selected = dialog.selected_model().expect("exact id match");
        assert_eq!(selected.id, "gemini-2.0-flash");
        assert_eq!(selected.name, "Gemini 2.0 Flash");
        assert_eq!(selected.context_window, 1_000_000);
    }

    #[test]
    fn handle_input_with_no_match_empties_filter_and_selection() {
        let mut dialog = ModelSelectDialog::new();

        type_no_match(&mut dialog);

        assert_eq!(dialog.query, "zzz_qqq_xxx");
        assert!(dialog.filtered.is_empty());
        assert_eq!(dialog.state.selected(), None);
        assert!(dialog.selected_model().is_none());
    }

    #[test]
    fn handle_backspace_removes_last_char_and_refilters() {
        let mut dialog = ModelSelectDialog::new();
        for c in "claude".chars() {
            dialog.handle_input(c);
        }
        assert_eq!(dialog.query, "claude");
        let narrow_len = dialog.filtered.len();
        assert_eq!(narrow_len, 3);

        dialog.handle_backspace();

        assert_eq!(dialog.query, "claud");
        assert!(dialog.filtered.len() >= narrow_len);
        assert_eq!(dialog.state.selected(), Some(0));
    }

    #[test]
    fn handle_backspace_until_empty_restores_full_list() {
        let mut dialog = ModelSelectDialog::new();
        let total = dialog.models.len();
        for c in "claude".chars() {
            dialog.handle_input(c);
        }
        assert!(dialog.filtered.len() < total);

        for _ in 0..6 {
            dialog.handle_backspace();
        }

        assert!(dialog.query.is_empty());
        assert_eq!(dialog.filtered.len(), total);
        assert_eq!(dialog.state.selected(), Some(0));
    }

    #[test]
    fn handle_backspace_on_empty_query_is_noop() {
        let mut dialog = ModelSelectDialog::new();
        let total = dialog.models.len();

        dialog.handle_backspace();

        assert!(dialog.query.is_empty());
        assert_eq!(dialog.filtered.len(), total);
        assert_eq!(dialog.state.selected(), Some(0));
    }

    // --- 選択移動の状態遷移 (move_up / move_down / selected_model) ---

    #[test]
    fn move_down_advances_selection_and_selected_model() {
        let mut dialog = ModelSelectDialog::new();
        assert_eq!(dialog.state.selected(), Some(0));
        let first_id = dialog
            .selected_model()
            .expect("first model should be selected")
            .id
            .clone();

        dialog.move_down();

        assert_eq!(dialog.state.selected(), Some(1));
        let second_id = dialog
            .selected_model()
            .expect("second model should be selected")
            .id
            .clone();
        assert_eq!(first_id, "claude-sonnet-4");
        assert_eq!(second_id, "claude-3-5-sonnet");
        assert_ne!(second_id, first_id);
    }

    #[test]
    fn move_down_clamps_at_last_item() {
        let mut dialog = ModelSelectDialog::new();
        dialog.open();
        let last = dialog.filtered.len().saturating_sub(1);

        for _ in 0..(dialog.filtered.len() + 5) {
            dialog.move_down();
        }

        assert_eq!(dialog.state.selected(), Some(last));
        let selected = dialog
            .selected_model()
            .expect("last model should be selected");
        assert_eq!(selected.id, "llama-3.1-70b");
    }

    #[test]
    fn move_up_moves_back_and_clamps_at_zero() {
        let mut dialog = ModelSelectDialog::new();
        dialog.open();
        dialog.move_down();
        dialog.move_down();
        assert_eq!(dialog.state.selected(), Some(2));

        dialog.move_up();
        assert_eq!(dialog.state.selected(), Some(1));
        dialog.move_up();
        assert_eq!(dialog.state.selected(), Some(0));
        dialog.move_up();
        assert_eq!(dialog.state.selected(), Some(0));
    }

    #[test]
    fn move_up_and_move_down_are_noop_without_selection() {
        let mut dialog = ModelSelectDialog::new();
        type_no_match(&mut dialog);
        assert!(dialog.filtered.is_empty());
        assert_eq!(dialog.state.selected(), None);

        dialog.move_up();
        dialog.move_down();

        assert_eq!(
            dialog.state.selected(),
            None,
            "empty list must not gain a selection"
        );
        assert!(dialog.selected_model().is_none());
    }

    #[test]
    fn move_down_is_noop_when_filtered_is_empty_even_with_selection() {
        let mut dialog = ModelSelectDialog::new();
        dialog.state.select(Some(0));
        dialog.filtered.clear();
        assert_eq!(dialog.state.selected(), Some(0));
        assert!(dialog.filtered.is_empty());

        dialog.move_down();

        // filtered が空のとき saturating_sub(1) == 0 となり、選択はクランプされ据え置きになる。
        assert_eq!(dialog.state.selected(), Some(0));
        assert!(dialog.selected_model().is_none());
    }

    #[test]
    fn move_down_is_clamped_when_models_are_cleared() {
        let mut dialog = ModelSelectDialog::new();
        dialog.open();
        dialog.move_down();
        assert_eq!(dialog.state.selected(), Some(1));

        // set_models は選択インデックスを触らないため Some(1) が残る。
        dialog.set_models(Vec::new());
        assert!(dialog.models.is_empty());
        assert!(dialog.filtered.is_empty());
        assert_eq!(dialog.state.selected(), Some(1));

        dialog.move_down();

        // データ未設定（空）では上限が 0 クランプされ、Some(1) のまま動かない。
        assert_eq!(dialog.state.selected(), Some(1));
        assert!(dialog.selected_model().is_none());
    }

    #[test]
    fn selected_model_follows_filter() {
        let mut dialog = ModelSelectDialog::new();
        for c in "gemini".chars() {
            dialog.handle_input(c);
        }
        assert_eq!(dialog.filtered.len(), 2);

        let selected = dialog.selected_model().expect("filtered model");
        assert_eq!(selected.id, "gemini-2.0-flash");
        assert_eq!(selected.provider, "google");

        dialog.move_down();
        let next = dialog.selected_model().expect("next filtered model");
        assert_eq!(next.id, "gemini-1.5-pro");
        assert_eq!(next.name, "Gemini 1.5 Pro");
    }

    #[test]
    fn selected_model_none_without_selection() {
        let mut dialog = ModelSelectDialog::new();
        type_no_match(&mut dialog);

        assert_eq!(dialog.state.selected(), None);
        assert!(dialog.selected_model().is_none());
    }

    // --- render ---

    #[test]
    fn render_when_closed_draws_nothing() {
        let dialog = ModelSelectDialog::new();
        let theme = Theme::default();
        assert!(!dialog.is_open());

        let text = render_to_string(80, 24, |frame| {
            dialog.render(frame, Rect::new(0, 0, 80, 24), &theme);
        });

        assert!(
            text.trim().is_empty(),
            "closed model select dialog must not draw any content"
        );
    }

    #[test]
    fn render_when_open_shows_query_and_filtered_models() {
        let mut dialog = ModelSelectDialog::new();
        dialog.open();
        for c in "gemini".chars() {
            dialog.handle_input(c);
        }
        let theme = Theme::default();

        let text = render_to_string(80, 24, |frame| {
            dialog.render(frame, Rect::new(0, 0, 80, 24), &theme);
        });

        assert!(text.contains("Select Model"), "dialog title");
        assert!(text.contains("gemini"), "query text");
        assert!(text.contains("Gemini 2.0 Flash"), "first filtered model");
        assert!(text.contains("Gemini 1.5 Pro"), "second filtered model");
        assert!(text.contains("google"), "provider text");
    }

    #[test]
    fn render_when_open_shows_default_models_and_providers() {
        let mut dialog = ModelSelectDialog::new();
        dialog.open();
        let theme = Theme::default();

        let text = render_to_string(80, 24, |frame| {
            dialog.render(frame, Rect::new(0, 0, 80, 24), &theme);
        });

        assert!(text.contains("Claude Sonnet 4"), "first default model");
        assert!(text.contains("GPT-4o"), "fourth default model");
        assert!(text.contains("anthropic"), "provider of first model");
        assert!(text.contains("openai"), "provider of fourth model");
    }

    #[test]
    fn click_selects_list_item_and_leaves_backdrop_unconsumed() {
        let mut dialog = ModelSelectDialog::new();
        dialog.open();
        let theme = Theme::default();
        let area = Rect::new(0, 0, 80, 24);
        let _ = render_to_string(80, 24, |frame| dialog.render(frame, area, &theme));

        // 80x24 では 10 件のリストが row 8 から始まる（枠と検索行を除く）。
        assert_eq!(dialog.state.selected(), Some(0));
        assert!(
            dialog.handle_click(17, 9),
            "click inside dialog is consumed"
        );
        assert_eq!(dialog.state.selected(), Some(1));
        assert!(
            dialog.handle_click(17, 8),
            "click inside dialog is consumed"
        );
        assert_eq!(dialog.state.selected(), Some(0));
        assert!(!dialog.handle_click(0, 0), "backdrop click is not consumed");
        assert_eq!(dialog.state.selected(), Some(0));
    }
}

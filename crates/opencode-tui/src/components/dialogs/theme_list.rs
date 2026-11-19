use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph},
    Frame,
};

use crate::theme::Theme;

use super::DialogClickMap;

#[derive(Clone, Debug)]
pub struct ThemeOption {
    pub id: String,
    pub name: String,
}

pub struct ThemeListDialog {
    options: Vec<ThemeOption>,
    filtered: Vec<usize>,
    query: String,
    initial_theme: String,
    state: ListState,
    open: bool,
    click_map: DialogClickMap,
}

impl ThemeListDialog {
    pub fn new() -> Self {
        let mut state = ListState::default();
        state.select(Some(0));
        Self {
            options: Vec::new(),
            filtered: Vec::new(),
            query: String::new(),
            initial_theme: "opencode@dark".to_string(),
            state,
            open: false,
            click_map: DialogClickMap::default(),
        }
    }

    pub fn set_options(&mut self, mut options: Vec<ThemeOption>) {
        options.sort_by_key(|a| a.name.to_lowercase());
        self.options = options;
        self.filter();
    }

    pub fn open(&mut self, current_theme: &str) {
        self.open = true;
        self.initial_theme = canonical_theme_id(current_theme);
        self.query.clear();
        self.filter();
        let selected = self
            .filtered
            .iter()
            .position(|idx| {
                self.options
                    .get(*idx)
                    .is_some_and(|item| item.id == self.initial_theme)
            })
            .unwrap_or(0);
        self.state.select(if self.filtered.is_empty() {
            None
        } else {
            Some(selected)
        });
    }

    pub fn close(&mut self) {
        self.open = false;
    }

    pub fn is_open(&self) -> bool {
        self.open
    }

    pub fn initial_theme_id(&self) -> &str {
        &self.initial_theme
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

    pub fn handle_input(&mut self, c: char) {
        self.query.push(c);
        self.filter();
    }

    pub fn handle_backspace(&mut self) {
        self.query.pop();
        self.filter();
    }

    pub fn selected_theme_id(&self) -> Option<String> {
        self.state
            .selected()
            .and_then(|idx| self.filtered.get(idx))
            .and_then(|idx| self.options.get(*idx))
            .map(|theme| theme.id.clone())
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
        let query = self.query.to_lowercase();
        self.filtered = self
            .options
            .iter()
            .enumerate()
            .filter(|(_, option)| option.name.to_lowercase().contains(&query))
            .map(|(idx, _)| idx)
            .collect();
        self.state.select(if self.filtered.is_empty() {
            None
        } else {
            Some(0)
        });
    }

    pub fn render(&self, frame: &mut Frame, area: Rect, theme: &Theme) {
        if !self.open {
            return;
        }

        let dialog_area = centered_rect(58, 16, area);
        self.click_map.set_area(dialog_area);
        frame.render_widget(Clear, dialog_area);

        let block = Block::default()
            .title(Span::styled(
                " Themes ",
                Style::default()
                    .fg(theme.primary)
                    .add_modifier(Modifier::BOLD),
            ))
            .borders(Borders::ALL)
            .border_style(Style::default().fg(theme.border))
            .style(Style::default().bg(theme.background_panel));
        let inner = super::dialog_inner(block.inner(dialog_area));
        frame.render_widget(block, dialog_area);

        let layout = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(1),
                Constraint::Min(1),
                Constraint::Length(1),
            ])
            .split(inner);

        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled("> ", Style::default().fg(theme.primary)),
                Span::styled(&self.query, Style::default().fg(theme.text)),
                Span::styled("▏", Style::default().fg(theme.primary)),
            ])),
            layout[0],
        );

        let items: Vec<ListItem> = self
            .filtered
            .iter()
            .filter_map(|idx| self.options.get(*idx))
            .map(|item| {
                let mut spans = vec![Span::styled(&item.name, Style::default().fg(theme.text))];
                if item.id == self.initial_theme {
                    spans.push(Span::styled(
                        "  (current)",
                        Style::default().fg(theme.text_muted),
                    ));
                }
                ListItem::new(Line::from(spans))
            })
            .collect();
        let list = List::new(items).highlight_style(
            Style::default()
                .bg(theme.background_element)
                .add_modifier(Modifier::BOLD),
        );
        let mut state = self.state;
        frame.render_stateful_widget(list, layout[1], &mut state);
        self.click_map
            .set_list(layout[1], state.offset(), self.filtered.len());

        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled("Enter", Style::default().fg(theme.primary)),
                Span::styled(" apply  ", Style::default().fg(theme.text_muted)),
                Span::styled("Esc", Style::default().fg(theme.primary)),
                Span::styled(" cancel (revert)", Style::default().fg(theme.text_muted)),
            ])),
            layout[2],
        );
    }
}

impl Default for ThemeListDialog {
    fn default() -> Self {
        Self::new()
    }
}

fn centered_rect(width: u16, height: u16, area: Rect) -> Rect {
    super::centered_rect(width, height, area)
}

fn canonical_theme_id(name: &str) -> String {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return "opencode@dark".to_string();
    }

    if let Some((base, variant)) = split_theme_variant(trimmed) {
        return format!("{base}@{variant}");
    }

    if trimmed.eq_ignore_ascii_case("dark") {
        return "opencode@dark".to_string();
    }
    if trimmed.eq_ignore_ascii_case("light") {
        return "opencode@light".to_string();
    }

    format!("{trimmed}@dark")
}

fn split_theme_variant(name: &str) -> Option<(&str, &str)> {
    let (base, variant) = name.rsplit_once('@').or_else(|| name.rsplit_once(':'))?;
    if base.is_empty() || !matches!(variant, "dark" | "light") {
        return None;
    }
    Some((base, variant))
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

    fn option(id: &str, name: &str) -> ThemeOption {
        ThemeOption {
            id: id.to_string(),
            name: name.to_string(),
        }
    }

    /// 名前順に並べ替えても順序が変わらないサンプル。
    fn sample_options() -> Vec<ThemeOption> {
        vec![
            option("opencode@dark", "Dark"),
            option("opencode@light", "Light"),
            option("nord@dark", "Nord"),
        ]
    }

    // --- ThemeOption データ保持 ---

    #[test]
    fn theme_option_stores_id_and_name() {
        let o = option("nord@dark", "Nord");

        assert_eq!(o.id, "nord@dark");
        assert_eq!(o.name, "Nord");
    }

    #[test]
    fn theme_option_clone_preserves_all_fields() {
        let original = option("opencode@light", "Light");

        let cloned = original.clone();

        assert_eq!(cloned.id, original.id);
        assert_eq!(cloned.name, original.name);
    }

    // --- new / Default ---

    #[test]
    fn new_starts_empty_with_default_theme_and_closed() {
        let dialog = ThemeListDialog::new();

        assert!(dialog.options.is_empty());
        assert!(dialog.filtered.is_empty());
        assert_eq!(dialog.query, "");
        assert_eq!(dialog.initial_theme_id(), "opencode@dark");
        assert_eq!(dialog.state.selected(), Some(0));
        assert!(!dialog.is_open());
    }

    #[test]
    fn default_starts_empty_with_default_theme_and_closed() {
        let dialog = ThemeListDialog::default();

        assert!(dialog.options.is_empty());
        assert_eq!(dialog.initial_theme_id(), "opencode@dark");
        assert_eq!(dialog.state.selected(), Some(0));
        assert!(!dialog.is_open());
    }

    // --- set_options ---

    #[test]
    fn set_options_sorts_by_name_case_insensitively() {
        let mut dialog = ThemeListDialog::new();
        dialog.set_options(vec![
            option("z@dark", "zebra"),
            option("a@dark", "Alpha"),
            option("m@dark", "beta"),
        ]);

        assert_eq!(dialog.options.len(), 3);
        assert_eq!(dialog.options[0].name, "Alpha");
        assert_eq!(dialog.options[1].name, "beta");
        assert_eq!(dialog.options[2].name, "zebra");
        assert_eq!(dialog.filtered, vec![0, 1, 2]);
        assert_eq!(dialog.selected_theme_id(), Some("a@dark".to_string()));
    }

    #[test]
    fn set_options_replaces_previous_options() {
        let mut dialog = ThemeListDialog::new();
        dialog.set_options(sample_options());
        assert_eq!(dialog.options.len(), 3);

        dialog.set_options(vec![option("solo@dark", "Solo")]);

        assert_eq!(dialog.options.len(), 1);
        assert_eq!(dialog.options[0].id, "solo@dark");
        assert_eq!(dialog.filtered, vec![0]);
    }

    #[test]
    fn set_options_empty_selects_none() {
        let mut dialog = ThemeListDialog::new();
        dialog.set_options(sample_options());
        assert_eq!(dialog.state.selected(), Some(0));

        dialog.set_options(Vec::new());

        assert!(dialog.options.is_empty());
        assert!(dialog.filtered.is_empty());
        assert_eq!(dialog.state.selected(), None);
        assert!(dialog.selected_theme_id().is_none());
    }

    // --- open / close / is_open ---

    #[test]
    fn open_sets_open_state_and_canonical_default_theme() {
        let mut dialog = ThemeListDialog::new();
        dialog.set_options(sample_options());
        assert!(!dialog.is_open());

        dialog.open("dark");

        assert!(dialog.is_open());
        assert_eq!(dialog.initial_theme_id(), "opencode@dark");
        assert_eq!(dialog.state.selected(), Some(0));
        assert_eq!(
            dialog.selected_theme_id(),
            Some("opencode@dark".to_string())
        );
    }

    #[test]
    fn open_selects_the_theme_matching_current_theme() {
        let mut dialog = ThemeListDialog::new();
        dialog.set_options(sample_options());

        dialog.open("light");

        assert!(dialog.is_open());
        assert_eq!(dialog.initial_theme_id(), "opencode@light");
        assert_eq!(dialog.state.selected(), Some(1));
        assert_eq!(
            dialog.selected_theme_id(),
            Some("opencode@light".to_string())
        );
    }

    #[test]
    fn open_canonicalizes_colon_variant_and_surrounding_spaces() {
        let mut dialog = ThemeListDialog::new();
        dialog.set_options(vec![
            option("custom@dark", "Custom dark"),
            option("custom@light", "Custom light"),
        ]);

        dialog.open("  custom:light  ");

        assert_eq!(dialog.initial_theme_id(), "custom@light");
        assert_eq!(dialog.state.selected(), Some(1));
        assert_eq!(dialog.selected_theme_id(), Some("custom@light".to_string()));
    }

    #[test]
    fn open_appends_dark_variant_to_bare_theme_name() {
        let mut dialog = ThemeListDialog::new();
        dialog.set_options(vec![option("nord@dark", "Nord")]);

        dialog.open("nord");

        assert_eq!(dialog.initial_theme_id(), "nord@dark");
        assert_eq!(dialog.state.selected(), Some(0));
        assert_eq!(dialog.selected_theme_id(), Some("nord@dark".to_string()));
    }

    #[test]
    fn open_with_blank_theme_uses_default_theme() {
        let mut dialog = ThemeListDialog::new();
        dialog.set_options(sample_options());

        dialog.open("   ");

        assert_eq!(dialog.initial_theme_id(), "opencode@dark");
        assert_eq!(dialog.state.selected(), Some(0));
    }

    #[test]
    fn open_with_unknown_theme_falls_back_to_first_item() {
        let mut dialog = ThemeListDialog::new();
        dialog.set_options(sample_options());

        dialog.open("unknown@dark");

        assert_eq!(dialog.initial_theme_id(), "unknown@dark");
        assert_eq!(dialog.state.selected(), Some(0));
        assert_eq!(
            dialog.selected_theme_id(),
            Some("opencode@dark".to_string())
        );
    }

    #[test]
    fn open_with_empty_options_selects_none() {
        let mut dialog = ThemeListDialog::new();
        dialog.set_options(Vec::new());

        dialog.open("dark");

        assert!(dialog.is_open());
        assert_eq!(dialog.state.selected(), None);
        assert!(dialog.selected_theme_id().is_none());
    }

    #[test]
    fn move_down_after_open_with_empty_options_stays_clamped() {
        let mut dialog = ThemeListDialog::new();
        dialog.set_options(Vec::new());
        dialog.open("dark");
        assert_eq!(dialog.state.selected(), None);

        dialog.move_down();

        // 空アイテムでは選択が None のまま（直近ラウンドで失敗したパターン）。
        assert_eq!(dialog.state.selected(), None);
        assert!(dialog.selected_theme_id().is_none());
    }

    #[test]
    fn open_resets_query_and_selection() {
        let mut dialog = ThemeListDialog::new();
        dialog.set_options(sample_options());
        dialog.open("dark");
        dialog.handle_input('n');
        assert_eq!(dialog.selected_theme_id(), Some("nord@dark".to_string()));

        dialog.open("light");

        assert_eq!(dialog.query, "");
        assert_eq!(dialog.options.len(), 3);
        assert_eq!(dialog.state.selected(), Some(1));
        assert_eq!(
            dialog.selected_theme_id(),
            Some("opencode@light".to_string())
        );
    }

    #[test]
    fn close_clears_open_state_and_preserves_data() {
        let mut dialog = ThemeListDialog::new();
        dialog.set_options(sample_options());
        dialog.open("dark");
        dialog.move_down();
        assert_eq!(dialog.state.selected(), Some(1));

        dialog.close();

        assert!(!dialog.is_open());
        assert_eq!(dialog.initial_theme_id(), "opencode@dark");
        assert_eq!(dialog.options.len(), 3);
        assert_eq!(dialog.filtered.len(), 3);
        assert_eq!(dialog.state.selected(), Some(1));
        assert_eq!(
            dialog.selected_theme_id(),
            Some("opencode@light".to_string())
        );
    }

    #[test]
    fn open_close_cycles_toggle_state() {
        let mut dialog = ThemeListDialog::new();

        for _ in 0..3 {
            dialog.open("dark");
            assert!(dialog.is_open());
            dialog.close();
            assert!(!dialog.is_open());
        }
    }

    #[test]
    fn close_is_idempotent_from_new() {
        let mut dialog = ThemeListDialog::new();

        dialog.close();
        dialog.close();

        assert!(!dialog.is_open());
        assert_eq!(dialog.initial_theme_id(), "opencode@dark");
    }

    // --- 選択移動 ---

    #[test]
    fn move_down_advances_selection_and_selected_theme() {
        let mut dialog = ThemeListDialog::new();
        dialog.set_options(sample_options());
        dialog.open("dark");
        assert_eq!(dialog.state.selected(), Some(0));

        dialog.move_down();

        assert_eq!(dialog.state.selected(), Some(1));
        assert_eq!(
            dialog.selected_theme_id(),
            Some("opencode@light".to_string())
        );
    }

    #[test]
    fn move_down_clamps_at_last_item() {
        let mut dialog = ThemeListDialog::new();
        dialog.set_options(sample_options());
        dialog.open("dark");

        for _ in 0..10 {
            dialog.move_down();
        }

        assert_eq!(dialog.state.selected(), Some(2));
        assert_eq!(dialog.selected_theme_id(), Some("nord@dark".to_string()));
    }

    #[test]
    fn move_up_moves_back_and_clamps_at_zero() {
        let mut dialog = ThemeListDialog::new();
        dialog.set_options(sample_options());
        dialog.open("dark");
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
    fn move_down_before_set_options_stays_clamped() {
        let mut dialog = ThemeListDialog::new();
        assert_eq!(dialog.state.selected(), Some(0));

        dialog.move_down();

        // データ未設定時は filtered.len().saturating_sub(1) == 0 で進まない。
        assert_eq!(dialog.state.selected(), Some(0));
        assert!(dialog.selected_theme_id().is_none());
    }

    #[test]
    fn move_up_and_move_down_are_noop_without_selection() {
        let mut dialog = ThemeListDialog::new();
        dialog.set_options(Vec::new());
        assert_eq!(dialog.state.selected(), None);

        dialog.move_up();
        dialog.move_down();

        assert_eq!(dialog.state.selected(), None);
        assert!(dialog.selected_theme_id().is_none());
    }

    // --- フィルタ入力 ---

    #[test]
    fn handle_input_filters_by_name_case_insensitively() {
        let mut dialog = ThemeListDialog::new();
        dialog.set_options(sample_options());
        dialog.open("dark");

        dialog.handle_input('N');

        assert_eq!(dialog.query, "N");
        assert_eq!(dialog.filtered, vec![2]);
        assert_eq!(dialog.state.selected(), Some(0));
        assert_eq!(dialog.selected_theme_id(), Some("nord@dark".to_string()));
    }

    #[test]
    fn handle_backspace_restores_full_list() {
        let mut dialog = ThemeListDialog::new();
        dialog.set_options(sample_options());
        dialog.open("dark");
        dialog.handle_input('n');
        assert_eq!(dialog.selected_theme_id(), Some("nord@dark".to_string()));

        dialog.handle_backspace();

        assert_eq!(dialog.query, "");
        assert_eq!(dialog.filtered.len(), 3);
        assert_eq!(dialog.state.selected(), Some(0));
        assert_eq!(
            dialog.selected_theme_id(),
            Some("opencode@dark".to_string())
        );
    }

    #[test]
    fn handle_input_with_no_match_clears_selection() {
        let mut dialog = ThemeListDialog::new();
        dialog.set_options(sample_options());
        dialog.open("dark");

        dialog.handle_input('z');

        assert!(dialog.filtered.is_empty());
        assert_eq!(dialog.state.selected(), None);
        assert!(dialog.selected_theme_id().is_none());
    }

    // --- render ---

    #[test]
    fn render_when_closed_draws_nothing() {
        let dialog = ThemeListDialog::new();
        let theme = Theme::default();

        let text = render_to_string(80, 24, |frame| {
            dialog.render(frame, Rect::new(0, 0, 80, 24), &theme);
        });

        assert!(
            text.trim().is_empty(),
            "closed theme dialog must not draw any content"
        );
    }

    #[test]
    fn render_when_open_shows_title_options_and_hints() {
        let mut dialog = ThemeListDialog::new();
        dialog.set_options(sample_options());
        dialog.open("dark");
        let theme = Theme::default();

        let text = render_to_string(80, 24, |frame| {
            dialog.render(frame, Rect::new(0, 0, 80, 24), &theme);
        });

        assert!(text.contains("Themes"), "dialog title");
        assert!(text.contains("Dark"), "first option name");
        assert!(text.contains("Light"), "second option name");
        assert!(text.contains("Nord"), "third option name");
        assert!(text.contains("(current)"), "current theme marker");
        assert!(text.contains("Enter"), "apply hint");
        assert!(text.contains("Esc"), "cancel hint");
    }

    #[test]
    fn render_shows_query_text_and_filtered_option() {
        let mut dialog = ThemeListDialog::new();
        dialog.set_options(sample_options());
        dialog.open("dark");
        dialog.handle_input('n');
        let theme = Theme::default();

        let text = render_to_string(80, 24, |frame| {
            dialog.render(frame, Rect::new(0, 0, 80, 24), &theme);
        });

        assert!(text.contains("> n"), "query prompt with input");
        assert!(text.contains("Nord"), "filtered option");
    }
}

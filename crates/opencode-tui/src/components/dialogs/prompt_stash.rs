use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph},
    Frame,
};

use crate::theme::Theme;

#[derive(Clone, Debug)]
pub struct StashItem {
    pub input: String,
    pub created_at: i64,
}

pub struct PromptStashDialog {
    entries: Vec<StashItem>,
    filtered: Vec<usize>,
    query: String,
    state: ListState,
    open: bool,
}

impl PromptStashDialog {
    pub fn new() -> Self {
        let mut state = ListState::default();
        state.select(Some(0));
        Self {
            entries: Vec::new(),
            filtered: Vec::new(),
            query: String::new(),
            state,
            open: false,
        }
    }

    pub fn set_entries(&mut self, entries: Vec<StashItem>) {
        self.entries = entries;
        self.filter();
    }

    pub fn open(&mut self) {
        self.open = true;
        self.query.clear();
        self.filter();
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

    pub fn selected_index(&self) -> Option<usize> {
        self.state
            .selected()
            .and_then(|idx| self.filtered.get(idx))
            .copied()
    }

    pub fn remove_selected(&mut self) -> Option<usize> {
        let index = self.selected_index()?;
        if index >= self.entries.len() {
            return None;
        }
        self.entries.remove(index);
        self.filter();
        Some(index)
    }

    fn filter(&mut self) {
        let query = self.query.to_ascii_lowercase();
        self.filtered = self
            .entries
            .iter()
            .enumerate()
            .filter(|(_, entry)| entry.input.to_ascii_lowercase().contains(&query))
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

        let dialog_area = centered_rect(84, 20, area);
        frame.render_widget(Clear, dialog_area);

        let block = Block::default()
            .title(Span::styled(
                " Prompt Stash ",
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

        let items = self
            .filtered
            .iter()
            .filter_map(|idx| self.entries.get(*idx))
            .map(|entry| {
                let preview = entry.input.lines().next().unwrap_or_default().trim();
                let time = chrono::DateTime::<chrono::Utc>::from_timestamp_millis(entry.created_at)
                    .map(|dt| {
                        dt.with_timezone(&chrono::Local)
                            .format("%m-%d %H:%M")
                            .to_string()
                    })
                    .unwrap_or_else(|| "-".to_string());
                ListItem::new(Line::from(vec![
                    Span::styled(preview, Style::default().fg(theme.text)),
                    Span::styled(format!("  {}", time), Style::default().fg(theme.text_muted)),
                ]))
            })
            .collect::<Vec<_>>();

        frame.render_stateful_widget(
            List::new(items).highlight_style(
                Style::default()
                    .bg(theme.background_element)
                    .add_modifier(Modifier::BOLD),
            ),
            layout[1],
            &mut self.state.clone(),
        );

        frame.render_widget(
            Paragraph::new("Enter load  d delete  Esc close")
                .style(Style::default().fg(theme.text_muted)),
            layout[2],
        );
    }
}

impl Default for PromptStashDialog {
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

    /// テスト用 StashItem を構築するヘルパー。
    fn item(input: &str, created_at: i64) -> StashItem {
        StashItem {
            input: input.to_string(),
            created_at,
        }
    }

    /// テスト用の 3 件の stash を返すヘルパー。
    fn sample_items() -> Vec<StashItem> {
        vec![
            item("apple", 1_700_000_000_000),
            item("banana", 1_700_000_006_000),
            item("cherry", 1_700_000_012_000),
        ]
    }

    // --- StashItem データ保持 ---

    #[test]
    fn item_stores_input_and_created_at() {
        let it = item("draft note", 1_700_000_000_000);

        assert_eq!(it.input, "draft note");
        assert_eq!(it.created_at, 1_700_000_000_000);
    }

    #[test]
    fn item_clone_preserves_all_fields() {
        let original = item("draft note", 1_700_000_000_000);

        let cloned = original.clone();

        assert_eq!(cloned.input, original.input);
        assert_eq!(cloned.created_at, original.created_at);
    }

    // --- new / Default ---

    #[test]
    fn new_starts_empty_closed_and_pre_selects_zero() {
        let dialog = PromptStashDialog::new();

        assert!(dialog.entries.is_empty());
        assert!(dialog.filtered.is_empty());
        assert!(dialog.query.is_empty());
        assert_eq!(dialog.state.selected(), Some(0));
        assert_eq!(dialog.selected_index(), None);
        assert!(!dialog.is_open());
    }

    #[test]
    fn default_starts_same_as_new() {
        let dialog = PromptStashDialog::default();

        assert!(dialog.entries.is_empty());
        assert!(dialog.filtered.is_empty());
        assert!(dialog.query.is_empty());
        assert_eq!(dialog.state.selected(), Some(0));
        assert_eq!(dialog.selected_index(), None);
        assert!(!dialog.is_open());
    }

    // --- open / close / is_open ---

    #[test]
    fn open_sets_open_state_and_clears_query() {
        let mut dialog = PromptStashDialog::new();
        dialog.set_entries(sample_items());
        dialog.handle_input('a');
        assert_eq!(dialog.query, "a");
        assert_eq!(dialog.filtered, vec![0, 1]);
        assert!(!dialog.is_open());

        dialog.open();

        assert!(dialog.is_open());
        assert!(dialog.query.is_empty());
        assert_eq!(dialog.filtered, vec![0, 1, 2]);
        assert_eq!(dialog.state.selected(), Some(0));
        assert_eq!(dialog.selected_index(), Some(0));
    }

    #[test]
    fn close_clears_open_state_and_preserves_data() {
        let mut dialog = PromptStashDialog::new();
        dialog.set_entries(sample_items());
        dialog.open();
        dialog.move_down();
        assert_eq!(dialog.state.selected(), Some(1));

        dialog.close();

        assert!(!dialog.is_open());
        assert_eq!(dialog.entries.len(), 3);
        assert_eq!(dialog.state.selected(), Some(1));
        assert_eq!(dialog.selected_index(), Some(1));
    }

    #[test]
    fn open_close_cycles_toggle_state() {
        let mut dialog = PromptStashDialog::new();

        for _ in 0..3 {
            dialog.open();
            assert!(dialog.is_open());
            dialog.close();
            assert!(!dialog.is_open());
        }
    }

    #[test]
    fn close_is_idempotent_from_new() {
        let mut dialog = PromptStashDialog::new();

        dialog.close();
        dialog.close();

        assert!(!dialog.is_open());
    }

    #[test]
    fn open_is_idempotent() {
        let mut dialog = PromptStashDialog::new();
        dialog.set_entries(sample_items());

        dialog.open();
        dialog.open();

        assert!(dialog.is_open());
        assert!(dialog.query.is_empty());
        assert_eq!(dialog.selected_index(), Some(0));
    }

    // --- set_entries / filter / handle_input / handle_backspace ---

    #[test]
    fn set_entries_populates_filter_and_selects_first() {
        let mut dialog = PromptStashDialog::new();

        dialog.set_entries(sample_items());

        assert_eq!(dialog.entries.len(), 3);
        assert_eq!(dialog.filtered, vec![0, 1, 2]);
        assert_eq!(dialog.state.selected(), Some(0));
        assert_eq!(dialog.selected_index(), Some(0));
    }

    #[test]
    fn set_entries_replaces_existing_items() {
        let mut dialog = PromptStashDialog::new();
        dialog.set_entries(sample_items());
        assert_eq!(dialog.entries.len(), 3);

        dialog.set_entries(vec![item("only", 42)]);

        assert_eq!(dialog.entries.len(), 1);
        assert_eq!(dialog.entries[0].input, "only");
        assert_eq!(dialog.entries[0].created_at, 42);
        assert_eq!(dialog.filtered, vec![0]);
    }

    #[test]
    fn set_entries_can_clear_items() {
        let mut dialog = PromptStashDialog::new();
        dialog.set_entries(sample_items());
        assert_eq!(dialog.entries.len(), 3);

        dialog.set_entries(Vec::new());

        assert!(dialog.entries.is_empty());
        assert!(dialog.filtered.is_empty());
        assert_eq!(dialog.state.selected(), None);
        assert_eq!(dialog.selected_index(), None);
    }

    #[test]
    fn handle_input_filters_case_insensitively() {
        let mut dialog = PromptStashDialog::new();
        dialog.set_entries(vec![item("Apple", 1), item("banana", 2), item("CHERRY", 3)]);
        assert_eq!(dialog.filtered, vec![0, 1, 2]);

        dialog.handle_input('a');

        assert_eq!(dialog.query, "a");
        assert_eq!(dialog.filtered, vec![0, 1]);
        assert_eq!(dialog.selected_index(), Some(0));
    }

    #[test]
    fn handle_input_narrows_and_backspace_widens_filter() {
        let mut dialog = PromptStashDialog::new();
        dialog.set_entries(sample_items());

        dialog.handle_input('a');
        assert_eq!(dialog.filtered, vec![0, 1]);

        dialog.handle_input('p');
        assert_eq!(dialog.query, "ap");
        assert_eq!(dialog.filtered, vec![0]);

        dialog.handle_backspace();
        assert_eq!(dialog.query, "a");
        assert_eq!(dialog.filtered, vec![0, 1]);
    }

    #[test]
    fn handle_backspace_on_empty_query_is_noop() {
        let mut dialog = PromptStashDialog::new();
        dialog.set_entries(sample_items());
        assert!(dialog.query.is_empty());
        assert_eq!(dialog.filtered, vec![0, 1, 2]);

        dialog.handle_backspace();

        assert!(dialog.query.is_empty());
        assert_eq!(dialog.filtered, vec![0, 1, 2]);
    }

    #[test]
    fn filter_resets_selection_to_first() {
        let mut dialog = PromptStashDialog::new();
        dialog.set_entries(sample_items());
        dialog.move_down();
        assert_eq!(dialog.state.selected(), Some(1));
        assert_eq!(dialog.selected_index(), Some(1));

        dialog.handle_input('a');

        assert_eq!(dialog.filtered, vec![0, 1]);
        assert_eq!(dialog.state.selected(), Some(0));
        assert_eq!(dialog.selected_index(), Some(0));
    }

    // --- 選択状態遷移 (move_up / move_down / selected_index / remove_selected) ---

    #[test]
    fn move_down_advances_selection() {
        let mut dialog = PromptStashDialog::new();
        dialog.set_entries(sample_items());
        dialog.open();
        assert_eq!(dialog.state.selected(), Some(0));

        dialog.move_down();

        assert_eq!(dialog.state.selected(), Some(1));
        assert_eq!(dialog.selected_index(), Some(1));
    }

    #[test]
    fn move_down_clamps_at_last_item() {
        let mut dialog = PromptStashDialog::new();
        dialog.set_entries(sample_items());
        dialog.open();

        for _ in 0..10 {
            dialog.move_down();
        }

        assert_eq!(dialog.state.selected(), Some(2));
        assert_eq!(dialog.selected_index(), Some(2));
    }

    #[test]
    fn move_up_moves_back_and_clamps_at_zero() {
        let mut dialog = PromptStashDialog::new();
        dialog.set_entries(sample_items());
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
    fn move_after_open_without_entries_stays_unselected() {
        let mut dialog = PromptStashDialog::new();
        assert_eq!(dialog.state.selected(), Some(0));

        dialog.open();
        assert_eq!(dialog.state.selected(), None);

        dialog.move_down();
        assert_eq!(dialog.state.selected(), None);
        dialog.move_up();
        assert_eq!(dialog.state.selected(), None);
        assert_eq!(dialog.selected_index(), None);
    }

    #[test]
    fn selected_index_maps_filtered_position_to_entry_index() {
        let mut dialog = PromptStashDialog::new();
        dialog.set_entries(vec![
            item("apple", 1),
            item("bob", 2),
            item("application", 3),
            item("xylophone", 4),
        ]);
        dialog.handle_input('a');
        assert_eq!(dialog.filtered, vec![0, 2]);

        dialog.move_down();

        assert_eq!(dialog.state.selected(), Some(1));
        assert_eq!(dialog.selected_index(), Some(2));
    }

    #[test]
    fn remove_selected_removes_entry_and_refilters() {
        let mut dialog = PromptStashDialog::new();
        dialog.set_entries(sample_items());
        dialog.open();
        dialog.move_down();
        assert_eq!(dialog.selected_index(), Some(1));

        let removed = dialog.remove_selected();

        assert_eq!(removed, Some(1));
        assert_eq!(dialog.entries.len(), 2);
        assert_eq!(dialog.entries[0].input, "apple");
        assert_eq!(dialog.entries[1].input, "cherry");
        assert_eq!(dialog.filtered, vec![0, 1]);
        assert_eq!(dialog.selected_index(), Some(0));
    }

    #[test]
    fn remove_selected_returns_none_without_selection() {
        let mut dialog = PromptStashDialog::new();
        dialog.open();
        assert_eq!(dialog.state.selected(), None);

        let removed = dialog.remove_selected();

        assert_eq!(removed, None);
        assert!(dialog.entries.is_empty());
    }

    // --- render ---

    #[test]
    fn render_when_closed_draws_nothing() {
        let dialog = PromptStashDialog::new();
        let theme = Theme::default();
        assert!(!dialog.is_open());

        let text = render_to_string(80, 24, |frame| {
            dialog.render(frame, Rect::new(0, 0, 80, 24), &theme);
        });

        assert!(
            text.trim().is_empty(),
            "closed prompt stash dialog must not draw any content"
        );
    }

    #[test]
    fn render_when_open_without_entries_shows_chrome() {
        let mut dialog = PromptStashDialog::new();
        dialog.open();
        let theme = Theme::default();
        assert!(dialog.is_open());
        assert!(dialog.entries.is_empty());

        let text = render_to_string(80, 24, |frame| {
            dialog.render(frame, Rect::new(0, 0, 80, 24), &theme);
        });

        assert!(text.contains("Prompt Stash"), "dialog title");
        assert!(text.contains("Enter load"), "hint");
        assert!(text.contains("d delete"), "hint");
        assert!(text.contains("Esc close"), "hint");
        assert!(text.contains("▏"), "query cursor");
    }

    #[test]
    fn render_when_open_shows_entry_previews() {
        let mut dialog = PromptStashDialog::new();
        dialog.set_entries(sample_items());
        dialog.open();
        let theme = Theme::default();

        let text = render_to_string(80, 24, |frame| {
            dialog.render(frame, Rect::new(0, 0, 80, 24), &theme);
        });

        assert!(text.contains("Prompt Stash"), "dialog title");
        assert!(text.contains("apple"), "first preview");
        assert!(text.contains("banana"), "second preview");
        assert!(text.contains("cherry"), "third preview");
    }

    #[test]
    fn render_shows_typed_query() {
        let mut dialog = PromptStashDialog::new();
        dialog.set_entries(sample_items());
        dialog.open();
        dialog.handle_input('a');
        dialog.handle_input('p');
        let theme = Theme::default();

        let text = render_to_string(80, 24, |frame| {
            dialog.render(frame, Rect::new(0, 0, 80, 24), &theme);
        });

        assert!(text.contains("ap"), "typed query must be rendered");
    }
}

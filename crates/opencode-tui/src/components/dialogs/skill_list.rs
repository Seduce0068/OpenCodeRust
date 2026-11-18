use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph},
    Frame,
};

use crate::theme::Theme;

pub struct SkillListDialog {
    skills: Vec<String>,
    filtered: Vec<usize>,
    query: String,
    state: ListState,
    open: bool,
}

impl SkillListDialog {
    pub fn new() -> Self {
        let mut state = ListState::default();
        state.select(Some(0));
        Self {
            skills: Vec::new(),
            filtered: Vec::new(),
            query: String::new(),
            state,
            open: false,
        }
    }

    pub fn set_skills(&mut self, mut skills: Vec<String>) {
        skills.sort_by_key(|a| a.to_ascii_lowercase());
        skills.dedup_by(|a, b| a.eq_ignore_ascii_case(b));
        self.skills = skills;
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

    pub fn selected_skill(&self) -> Option<&str> {
        let idx = self.state.selected().and_then(|s| self.filtered.get(s))?;
        self.skills.get(*idx).map(String::as_str)
    }

    fn filter(&mut self) {
        let query = self.query.to_ascii_lowercase();
        self.filtered = self
            .skills
            .iter()
            .enumerate()
            .filter(|(_, skill)| skill.to_ascii_lowercase().contains(&query))
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

        let dialog_area = centered_rect(72, 18, area);
        frame.render_widget(Clear, dialog_area);

        let block = Block::default()
            .title(Span::styled(
                " Skills ",
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

        let items = if self.filtered.is_empty() {
            vec![ListItem::new(Line::from(Span::styled(
                "No skills available",
                Style::default().fg(theme.text_muted),
            )))]
        } else {
            self.filtered
                .iter()
                .filter_map(|idx| self.skills.get(*idx))
                .map(|skill| ListItem::new(Line::from(format!("/{}", skill))))
                .collect::<Vec<_>>()
        };

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
            Paragraph::new("Enter insert /skill  Esc close")
                .style(Style::default().fg(theme.text_muted)),
            layout[2],
        );
    }
}

impl Default for SkillListDialog {
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

    fn sample_skills() -> Vec<String> {
        vec!["alpha".to_string(), "beta".to_string(), "gamma".to_string()]
    }

    fn skill_names(dialog: &SkillListDialog) -> Vec<&str> {
        dialog.skills.iter().map(String::as_str).collect()
    }

    // --- new / Default ---

    #[test]
    fn new_starts_empty_with_selection_zero_and_closed() {
        let dialog = SkillListDialog::new();

        assert!(dialog.skills.is_empty());
        assert!(dialog.filtered.is_empty());
        assert_eq!(dialog.query, "");
        assert_eq!(dialog.state.selected(), Some(0));
        assert!(!dialog.is_open());
        assert!(dialog.selected_skill().is_none());
    }

    #[test]
    fn default_starts_empty_with_selection_zero_and_closed() {
        let dialog = SkillListDialog::default();

        assert!(dialog.skills.is_empty());
        assert_eq!(dialog.state.selected(), Some(0));
        assert!(!dialog.is_open());
        assert!(dialog.selected_skill().is_none());
    }

    // --- set_skills ---

    #[test]
    fn set_skills_sorts_case_insensitively_and_dedups() {
        let mut dialog = SkillListDialog::new();

        dialog.set_skills(vec![
            "Zebra".to_string(),
            "alpha".to_string(),
            "ALPHA".to_string(),
            "beta".to_string(),
        ]);

        assert_eq!(skill_names(&dialog), vec!["alpha", "beta", "Zebra"]);
        assert_eq!(dialog.filtered, vec![0, 1, 2]);
        assert_eq!(dialog.state.selected(), Some(0));
        assert_eq!(dialog.selected_skill(), Some("alpha"));
    }

    #[test]
    fn set_skills_replaces_previous_skills() {
        let mut dialog = SkillListDialog::new();
        dialog.set_skills(sample_skills());
        assert_eq!(dialog.skills.len(), 3);

        dialog.set_skills(vec!["solo".to_string()]);

        assert_eq!(skill_names(&dialog), vec!["solo"]);
        assert_eq!(dialog.filtered, vec![0]);
        assert_eq!(dialog.selected_skill(), Some("solo"));
    }

    #[test]
    fn set_skills_empty_selects_none_and_no_skill() {
        let mut dialog = SkillListDialog::new();
        dialog.set_skills(sample_skills());
        assert_eq!(dialog.state.selected(), Some(0));

        dialog.set_skills(Vec::new());

        assert!(dialog.skills.is_empty());
        assert!(dialog.filtered.is_empty());
        assert_eq!(dialog.state.selected(), None);
        assert!(dialog.selected_skill().is_none());
    }

    // --- open / close / is_open ---

    #[test]
    fn open_sets_open_state_clears_query_and_resets_selection() {
        let mut dialog = SkillListDialog::new();
        dialog.set_skills(sample_skills());
        dialog.handle_input('g');
        dialog.move_down();
        assert_eq!(dialog.query, "g");
        assert_eq!(dialog.state.selected(), Some(0));
        assert!(!dialog.is_open());

        dialog.open();

        assert!(dialog.is_open());
        assert_eq!(dialog.query, "");
        assert_eq!(dialog.filtered, vec![0, 1, 2]);
        assert_eq!(dialog.state.selected(), Some(0));
        assert_eq!(dialog.selected_skill(), Some("alpha"));
    }

    #[test]
    fn open_with_empty_skills_selects_none() {
        let mut dialog = SkillListDialog::new();
        dialog.set_skills(Vec::new());

        dialog.open();

        assert!(dialog.is_open());
        assert_eq!(dialog.state.selected(), None);
        assert!(dialog.selected_skill().is_none());
    }

    #[test]
    fn open_is_idempotent_and_resets_query() {
        let mut dialog = SkillListDialog::new();
        dialog.set_skills(sample_skills());
        dialog.open();
        dialog.handle_input('g');
        assert_eq!(dialog.query, "g");

        dialog.open();

        assert!(dialog.is_open());
        assert_eq!(dialog.query, "");
        assert_eq!(dialog.filtered, vec![0, 1, 2]);
    }

    #[test]
    fn close_clears_open_state_and_preserves_data() {
        let mut dialog = SkillListDialog::new();
        dialog.set_skills(sample_skills());
        dialog.open();
        dialog.move_down();
        assert_eq!(dialog.state.selected(), Some(1));

        dialog.close();

        assert!(!dialog.is_open());
        assert_eq!(skill_names(&dialog), vec!["alpha", "beta", "gamma"]);
        assert_eq!(dialog.filtered, vec![0, 1, 2]);
        assert_eq!(dialog.state.selected(), Some(1));
        assert_eq!(dialog.selected_skill(), Some("beta"));
    }

    #[test]
    fn open_close_cycles_toggle_state() {
        let mut dialog = SkillListDialog::new();

        for _ in 0..3 {
            dialog.open();
            assert!(dialog.is_open());
            dialog.close();
            assert!(!dialog.is_open());
        }
    }

    #[test]
    fn close_is_idempotent_from_new() {
        let mut dialog = SkillListDialog::new();

        dialog.close();
        dialog.close();

        assert!(!dialog.is_open());
        assert!(dialog.skills.is_empty());
    }

    // --- 選択移動 ---

    #[test]
    fn move_down_advances_selection_and_selected_skill() {
        let mut dialog = SkillListDialog::new();
        dialog.set_skills(sample_skills());
        dialog.open();
        assert_eq!(dialog.state.selected(), Some(0));

        dialog.move_down();

        assert_eq!(dialog.state.selected(), Some(1));
        assert_eq!(dialog.selected_skill(), Some("beta"));
    }

    #[test]
    fn move_down_clamps_at_last_item() {
        let mut dialog = SkillListDialog::new();
        dialog.set_skills(sample_skills());
        dialog.open();

        for _ in 0..10 {
            dialog.move_down();
        }

        assert_eq!(dialog.state.selected(), Some(2));
        assert_eq!(dialog.selected_skill(), Some("gamma"));
    }

    #[test]
    fn move_up_moves_back_and_clamps_at_zero() {
        let mut dialog = SkillListDialog::new();
        dialog.set_skills(sample_skills());
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
        assert_eq!(dialog.selected_skill(), Some("alpha"));
    }

    #[test]
    fn move_down_before_set_skills_stays_clamped() {
        let mut dialog = SkillListDialog::new();
        assert_eq!(dialog.state.selected(), Some(0));

        dialog.move_down();

        // filtered が空なので len().saturating_sub(1) == 0 で進まない。
        assert_eq!(dialog.state.selected(), Some(0));
        assert!(dialog.selected_skill().is_none());
    }

    #[test]
    fn move_down_after_open_with_empty_skills_stays_none() {
        let mut dialog = SkillListDialog::new();
        dialog.set_skills(Vec::new());
        dialog.open();
        assert_eq!(dialog.state.selected(), None);

        dialog.move_down();

        // 空アイテムでは filter() が選択を None にしており、move_down は no-op。
        assert_eq!(dialog.state.selected(), None);
        assert!(dialog.selected_skill().is_none());
    }

    #[test]
    fn move_up_and_move_down_are_noop_without_selection() {
        let mut dialog = SkillListDialog::new();
        dialog.set_skills(Vec::new());
        assert_eq!(dialog.state.selected(), None);

        dialog.move_up();
        dialog.move_down();

        assert_eq!(dialog.state.selected(), None);
        assert!(dialog.selected_skill().is_none());
    }

    #[test]
    fn move_down_clamps_at_last_of_filtered_list() {
        let mut dialog = SkillListDialog::new();
        dialog.set_skills(vec![
            "dog".to_string(),
            "cat".to_string(),
            "car".to_string(),
        ]);
        dialog.handle_input('c');
        assert_eq!(dialog.filtered, vec![0, 1]);
        assert_eq!(dialog.state.selected(), Some(0));

        dialog.move_down();
        assert_eq!(dialog.state.selected(), Some(1));
        assert_eq!(dialog.selected_skill(), Some("cat"));
        dialog.move_down();
        assert_eq!(dialog.state.selected(), Some(1));
    }

    // --- フィルタ入力 ---

    #[test]
    fn handle_input_filters_by_skill_case_insensitively() {
        let mut dialog = SkillListDialog::new();
        dialog.set_skills(sample_skills());
        dialog.open();

        dialog.handle_input('G');

        assert_eq!(dialog.query, "G");
        assert_eq!(dialog.filtered, vec![2]);
        assert_eq!(dialog.state.selected(), Some(0));
        assert_eq!(dialog.selected_skill(), Some("gamma"));
    }

    #[test]
    fn handle_input_with_no_match_clears_selection() {
        let mut dialog = SkillListDialog::new();
        dialog.set_skills(sample_skills());
        dialog.open();

        dialog.handle_input('z');

        assert_eq!(dialog.query, "z");
        assert!(dialog.filtered.is_empty());
        assert_eq!(dialog.state.selected(), None);
        assert!(dialog.selected_skill().is_none());
    }

    #[test]
    fn handle_backspace_restores_full_list() {
        let mut dialog = SkillListDialog::new();
        dialog.set_skills(sample_skills());
        dialog.open();
        dialog.handle_input('g');
        assert_eq!(dialog.selected_skill(), Some("gamma"));

        dialog.handle_backspace();

        assert_eq!(dialog.query, "");
        assert_eq!(dialog.filtered, vec![0, 1, 2]);
        assert_eq!(dialog.state.selected(), Some(0));
        assert_eq!(dialog.selected_skill(), Some("alpha"));
    }

    #[test]
    fn handle_backspace_pops_one_character_and_refilters() {
        let mut dialog = SkillListDialog::new();
        dialog.set_skills(sample_skills());
        dialog.open();
        dialog.handle_input('g');
        dialog.handle_input('x');
        assert_eq!(dialog.query, "gx");
        assert!(dialog.filtered.is_empty());
        assert_eq!(dialog.state.selected(), None);

        dialog.handle_backspace();

        // pop() は 1 文字だけ除き、filter() が再実行される。
        assert_eq!(dialog.query, "g");
        assert_eq!(dialog.filtered, vec![2]);
        assert_eq!(dialog.state.selected(), Some(0));
        assert_eq!(dialog.selected_skill(), Some("gamma"));
    }

    #[test]
    fn handle_backspace_on_empty_query_keeps_empty_and_refilters() {
        let mut dialog = SkillListDialog::new();
        dialog.set_skills(sample_skills());
        dialog.open();
        dialog.handle_input('g');
        assert_eq!(dialog.filtered, vec![2]);
        dialog.handle_backspace();
        assert_eq!(dialog.query, "");

        // 実装は単純な pop() なので空クエリでも filter() が走り選択が先頭へ戻る。
        dialog.handle_backspace();

        assert_eq!(dialog.query, "");
        assert_eq!(dialog.filtered, vec![0, 1, 2]);
        assert_eq!(dialog.state.selected(), Some(0));
        assert_eq!(dialog.selected_skill(), Some("alpha"));
    }

    // --- render ---

    #[test]
    fn render_when_closed_draws_nothing() {
        let dialog = SkillListDialog::new();
        let theme = Theme::default();

        let text = render_to_string(80, 24, |frame| {
            dialog.render(frame, Rect::new(0, 0, 80, 24), &theme);
        });

        assert!(
            text.trim().is_empty(),
            "closed skill dialog must not draw any content"
        );
    }

    #[test]
    fn render_when_open_shows_title_items_and_hints() {
        let mut dialog = SkillListDialog::new();
        dialog.set_skills(sample_skills());
        dialog.open();
        let theme = Theme::default();

        let text = render_to_string(80, 24, |frame| {
            dialog.render(frame, Rect::new(0, 0, 80, 24), &theme);
        });

        assert!(text.contains("Skills"), "dialog title");
        assert!(text.contains("/alpha"), "first skill");
        assert!(text.contains("/beta"), "second skill");
        assert!(text.contains("/gamma"), "third skill");
        assert!(text.contains("Enter"), "insert hint");
        assert!(text.contains("Esc"), "close hint");
    }

    #[test]
    fn render_shows_query_and_only_filtered_item() {
        let mut dialog = SkillListDialog::new();
        dialog.set_skills(sample_skills());
        dialog.open();
        dialog.handle_input('g');
        let theme = Theme::default();

        let text = render_to_string(80, 24, |frame| {
            dialog.render(frame, Rect::new(0, 0, 80, 24), &theme);
        });

        assert!(text.contains("> g"), "query prompt with input");
        assert!(text.contains("/gamma"), "filtered skill");
        assert!(!text.contains("/alpha"), "non-matching skill is hidden");
    }

    #[test]
    fn render_with_empty_skills_shows_placeholder() {
        let mut dialog = SkillListDialog::new();
        dialog.set_skills(Vec::new());
        dialog.open();
        let theme = Theme::default();

        let text = render_to_string(80, 24, |frame| {
            dialog.render(frame, Rect::new(0, 0, 80, 24), &theme);
        });

        assert!(text.contains("Skills"), "dialog title");
        assert!(text.contains("No skills available"), "empty placeholder");
    }
}

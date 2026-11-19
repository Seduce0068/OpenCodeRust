use ratatui::{
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, List, ListItem, ListState},
    Frame,
};

use crate::theme::Theme;

use super::DialogClickMap;

#[derive(Clone, Debug)]
pub struct Agent {
    pub name: String,
    pub description: String,
    pub color: ratatui::style::Color,
}

pub struct AgentSelectDialog {
    agents: Vec<Agent>,
    state: ListState,
    open: bool,
    click_map: DialogClickMap,
}

impl AgentSelectDialog {
    pub fn new() -> Self {
        let agents = vec![
            Agent {
                name: "build".into(),
                description: "Code generation and modification".into(),
                color: ratatui::style::Color::Cyan,
            },
            Agent {
                name: "oracle".into(),
                description: "Read-only consultation".into(),
                color: ratatui::style::Color::Magenta,
            },
            Agent {
                name: "metis".into(),
                description: "Pre-planning analysis".into(),
                color: ratatui::style::Color::Yellow,
            },
            Agent {
                name: "momus".into(),
                description: "Expert reviewer".into(),
                color: ratatui::style::Color::Green,
            },
            Agent {
                name: "explore".into(),
                description: "Codebase exploration".into(),
                color: ratatui::style::Color::Blue,
            },
            Agent {
                name: "librarian".into(),
                description: "Documentation lookup".into(),
                color: ratatui::style::Color::Rgb(180, 100, 255),
            },
        ];

        let mut state = ListState::default();
        state.select(Some(0));

        Self {
            agents,
            state,
            open: false,
            click_map: DialogClickMap::default(),
        }
    }

    pub fn set_agents(&mut self, agents: Vec<Agent>) {
        self.agents = agents;
    }

    pub fn agents(&self) -> &[Agent] {
        &self.agents
    }

    pub fn open(&mut self) {
        self.open = true;
        self.state.select(Some(0));
    }

    pub fn close(&mut self) {
        self.open = false;
    }

    pub fn is_open(&self) -> bool {
        self.open
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
            if selected < self.agents.len().saturating_sub(1) {
                self.state.select(Some(selected + 1));
            }
        }
    }

    pub fn selected_agent(&self) -> Option<&Agent> {
        self.state.selected().and_then(|i| self.agents.get(i))
    }

    /// Select the item under the pointer. Returns `true` when the click landed
    /// on the dialog frame (so the app must not close it).
    pub fn handle_click(&mut self, col: u16, row: u16) -> bool {
        if let Some(index) = self.click_map.item_at(col, row) {
            self.state.select(Some(index));
        }
        self.click_map.contains(col, row)
    }

    pub fn render(&self, frame: &mut Frame, area: Rect, theme: &Theme) {
        if !self.open {
            return;
        }

        let dialog_width = 45;
        let dialog_height = (self.agents.len() + 2).min(12) as u16;
        let dialog_area = centered_rect(dialog_width, dialog_height, area);
        self.click_map.set_area(dialog_area);

        frame.render_widget(Clear, dialog_area);

        let block = Block::default()
            .title(Span::styled(
                " Select Agent ",
                Style::default()
                    .fg(theme.primary)
                    .add_modifier(Modifier::BOLD),
            ))
            .borders(Borders::ALL)
            .border_style(Style::default().fg(theme.border))
            .style(Style::default().bg(theme.background_panel));

        let inner_area = super::dialog_inner(block.inner(dialog_area));
        frame.render_widget(block, dialog_area);

        let items: Vec<ListItem> = self
            .agents
            .iter()
            .enumerate()
            .map(|(i, agent)| {
                let is_selected = self.state.selected() == Some(i);
                let style = if is_selected {
                    Style::default().fg(theme.text).bg(theme.background_element)
                } else {
                    Style::default().fg(theme.text)
                };

                ListItem::new(Line::from(vec![
                    Span::styled("● ", Style::default().fg(agent.color)),
                    Span::styled(&agent.name, style.add_modifier(Modifier::BOLD)),
                    Span::styled("  ", style),
                    Span::styled(&agent.description, Style::default().fg(theme.text_muted)),
                ]))
            })
            .collect();

        let list = List::new(items);
        let mut state = self.state;
        frame.render_stateful_widget(list, inner_area, &mut state);
        self.click_map
            .set_list(inner_area, state.offset(), self.agents.len());
    }
}

impl Default for AgentSelectDialog {
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
    use ratatui::{backend::TestBackend, style::Color, Terminal};

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

    fn agent(name: &str, description: &str, color: Color) -> Agent {
        Agent {
            name: name.to_string(),
            description: description.to_string(),
            color,
        }
    }

    fn sample_agents() -> Vec<Agent> {
        vec![
            agent("alpha", "First agent", Color::Cyan),
            agent("beta", "Second agent", Color::Magenta),
            agent("gamma", "Third agent", Color::Rgb(180, 100, 255)),
        ]
    }

    // --- Agent データ保持 ---

    #[test]
    fn agent_stores_name_description_and_color() {
        let a = agent("alpha", "First agent", Color::Rgb(180, 100, 255));

        assert_eq!(a.name, "alpha");
        assert_eq!(a.description, "First agent");
        assert!(matches!(a.color, Color::Rgb(180, 100, 255)));
    }

    #[test]
    fn agent_clone_preserves_all_fields() {
        let original = agent("beta", "Second agent", Color::Magenta);

        let cloned = original.clone();

        assert_eq!(cloned.name, original.name);
        assert_eq!(cloned.description, original.description);
        assert!(matches!(cloned.color, Color::Magenta));
    }

    // --- new / Default ---

    #[test]
    fn new_provides_builtin_agents_and_selects_first() {
        let dialog = AgentSelectDialog::new();

        let names: Vec<&str> = dialog.agents().iter().map(|a| a.name.as_str()).collect();
        assert_eq!(
            names,
            vec!["build", "oracle", "metis", "momus", "explore", "librarian"]
        );
        assert_eq!(dialog.state.selected(), Some(0));
        assert!(matches!(dialog.selected_agent(), Some(a) if a.name == "build"));
        assert!(!dialog.is_open());
    }

    #[test]
    fn default_provides_builtin_agents_and_selects_first() {
        let dialog = AgentSelectDialog::default();

        assert_eq!(dialog.agents().len(), 6);
        assert_eq!(dialog.state.selected(), Some(0));
        assert!(matches!(dialog.selected_agent(), Some(a) if a.name == "build"));
        assert!(!dialog.is_open());
    }

    // --- set_agents ---

    #[test]
    fn set_agents_replaces_builtin_agents() {
        let mut dialog = AgentSelectDialog::new();

        dialog.set_agents(sample_agents());

        assert_eq!(dialog.agents().len(), 3);
        assert_eq!(dialog.agents()[0].name, "alpha");
        assert_eq!(dialog.agents()[1].description, "Second agent");
        assert!(matches!(
            dialog.agents()[2].color,
            Color::Rgb(180, 100, 255)
        ));
        assert!(matches!(dialog.selected_agent(), Some(a) if a.name == "alpha"));
    }

    #[test]
    fn set_agents_preserves_open_and_selection_state() {
        let mut dialog = AgentSelectDialog::new();
        dialog.open();
        dialog.move_down();
        assert_eq!(dialog.state.selected(), Some(1));

        dialog.set_agents(sample_agents());

        assert!(dialog.is_open());
        assert_eq!(dialog.state.selected(), Some(1));
        assert!(matches!(dialog.selected_agent(), Some(a) if a.name == "beta"));
    }

    #[test]
    fn set_agents_can_clear_agents() {
        let mut dialog = AgentSelectDialog::new();
        dialog.open();
        assert_eq!(dialog.state.selected(), Some(0));

        dialog.set_agents(Vec::new());

        assert!(dialog.agents().is_empty());
        assert_eq!(dialog.state.selected(), Some(0));
        assert!(dialog.selected_agent().is_none());
    }

    #[test]
    fn set_agents_shorter_than_selection_leaves_selection_out_of_range() {
        let mut dialog = AgentSelectDialog::new();
        dialog.open();
        dialog.move_down();
        dialog.move_down();
        assert_eq!(dialog.state.selected(), Some(2));

        dialog.set_agents(vec![agent("only", "Only agent", Color::Green)]);

        // set_agents は選択インデックスをリセットしないため範囲外になる。
        assert_eq!(dialog.state.selected(), Some(2));
        assert!(dialog.selected_agent().is_none());
    }

    // --- open / close / is_open ---

    #[test]
    fn open_sets_open_state_and_resets_selection() {
        let mut dialog = AgentSelectDialog::new();
        dialog.move_down();
        assert_eq!(dialog.state.selected(), Some(1));
        assert!(!dialog.is_open());

        dialog.open();

        assert!(dialog.is_open());
        assert_eq!(dialog.state.selected(), Some(0));
        assert!(matches!(dialog.selected_agent(), Some(a) if a.name == "build"));
    }

    #[test]
    fn close_clears_open_state_and_preserves_data() {
        let mut dialog = AgentSelectDialog::new();
        dialog.set_agents(sample_agents());
        dialog.open();
        dialog.move_down();
        assert_eq!(dialog.state.selected(), Some(1));

        dialog.close();

        assert!(!dialog.is_open());
        assert_eq!(dialog.agents().len(), 3);
        assert_eq!(dialog.state.selected(), Some(1));
        assert!(matches!(dialog.selected_agent(), Some(a) if a.name == "beta"));
    }

    #[test]
    fn open_close_cycles_toggle_state() {
        let mut dialog = AgentSelectDialog::new();

        for _ in 0..3 {
            dialog.open();
            assert!(dialog.is_open());
            dialog.close();
            assert!(!dialog.is_open());
        }
    }

    #[test]
    fn close_is_idempotent_from_new() {
        let mut dialog = AgentSelectDialog::new();

        dialog.close();
        dialog.close();

        assert!(!dialog.is_open());
        assert_eq!(dialog.agents().len(), 6);
    }

    #[test]
    fn open_is_idempotent_and_resets_selection() {
        let mut dialog = AgentSelectDialog::new();
        dialog.open();
        dialog.move_down();
        assert_eq!(dialog.state.selected(), Some(1));

        dialog.open();

        assert!(dialog.is_open());
        assert_eq!(dialog.state.selected(), Some(0));
    }

    // --- 選択移動 ---

    #[test]
    fn move_down_advances_selection_and_selected_agent() {
        let mut dialog = AgentSelectDialog::new();
        dialog.open();
        assert_eq!(dialog.state.selected(), Some(0));

        dialog.move_down();

        assert_eq!(dialog.state.selected(), Some(1));
        assert!(matches!(dialog.selected_agent(), Some(a) if a.name == "oracle"));
    }

    #[test]
    fn move_down_clamps_at_last_item() {
        let mut dialog = AgentSelectDialog::new();
        dialog.open();

        for _ in 0..10 {
            dialog.move_down();
        }

        assert_eq!(dialog.state.selected(), Some(5));
        assert!(matches!(dialog.selected_agent(), Some(a) if a.name == "librarian"));
    }

    #[test]
    fn move_up_moves_back_and_clamps_at_zero() {
        let mut dialog = AgentSelectDialog::new();
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
    fn move_down_with_empty_agents_stays_clamped() {
        let mut dialog = AgentSelectDialog::new();
        dialog.set_agents(Vec::new());
        assert_eq!(dialog.state.selected(), Some(0));
        assert!(dialog.selected_agent().is_none());

        dialog.move_down();

        // 空 agents では agents.len().saturating_sub(1) == 0 で進まない。
        assert_eq!(dialog.state.selected(), Some(0));
        assert!(dialog.selected_agent().is_none());
    }

    #[test]
    fn move_up_and_move_down_are_noop_without_selection() {
        let mut dialog = AgentSelectDialog::new();
        dialog.state.select(None);
        assert_eq!(dialog.state.selected(), None);

        dialog.move_up();
        dialog.move_down();

        assert_eq!(dialog.state.selected(), None);
        assert!(dialog.selected_agent().is_none());
    }

    // --- render ---

    #[test]
    fn render_when_closed_draws_nothing() {
        let dialog = AgentSelectDialog::new();
        let theme = Theme::default();

        let text = render_to_string(80, 24, |frame| {
            dialog.render(frame, Rect::new(0, 0, 80, 24), &theme);
        });

        assert!(
            text.trim().is_empty(),
            "closed agent dialog must not draw any content"
        );
    }

    #[test]
    fn render_when_open_shows_title_names_and_descriptions() {
        let mut dialog = AgentSelectDialog::new();
        dialog.set_agents(sample_agents());
        dialog.open();
        let theme = Theme::default();

        let text = render_to_string(80, 24, |frame| {
            dialog.render(frame, Rect::new(0, 0, 80, 24), &theme);
        });

        assert!(text.contains("Select Agent"), "dialog title");
        assert!(text.contains("●"), "agent marker");
        assert!(text.contains("alpha"), "first agent name");
        assert!(text.contains("First agent"), "first agent description");
        assert!(text.contains("beta"), "second agent name");
        assert!(text.contains("Second agent"), "second agent description");
        assert!(text.contains("gamma"), "third agent name");
        assert!(text.contains("Third agent"), "third agent description");
    }

    #[test]
    fn render_when_open_with_empty_agents_still_shows_title() {
        let mut dialog = AgentSelectDialog::new();
        dialog.set_agents(Vec::new());
        dialog.open();
        let theme = Theme::default();

        let text = render_to_string(80, 24, |frame| {
            dialog.render(frame, Rect::new(0, 0, 80, 24), &theme);
        });

        assert!(text.contains("Select Agent"), "dialog title");
    }
}

use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph},
    Frame,
};

use crate::theme::Theme;

#[derive(Clone, Debug)]
pub struct McpItem {
    pub name: String,
    pub status: String,
    pub tools: usize,
    pub resources: usize,
    pub error: Option<String>,
}

pub struct McpDialog {
    items: Vec<McpItem>,
    state: ListState,
    open: bool,
}

impl McpDialog {
    pub fn new() -> Self {
        let mut state = ListState::default();
        state.select(Some(0));
        Self {
            items: Vec::new(),
            state,
            open: false,
        }
    }

    pub fn set_items(&mut self, mut items: Vec<McpItem>) {
        items.sort_by(|a, b| a.name.cmp(&b.name));
        self.items = items;
        self.state
            .select(if self.items.is_empty() { None } else { Some(0) });
    }

    pub fn open(&mut self) {
        self.open = true;
        if self.items.is_empty() {
            self.state.select(None);
        } else if self.state.selected().is_none() {
            self.state.select(Some(0));
        }
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
            if selected < self.items.len().saturating_sub(1) {
                self.state.select(Some(selected + 1));
            }
        }
    }

    pub fn selected_item(&self) -> Option<McpItem> {
        self.state
            .selected()
            .and_then(|idx| self.items.get(idx))
            .cloned()
    }

    pub fn render(&self, frame: &mut Frame, area: Rect, theme: &Theme) {
        if !self.open {
            return;
        }

        let dialog_area = centered_rect(86, 20, area);
        frame.render_widget(Clear, dialog_area);

        let block = Block::default()
            .title(Span::styled(
                " MCP Servers ",
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
            .constraints([Constraint::Min(1), Constraint::Length(1)])
            .split(inner);

        let items = if self.items.is_empty() {
            vec![ListItem::new(Line::from(Span::styled(
                "No MCP server found.",
                Style::default().fg(theme.text_muted),
            )))]
        } else {
            self.items
                .iter()
                .map(|server| {
                    let (status_label, status_color) = match server.status.as_str() {
                        "connected" => ("Connected".to_string(), theme.success),
                        "failed" => (
                            server.error.clone().unwrap_or_else(|| "Failed".to_string()),
                            theme.error,
                        ),
                        "needs_auth" => ("Needs authentication".to_string(), theme.warning),
                        "needs_client_registration" => {
                            ("Needs client registration".to_string(), theme.error)
                        }
                        "disabled" => ("Disabled".to_string(), theme.text_muted),
                        "disconnected" => ("Disconnected".to_string(), theme.text_muted),
                        other => (other.to_string(), theme.info),
                    };

                    let mut spans = vec![
                        Span::styled("● ", Style::default().fg(status_color)),
                        Span::styled(&server.name, Style::default().fg(theme.text)),
                        Span::styled(
                            format!("  {}", status_label),
                            Style::default().fg(theme.text_muted),
                        ),
                    ];
                    if server.tools > 0 || server.resources > 0 {
                        spans.push(Span::styled(
                            format!("  tools:{} resources:{}", server.tools, server.resources),
                            Style::default().fg(theme.text_muted),
                        ));
                    }
                    ListItem::new(Line::from(spans))
                })
                .collect()
        };
        let list = List::new(items).highlight_style(
            Style::default()
                .bg(theme.background_element)
                .add_modifier(Modifier::BOLD),
        );
        frame.render_stateful_widget(list, layout[0], &mut self.state.clone());

        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled("Enter", Style::default().fg(theme.primary)),
                Span::styled(
                    " connect/disconnect  ",
                    Style::default().fg(theme.text_muted),
                ),
                Span::styled("a", Style::default().fg(theme.primary)),
                Span::styled(" auth  ", Style::default().fg(theme.text_muted)),
                Span::styled("x", Style::default().fg(theme.primary)),
                Span::styled(" clear auth  ", Style::default().fg(theme.text_muted)),
                Span::styled("r", Style::default().fg(theme.primary)),
                Span::styled(" refresh  ", Style::default().fg(theme.text_muted)),
                Span::styled("Esc", Style::default().fg(theme.primary)),
                Span::styled(" close", Style::default().fg(theme.text_muted)),
            ])),
            layout[1],
        );
    }
}

impl Default for McpDialog {
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

    /// テスト用 McpItem を構築するヘルパー。
    fn item(
        name: &str,
        status: &str,
        tools: usize,
        resources: usize,
        error: Option<&str>,
    ) -> McpItem {
        McpItem {
            name: name.to_string(),
            status: status.to_string(),
            tools,
            resources,
            error: error.map(|e| e.to_string()),
        }
    }

    fn sample_items() -> Vec<McpItem> {
        vec![
            item("zeta", "connected", 2, 3, None),
            item("alpha", "failed", 0, 0, Some("boom")),
            item("mid", "needs_auth", 1, 0, None),
        ]
    }

    fn names(dialog: &McpDialog) -> Vec<&str> {
        dialog
            .items
            .iter()
            .map(|server| server.name.as_str())
            .collect()
    }

    // --- McpItem データ保持 ---

    #[test]
    fn mcp_item_stores_all_fields() {
        let server = item("srv", "failed", 4, 7, Some("auth failed"));

        assert_eq!(server.name, "srv");
        assert_eq!(server.status, "failed");
        assert_eq!(server.tools, 4);
        assert_eq!(server.resources, 7);
        assert!(matches!(server.error.as_deref(), Some("auth failed")));
    }

    #[test]
    fn mcp_item_allows_missing_error() {
        let server = item("srv", "connected", 0, 0, None);

        assert_eq!(server.name, "srv");
        assert_eq!(server.status, "connected");
        assert_eq!(server.tools, 0);
        assert_eq!(server.resources, 0);
        assert!(server.error.is_none());
    }

    #[test]
    fn mcp_item_clone_preserves_all_fields() {
        let original = item("clone", "disabled", 3, 5, Some("note"));

        let cloned = original.clone();

        assert_eq!(cloned.name, original.name);
        assert_eq!(cloned.status, original.status);
        assert_eq!(cloned.tools, original.tools);
        assert_eq!(cloned.resources, original.resources);
        assert!(matches!(cloned.error.as_deref(), Some("note")));
    }

    // --- new / Default ---

    #[test]
    fn new_starts_empty_with_selection_zero_and_closed() {
        let dialog = McpDialog::new();

        assert!(dialog.items.is_empty());
        assert_eq!(dialog.state.selected(), Some(0));
        assert!(!dialog.is_open());
        assert!(dialog.selected_item().is_none());
    }

    #[test]
    fn default_matches_new_state() {
        let dialog = McpDialog::default();

        assert!(dialog.items.is_empty());
        assert_eq!(dialog.state.selected(), Some(0));
        assert!(!dialog.is_open());
        assert!(dialog.selected_item().is_none());
    }

    // --- set_items（並び順とデータ差し替え） ---

    #[test]
    fn set_items_sorts_by_name_and_selects_first() {
        let mut dialog = McpDialog::new();

        dialog.set_items(sample_items());

        assert_eq!(names(&dialog), vec!["alpha", "mid", "zeta"]);
        assert_eq!(dialog.state.selected(), Some(0));
        let selected = dialog
            .selected_item()
            .expect("first item should be selected");
        assert_eq!(selected.name, "alpha");
        assert_eq!(selected.status, "failed");
        assert_eq!(selected.tools, 0);
        assert_eq!(selected.resources, 0);
        assert!(matches!(selected.error.as_deref(), Some("boom")));
    }

    #[test]
    fn set_items_sorts_case_sensitively() {
        let mut dialog = McpDialog::new();

        dialog.set_items(vec![
            item("beta", "connected", 0, 0, None),
            item("Alpha", "connected", 0, 0, None),
            item("alpha", "connected", 0, 0, None),
        ]);

        // sort_by は name.cmp() なので大文字が小文字より前に並ぶ。
        assert_eq!(names(&dialog), vec!["Alpha", "alpha", "beta"]);
    }

    #[test]
    fn set_items_replaces_previous_items() {
        let mut dialog = McpDialog::new();
        dialog.set_items(sample_items());
        assert_eq!(dialog.items.len(), 3);

        dialog.set_items(vec![item("solo", "connected", 1, 2, None)]);

        assert_eq!(names(&dialog), vec!["solo"]);
        let selected = dialog.selected_item().expect("solo should be selected");
        assert_eq!(selected.name, "solo");
        assert_eq!(selected.tools, 1);
        assert_eq!(selected.resources, 2);
        assert!(selected.error.is_none());
    }

    #[test]
    fn set_items_empty_selects_none_and_no_item() {
        let mut dialog = McpDialog::new();
        dialog.set_items(sample_items());
        assert_eq!(dialog.state.selected(), Some(0));

        dialog.set_items(Vec::new());

        assert!(dialog.items.is_empty());
        assert_eq!(dialog.state.selected(), None);
        assert!(dialog.selected_item().is_none());
    }

    #[test]
    fn set_items_resets_selection_to_first() {
        let mut dialog = McpDialog::new();
        dialog.set_items(sample_items());
        dialog.move_down();
        assert_eq!(dialog.state.selected(), Some(1));

        dialog.set_items(sample_items());

        // 実装は空でなければ常に Some(0) へ戻す（ModelSelectDialog とは異なる）。
        assert_eq!(dialog.state.selected(), Some(0));
        let selected = dialog.selected_item().expect("selection resets to first");
        assert_eq!(selected.name, "alpha");
    }

    // --- open / close / is_open ---

    #[test]
    fn open_sets_open_state_and_selects_none_when_empty() {
        let mut dialog = McpDialog::new();

        dialog.open();

        assert!(dialog.is_open());
        assert!(dialog.items.is_empty());
        assert_eq!(dialog.state.selected(), None);
        assert!(dialog.selected_item().is_none());
    }

    #[test]
    fn open_preserves_existing_selection() {
        let mut dialog = McpDialog::new();
        dialog.set_items(sample_items());
        dialog.move_down();
        assert_eq!(dialog.state.selected(), Some(1));
        assert!(!dialog.is_open());

        dialog.open();

        assert!(dialog.is_open());
        // open() は選択が Some のときは触らない。
        assert_eq!(dialog.state.selected(), Some(1));
        let selected = dialog
            .selected_item()
            .expect("existing selection preserved");
        assert_eq!(selected.name, "mid");
    }

    #[test]
    fn open_recovers_selection_when_none_with_items() {
        let mut dialog = McpDialog::new();
        dialog.set_items(sample_items());
        dialog.state.select(None);
        assert_eq!(dialog.state.selected(), None);

        dialog.open();

        assert!(dialog.is_open());
        assert_eq!(dialog.state.selected(), Some(0));
        let selected = dialog.selected_item().expect("first item selected");
        assert_eq!(selected.name, "alpha");
    }

    #[test]
    fn close_clears_open_state_and_preserves_data() {
        let mut dialog = McpDialog::new();
        dialog.set_items(sample_items());
        dialog.open();
        dialog.move_down();
        assert_eq!(dialog.state.selected(), Some(1));

        dialog.close();

        assert!(!dialog.is_open());
        assert_eq!(names(&dialog), vec!["alpha", "mid", "zeta"]);
        assert_eq!(dialog.state.selected(), Some(1));
        let selected = dialog.selected_item().expect("selection preserved");
        assert_eq!(selected.name, "mid");
    }

    #[test]
    fn open_close_cycles_toggle_state() {
        let mut dialog = McpDialog::new();

        for _ in 0..3 {
            dialog.open();
            assert!(dialog.is_open());
            dialog.close();
            assert!(!dialog.is_open());
        }
    }

    #[test]
    fn open_and_close_are_idempotent_from_new() {
        let mut dialog = McpDialog::new();

        dialog.close();
        dialog.close();
        assert!(!dialog.is_open());

        dialog.open();
        dialog.open();
        assert!(dialog.is_open());
        assert_eq!(dialog.state.selected(), None);
    }

    // --- 選択移動 ---

    #[test]
    fn move_down_advances_selection_and_selected_item() {
        let mut dialog = McpDialog::new();
        dialog.set_items(sample_items());
        assert_eq!(dialog.state.selected(), Some(0));
        let first = dialog.selected_item().expect("first item");
        assert_eq!(first.name, "alpha");

        dialog.move_down();

        assert_eq!(dialog.state.selected(), Some(1));
        let second = dialog.selected_item().expect("second item");
        assert_eq!(second.name, "mid");
        assert_ne!(second.name, first.name);
    }

    #[test]
    fn move_down_clamps_at_last_item() {
        let mut dialog = McpDialog::new();
        dialog.set_items(sample_items());

        for _ in 0..10 {
            dialog.move_down();
        }

        assert_eq!(dialog.state.selected(), Some(2));
        let selected = dialog.selected_item().expect("last item");
        assert_eq!(selected.name, "zeta");
    }

    #[test]
    fn move_up_moves_back_and_clamps_at_zero() {
        let mut dialog = McpDialog::new();
        dialog.set_items(sample_items());
        dialog.move_down();
        dialog.move_down();
        assert_eq!(dialog.state.selected(), Some(2));

        dialog.move_up();
        assert_eq!(dialog.state.selected(), Some(1));
        dialog.move_up();
        assert_eq!(dialog.state.selected(), Some(0));
        dialog.move_up();
        assert_eq!(dialog.state.selected(), Some(0));
        let selected = dialog.selected_item().expect("first item");
        assert_eq!(selected.name, "alpha");
    }

    #[test]
    fn move_down_before_set_items_stays_clamped() {
        let mut dialog = McpDialog::new();
        assert_eq!(dialog.state.selected(), Some(0));
        assert!(dialog.items.is_empty());

        dialog.move_down();

        // items.len().saturating_sub(1) == 0 なので Some(0) のまま動かない。
        assert_eq!(dialog.state.selected(), Some(0));
        assert!(dialog.selected_item().is_none());
    }

    #[test]
    fn move_down_after_open_with_empty_items_stays_none() {
        let mut dialog = McpDialog::new();
        dialog.open();
        assert_eq!(dialog.state.selected(), None);

        dialog.move_down();

        // 空アイテムでは open() が選択を None にしており move_down は no-op。
        assert_eq!(dialog.state.selected(), None);
        assert!(dialog.selected_item().is_none());
    }

    #[test]
    fn move_up_and_move_down_are_noop_without_selection() {
        let mut dialog = McpDialog::new();
        dialog.set_items(Vec::new());
        assert_eq!(dialog.state.selected(), None);

        dialog.move_up();
        dialog.move_down();

        assert_eq!(dialog.state.selected(), None);
        assert!(dialog.selected_item().is_none());
    }

    #[test]
    fn move_down_does_not_advance_with_stale_selection_and_empty_items() {
        let mut dialog = McpDialog::new();
        dialog.set_items(sample_items());
        dialog.move_down();
        assert_eq!(dialog.state.selected(), Some(1));

        // set_items(Vec::new()) は選択を None にするため、直接状態を置いて
        // 空データ＋外れた選択のクランプ挙動を確認する。
        dialog.set_items(Vec::new());
        dialog.state.select(Some(2));
        assert!(dialog.items.is_empty());

        dialog.move_down();

        // 上限が 0 にクランプされ、選択は据え置き。
        assert_eq!(dialog.state.selected(), Some(2));
        assert!(dialog.selected_item().is_none());
    }

    #[test]
    fn selected_item_none_without_selection() {
        let mut dialog = McpDialog::new();
        dialog.set_items(sample_items());
        dialog.state.select(None);

        assert!(dialog.selected_item().is_none());
    }

    // --- render ---

    #[test]
    fn render_when_closed_draws_nothing() {
        let dialog = McpDialog::new();
        let theme = Theme::default();

        let text = render_to_string(80, 24, |frame| {
            dialog.render(frame, Rect::new(0, 0, 80, 24), &theme);
        });

        assert!(
            text.trim().is_empty(),
            "closed MCP dialog must not draw any content"
        );
    }

    #[test]
    fn render_when_open_shows_title_items_counts_and_hints() {
        let mut dialog = McpDialog::new();
        dialog.set_items(vec![
            item("alpha", "connected", 4, 6, None),
            item("beta", "failed", 0, 0, Some("handshake failed")),
        ]);
        dialog.open();
        let theme = Theme::default();

        let text = render_to_string(80, 24, |frame| {
            dialog.render(frame, Rect::new(0, 0, 80, 24), &theme);
        });

        assert!(text.contains("MCP Servers"), "dialog title");
        assert!(text.contains("alpha"), "first server name");
        assert!(text.contains("Connected"), "connected status label");
        assert!(text.contains("tools:4 resources:6"), "tool/resource counts");
        assert!(text.contains("beta"), "second server name");
        assert!(
            text.contains("handshake failed"),
            "failed status uses the error text"
        );
        assert!(text.contains("Enter"), "connect/disconnect hint");
        assert!(text.contains("Esc"), "close hint");
    }

    #[test]
    fn render_shows_status_labels_for_known_and_unknown_states() {
        let mut dialog = McpDialog::new();
        dialog.set_items(vec![
            item("a", "needs_auth", 0, 0, None),
            item("b", "needs_client_registration", 0, 0, None),
            item("c", "disabled", 0, 0, None),
            item("d", "disconnected", 0, 0, None),
            item("e", "custom_status", 0, 0, None),
        ]);
        dialog.open();
        let theme = Theme::default();

        let text = render_to_string(80, 24, |frame| {
            dialog.render(frame, Rect::new(0, 0, 80, 24), &theme);
        });

        assert!(text.contains("Needs authentication"), "needs_auth label");
        assert!(
            text.contains("Needs client registration"),
            "needs_client_registration label"
        );
        assert!(text.contains("Disabled"), "disabled label");
        assert!(text.contains("Disconnected"), "disconnected label");
        assert!(
            text.contains("custom_status"),
            "unknown status is shown verbatim"
        );
    }

    #[test]
    fn render_with_empty_items_shows_placeholder() {
        let mut dialog = McpDialog::new();
        dialog.open();
        let theme = Theme::default();

        let text = render_to_string(80, 24, |frame| {
            dialog.render(frame, Rect::new(0, 0, 80, 24), &theme);
        });

        assert!(text.contains("MCP Servers"), "dialog title");
        assert!(text.contains("No MCP server found."), "empty placeholder");
    }
}

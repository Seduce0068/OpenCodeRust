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
pub struct ForkEntry {
    pub message_id: String,
    pub role: String,
    pub preview: String,
    pub timestamp: String,
}

pub struct ForkDialog {
    entries: Vec<ForkEntry>,
    state: ListState,
    session_id: Option<String>,
    open: bool,
    click_map: DialogClickMap,
}

impl ForkDialog {
    pub fn new() -> Self {
        Self {
            entries: Vec::new(),
            state: ListState::default(),
            session_id: None,
            open: false,
            click_map: DialogClickMap::default(),
        }
    }

    pub fn open(&mut self, session_id: String, entries: Vec<ForkEntry>) {
        self.session_id = Some(session_id);
        self.entries = entries;
        self.state.select(if self.entries.is_empty() {
            None
        } else {
            Some(0)
        });
        self.open = true;
    }

    pub fn close(&mut self) {
        self.open = false;
    }

    pub fn is_open(&self) -> bool {
        self.open
    }

    pub fn session_id(&self) -> Option<&str> {
        self.session_id.as_deref()
    }

    pub fn move_up(&mut self) {
        if let Some(i) = self.state.selected() {
            if i > 0 {
                self.state.select(Some(i - 1));
            }
        }
    }

    pub fn move_down(&mut self) {
        if let Some(i) = self.state.selected() {
            if i < self.entries.len().saturating_sub(1) {
                self.state.select(Some(i + 1));
            }
        }
    }

    pub fn selected_entry(&self) -> Option<&ForkEntry> {
        self.state.selected().and_then(|i| self.entries.get(i))
    }

    pub fn selected_message_id(&self) -> Option<&str> {
        self.selected_entry().map(|e| e.message_id.as_str())
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
        let dialog_width = 70u16.min(area.width.saturating_sub(4));
        let dialog_height = 20u16.min(area.height.saturating_sub(4));
        let dialog_area = centered_rect(dialog_width, dialog_height, area);
        self.click_map.set_area(dialog_area);
        frame.render_widget(Clear, dialog_area);
        let block = Block::default()
            .title(Span::styled(
                " Fork Session ",
                Style::default()
                    .fg(theme.primary)
                    .add_modifier(Modifier::BOLD),
            ))
            .borders(Borders::ALL)
            .border_style(Style::default().fg(theme.border))
            .style(Style::default().bg(theme.background_panel));
        let inner = super::dialog_inner(block.inner(dialog_area));
        frame.render_widget(block, dialog_area);
        if self.entries.is_empty() {
            self.click_map.set_list(inner, 0, 0);
            let empty = ratatui::widgets::Paragraph::new("No messages to fork from")
                .style(Style::default().fg(theme.text_muted));
            frame.render_widget(empty, inner);
            return;
        }
        let hint = ratatui::widgets::Paragraph::new(
            "Select a message to fork from. Enter to confirm, Esc to cancel.",
        )
        .style(Style::default().fg(theme.text_muted));
        frame.render_widget(
            hint,
            Rect {
                x: inner.x,
                y: inner.y,
                width: inner.width,
                height: 1,
            },
        );
        let list_area = Rect {
            x: inner.x,
            y: inner.y + 2,
            width: inner.width,
            height: inner.height.saturating_sub(2),
        };
        let items: Vec<ListItem> = self
            .entries
            .iter()
            .map(|entry| {
                let role_icon = match entry.role.as_str() {
                    "user" => "[U]",
                    "assistant" => "[A]",
                    _ => "[S]",
                };
                ListItem::new(Line::from(vec![
                    Span::styled(
                        format!("{} ", role_icon),
                        Style::default().fg(theme.primary),
                    ),
                    Span::styled(&entry.preview, Style::default().fg(theme.text)),
                    Span::styled(
                        format!("  {}", entry.timestamp),
                        Style::default().fg(theme.text_muted),
                    ),
                ]))
            })
            .collect();
        let list = List::new(items).highlight_style(
            Style::default()
                .bg(theme.background_element)
                .add_modifier(Modifier::BOLD),
        );
        let mut state = self.state;
        frame.render_stateful_widget(list, list_area, &mut state);
        self.click_map
            .set_list(list_area, state.offset(), self.entries.len());
    }
}

impl Default for ForkDialog {
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

    /// テスト用 ForkEntry を構築するヘルパー。
    fn entry(message_id: &str, role: &str, preview: &str, timestamp: &str) -> ForkEntry {
        ForkEntry {
            message_id: message_id.to_string(),
            role: role.to_string(),
            preview: preview.to_string(),
            timestamp: timestamp.to_string(),
        }
    }

    /// テスト用の 3 件のエントリを返すヘルパー。
    fn sample_entries() -> Vec<ForkEntry> {
        vec![
            entry("m1", "user", "first question", "10-01 09:00"),
            entry("m2", "assistant", "first answer", "10-01 09:01"),
            entry("m3", "system", "context note", "10-01 09:02"),
        ]
    }

    // --- ForkEntry データ保持 ---

    #[test]
    fn entry_stores_all_fields() {
        let e = entry("m1", "user", "first question", "10-01 09:00");

        assert_eq!(e.message_id, "m1");
        assert_eq!(e.role, "user");
        assert_eq!(e.preview, "first question");
        assert_eq!(e.timestamp, "10-01 09:00");
    }

    #[test]
    fn entry_clone_preserves_all_fields() {
        let original = entry("m2", "assistant", "first answer", "10-01 09:01");

        let cloned = original.clone();

        assert_eq!(cloned.message_id, original.message_id);
        assert_eq!(cloned.role, original.role);
        assert_eq!(cloned.preview, original.preview);
        assert_eq!(cloned.timestamp, original.timestamp);
    }

    // --- new / Default ---

    #[test]
    fn new_starts_empty_and_closed() {
        let dialog = ForkDialog::new();

        assert!(dialog.entries.is_empty());
        assert!(dialog.session_id().is_none());
        assert_eq!(dialog.state.selected(), None);
        assert!(!dialog.is_open());
    }

    #[test]
    fn default_starts_empty_and_closed() {
        let dialog = ForkDialog::default();

        assert!(dialog.entries.is_empty());
        assert!(dialog.session_id().is_none());
        assert_eq!(dialog.state.selected(), None);
        assert!(!dialog.is_open());
    }

    // --- open / close / is_open ---

    #[test]
    fn open_sets_open_state_session_and_selects_first() {
        let mut dialog = ForkDialog::new();
        assert!(!dialog.is_open());

        dialog.open("s1".to_string(), sample_entries());

        assert!(dialog.is_open());
        assert_eq!(dialog.session_id(), Some("s1"));
        assert_eq!(dialog.state.selected(), Some(0));
        assert!(matches!(dialog.selected_entry(), Some(e) if e.message_id == "m1"));
        assert_eq!(dialog.selected_message_id(), Some("m1"));
    }

    #[test]
    fn open_with_empty_entries_selects_none() {
        let mut dialog = ForkDialog::new();

        dialog.open("s-empty".to_string(), Vec::new());

        assert!(dialog.is_open());
        assert_eq!(dialog.session_id(), Some("s-empty"));
        assert_eq!(dialog.state.selected(), None);
        assert!(dialog.selected_entry().is_none());
        assert_eq!(dialog.selected_message_id(), None);
    }

    #[test]
    fn move_down_after_open_with_empty_entries_stays_clamped() {
        let mut dialog = ForkDialog::new();
        dialog.open("s-empty".to_string(), Vec::new());
        assert_eq!(dialog.state.selected(), None);

        dialog.move_down();

        // 空 entries では選択が None のまま（直近ラウンドで失敗したパターン）。
        assert_eq!(dialog.state.selected(), None);
        assert!(dialog.selected_entry().is_none());
        assert_eq!(dialog.selected_message_id(), None);
    }

    #[test]
    fn close_clears_open_state_and_preserves_data() {
        let mut dialog = ForkDialog::new();
        dialog.open("s1".to_string(), sample_entries());
        dialog.move_down();
        assert_eq!(dialog.state.selected(), Some(1));

        dialog.close();

        assert!(!dialog.is_open());
        assert_eq!(dialog.session_id(), Some("s1"));
        assert_eq!(dialog.entries.len(), 3);
        assert_eq!(dialog.state.selected(), Some(1));
        assert_eq!(dialog.selected_message_id(), Some("m2"));
    }

    #[test]
    fn open_close_cycles_toggle_state() {
        let mut dialog = ForkDialog::new();

        for _ in 0..3 {
            dialog.open("s1".to_string(), sample_entries());
            assert!(dialog.is_open());
            dialog.close();
            assert!(!dialog.is_open());
        }
    }

    #[test]
    fn close_is_idempotent_from_new() {
        let mut dialog = ForkDialog::new();

        dialog.close();
        dialog.close();

        assert!(!dialog.is_open());
        assert!(dialog.session_id().is_none());
    }

    #[test]
    fn open_replaces_entries_and_resets_selection() {
        let mut dialog = ForkDialog::new();
        dialog.open("s1".to_string(), sample_entries());
        dialog.move_down();
        assert_eq!(dialog.state.selected(), Some(1));

        dialog.open(
            "s2".to_string(),
            vec![entry("x1", "user", "only", "10-02 00:00")],
        );

        assert!(dialog.is_open());
        assert_eq!(dialog.session_id(), Some("s2"));
        assert_eq!(dialog.entries.len(), 1);
        assert_eq!(dialog.state.selected(), Some(0));
        assert_eq!(dialog.selected_message_id(), Some("x1"));
    }

    // --- 選択状態遷移 (move_up / move_down) ---

    #[test]
    fn move_down_advances_selection_and_selected_entry() {
        let mut dialog = ForkDialog::new();
        dialog.open("s1".to_string(), sample_entries());
        assert_eq!(dialog.state.selected(), Some(0));

        dialog.move_down();

        assert_eq!(dialog.state.selected(), Some(1));
        assert_eq!(dialog.selected_message_id(), Some("m2"));
        assert!(matches!(dialog.selected_entry(), Some(e) if e.role == "assistant"));
    }

    #[test]
    fn move_down_clamps_at_last_item() {
        let mut dialog = ForkDialog::new();
        dialog.open("s1".to_string(), sample_entries());

        for _ in 0..10 {
            dialog.move_down();
        }

        assert_eq!(dialog.state.selected(), Some(2));
        assert_eq!(dialog.selected_message_id(), Some("m3"));
    }

    #[test]
    fn move_up_moves_back_and_clamps_at_zero() {
        let mut dialog = ForkDialog::new();
        dialog.open("s1".to_string(), sample_entries());
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
        let mut dialog = ForkDialog::new();
        assert_eq!(dialog.state.selected(), None);

        dialog.move_up();
        dialog.move_down();

        assert_eq!(dialog.state.selected(), None);
        assert!(dialog.selected_entry().is_none());
    }

    // --- render ---

    #[test]
    fn render_when_closed_draws_nothing() {
        let dialog = ForkDialog::new();
        let theme = Theme::default();
        assert!(!dialog.is_open());

        let text = render_to_string(80, 24, |frame| {
            dialog.render(frame, Rect::new(0, 0, 80, 24), &theme);
        });

        assert!(
            text.trim().is_empty(),
            "closed fork dialog must not draw any content"
        );
    }

    #[test]
    fn render_when_open_without_entries_shows_empty_message() {
        let mut dialog = ForkDialog::new();
        dialog.open("s-empty".to_string(), Vec::new());
        let theme = Theme::default();
        assert!(dialog.is_open());

        let text = render_to_string(80, 24, |frame| {
            dialog.render(frame, Rect::new(0, 0, 80, 24), &theme);
        });

        assert!(text.contains("Fork Session"), "dialog title");
        assert!(text.contains("No messages to fork from"), "empty message");
    }

    #[test]
    fn render_when_open_shows_entries_and_role_icons() {
        let mut dialog = ForkDialog::new();
        dialog.open("s1".to_string(), sample_entries());
        let theme = Theme::default();

        let text = render_to_string(80, 24, |frame| {
            dialog.render(frame, Rect::new(0, 0, 80, 24), &theme);
        });

        assert!(text.contains("Fork Session"), "dialog title");
        assert!(text.contains("[U]"), "user role icon");
        assert!(text.contains("[A]"), "assistant role icon");
        assert!(text.contains("[S]"), "system role icon");
        assert!(text.contains("first question"), "first preview");
        assert!(text.contains("first answer"), "second preview");
        assert!(text.contains("context note"), "third preview");
        assert!(text.contains("10-01 09:00"), "timestamp");
        assert!(text.contains("Enter to confirm"), "hint text");
    }
}

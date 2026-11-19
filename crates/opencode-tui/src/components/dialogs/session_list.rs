use chrono::{Local, TimeZone};
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
pub struct SessionItem {
    pub id: String,
    pub title: String,
    pub directory: String,
    pub parent_id: Option<String>,
    pub updated_at: i64,
    pub is_busy: bool,
}

#[derive(Clone, Debug)]
pub enum DeleteState {
    Armed(String),
    Confirmed(String),
}

pub struct SessionListDialog {
    sessions: Vec<SessionItem>,
    filtered: Vec<usize>,
    query: String,
    state: ListState,
    open: bool,
    active_session_id: Option<String>,
    pending_delete_session_id: Option<String>,
    rename_session_id: Option<String>,
    rename_input: String,
    click_map: DialogClickMap,
}

impl SessionListDialog {
    pub fn new() -> Self {
        let mut state = ListState::default();
        state.select(Some(0));
        Self {
            sessions: Vec::new(),
            filtered: Vec::new(),
            query: String::new(),
            state,
            open: false,
            active_session_id: None,
            pending_delete_session_id: None,
            rename_session_id: None,
            rename_input: String::new(),
            click_map: DialogClickMap::default(),
        }
    }

    pub fn set_sessions(&mut self, mut sessions: Vec<SessionItem>) {
        sessions.retain(|s| s.parent_id.is_none());
        sessions.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
        self.sessions = sessions;
        self.filter();
    }

    pub fn open(&mut self, current_session_id: Option<&str>) {
        self.open = true;
        self.query.clear();
        self.pending_delete_session_id = None;
        self.rename_session_id = None;
        self.rename_input.clear();
        self.active_session_id = current_session_id.map(|id| id.to_string());
        self.filter();

        if let Some(current_id) = current_session_id {
            if let Some(filtered_index) = self
                .filtered
                .iter()
                .position(|idx| self.sessions.get(*idx).is_some_and(|s| s.id == current_id))
            {
                self.state.select(Some(filtered_index));
            }
        }
    }

    pub fn close(&mut self) {
        self.open = false;
        self.pending_delete_session_id = None;
        self.rename_session_id = None;
        self.rename_input.clear();
    }

    pub fn is_open(&self) -> bool {
        self.open
    }

    pub fn is_renaming(&self) -> bool {
        self.rename_session_id.is_some()
    }

    pub fn query(&self) -> &str {
        &self.query
    }

    pub fn handle_input(&mut self, c: char) {
        self.query.push(c);
        self.pending_delete_session_id = None;
        self.filter();
    }

    pub fn handle_backspace(&mut self) {
        self.query.pop();
        self.pending_delete_session_id = None;
        self.filter();
    }

    pub fn move_up(&mut self) {
        if let Some(selected) = self.state.selected() {
            if selected > 0 {
                self.state.select(Some(selected - 1));
                self.pending_delete_session_id = None;
            }
        }
    }

    pub fn move_down(&mut self) {
        if let Some(selected) = self.state.selected() {
            if selected < self.filtered.len().saturating_sub(1) {
                self.state.select(Some(selected + 1));
                self.pending_delete_session_id = None;
            }
        }
    }

    /// Select the item under the pointer. Returns `true` when the click landed
    /// on the dialog frame (so the app must not close it).
    pub fn handle_click(&mut self, col: u16, row: u16) -> bool {
        if !self.is_renaming() {
            if let Some(index) = self.click_map.item_at(col, row) {
                self.state.select(Some(index));
            }
        }
        self.click_map.contains(col, row)
    }

    pub fn selected_session_id(&self) -> Option<String> {
        self.state
            .selected()
            .and_then(|idx| self.filtered.get(idx))
            .and_then(|session_idx| self.sessions.get(*session_idx))
            .map(|s| s.id.clone())
    }

    pub fn start_rename_selected(&mut self) -> bool {
        let Some(selected_id) = self.selected_session_id() else {
            return false;
        };
        let Some(session) = self
            .sessions
            .iter()
            .find(|session| session.id == selected_id)
        else {
            return false;
        };
        self.pending_delete_session_id = None;
        self.rename_session_id = Some(session.id.clone());
        self.rename_input = session.title.clone();
        true
    }

    pub fn cancel_rename(&mut self) {
        self.rename_session_id = None;
        self.rename_input.clear();
    }

    pub fn handle_rename_input(&mut self, c: char) {
        self.rename_input.push(c);
    }

    pub fn handle_rename_backspace(&mut self) {
        self.rename_input.pop();
    }

    pub fn confirm_rename(&mut self) -> Option<(String, String)> {
        let session_id = self.rename_session_id.clone()?;
        let title = self.rename_input.trim().to_string();
        if title.is_empty() {
            return None;
        }
        self.rename_session_id = None;
        self.rename_input.clear();
        Some((session_id, title))
    }

    pub fn trigger_delete_selected(&mut self) -> Option<DeleteState> {
        let selected_id = self.selected_session_id()?;
        if self.pending_delete_session_id.as_deref() == Some(selected_id.as_str()) {
            self.pending_delete_session_id = None;
            return Some(DeleteState::Confirmed(selected_id));
        }
        self.pending_delete_session_id = Some(selected_id.clone());
        Some(DeleteState::Armed(selected_id))
    }

    fn filter(&mut self) {
        let query = self.query.to_lowercase();
        self.filtered = self
            .sessions
            .iter()
            .enumerate()
            .filter(|(_, session)| {
                session.title.to_lowercase().contains(&query)
                    || session.id.to_lowercase().contains(&query)
                    || session.directory.to_lowercase().contains(&query)
            })
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

        let dialog_area = centered_rect(82, 24, area);
        self.click_map.set_area(dialog_area);
        frame.render_widget(Clear, dialog_area);

        let block = Block::default()
            .title(Span::styled(
                " Sessions ",
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
            .filter_map(|idx| self.sessions.get(*idx))
            .map(|session| {
                let is_current = self.active_session_id.as_deref() == Some(session.id.as_str());
                let is_pending_delete =
                    self.pending_delete_session_id.as_deref() == Some(session.id.as_str());
                let (category, time_label) = format_session_time(session.updated_at);
                let title = if is_pending_delete {
                    "Press ctrl+d again to confirm delete".to_string()
                } else {
                    session.title.clone()
                };
                let title_style = if is_pending_delete {
                    Style::default()
                        .fg(theme.error)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(theme.text)
                };
                let marker = if is_current { "● " } else { "  " };
                let busy = if session.is_busy { "◌ " } else { "  " };
                ListItem::new(Line::from(vec![
                    Span::styled(
                        marker,
                        Style::default().fg(if is_current {
                            theme.success
                        } else {
                            theme.text_muted
                        }),
                    ),
                    Span::styled(
                        busy,
                        Style::default().fg(if session.is_busy {
                            theme.warning
                        } else {
                            theme.text_muted
                        }),
                    ),
                    Span::styled(title, title_style),
                    Span::styled(
                        format!(
                            "  {}  {}  {}  {}",
                            category, time_label, session.id, session.directory
                        ),
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
        frame.render_stateful_widget(list, layout[1], &mut state);
        self.click_map
            .set_list(layout[1], state.offset(), self.filtered.len());

        let action_line = if self.is_renaming() {
            Line::from(vec![
                Span::styled("Rename: ", Style::default().fg(theme.primary)),
                Span::styled(&self.rename_input, Style::default().fg(theme.text)),
                Span::styled("▏", Style::default().fg(theme.primary)),
            ])
        } else {
            Line::from(vec![
                Span::styled("ctrl+r", Style::default().fg(theme.primary)),
                Span::styled(" rename  ", Style::default().fg(theme.text_muted)),
                Span::styled("ctrl+d", Style::default().fg(theme.primary)),
                Span::styled(" delete", Style::default().fg(theme.text_muted)),
            ])
        };
        frame.render_widget(Paragraph::new(action_line), layout[2]);

        let footer = if self.is_renaming() {
            Paragraph::new(Line::from(vec![
                Span::styled("Enter", Style::default().fg(theme.primary)),
                Span::styled(" save  ", Style::default().fg(theme.text_muted)),
                Span::styled("Esc", Style::default().fg(theme.primary)),
                Span::styled(" cancel", Style::default().fg(theme.text_muted)),
            ]))
        } else {
            Paragraph::new(Line::from(vec![
                Span::styled("Enter", Style::default().fg(theme.primary)),
                Span::styled(" open session  ", Style::default().fg(theme.text_muted)),
                Span::styled("Esc", Style::default().fg(theme.primary)),
                Span::styled(" close", Style::default().fg(theme.text_muted)),
            ]))
        };
        frame.render_widget(footer, layout[3]);
    }
}

impl Default for SessionListDialog {
    fn default() -> Self {
        Self::new()
    }
}

fn centered_rect(width: u16, height: u16, area: Rect) -> Rect {
    super::centered_rect(width, height, area)
}

fn format_session_time(updated_at_ms: i64) -> (String, String) {
    let Some(updated_local) = Local.timestamp_millis_opt(updated_at_ms).single() else {
        return ("Unknown".to_string(), "--:--".to_string());
    };

    let now = Local::now();
    let category = if updated_local.date_naive() == now.date_naive() {
        "Today".to_string()
    } else {
        updated_local.format("%Y-%m-%d").to_string()
    };

    (category, updated_local.format("%H:%M").to_string())
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

    /// テスト用 SessionItem を構築するヘルパー。
    fn session(
        id: &str,
        title: &str,
        directory: &str,
        parent_id: Option<&str>,
        updated_at: i64,
        is_busy: bool,
    ) -> SessionItem {
        SessionItem {
            id: id.to_string(),
            title: title.to_string(),
            directory: directory.to_string(),
            parent_id: parent_id.map(str::to_string),
            updated_at,
            is_busy,
        }
    }

    /// 更新時刻降順に並ぶ 3 件のトップレベルセッションを返すヘルパー。
    fn sample_sessions() -> Vec<SessionItem> {
        vec![
            session("s1", "Alpha", "/proj/alpha", None, 1_700_000_000_000, false),
            session("s2", "Beta", "/proj/beta", None, 1_700_000_060_000, true),
            session("s3", "Gamma", "/proj/gamma", None, 1_700_000_120_000, false),
        ]
    }

    /// セッションを設定して開いたダイアログを返すヘルパー。
    fn opened_dialog() -> SessionListDialog {
        let mut dialog = SessionListDialog::new();
        dialog.set_sessions(sample_sessions());
        dialog.open(None);
        dialog
    }

    // --- SessionItem データ保持 ---

    #[test]
    fn session_stores_all_fields() {
        let item = session(
            "s1",
            "Alpha",
            "/proj/alpha",
            Some("parent"),
            1_700_000_000_000,
            true,
        );

        assert_eq!(item.id, "s1");
        assert_eq!(item.title, "Alpha");
        assert_eq!(item.directory, "/proj/alpha");
        assert_eq!(item.parent_id.as_deref(), Some("parent"));
        assert_eq!(item.updated_at, 1_700_000_000_000);
        assert!(item.is_busy);
    }

    #[test]
    fn session_allows_missing_parent_and_idle_state() {
        let item = session("s2", "Beta", "/proj/beta", None, 1_700_000_060_000, false);

        assert_eq!(item.id, "s2");
        assert!(item.parent_id.is_none());
        assert!(!item.is_busy);
    }

    #[test]
    fn session_clone_preserves_all_fields() {
        let original = session(
            "s3",
            "Gamma",
            "/proj/gamma",
            Some("root"),
            1_700_000_120_000,
            true,
        );

        let cloned = original.clone();

        assert_eq!(cloned.id, original.id);
        assert_eq!(cloned.title, original.title);
        assert_eq!(cloned.directory, original.directory);
        assert_eq!(cloned.parent_id, original.parent_id);
        assert_eq!(cloned.updated_at, original.updated_at);
        assert_eq!(cloned.is_busy, original.is_busy);
    }

    // --- DeleteState ---

    #[test]
    fn delete_state_keeps_armed_and_confirmed_payload() {
        let armed = DeleteState::Armed("s1".to_string());
        let confirmed = DeleteState::Confirmed("s2".to_string());

        assert!(matches!(armed, DeleteState::Armed(id) if id == "s1"));
        assert!(matches!(confirmed, DeleteState::Confirmed(id) if id == "s2"));
    }

    #[test]
    fn delete_state_clone_preserves_variant_and_id() {
        let original = DeleteState::Armed("s3".to_string());

        let cloned = original.clone();

        assert!(matches!(cloned, DeleteState::Armed(id) if id == "s3"));
        assert!(matches!(original, DeleteState::Armed(id) if id == "s3"));
    }

    // --- new / Default ---

    #[test]
    fn new_starts_empty_and_closed() {
        let dialog = SessionListDialog::new();

        assert!(!dialog.is_open());
        assert!(dialog.query().is_empty());
        assert!(!dialog.is_renaming());
        // new() は選択を Some(0) にするが filtered は空なので実セッションは無い。
        assert_eq!(dialog.state.selected(), Some(0));
        assert!(dialog.selected_session_id().is_none());
    }

    #[test]
    fn default_matches_new() {
        let dialog = SessionListDialog::default();

        assert!(!dialog.is_open());
        assert!(dialog.query().is_empty());
        assert_eq!(dialog.state.selected(), Some(0));
        assert!(dialog.selected_session_id().is_none());
    }

    // --- set_sessions ---

    #[test]
    fn set_sessions_sorts_by_updated_at_descending() {
        let mut dialog = SessionListDialog::new();

        dialog.set_sessions(sample_sessions());

        assert_eq!(dialog.state.selected(), Some(0));
        assert_eq!(dialog.selected_session_id().as_deref(), Some("s3"));
    }

    #[test]
    fn set_sessions_excludes_child_sessions() {
        let mut dialog = SessionListDialog::new();
        let mut sessions = sample_sessions();
        sessions.push(session(
            "s3-child",
            "Child",
            "/proj/child",
            Some("s3"),
            1_800_000_000_000,
            false,
        ));

        dialog.set_sessions(sessions);

        assert_eq!(dialog.selected_session_id().as_deref(), Some("s3"));
        dialog.move_down();
        assert_eq!(dialog.selected_session_id().as_deref(), Some("s2"));
        dialog.move_down();
        assert_eq!(dialog.selected_session_id().as_deref(), Some("s1"));
        dialog.move_down();
        // 子セッションは filtered に含まれないため末尾でクランプされる。
        assert_eq!(dialog.selected_session_id().as_deref(), Some("s1"));
    }

    #[test]
    fn set_sessions_replaces_existing_sessions() {
        let mut dialog = SessionListDialog::new();
        dialog.set_sessions(sample_sessions());
        assert_eq!(dialog.selected_session_id().as_deref(), Some("s3"));

        dialog.set_sessions(vec![session("x1", "Only", "/only", None, 1, false)]);

        assert_eq!(dialog.selected_session_id().as_deref(), Some("x1"));
        dialog.move_down();
        assert_eq!(dialog.selected_session_id().as_deref(), Some("x1"));
    }

    #[test]
    fn set_sessions_can_clear_sessions_and_selection() {
        let mut dialog = SessionListDialog::new();
        dialog.set_sessions(sample_sessions());
        assert!(dialog.selected_session_id().is_some());

        dialog.set_sessions(Vec::new());

        assert_eq!(dialog.state.selected(), None);
        assert!(dialog.selected_session_id().is_none());
    }

    // --- open / close / is_open ---

    #[test]
    fn open_sets_open_state_and_selects_first_session() {
        let mut dialog = SessionListDialog::new();
        dialog.set_sessions(sample_sessions());
        assert!(!dialog.is_open());

        dialog.open(None);

        assert!(dialog.is_open());
        assert_eq!(dialog.state.selected(), Some(0));
        assert_eq!(dialog.selected_session_id().as_deref(), Some("s3"));
    }

    #[test]
    fn open_selects_current_session() {
        let mut dialog = SessionListDialog::new();
        dialog.set_sessions(sample_sessions());

        dialog.open(Some("s1"));

        assert!(dialog.is_open());
        assert_eq!(dialog.state.selected(), Some(2));
        assert_eq!(dialog.selected_session_id().as_deref(), Some("s1"));
        assert_eq!(dialog.active_session_id.as_deref(), Some("s1"));
    }

    #[test]
    fn open_with_unknown_current_session_falls_back_to_first() {
        let mut dialog = SessionListDialog::new();
        dialog.set_sessions(sample_sessions());

        dialog.open(Some("missing"));

        assert!(dialog.is_open());
        assert_eq!(dialog.state.selected(), Some(0));
        assert_eq!(dialog.selected_session_id().as_deref(), Some("s3"));
        assert_eq!(dialog.active_session_id.as_deref(), Some("missing"));
    }

    #[test]
    fn open_without_sessions_selects_none() {
        let mut dialog = SessionListDialog::new();

        dialog.open(None);

        assert!(dialog.is_open());
        assert_eq!(dialog.state.selected(), None);
        assert!(dialog.selected_session_id().is_none());
    }

    #[test]
    fn open_clears_query_pending_delete_and_rename() {
        let mut dialog = opened_dialog();
        dialog.handle_input('b');
        assert!(matches!(
            dialog.trigger_delete_selected(),
            Some(DeleteState::Armed(id)) if id == "s2"
        ));
        assert!(dialog.start_rename_selected());
        assert!(dialog.is_renaming());

        dialog.open(None);

        assert!(dialog.is_open());
        assert!(dialog.query().is_empty());
        assert!(!dialog.is_renaming());
        // pending delete がクリアされているので再び Armed から始まる。
        assert!(matches!(
            dialog.trigger_delete_selected(),
            Some(DeleteState::Armed(id)) if id == "s3"
        ));
    }

    #[test]
    fn close_clears_open_and_transient_state_but_keeps_data() {
        let mut dialog = opened_dialog();
        dialog.handle_input('g');
        assert!(dialog.trigger_delete_selected().is_some());
        assert!(dialog.start_rename_selected());
        assert!(dialog.is_open());

        dialog.close();

        assert!(!dialog.is_open());
        assert!(!dialog.is_renaming());
        // close() は query を消さない。
        assert_eq!(dialog.query(), "g");
        assert!(dialog.selected_session_id().is_some());
        assert!(matches!(
            dialog.trigger_delete_selected(),
            Some(DeleteState::Armed(id)) if id == "s3"
        ));
    }

    #[test]
    fn open_close_cycles_toggle_state() {
        let mut dialog = SessionListDialog::new();
        dialog.set_sessions(sample_sessions());

        for _ in 0..3 {
            dialog.open(None);
            assert!(dialog.is_open());
            dialog.close();
            assert!(!dialog.is_open());
        }
    }

    #[test]
    fn close_is_idempotent_from_new() {
        let mut dialog = SessionListDialog::new();

        dialog.close();
        dialog.close();

        assert!(!dialog.is_open());
        assert_eq!(dialog.state.selected(), Some(0));
    }

    // --- 選択状態遷移 ---

    #[test]
    fn move_down_advances_selection() {
        let mut dialog = opened_dialog();
        assert_eq!(dialog.selected_session_id().as_deref(), Some("s3"));

        dialog.move_down();

        assert_eq!(dialog.state.selected(), Some(1));
        assert_eq!(dialog.selected_session_id().as_deref(), Some("s2"));
    }

    #[test]
    fn move_down_clamps_at_last_item() {
        let mut dialog = opened_dialog();

        for _ in 0..10 {
            dialog.move_down();
        }

        assert_eq!(dialog.state.selected(), Some(2));
        assert_eq!(dialog.selected_session_id().as_deref(), Some("s1"));
    }

    #[test]
    fn move_up_moves_back_and_clamps_at_zero() {
        let mut dialog = opened_dialog();
        dialog.move_down();
        dialog.move_down();
        assert_eq!(dialog.state.selected(), Some(2));

        dialog.move_up();
        assert_eq!(dialog.selected_session_id().as_deref(), Some("s2"));
        dialog.move_up();
        assert_eq!(dialog.selected_session_id().as_deref(), Some("s3"));
        dialog.move_up();
        assert_eq!(dialog.state.selected(), Some(0));
    }

    #[test]
    fn move_up_and_move_down_are_noop_without_selection() {
        let mut dialog = SessionListDialog::new();
        dialog.set_sessions(Vec::new());
        assert_eq!(dialog.state.selected(), None);

        dialog.move_up();
        dialog.move_down();

        assert_eq!(dialog.state.selected(), None);
        assert!(dialog.selected_session_id().is_none());
    }

    #[test]
    fn move_down_before_data_is_set_stays_clamped() {
        let mut dialog = SessionListDialog::new();
        assert_eq!(dialog.state.selected(), Some(0));
        assert!(dialog.selected_session_id().is_none());

        dialog.move_down();
        dialog.move_down();

        // new() 直後は選択が Some(0) だが filtered が空のため移動しない（直近ラウンドで失敗したパターン）。
        assert_eq!(dialog.state.selected(), Some(0));
        assert!(dialog.selected_session_id().is_none());
    }

    #[test]
    fn move_down_with_empty_items_stays_none() {
        let mut dialog = SessionListDialog::new();
        dialog.open(None);
        assert_eq!(dialog.state.selected(), None);

        dialog.move_down();

        assert_eq!(dialog.state.selected(), None);
        assert!(dialog.selected_session_id().is_none());
    }

    // --- 削除状態遷移 ---

    #[test]
    fn trigger_delete_selected_returns_armed_first() {
        let mut dialog = opened_dialog();

        let state = dialog.trigger_delete_selected();

        assert!(matches!(state, Some(DeleteState::Armed(id)) if id == "s3"));
    }

    #[test]
    fn trigger_delete_selected_returns_confirmed_on_second_call() {
        let mut dialog = opened_dialog();
        assert!(matches!(
            dialog.trigger_delete_selected(),
            Some(DeleteState::Armed(id)) if id == "s3"
        ));

        let state = dialog.trigger_delete_selected();
        assert!(matches!(state, Some(DeleteState::Confirmed(id)) if id == "s3"));

        // confirmed で pending が消えるため 3 回目は Armed に戻る。
        assert!(matches!(
            dialog.trigger_delete_selected(),
            Some(DeleteState::Armed(id)) if id == "s3"
        ));
    }

    #[test]
    fn trigger_delete_selected_uses_currently_selected_session() {
        let mut dialog = opened_dialog();
        dialog.move_down();
        assert_eq!(dialog.selected_session_id().as_deref(), Some("s2"));

        let state = dialog.trigger_delete_selected();

        assert!(matches!(state, Some(DeleteState::Armed(id)) if id == "s2"));
    }

    #[test]
    fn trigger_delete_selected_is_none_without_selection() {
        let mut dialog = SessionListDialog::new();
        assert!(dialog.trigger_delete_selected().is_none());

        dialog.set_sessions(Vec::new());
        assert!(dialog.trigger_delete_selected().is_none());
    }

    #[test]
    fn selection_change_resets_pending_delete() {
        let mut dialog = opened_dialog();
        assert!(matches!(
            dialog.trigger_delete_selected(),
            Some(DeleteState::Armed(id)) if id == "s3"
        ));

        dialog.move_down();

        assert!(matches!(
            dialog.trigger_delete_selected(),
            Some(DeleteState::Armed(id)) if id == "s2"
        ));
    }

    #[test]
    fn pending_delete_survives_move_down_at_last_item() {
        let mut dialog = opened_dialog();
        dialog.move_down();
        dialog.move_down();
        assert_eq!(dialog.selected_session_id().as_deref(), Some("s1"));
        assert!(matches!(
            dialog.trigger_delete_selected(),
            Some(DeleteState::Armed(id)) if id == "s1"
        ));

        // 末尾では選択が動かないため pending delete は維持される。
        dialog.move_down();

        assert!(matches!(
            dialog.trigger_delete_selected(),
            Some(DeleteState::Confirmed(id)) if id == "s1"
        ));
    }

    #[test]
    fn query_input_resets_pending_delete() {
        let mut dialog = opened_dialog();
        assert!(matches!(
            dialog.trigger_delete_selected(),
            Some(DeleteState::Armed(id)) if id == "s3"
        ));

        dialog.handle_input('b');
        assert_eq!(dialog.query(), "b");
        assert_eq!(dialog.selected_session_id().as_deref(), Some("s2"));

        assert!(matches!(
            dialog.trigger_delete_selected(),
            Some(DeleteState::Armed(id)) if id == "s2"
        ));
    }

    #[test]
    fn backspace_resets_pending_delete_and_filter() {
        let mut dialog = opened_dialog();
        dialog.handle_input('b');
        assert_eq!(dialog.selected_session_id().as_deref(), Some("s2"));
        assert!(matches!(
            dialog.trigger_delete_selected(),
            Some(DeleteState::Armed(id)) if id == "s2"
        ));

        dialog.handle_backspace();

        assert!(dialog.query().is_empty());
        assert_eq!(dialog.selected_session_id().as_deref(), Some("s3"));
        assert!(matches!(
            dialog.trigger_delete_selected(),
            Some(DeleteState::Armed(id)) if id == "s3"
        ));
    }

    // --- render ---

    #[test]
    fn render_when_closed_draws_nothing() {
        let mut dialog = opened_dialog();
        dialog.close();
        let theme = Theme::default();
        assert!(!dialog.is_open());

        let text = render_to_string(100, 30, |frame| {
            dialog.render(frame, Rect::new(0, 0, 100, 30), &theme);
        });

        assert!(
            text.trim().is_empty(),
            "closed session dialog must not draw any content"
        );
    }

    #[test]
    fn render_when_open_shows_sessions_and_controls() {
        let dialog = opened_dialog();
        let theme = Theme::default();

        let text = render_to_string(100, 30, |frame| {
            dialog.render(frame, Rect::new(0, 0, 100, 30), &theme);
        });

        assert!(text.contains("Sessions"), "dialog title");
        assert!(text.contains("Alpha"), "first session title");
        assert!(text.contains("Beta"), "second session title");
        assert!(text.contains("Gamma"), "third session title");
        assert!(text.contains("ctrl+d"), "delete hint");
        assert!(text.contains("open session"), "open hint");
    }

    #[test]
    fn render_shows_pending_delete_hint() {
        let mut dialog = opened_dialog();
        assert!(matches!(
            dialog.trigger_delete_selected(),
            Some(DeleteState::Armed(id)) if id == "s3"
        ));
        let theme = Theme::default();

        let text = render_to_string(100, 30, |frame| {
            dialog.render(frame, Rect::new(0, 0, 100, 30), &theme);
        });

        assert!(text.contains("Press ctrl+d again to confirm delete"));
        assert!(
            !text.contains("Gamma"),
            "pending row hides the original title"
        );
    }

    #[test]
    fn render_marks_current_session() {
        let mut dialog = SessionListDialog::new();
        dialog.set_sessions(sample_sessions());
        dialog.open(Some("s2"));
        let theme = Theme::default();

        let text = render_to_string(100, 30, |frame| {
            dialog.render(frame, Rect::new(0, 0, 100, 30), &theme);
        });

        assert!(text.contains("●"), "current session marker");
    }
}

use ratatui::{
    layout::Rect,
    style::Style,
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, ListState},
    Frame,
};

use crate::command::{CommandAction, CommandRegistry};
use crate::theme::Theme;

use super::dialogs::DialogClickMap;

pub struct SlashCommandPopup {
    pub registry: CommandRegistry,
    pub query: String,
    pub filtered: Vec<String>,
    pub state: ListState,
    pub open: bool,
    pub selected_action: Option<CommandAction>,
    /// 描画ジオメトリを記録し、クリック位置を項目インデックスへ変換する。
    click_map: DialogClickMap,
}

impl SlashCommandPopup {
    pub fn new() -> Self {
        Self {
            registry: CommandRegistry::new(),
            query: String::new(),
            filtered: Vec::new(),
            state: ListState::default(),
            open: false,
            selected_action: None,
            click_map: DialogClickMap::default(),
        }
    }

    pub fn open(&mut self) {
        self.query = String::new();
        self.refresh_filter();
        self.state.select(Some(0));
        self.open = true;
        self.selected_action = None;
    }

    pub fn close(&mut self) {
        self.open = false;
        self.query.clear();
        self.filtered.clear();
        // Drop the previous frame geometry so a stale map cannot swallow a click
        // before the next render records a fresh one.
        self.click_map.clear();
    }

    pub fn is_open(&self) -> bool {
        self.open
    }

    pub fn take_action(&mut self) -> Option<CommandAction> {
        self.selected_action.take()
    }

    pub fn query(&self) -> &str {
        &self.query
    }

    fn refresh_filter(&mut self) {
        if self.query.is_empty() {
            self.filtered = self
                .registry
                .suggested_commands()
                .iter()
                .map(|cmd| cmd.name.clone())
                .collect();
        } else {
            self.filtered = self
                .registry
                .search(&self.query)
                .iter()
                .map(|cmd| cmd.name.clone())
                .collect();
        }
        self.state.select(Some(0));
    }

    pub fn handle_input(&mut self, c: char) {
        self.query.push(c);
        self.refresh_filter();
    }

    pub fn handle_backspace(&mut self) -> bool {
        if self.query.pop().is_some() {
            self.refresh_filter();
            true
        } else {
            false
        }
    }

    pub fn move_up(&mut self) {
        if let Some(selected) = self.state.selected() {
            let new = selected.saturating_sub(1);
            self.state.select(Some(new));
        }
    }

    pub fn move_down(&mut self) {
        if let Some(selected) = self.state.selected() {
            let new = (selected + 1).min(self.filtered.len().saturating_sub(1));
            self.state.select(Some(new));
        }
    }

    pub fn select_current(&mut self) {
        if let Some(idx) = self.state.selected() {
            if let Some(name) = self.filtered.get(idx) {
                if let Some(cmd) = self.registry.get(name) {
                    self.selected_action = Some(cmd.action.clone());
                    self.close();
                }
            }
        }
    }

    /// ポインタ下の項目を選択する。ダイアログ枠内のクリックを消費し、
    /// 背景クリックは `false` を返して呼び出し側の close 処理へ委譲する。
    pub fn handle_click(&mut self, col: u16, row: u16) -> bool {
        if let Some(index) = self.click_map.item_at(col, row) {
            self.state.select(Some(index));
        }
        self.click_map.contains(col, row)
    }

    pub fn render(&self, frame: &mut Frame, area: Rect, theme: &Theme) {
        if !self.open || self.filtered.is_empty() {
            // 非表示／候補なしのときは前フレームのジオメトリを破棄し、
            // 見えないポップアップが背景クリックを飲み込まないようにする。
            self.click_map.clear();
            return;
        }

        let width = 50.min(area.width.saturating_sub(4));
        let height = (10.min(self.filtered.len()) as u16).saturating_add(2);

        let x = area.x + (area.width - width) / 2;
        let y = area.y.saturating_sub(height + 1);

        let popup_area = Rect::new(x.max(1), y.max(1), width, height);

        let query_line = Line::from(vec![
            Span::raw("/"),
            Span::styled(&self.query, Style::default().fg(theme.primary)),
        ]);

        let items: Vec<ListItem> = self
            .filtered
            .iter()
            .enumerate()
            .map(|(idx, name)| {
                let cmd = self.registry.get(name);
                let title = cmd.map(|c| c.title.as_str()).unwrap_or(name);
                let _desc = cmd.map(|c| c.description.as_str()).unwrap_or("");
                let keybind = cmd.and_then(|c| c.keybind.clone());

                let is_selected = self.state.selected() == Some(idx);
                let style = if is_selected {
                    Style::default()
                        .fg(theme.primary)
                        .bg(theme.background_element)
                } else {
                    Style::default().fg(theme.text)
                };

                let content = if let Some(kb) = keybind {
                    Line::from(vec![
                        Span::styled(title, style),
                        Span::styled(format!("  ({})", kb), Style::default().fg(theme.text_muted)),
                    ])
                } else {
                    Line::from(Span::styled(title, style))
                };

                ListItem::new(content)
            })
            .collect();

        let block = Block::default()
            .title(query_line)
            .borders(Borders::ALL)
            .border_style(Style::default().fg(theme.border));
        let list_area = block.inner(popup_area);
        let list = List::new(items)
            .block(block)
            .highlight_style(Style::default().fg(theme.primary));

        self.click_map.set_area(popup_area);
        // The popup renders its list without stateful scrolling, so the visible
        // offset is always zero.
        self.click_map.set_list(list_area, 0, self.filtered.len());

        frame.render_widget(list, popup_area);
    }
}

impl Default for SlashCommandPopup {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::{backend::TestBackend, Terminal};

    /// Stable (sorted) view of the built-in suggested command names.
    fn suggested_names() -> Vec<String> {
        let mut names: Vec<String> = CommandRegistry::new()
            .suggested_commands()
            .iter()
            .map(|cmd| cmd.name.clone())
            .collect();
        names.sort();
        names
    }

    fn sorted(names: &[String]) -> Vec<String> {
        let mut names = names.to_vec();
        names.sort();
        names
    }

    // --- new / Default ---

    #[test]
    fn new_starts_closed_with_empty_state() {
        let popup = SlashCommandPopup::new();

        assert!(!popup.is_open());
        assert_eq!(popup.query(), "");
        assert!(popup.filtered.is_empty());
        assert_eq!(popup.state.selected(), None);
        assert!(popup.selected_action.is_none());
    }

    #[test]
    fn default_matches_new_initial_state() {
        let new = SlashCommandPopup::new();
        let default = SlashCommandPopup::default();

        assert!(!default.is_open());
        assert_eq!(default.query(), new.query());
        assert_eq!(default.filtered, new.filtered);
        assert_eq!(default.state.selected(), new.state.selected());
        assert!(default.selected_action.is_none());
    }

    // --- open / is_open ---

    #[test]
    fn open_sets_open_state_and_lists_suggested_commands() {
        let mut popup = SlashCommandPopup::new();
        popup.open();

        assert!(popup.is_open());
        assert_eq!(popup.query(), "");
        assert_eq!(sorted(&popup.filtered), suggested_names());
        assert_eq!(popup.state.selected(), Some(0));
        assert!(popup.selected_action.is_none());
    }

    #[test]
    fn open_resets_previous_query_filter_and_action() {
        let mut popup = SlashCommandPopup::new();
        popup.open();

        for c in "new".chars() {
            popup.handle_input(c);
        }
        popup.selected_action = Some(CommandAction::Exit);
        assert_ne!(popup.query(), "");

        popup.open();

        assert!(popup.is_open());
        assert_eq!(popup.query(), "");
        assert_eq!(sorted(&popup.filtered), suggested_names());
        assert_eq!(popup.state.selected(), Some(0));
        assert!(popup.selected_action.is_none());
    }

    // --- close ---

    #[test]
    fn close_clears_open_query_and_filter() {
        let mut popup = SlashCommandPopup::new();
        popup.open();
        popup.handle_input('n');

        popup.close();

        assert!(!popup.is_open());
        assert_eq!(popup.query(), "");
        assert!(popup.filtered.is_empty());
    }

    #[test]
    fn close_is_idempotent_from_new() {
        let mut popup = SlashCommandPopup::new();

        popup.close();

        assert!(!popup.is_open());
        assert_eq!(popup.query(), "");
        assert!(popup.filtered.is_empty());
    }

    // --- query / filtering ---

    #[test]
    fn handle_input_updates_query_and_filters_by_name() {
        let mut popup = SlashCommandPopup::new();
        popup.open();

        for c in "new".chars() {
            popup.handle_input(c);
        }

        assert_eq!(popup.query(), "new");
        assert!(!popup.filtered.is_empty());
        assert!(popup.filtered.contains(&"/new".to_string()));
        assert!(!popup.filtered.contains(&"/exit".to_string()));
        assert_eq!(popup.state.selected(), Some(0));
    }

    #[test]
    fn handle_input_with_no_match_empties_filter() {
        let mut popup = SlashCommandPopup::new();
        popup.open();
        assert!(!popup.filtered.is_empty());

        for c in "qqqq".chars() {
            popup.handle_input(c);
        }

        assert_eq!(popup.query(), "qqqq");
        assert!(popup.filtered.is_empty());
    }

    #[test]
    fn handle_backspace_restores_previous_filter() {
        let mut popup = SlashCommandPopup::new();
        popup.open();
        let suggested = popup.filtered.clone();

        popup.handle_input('z');
        assert_eq!(popup.query(), "z");

        assert!(popup.handle_backspace());
        assert_eq!(popup.query(), "");
        assert_eq!(sorted(&popup.filtered), sorted(&suggested));
    }

    #[test]
    fn handle_backspace_on_empty_query_returns_false() {
        let mut popup = SlashCommandPopup::new();
        popup.open();
        let suggested = popup.filtered.clone();

        assert!(!popup.handle_backspace());
        assert_eq!(popup.query(), "");
        assert_eq!(sorted(&popup.filtered), sorted(&suggested));
    }

    // --- take_action / selection ---

    #[test]
    fn take_action_returns_action_once_then_clears() {
        let mut popup = SlashCommandPopup::new();
        assert!(popup.take_action().is_none());

        popup.selected_action = Some(CommandAction::ShowHelp);

        let action = popup.take_action().expect("selected action");
        assert!(matches!(action, CommandAction::ShowHelp));
        assert!(popup.selected_action.is_none());
        assert!(popup.take_action().is_none());
    }

    #[test]
    fn select_current_sets_action_and_closes_popup() {
        let mut popup = SlashCommandPopup::new();
        popup.open();

        let name = popup.filtered[0].clone();
        let expected = popup
            .registry
            .get(&name)
            .expect("selected command exists")
            .action
            .clone();

        popup.select_current();

        assert!(!popup.is_open());
        let action = popup
            .take_action()
            .expect("action captured by select_current");
        assert_eq!(format!("{:?}", action), format!("{:?}", expected));
    }

    #[test]
    fn navigation_clamps_to_filter_bounds() {
        let mut popup = SlashCommandPopup::new();
        popup.open();
        let last = popup.filtered.len().saturating_sub(1);
        assert!(
            last >= 1,
            "built-in registry should expose several suggestions"
        );

        popup.move_down();
        assert_eq!(popup.state.selected(), Some(1));
        for _ in 0..last + 2 {
            popup.move_down();
        }
        assert_eq!(popup.state.selected(), Some(last));

        popup.move_up();
        assert_eq!(popup.state.selected(), Some(last - 1));
        for _ in 0..last + 2 {
            popup.move_up();
        }
        assert_eq!(popup.state.selected(), Some(0));
    }

    // --- mouse hit testing ---

    /// Reproduce the frame rectangle computed by `SlashCommandPopup::render`.
    fn rendered_frame(area: Rect, filtered_len: usize) -> Rect {
        let width = 50.min(area.width.saturating_sub(4));
        let height = (10.min(filtered_len) as u16).saturating_add(2);
        let x = area.x + (area.width - width) / 2;
        let y = area.y.saturating_sub(height + 1);
        Rect::new(x.max(1), y.max(1), width, height)
    }

    fn render_open_popup(popup: &SlashCommandPopup, area: Rect) {
        let theme = Theme::default();
        let backend = TestBackend::new(area.width, area.height);
        let mut terminal = Terminal::new(backend).expect("TestBackend should initialize");
        terminal
            .draw(|frame| popup.render(frame, area, &theme))
            .expect("draw should succeed");
    }

    #[test]
    fn click_on_rendered_item_selects_it_and_consumes_click() {
        let mut popup = SlashCommandPopup::new();
        popup.open();
        let area = Rect::new(0, 0, 80, 24);
        render_open_popup(&popup, area);

        let frame = rendered_frame(area, popup.filtered.len());
        let item_col = frame.x + 1;
        let item_row = frame.y + 1 + 2;
        assert!(popup.handle_click(item_col, item_row));
        assert_eq!(popup.state.selected(), Some(2));
        assert!(popup.is_open());
        assert!(popup.selected_action.is_none());
    }

    #[test]
    fn click_on_popup_border_is_consumed_without_changing_selection() {
        let mut popup = SlashCommandPopup::new();
        popup.open();
        let area = Rect::new(0, 0, 80, 24);
        render_open_popup(&popup, area);

        let frame = rendered_frame(area, popup.filtered.len());
        assert!(popup.handle_click(frame.x, frame.y));
        assert_eq!(popup.state.selected(), Some(0));
    }

    #[test]
    fn click_outside_rendered_popup_is_not_consumed() {
        let mut popup = SlashCommandPopup::new();
        popup.open();
        let area = Rect::new(0, 0, 80, 24);
        render_open_popup(&popup, area);

        let frame = rendered_frame(area, popup.filtered.len());
        assert!(!popup.handle_click(frame.x.saturating_sub(1), frame.y + 2));
        assert_eq!(popup.state.selected(), Some(0));
        assert!(popup.is_open());
    }

    #[test]
    fn close_discards_recorded_click_geometry() {
        let mut popup = SlashCommandPopup::new();
        popup.open();
        let area = Rect::new(0, 0, 80, 24);
        render_open_popup(&popup, area);
        let frame = rendered_frame(area, popup.filtered.len());

        popup.close();

        assert!(!popup.handle_click(frame.x + 1, frame.y + 1));
    }
}

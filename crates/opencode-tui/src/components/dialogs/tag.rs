use ratatui::{
    layout::Rect,
    style::Style,
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, ListState},
    Frame,
};

use crate::theme::Theme;

#[derive(Clone, Debug)]
pub struct Tag {
    pub id: String,
    pub name: String,
    pub color: Option<String>,
}

pub struct TagDialog {
    pub tags: Vec<Tag>,
    pub selected_tags: Vec<String>,
    pub state: ListState,
    pub open: bool,
}

impl TagDialog {
    pub fn new() -> Self {
        Self {
            tags: Vec::new(),
            selected_tags: Vec::new(),
            state: ListState::default(),
            open: false,
        }
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

    pub fn set_tags(&mut self, tags: Vec<Tag>) {
        self.tags = tags;
    }

    pub fn toggle_selection(&mut self) {
        if let Some(selected) = self.state.selected() {
            if let Some(tag) = self.tags.get(selected) {
                if self.selected_tags.contains(&tag.id) {
                    self.selected_tags.retain(|id| id != &tag.id);
                } else {
                    self.selected_tags.push(tag.id.clone());
                }
            }
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
            let new = (selected + 1).min(self.tags.len().saturating_sub(1));
            self.state.select(Some(new));
        }
    }

    pub fn selected_tags(&self) -> &[String] {
        &self.selected_tags
    }

    pub fn render(&self, frame: &mut Frame, area: Rect, theme: &Theme) {
        if !self.open || self.tags.is_empty() {
            return;
        }

        let height = (self.tags.len() as u16 + 2).min(15);
        let width = 40u16;
        let popup_area = super::centered_rect(width, height, area);
        let block = Block::default()
            .title(" Select Tags ")
            .borders(Borders::ALL)
            .border_style(Style::default().fg(theme.border))
            .style(Style::default().bg(theme.background_panel));
        let content_area = super::dialog_inner(block.inner(popup_area));

        let items: Vec<ListItem> = self
            .tags
            .iter()
            .map(|tag| {
                let is_checked = self.selected_tags.contains(&tag.id);
                let check_mark = if is_checked { "☑" } else { "☐" };
                ListItem::new(Line::from(vec![
                    Span::styled(check_mark, Style::default().fg(theme.primary)),
                    Span::styled(format!(" {}", tag.name), Style::default().fg(theme.text)),
                ]))
            })
            .collect();

        frame.render_widget(block, popup_area);

        let list = List::new(items).highlight_style(Style::default().fg(theme.primary));

        frame.render_widget(list, content_area);
    }
}

impl Default for TagDialog {
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

    /// テスト用 Tag を構築するヘルパー。
    fn tag(id: &str, name: &str, color: Option<&str>) -> Tag {
        Tag {
            id: id.to_string(),
            name: name.to_string(),
            color: color.map(str::to_string),
        }
    }

    /// テスト用の 3 件のタグを返すヘルパー。
    fn sample_tags() -> Vec<Tag> {
        vec![
            tag("t1", "Rust", Some("#ff0000")),
            tag("t2", "Python", None),
            tag("t3", "TypeScript", Some("#3178c6")),
        ]
    }

    /// 選択中タグ ID を &str の Vec に変換するヘルパー。
    fn selected_ids(dialog: &TagDialog) -> Vec<&str> {
        dialog.selected_tags().iter().map(String::as_str).collect()
    }

    // --- Tag データ保持 ---

    #[test]
    fn tag_stores_id_name_and_color() {
        let t = tag("t1", "Rust", Some("#ff0000"));

        assert_eq!(t.id, "t1");
        assert_eq!(t.name, "Rust");
        assert_eq!(t.color.as_deref(), Some("#ff0000"));
    }

    #[test]
    fn tag_allows_missing_color() {
        let t = tag("t2", "Python", None);

        assert_eq!(t.id, "t2");
        assert_eq!(t.name, "Python");
        assert!(t.color.is_none());
    }

    #[test]
    fn tag_clone_preserves_all_fields() {
        let original = tag("t3", "TypeScript", Some("#3178c6"));

        let cloned = original.clone();

        assert_eq!(cloned.id, original.id);
        assert_eq!(cloned.name, original.name);
        assert_eq!(cloned.color, original.color);
    }

    // --- new / Default ---

    #[test]
    fn new_starts_empty_and_closed() {
        let dialog = TagDialog::new();

        assert!(dialog.tags.is_empty());
        assert!(dialog.selected_tags().is_empty());
        assert_eq!(dialog.state.selected(), None);
        assert!(!dialog.is_open());
    }

    #[test]
    fn default_starts_empty_and_closed() {
        let dialog = TagDialog::default();

        assert!(dialog.tags.is_empty());
        assert!(dialog.selected_tags().is_empty());
        assert_eq!(dialog.state.selected(), None);
        assert!(!dialog.is_open());
    }

    // --- open / close / is_open ---

    #[test]
    fn open_sets_open_state_and_selects_first_item() {
        let mut dialog = TagDialog::new();
        dialog.set_tags(sample_tags());
        assert!(!dialog.is_open());

        dialog.open();

        assert!(dialog.is_open());
        assert_eq!(dialog.state.selected(), Some(0));
        assert!(dialog.selected_tags().is_empty());
    }

    #[test]
    fn open_selects_first_item_even_without_tags() {
        let mut dialog = TagDialog::new();

        dialog.open();

        assert!(dialog.is_open());
        assert_eq!(dialog.state.selected(), Some(0));
    }

    #[test]
    fn close_clears_open_state_and_preserves_data() {
        let mut dialog = TagDialog::new();
        dialog.set_tags(sample_tags());
        dialog.open();
        dialog.toggle_selection();
        assert!(dialog.is_open());

        dialog.close();

        assert!(!dialog.is_open());
        assert_eq!(dialog.tags.len(), 3);
        assert_eq!(selected_ids(&dialog), vec!["t1"]);
        assert_eq!(dialog.state.selected(), Some(0));
    }

    #[test]
    fn open_close_cycles_toggle_state() {
        let mut dialog = TagDialog::new();

        for _ in 0..3 {
            dialog.open();
            assert!(dialog.is_open());
            dialog.close();
            assert!(!dialog.is_open());
        }
    }

    #[test]
    fn close_is_idempotent_from_new() {
        let mut dialog = TagDialog::new();

        dialog.close();
        dialog.close();

        assert!(!dialog.is_open());
    }

    #[test]
    fn open_is_idempotent() {
        let mut dialog = TagDialog::new();

        dialog.open();
        dialog.open();

        assert!(dialog.is_open());
        assert_eq!(dialog.state.selected(), Some(0));
    }

    // --- set_tags ---

    #[test]
    fn set_tags_replaces_existing_tags() {
        let mut dialog = TagDialog::new();
        dialog.set_tags(sample_tags());
        assert_eq!(dialog.tags.len(), 3);

        dialog.set_tags(vec![tag("x1", "Only", None)]);

        assert_eq!(dialog.tags.len(), 1);
        assert_eq!(dialog.tags[0].id, "x1");
        assert_eq!(dialog.tags[0].name, "Only");
        assert!(dialog.tags[0].color.is_none());
    }

    #[test]
    fn set_tags_does_not_change_open_or_selection_state() {
        let mut dialog = TagDialog::new();
        dialog.set_tags(sample_tags());
        dialog.open();
        dialog.move_down();
        assert_eq!(dialog.state.selected(), Some(1));

        // 別の 3 件へ差し替えても open / 選択状態は変わらない。
        dialog.set_tags(vec![
            tag("u1", "Go", None),
            tag("u2", "Zig", None),
            tag("u3", "Nim", None),
        ]);

        assert!(dialog.is_open());
        assert_eq!(dialog.state.selected(), Some(1));
    }

    #[test]
    fn set_tags_can_clear_tags() {
        let mut dialog = TagDialog::new();
        dialog.set_tags(sample_tags());
        assert_eq!(dialog.tags.len(), 3);

        dialog.set_tags(Vec::new());

        assert!(dialog.tags.is_empty());
    }

    // --- 選択状態遷移 (move_up / move_down / toggle_selection) ---

    #[test]
    fn move_down_advances_selection() {
        let mut dialog = TagDialog::new();
        dialog.set_tags(sample_tags());
        dialog.open();
        assert_eq!(dialog.state.selected(), Some(0));

        dialog.move_down();

        assert_eq!(dialog.state.selected(), Some(1));
    }

    #[test]
    fn move_down_clamps_at_last_item() {
        let mut dialog = TagDialog::new();
        dialog.set_tags(sample_tags());
        dialog.open();

        for _ in 0..10 {
            dialog.move_down();
        }

        assert_eq!(dialog.state.selected(), Some(2));
    }

    #[test]
    fn move_up_moves_back_and_clamps_at_zero() {
        let mut dialog = TagDialog::new();
        dialog.set_tags(sample_tags());
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
        let mut dialog = TagDialog::new();
        dialog.set_tags(sample_tags());
        assert_eq!(dialog.state.selected(), None);

        dialog.move_up();
        dialog.move_down();

        assert_eq!(dialog.state.selected(), None);
    }

    #[test]
    fn toggle_selection_adds_current_tag_id() {
        let mut dialog = TagDialog::new();
        dialog.set_tags(sample_tags());
        dialog.open();
        assert!(dialog.selected_tags().is_empty());

        dialog.toggle_selection();

        assert_eq!(selected_ids(&dialog), vec!["t1"]);
    }

    #[test]
    fn toggle_selection_removes_current_tag_id_on_second_call() {
        let mut dialog = TagDialog::new();
        dialog.set_tags(sample_tags());
        dialog.open();
        dialog.toggle_selection();
        assert_eq!(selected_ids(&dialog), vec!["t1"]);

        dialog.toggle_selection();

        assert!(dialog.selected_tags().is_empty());
    }

    #[test]
    fn toggle_selection_uses_currently_selected_tag() {
        let mut dialog = TagDialog::new();
        dialog.set_tags(sample_tags());
        dialog.open();
        dialog.move_down();
        assert_eq!(dialog.state.selected(), Some(1));

        dialog.toggle_selection();

        assert_eq!(selected_ids(&dialog), vec!["t2"]);
    }

    #[test]
    fn toggle_selection_accumulates_multiple_tags() {
        let mut dialog = TagDialog::new();
        dialog.set_tags(sample_tags());
        dialog.open();

        dialog.toggle_selection(); // t1
        dialog.move_down();
        dialog.toggle_selection(); // t2
        dialog.move_down();
        dialog.toggle_selection(); // t3

        assert_eq!(selected_ids(&dialog), vec!["t1", "t2", "t3"]);
    }

    #[test]
    fn toggle_selection_is_noop_without_selection() {
        let mut dialog = TagDialog::new();
        dialog.set_tags(sample_tags());
        assert_eq!(dialog.state.selected(), None);

        dialog.toggle_selection();

        assert!(dialog.selected_tags().is_empty());
    }

    #[test]
    fn toggle_selection_is_noop_when_index_out_of_range() {
        let mut dialog = TagDialog::new();
        dialog.open(); // 選択インデックス 0 だがタグは未設定
        assert_eq!(dialog.state.selected(), Some(0));

        dialog.toggle_selection();

        assert!(dialog.selected_tags().is_empty());
    }

    // --- render ---

    #[test]
    fn render_when_closed_draws_nothing() {
        let mut dialog = TagDialog::new();
        dialog.set_tags(sample_tags());
        let theme = Theme::default();
        assert!(!dialog.is_open());

        let text = render_to_string(80, 24, |frame| {
            dialog.render(frame, Rect::new(0, 0, 80, 24), &theme);
        });

        assert!(
            text.trim().is_empty(),
            "closed tag dialog must not draw any content"
        );
    }

    #[test]
    fn render_when_open_without_tags_draws_nothing() {
        let mut dialog = TagDialog::new();
        dialog.open();
        let theme = Theme::default();
        assert!(dialog.is_open());
        assert!(dialog.tags.is_empty());

        let text = render_to_string(80, 24, |frame| {
            dialog.render(frame, Rect::new(0, 0, 80, 24), &theme);
        });

        assert!(
            text.trim().is_empty(),
            "open tag dialog with no tags must not draw any content"
        );
    }

    #[test]
    fn render_when_open_shows_title_and_tag_names() {
        let mut dialog = TagDialog::new();
        dialog.set_tags(sample_tags());
        dialog.open();
        let theme = Theme::default();

        let text = render_to_string(80, 24, |frame| {
            dialog.render(frame, Rect::new(0, 0, 80, 24), &theme);
        });

        assert!(text.contains("Select Tags"), "dialog title");
        assert!(text.contains("Rust"), "first tag name");
        assert!(text.contains("Python"), "second tag name");
        assert!(text.contains("TypeScript"), "third tag name");
    }

    #[test]
    fn render_shows_checked_and_unchecked_marks() {
        let mut dialog = TagDialog::new();
        dialog.set_tags(sample_tags());
        dialog.open();
        dialog.toggle_selection(); // t1 = Rust を選択
        let theme = Theme::default();

        let text = render_to_string(80, 24, |frame| {
            dialog.render(frame, Rect::new(0, 0, 80, 24), &theme);
        });

        assert!(text.contains("☑"), "checked mark for selected tag");
        assert!(text.contains("☐"), "unchecked mark for unselected tag");
    }
}

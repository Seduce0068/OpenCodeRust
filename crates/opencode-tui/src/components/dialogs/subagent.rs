use ratatui::{
    layout::Rect,
    style::Style,
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph},
    Frame,
};

use crate::theme::Theme;

use super::DialogClickMap;

#[derive(Clone, Debug)]
pub struct SubagentInfo {
    pub id: String,
    pub name: String,
    pub category: String,
    pub messages: Vec<SubagentMessage>,
}

#[derive(Clone, Debug)]
pub struct SubagentMessage {
    pub role: String,
    pub content: String,
}

pub struct SubagentDialog {
    pub subagent: Option<SubagentInfo>,
    pub open: bool,
    pub scroll_offset: u16,
    click_map: DialogClickMap,
}

impl SubagentDialog {
    pub fn new() -> Self {
        Self {
            subagent: None,
            open: false,
            scroll_offset: 0,
            click_map: DialogClickMap::default(),
        }
    }

    pub fn open(&mut self, subagent: SubagentInfo) {
        self.subagent = Some(subagent);
        self.open = true;
        self.scroll_offset = 0;
    }

    pub fn close(&mut self) {
        self.open = false;
        self.subagent = None;
    }

    pub fn is_open(&self) -> bool {
        self.open
    }

    /// Return `true` when the click landed on the dialog frame, so the app
    /// keeps the dialog open.
    pub fn handle_click(&self, col: u16, row: u16) -> bool {
        self.click_map.contains(col, row)
    }

    pub fn scroll_up(&mut self) {
        if self.scroll_offset > 0 {
            self.scroll_offset -= 1;
        }
    }

    pub fn scroll_down(&mut self, max: u16) {
        if self.scroll_offset < max {
            self.scroll_offset += 1;
        }
    }

    pub fn render(&self, frame: &mut Frame, area: Rect, theme: &Theme) {
        if !self.open {
            return;
        }

        let subagent = match &self.subagent {
            Some(s) => s,
            None => return,
        };

        let height = area.height.saturating_sub(4).min(20);
        let width = area.width.saturating_sub(4).min(80);
        let popup_area = super::centered_rect(width, height, area);
        self.click_map.set_area(popup_area);

        let title = format!(" Subagent: {} [{}] ", subagent.name, subagent.category);

        let mut lines = Vec::new();
        lines.push(Line::from(Span::styled(
            &title,
            Style::default().fg(theme.primary).bold(),
        )));
        lines.push(Line::from(""));

        for msg in &subagent.messages {
            let role_style = if msg.role == "user" {
                Style::default().fg(theme.primary)
            } else {
                Style::default().fg(theme.success)
            };
            lines.push(Line::from(vec![
                Span::styled(&msg.role, role_style.bold()),
                Span::raw(":"),
            ]));
            lines.push(Line::from(Span::styled(
                &msg.content,
                Style::default().fg(theme.text),
            )));
            lines.push(Line::from(""));
        }

        let block = Block::default()
            .title(" Subagent ")
            .borders(Borders::ALL)
            .border_style(Style::default().fg(theme.border))
            .style(Style::default().bg(theme.background_panel));
        let content_area = super::dialog_inner(block.inner(popup_area));
        frame.render_widget(block, popup_area);

        let paragraph = Paragraph::new(lines)
            .style(Style::default().bg(theme.background_panel))
            .scroll((self.scroll_offset, 0));

        frame.render_widget(paragraph, content_area);
    }
}

impl Default for SubagentDialog {
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

    fn message(role: &str, content: &str) -> SubagentMessage {
        SubagentMessage {
            role: role.to_string(),
            content: content.to_string(),
        }
    }

    fn info(id: &str, name: &str, category: &str, messages: Vec<SubagentMessage>) -> SubagentInfo {
        SubagentInfo {
            id: id.to_string(),
            name: name.to_string(),
            category: category.to_string(),
            messages,
        }
    }

    fn sample_messages() -> Vec<SubagentMessage> {
        vec![message("user", "hello"), message("assistant", "world")]
    }

    // --- SubagentMessage データ保持 ---

    #[test]
    fn subagent_message_stores_role_and_content() {
        let msg = message("assistant", "done");

        assert_eq!(msg.role, "assistant");
        assert_eq!(msg.content, "done");
    }

    #[test]
    fn subagent_message_clone_preserves_all_fields() {
        let original = message("user", "please review");

        let cloned = original.clone();

        // SubagentMessage は PartialEq 未実装のため matches! / フィールド比較で検証する。
        assert!(matches!(
            &cloned,
            m if m.role == "user" && m.content == "please review"
        ));
        assert_eq!(cloned.role, original.role);
        assert_eq!(cloned.content, original.content);
    }

    // --- SubagentInfo データ保持 ---

    #[test]
    fn subagent_info_stores_id_name_category_and_messages() {
        let data = info("sa-1", "scout", "explore", sample_messages());

        assert_eq!(data.id, "sa-1");
        assert_eq!(data.name, "scout");
        assert_eq!(data.category, "explore");
        assert_eq!(data.messages.len(), 2);
        assert!(matches!(&data.messages[0], m if m.role == "user" && m.content == "hello"));
        assert!(matches!(&data.messages[1], m if m.role == "assistant" && m.content == "world"));
    }

    #[test]
    fn subagent_info_clone_preserves_all_fields() {
        let original = info("sa-2", "reviewer", "expert", sample_messages());

        let cloned = original.clone();

        // SubagentInfo は PartialEq 未実装のためフィールド単位で検証する。
        assert_eq!(cloned.id, original.id);
        assert_eq!(cloned.name, original.name);
        assert_eq!(cloned.category, original.category);
        assert_eq!(cloned.messages.len(), 2);
        assert!(matches!(&cloned.messages[0], m if m.role == "user" && m.content == "hello"));
        assert!(matches!(&cloned.messages[1], m if m.role == "assistant" && m.content == "world"));
    }

    #[test]
    fn subagent_info_with_empty_messages_stores_empty_vec() {
        let data = info("sa-3", "empty", "misc", Vec::new());

        assert_eq!(data.id, "sa-3");
        assert!(data.messages.is_empty());
    }

    // --- new / Default ---

    #[test]
    fn new_starts_with_no_subagent_closed_and_zero_scroll() {
        let dialog = SubagentDialog::new();

        assert!(dialog.subagent.is_none());
        assert!(!dialog.is_open());
        assert_eq!(dialog.scroll_offset, 0);
    }

    #[test]
    fn default_starts_with_no_subagent_closed_and_zero_scroll() {
        let dialog = SubagentDialog::default();

        assert!(dialog.subagent.is_none());
        assert!(!dialog.is_open());
        assert_eq!(dialog.scroll_offset, 0);
    }

    // --- open / close / is_open ---

    #[test]
    fn open_sets_subagent_open_and_resets_scroll() {
        let mut dialog = SubagentDialog::new();
        dialog.scroll_down(10);
        assert_eq!(dialog.scroll_offset, 1);

        dialog.open(info("sa-1", "scout", "explore", sample_messages()));

        assert!(dialog.is_open());
        assert!(matches!(
            dialog.subagent.as_ref(),
            Some(s) if s.id == "sa-1" && s.name == "scout" && s.category == "explore"
        ));
        assert!(dialog
            .subagent
            .as_ref()
            .is_some_and(|s| s.messages.len() == 2));
        assert_eq!(dialog.scroll_offset, 0);
    }

    #[test]
    fn open_replaces_previous_subagent() {
        let mut dialog = SubagentDialog::new();
        dialog.open(info("sa-1", "scout", "explore", sample_messages()));

        dialog.open(info("sa-2", "reviewer", "expert", Vec::new()));

        assert!(dialog.is_open());
        assert!(matches!(
            dialog.subagent.as_ref(),
            Some(s) if s.id == "sa-2" && s.name == "reviewer" && s.category == "expert"
        ));
        assert!(dialog
            .subagent
            .as_ref()
            .is_some_and(|s| s.messages.is_empty()));
    }

    #[test]
    fn close_clears_open_and_subagent_but_keeps_scroll() {
        let mut dialog = SubagentDialog::new();
        dialog.open(info("sa-1", "scout", "explore", sample_messages()));
        dialog.scroll_down(5);
        assert_eq!(dialog.scroll_offset, 1);

        dialog.close();

        assert!(!dialog.is_open());
        assert!(dialog.subagent.is_none());
        // 実装は scroll_offset をリセットしないため値が残る。
        assert_eq!(dialog.scroll_offset, 1);
    }

    #[test]
    fn close_is_idempotent_from_new() {
        let mut dialog = SubagentDialog::new();

        dialog.close();
        dialog.close();

        assert!(!dialog.is_open());
        assert!(dialog.subagent.is_none());
        assert_eq!(dialog.scroll_offset, 0);
    }

    #[test]
    fn open_close_cycles_toggle_state() {
        let mut dialog = SubagentDialog::new();

        for i in 0..3 {
            let id = format!("sa-{i}");
            dialog.open(info(&id, "scout", "explore", Vec::new()));
            assert!(dialog.is_open());
            assert!(matches!(dialog.subagent.as_ref(), Some(s) if s.id == id));

            dialog.close();
            assert!(!dialog.is_open());
            assert!(dialog.subagent.is_none());
        }
    }

    // --- スクロール状態遷移 ---

    #[test]
    fn scroll_down_increments_up_to_max() {
        let mut dialog = SubagentDialog::new();

        dialog.scroll_down(2);
        assert_eq!(dialog.scroll_offset, 1);
        dialog.scroll_down(2);
        assert_eq!(dialog.scroll_offset, 2);
        dialog.scroll_down(2);
        assert_eq!(dialog.scroll_offset, 2);
    }

    #[test]
    fn scroll_up_decrements_and_clamps_at_zero() {
        let mut dialog = SubagentDialog::new();
        dialog.scroll_down(10);
        dialog.scroll_down(10);
        dialog.scroll_down(10);
        assert_eq!(dialog.scroll_offset, 3);

        dialog.scroll_up();
        assert_eq!(dialog.scroll_offset, 2);
        dialog.scroll_up();
        dialog.scroll_up();
        assert_eq!(dialog.scroll_offset, 0);
        dialog.scroll_up();
        assert_eq!(dialog.scroll_offset, 0);
    }

    #[test]
    fn scroll_up_from_new_is_noop() {
        let mut dialog = SubagentDialog::new();

        dialog.scroll_up();

        assert_eq!(dialog.scroll_offset, 0);
    }

    #[test]
    fn scroll_down_with_zero_max_is_noop() {
        let mut dialog = SubagentDialog::new();

        dialog.scroll_down(0);

        assert_eq!(dialog.scroll_offset, 0);
    }

    // --- render ---

    #[test]
    fn render_when_closed_draws_nothing() {
        let dialog = SubagentDialog::new();
        let theme = Theme::default();

        let text = render_to_string(80, 24, |frame| {
            dialog.render(frame, Rect::new(0, 0, 80, 24), &theme);
        });

        assert!(
            text.trim().is_empty(),
            "closed subagent dialog must not draw any content"
        );
    }

    #[test]
    fn render_when_open_but_no_subagent_draws_nothing() {
        let mut dialog = SubagentDialog::new();
        dialog.open = true;
        assert!(dialog.subagent.is_none());
        let theme = Theme::default();

        let text = render_to_string(80, 24, |frame| {
            dialog.render(frame, Rect::new(0, 0, 80, 24), &theme);
        });

        assert!(
            text.trim().is_empty(),
            "open dialog without subagent data must not draw any content"
        );
    }

    #[test]
    fn render_when_open_shows_title_category_roles_and_contents() {
        let mut dialog = SubagentDialog::new();
        dialog.open(info("sa-1", "scout", "explore", sample_messages()));
        let theme = Theme::default();

        let text = render_to_string(80, 24, |frame| {
            dialog.render(frame, Rect::new(0, 0, 80, 24), &theme);
        });

        assert!(text.contains("Subagent"), "dialog title");
        assert!(text.contains("scout"), "subagent name");
        assert!(text.contains("explore"), "subagent category");
        assert!(text.contains("user"), "first message role");
        assert!(text.contains("hello"), "first message content");
        assert!(text.contains("assistant"), "second message role");
        assert!(text.contains("world"), "second message content");
    }

    #[test]
    fn render_shows_multiple_messages_in_order() {
        let mut dialog = SubagentDialog::new();
        dialog.open(info(
            "sa-1",
            "scout",
            "explore",
            vec![message("user", "first"), message("assistant", "second")],
        ));
        let theme = Theme::default();

        let text = render_to_string(80, 24, |frame| {
            dialog.render(frame, Rect::new(0, 0, 80, 24), &theme);
        });

        let first = text.find("first").expect("first message content");
        let second = text.find("second").expect("second message content");
        assert!(first < second, "messages should render in insertion order");
    }

    #[test]
    fn render_with_empty_messages_still_shows_title() {
        let mut dialog = SubagentDialog::new();
        dialog.open(info("sa-3", "empty", "misc", Vec::new()));
        let theme = Theme::default();

        let text = render_to_string(80, 24, |frame| {
            dialog.render(frame, Rect::new(0, 0, 80, 24), &theme);
        });

        assert!(text.contains("Subagent"), "dialog title");
        assert!(text.contains("empty"), "subagent name");
        assert!(text.contains("misc"), "subagent category");
    }

    #[test]
    fn render_with_scroll_offset_changes_visible_content() {
        let mut dialog = SubagentDialog::new();
        dialog.open(info(
            "sa-1",
            "scout",
            "explore",
            vec![
                message("user", "one"),
                message("assistant", "two"),
                message("user", "three"),
            ],
        ));
        let theme = Theme::default();

        let no_scroll = render_to_string(80, 24, |frame| {
            dialog.render(frame, Rect::new(0, 0, 80, 24), &theme);
        });
        assert!(
            no_scroll.contains("Subagent: scout"),
            "title line visible at scroll 0"
        );

        dialog.scroll_offset = 5;
        let scrolled = render_to_string(80, 24, |frame| {
            dialog.render(frame, Rect::new(0, 0, 80, 24), &theme);
        });

        // Paragraph::scroll で先頭行が画面外へ送出される。
        assert!(
            !scrolled.contains("Subagent: scout"),
            "title line scrolled away"
        );
        assert_ne!(no_scroll, scrolled);
    }
}

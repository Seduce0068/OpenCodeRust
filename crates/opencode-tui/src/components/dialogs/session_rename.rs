use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph},
    Frame,
};

use crate::theme::Theme;

use super::DialogClickMap;

pub struct SessionRenameDialog {
    open: bool,
    session_id: Option<String>,
    input: String,
    click_map: DialogClickMap,
}

impl SessionRenameDialog {
    pub fn new() -> Self {
        Self {
            open: false,
            session_id: None,
            input: String::new(),
            click_map: DialogClickMap::default(),
        }
    }

    pub fn open(&mut self, session_id: String, title: String) {
        self.open = true;
        self.session_id = Some(session_id);
        self.input = title;
    }

    pub fn close(&mut self) {
        self.open = false;
        self.session_id = None;
        self.input.clear();
    }

    pub fn is_open(&self) -> bool {
        self.open
    }

    /// Return `true` when the click landed on the dialog frame, so the app
    /// keeps the dialog open.
    pub fn handle_click(&self, col: u16, row: u16) -> bool {
        self.click_map.contains(col, row)
    }

    pub fn handle_input(&mut self, c: char) {
        self.input.push(c);
    }

    pub fn handle_backspace(&mut self) {
        self.input.pop();
    }

    pub fn confirm(&mut self) -> Option<(String, String)> {
        let session_id = self.session_id.clone()?;
        let title = self.input.trim().to_string();
        if title.is_empty() {
            return None;
        }
        self.close();
        Some((session_id, title))
    }

    pub fn render(&self, frame: &mut Frame, area: Rect, theme: &Theme) {
        if !self.open {
            return;
        }

        let dialog_area = centered_rect(70, 8, area);
        self.click_map.set_area(dialog_area);
        frame.render_widget(Clear, dialog_area);

        let block = Block::default()
            .title(Span::styled(
                " Rename Session ",
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
                Constraint::Length(1),
                Constraint::Length(1),
            ])
            .split(inner);

        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled("> ", Style::default().fg(theme.primary)),
                Span::styled(&self.input, Style::default().fg(theme.text)),
                Span::styled("▏", Style::default().fg(theme.primary)),
            ])),
            layout[0],
        );

        frame.render_widget(
            Paragraph::new("Enter save  Esc cancel").style(Style::default().fg(theme.text_muted)),
            layout[2],
        );
    }
}

impl Default for SessionRenameDialog {
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

    // --- new / Default ---

    #[test]
    fn new_starts_closed_with_no_session_and_empty_input() {
        let dialog = SessionRenameDialog::new();

        assert!(!dialog.is_open());
        assert!(dialog.session_id.is_none());
        assert_eq!(dialog.input, "");
    }

    #[test]
    fn default_starts_closed_with_no_session_and_empty_input() {
        let dialog = SessionRenameDialog::default();

        assert!(!dialog.is_open());
        assert!(dialog.session_id.is_none());
        assert_eq!(dialog.input, "");
    }

    // --- open / close / is_open ---

    #[test]
    fn open_sets_state_stores_session_id_and_title() {
        let mut dialog = SessionRenameDialog::new();

        dialog.open("sess-1".to_string(), "Old title".to_string());

        assert!(dialog.is_open());
        assert_eq!(dialog.session_id.as_deref(), Some("sess-1"));
        assert_eq!(dialog.input, "Old title");
    }

    #[test]
    fn open_overwrites_previous_session_and_title() {
        let mut dialog = SessionRenameDialog::new();
        dialog.open("sess-1".to_string(), "First".to_string());

        dialog.open("sess-2".to_string(), "Second".to_string());

        assert!(dialog.is_open());
        assert_eq!(dialog.session_id.as_deref(), Some("sess-2"));
        assert_eq!(dialog.input, "Second");
    }

    #[test]
    fn close_clears_open_state_session_id_and_input() {
        let mut dialog = SessionRenameDialog::new();
        dialog.open("sess-1".to_string(), "Title".to_string());
        dialog.handle_input('!');
        assert_eq!(dialog.input, "Title!");

        dialog.close();

        assert!(!dialog.is_open());
        assert!(dialog.session_id.is_none());
        assert_eq!(dialog.input, "");
    }

    #[test]
    fn close_is_idempotent_from_new() {
        let mut dialog = SessionRenameDialog::new();

        dialog.close();
        dialog.close();

        assert!(!dialog.is_open());
        assert!(dialog.session_id.is_none());
        assert_eq!(dialog.input, "");
    }

    #[test]
    fn open_close_cycles_toggle_state() {
        let mut dialog = SessionRenameDialog::new();

        for _ in 0..3 {
            dialog.open("sess".to_string(), "title".to_string());
            assert!(dialog.is_open());
            dialog.close();
            assert!(!dialog.is_open());
        }
        assert!(dialog.session_id.is_none());
        assert_eq!(dialog.input, "");
    }

    // --- handle_input / handle_backspace ---

    #[test]
    fn handle_input_appends_chars_in_order_and_preserves_state() {
        let mut dialog = SessionRenameDialog::new();
        dialog.open("sess".to_string(), String::new());

        for c in ['a', 'b', 'c'] {
            dialog.handle_input(c);
        }

        assert_eq!(dialog.input, "abc");
        assert!(dialog.is_open());
        assert_eq!(dialog.session_id.as_deref(), Some("sess"));
    }

    #[test]
    fn handle_input_appends_to_existing_title() {
        let mut dialog = SessionRenameDialog::new();
        dialog.open("sess".to_string(), "Base".to_string());

        dialog.handle_input('!');
        dialog.handle_input('?');

        assert_eq!(dialog.input, "Base!?");
    }

    #[test]
    fn handle_input_accepts_whitespace_and_unicode() {
        let mut dialog = SessionRenameDialog::new();
        dialog.open("sess".to_string(), String::new());

        dialog.handle_input(' ');
        dialog.handle_input('日');
        dialog.handle_input('本');
        dialog.handle_input(' ');

        assert_eq!(dialog.input, " 日本 ");
    }

    #[test]
    fn handle_backspace_removes_last_char() {
        let mut dialog = SessionRenameDialog::new();
        dialog.open("sess".to_string(), "abc".to_string());

        dialog.handle_backspace();
        assert_eq!(dialog.input, "ab");

        dialog.handle_backspace();
        assert_eq!(dialog.input, "a");
    }

    #[test]
    fn handle_backspace_on_empty_input_is_noop() {
        let mut dialog = SessionRenameDialog::new();
        dialog.open("sess".to_string(), String::new());

        dialog.handle_backspace();
        dialog.handle_backspace();

        assert_eq!(dialog.input, "");
        assert_eq!(dialog.session_id.as_deref(), Some("sess"));
        assert!(dialog.is_open());
    }

    // --- confirm ---

    #[test]
    fn confirm_returns_session_id_and_trimmed_title_then_closes() {
        let mut dialog = SessionRenameDialog::new();
        dialog.open("sess-42".to_string(), "  New title  ".to_string());

        let result = dialog.confirm();

        assert_eq!(
            result,
            Some(("sess-42".to_string(), "New title".to_string()))
        );
        assert!(!dialog.is_open());
        assert!(dialog.session_id.is_none());
        assert_eq!(dialog.input, "");
    }

    #[test]
    fn confirm_reflects_handle_input_changes() {
        let mut dialog = SessionRenameDialog::new();
        dialog.open("sess".to_string(), String::new());
        for c in "Renamed".chars() {
            dialog.handle_input(c);
        }

        let result = dialog.confirm();

        assert_eq!(result, Some(("sess".to_string(), "Renamed".to_string())));
    }

    #[test]
    fn confirm_returns_none_for_blank_title_and_keeps_state() {
        let mut dialog = SessionRenameDialog::new();
        dialog.open("sess".to_string(), "   ".to_string());

        let result = dialog.confirm();

        assert_eq!(result, None);
        assert!(dialog.is_open(), "blank title must not close the dialog");
        assert_eq!(dialog.session_id.as_deref(), Some("sess"));
        assert_eq!(dialog.input, "   ");
    }

    #[test]
    fn confirm_returns_none_when_never_opened() {
        let mut dialog = SessionRenameDialog::new();

        let result = dialog.confirm();

        assert_eq!(result, None);
        assert!(!dialog.is_open());
    }

    #[test]
    fn confirm_after_close_returns_none() {
        let mut dialog = SessionRenameDialog::new();
        dialog.open("sess".to_string(), "title".to_string());
        dialog.close();

        let result = dialog.confirm();

        assert_eq!(result, None);
        assert!(!dialog.is_open());
    }

    // --- render ---

    #[test]
    fn render_when_closed_draws_nothing() {
        let dialog = SessionRenameDialog::new();
        let theme = Theme::default();

        let text = render_to_string(80, 24, |frame| {
            dialog.render(frame, Rect::new(0, 0, 80, 24), &theme);
        });

        assert!(
            text.trim().is_empty(),
            "closed session rename dialog must not draw any content"
        );
    }

    #[test]
    fn render_when_open_shows_title_input_and_hint() {
        let mut dialog = SessionRenameDialog::new();
        dialog.open("sess".to_string(), "My title".to_string());
        let theme = Theme::default();

        let text = render_to_string(80, 24, |frame| {
            dialog.render(frame, Rect::new(0, 0, 80, 24), &theme);
        });

        assert!(text.contains("Rename Session"), "dialog title");
        assert!(text.contains("My title"), "current input value");
        assert!(text.contains("Enter save"), "save hint");
        assert!(text.contains("Esc cancel"), "cancel hint");
    }

    #[test]
    fn render_reflects_handle_input_and_backspace() {
        let mut dialog = SessionRenameDialog::new();
        dialog.open("sess".to_string(), "Before".to_string());
        for _ in 0..6 {
            dialog.handle_backspace();
        }
        for c in "After".chars() {
            dialog.handle_input(c);
        }
        let theme = Theme::default();

        let text = render_to_string(80, 24, |frame| {
            dialog.render(frame, Rect::new(0, 0, 80, 24), &theme);
        });

        assert!(text.contains("After"));
        assert!(!text.contains("Before"));
    }
}

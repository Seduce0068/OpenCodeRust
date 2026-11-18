use ratatui::{
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph},
    Frame,
};

use crate::theme::Theme;

pub struct ConfirmDialog {
    title: String,
    message: String,
    confirm_text: String,
    cancel_text: String,
    focused: bool,
    open: bool,
}

impl ConfirmDialog {
    pub fn new(title: &str, message: &str) -> Self {
        Self {
            title: title.to_string(),
            message: message.to_string(),
            confirm_text: "Confirm".to_string(),
            cancel_text: "Cancel".to_string(),
            focused: false,
            open: false,
        }
    }

    pub fn open(&mut self) {
        self.open = true;
        self.focused = false;
    }

    pub fn close(&mut self) {
        self.open = false;
    }

    pub fn is_open(&self) -> bool {
        self.open
    }

    pub fn toggle_focus(&mut self) {
        self.focused = !self.focused;
    }

    pub fn is_confirm(&self) -> bool {
        self.focused
    }

    pub fn handle_left(&mut self) {
        self.focused = false;
    }

    pub fn handle_right(&mut self) {
        self.focused = true;
    }

    pub fn render(&self, frame: &mut Frame, area: Rect, theme: &Theme) {
        if !self.open {
            return;
        }

        let dialog_width = 50;
        let dialog_height = 7;
        let dialog_area = centered_rect(dialog_width, dialog_height, area);

        frame.render_widget(Clear, dialog_area);

        let block = Block::default()
            .title(Span::styled(
                format!(" {} ", self.title),
                Style::default()
                    .fg(theme.warning)
                    .add_modifier(Modifier::BOLD),
            ))
            .borders(Borders::ALL)
            .border_style(Style::default().fg(theme.border))
            .style(Style::default().bg(theme.background_panel));

        let inner = super::dialog_inner(block.inner(dialog_area));
        frame.render_widget(block, dialog_area);

        let message_line = Line::from(Span::styled(&self.message, Style::default().fg(theme.text)));
        frame.render_widget(
            Paragraph::new(message_line).centered(),
            Rect {
                x: inner.x,
                y: inner.y + 1,
                width: inner.width,
                height: 2,
            },
        );

        let cancel_style = if self.focused {
            Style::default().fg(theme.text_muted)
        } else {
            Style::default().fg(theme.text).bg(theme.primary)
        };

        let confirm_style = if self.focused {
            Style::default().fg(theme.text).bg(theme.error)
        } else {
            Style::default().fg(theme.text_muted)
        };

        let buttons = Line::from(vec![
            Span::styled(format!(" {} ", self.cancel_text), cancel_style),
            Span::raw("   "),
            Span::styled(format!(" {} ", self.confirm_text), confirm_style),
        ]);

        frame.render_widget(
            Paragraph::new(buttons).centered(),
            Rect {
                x: inner.x,
                y: inner.y + inner.height.saturating_sub(2),
                width: inner.width,
                height: 1,
            },
        );
    }
}

impl Default for ConfirmDialog {
    fn default() -> Self {
        Self::new("Confirm", "Are you sure?")
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
    fn new_stores_title_message_defaults_and_starts_closed() {
        let dialog = ConfirmDialog::new("Delete?", "Are you sure?");

        assert_eq!(dialog.title, "Delete?");
        assert_eq!(dialog.message, "Are you sure?");
        assert_eq!(dialog.confirm_text, "Confirm");
        assert_eq!(dialog.cancel_text, "Cancel");
        assert!(!dialog.is_open());
        assert!(!dialog.is_confirm());
    }

    #[test]
    fn default_matches_new_with_confirm_title() {
        let dialog = ConfirmDialog::default();

        assert_eq!(dialog.title, "Confirm");
        assert_eq!(dialog.message, "Are you sure?");
        assert_eq!(dialog.confirm_text, "Confirm");
        assert_eq!(dialog.cancel_text, "Cancel");
        assert!(!dialog.is_open());
        assert!(!dialog.is_confirm());
    }

    // --- open / close / is_open ---

    #[test]
    fn open_sets_open_state_resets_focus_and_preserves_data() {
        let mut dialog = ConfirmDialog::new("Delete?", "Are you sure?");
        dialog.toggle_focus();
        assert!(dialog.is_confirm());

        dialog.open();

        assert!(dialog.is_open());
        assert!(!dialog.is_confirm(), "open must reset focus to cancel");
        assert_eq!(dialog.title, "Delete?");
        assert_eq!(dialog.message, "Are you sure?");
        assert_eq!(dialog.confirm_text, "Confirm");
        assert_eq!(dialog.cancel_text, "Cancel");
    }

    #[test]
    fn close_clears_open_state_and_preserves_data_and_focus() {
        let mut dialog = ConfirmDialog::new("Delete?", "Are you sure?");
        dialog.open();
        dialog.toggle_focus();
        assert!(dialog.is_confirm());

        dialog.close();

        assert!(!dialog.is_open());
        assert!(dialog.is_confirm(), "close must not change focus");
        assert_eq!(dialog.title, "Delete?");
        assert_eq!(dialog.message, "Are you sure?");
        assert_eq!(dialog.confirm_text, "Confirm");
        assert_eq!(dialog.cancel_text, "Cancel");
    }

    #[test]
    fn open_close_cycles_toggle_state() {
        let mut dialog = ConfirmDialog::new("Title", "Message");

        for _ in 0..3 {
            dialog.open();
            assert!(dialog.is_open());
            dialog.close();
            assert!(!dialog.is_open());
        }
        assert_eq!(dialog.title, "Title");
        assert_eq!(dialog.message, "Message");
    }

    #[test]
    fn close_is_idempotent_from_new() {
        let mut dialog = ConfirmDialog::new("Title", "Message");

        dialog.close();
        dialog.close();

        assert!(!dialog.is_open());
        assert_eq!(dialog.title, "Title");
    }

    #[test]
    fn open_is_idempotent() {
        let mut dialog = ConfirmDialog::new("Title", "Message");

        dialog.open();
        dialog.open();

        assert!(dialog.is_open());
        assert!(!dialog.is_confirm());
    }

    // --- toggle_focus / is_confirm / handle_left / handle_right ---

    #[test]
    fn toggle_focus_alternates_confirm_state() {
        let mut dialog = ConfirmDialog::new("Title", "Message");

        assert!(!dialog.is_confirm());
        dialog.toggle_focus();
        assert!(dialog.is_confirm());
        dialog.toggle_focus();
        assert!(!dialog.is_confirm());
        dialog.toggle_focus();
        assert!(dialog.is_confirm());
    }

    #[test]
    fn toggle_focus_preserves_open_state_and_data() {
        let mut dialog = ConfirmDialog::new("Title", "Message");
        dialog.open();

        dialog.toggle_focus();

        assert!(dialog.is_open());
        assert!(dialog.is_confirm());
        assert_eq!(dialog.title, "Title");
        assert_eq!(dialog.message, "Message");
    }

    #[test]
    fn handle_left_selects_cancel_and_handle_right_selects_confirm() {
        let mut dialog = ConfirmDialog::new("Title", "Message");
        dialog.open();
        dialog.toggle_focus();
        assert!(dialog.is_confirm());

        dialog.handle_left();
        assert!(!dialog.is_confirm(), "left must focus cancel");
        assert!(dialog.is_open(), "focus change must not close dialog");

        dialog.handle_right();
        assert!(dialog.is_confirm(), "right must focus confirm");
        assert!(dialog.is_open());
    }

    #[test]
    fn handle_left_right_are_idempotent() {
        let mut dialog = ConfirmDialog::new("Title", "Message");
        dialog.open();

        dialog.handle_left();
        dialog.handle_left();
        assert!(!dialog.is_confirm());

        dialog.handle_right();
        dialog.handle_right();
        assert!(dialog.is_confirm());
    }

    // --- render ---

    #[test]
    fn render_when_closed_draws_nothing() {
        let dialog = ConfirmDialog::new("Hidden", "Hidden message");
        let theme = Theme::default();

        let text = render_to_string(80, 24, |frame| {
            dialog.render(frame, Rect::new(0, 0, 80, 24), &theme);
        });

        assert!(
            text.trim().is_empty(),
            "closed confirm dialog must not draw any content"
        );
    }

    #[test]
    fn render_when_open_shows_title_message_and_buttons() {
        let mut dialog = ConfirmDialog::new("Delete?", "Remove this item?");
        dialog.open();
        let theme = Theme::default();

        let text = render_to_string(80, 24, |frame| {
            dialog.render(frame, Rect::new(0, 0, 80, 24), &theme);
        });

        assert!(text.contains("Delete?"), "dialog title");
        assert!(text.contains("Remove this item?"), "message body");
        assert!(text.contains("Confirm"), "confirm button label");
        assert!(text.contains("Cancel"), "cancel button label");
    }
}

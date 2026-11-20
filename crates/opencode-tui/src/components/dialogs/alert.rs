use ratatui::{
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph, Wrap},
    Frame,
};

use crate::components::permission::wrapped_row_count;
use crate::theme::Theme;

use super::DialogClickMap;

pub enum AlertType {
    Info,
    Success,
    Warning,
    Error,
}

pub struct AlertDialog {
    title: String,
    message: String,
    alert_type: AlertType,
    open: bool,
    click_map: DialogClickMap,
}

impl AlertDialog {
    pub fn new(title: &str, message: &str, alert_type: AlertType) -> Self {
        Self {
            title: title.to_string(),
            message: message.to_string(),
            alert_type,
            open: false,
            click_map: DialogClickMap::default(),
        }
    }

    pub fn info(message: &str) -> Self {
        Self::new("Info", message, AlertType::Info)
    }

    pub fn success(message: &str) -> Self {
        Self::new("Success", message, AlertType::Success)
    }

    pub fn warning(message: &str) -> Self {
        Self::new("Warning", message, AlertType::Warning)
    }

    pub fn error(message: &str) -> Self {
        Self::new("Error", message, AlertType::Error)
    }

    pub fn open(&mut self) {
        self.open = true;
    }

    pub fn close(&mut self) {
        self.open = false;
    }

    pub fn is_open(&self) -> bool {
        self.open
    }

    pub fn set_message(&mut self, message: &str) {
        self.message = message.to_string();
    }

    pub fn message(&self) -> &str {
        &self.message
    }

    /// Return `true` when the click landed on the dialog frame, so the app
    /// keeps the dialog open.
    pub fn handle_click(&self, col: u16, row: u16) -> bool {
        self.click_map.contains(col, row)
    }

    fn get_color(&self, theme: &Theme) -> ratatui::style::Color {
        match self.alert_type {
            AlertType::Info => theme.info,
            AlertType::Success => theme.success,
            AlertType::Warning => theme.warning,
            AlertType::Error => theme.error,
        }
    }

    fn get_icon(&self) -> &str {
        match self.alert_type {
            AlertType::Info => "ℹ",
            AlertType::Success => "✓",
            AlertType::Warning => "⚠",
            AlertType::Error => "✗",
        }
    }

    pub fn render(&self, frame: &mut Frame, area: Rect, theme: &Theme) {
        if !self.open {
            return;
        }

        let color = self.get_color(theme);
        let icon = self.get_icon();

        let dialog_width = 50.min(area.width.saturating_sub(4)).max(1);
        // 枠(2) と dialog_inner の左右 padding(2) を除いた幅が本文の折り返し幅。
        let content_width = dialog_width.saturating_sub(4).max(1);
        let message_lines: Vec<Line<'_>> = if self.message.is_empty() {
            vec![Line::from("")]
        } else {
            self.message.lines().map(Line::from).collect()
        };
        let wrapped_rows = wrapped_row_count(&message_lines, content_width);
        // 改行数ではなく折り返し後の行数で本文領域を確保する（端末高さが上限）。
        let max_height = area.height.saturating_sub(2).max(1);
        let dialog_height = (wrapped_rows as u16)
            .saturating_add(6)
            .min(max_height)
            .max(1);

        let dialog_area = centered_rect(dialog_width, dialog_height, area);
        self.click_map.set_area(dialog_area);

        frame.render_widget(Clear, dialog_area);

        let block = Block::default()
            .title(Span::styled(
                format!(" {} {} ", icon, self.title),
                Style::default().fg(color).add_modifier(Modifier::BOLD),
            ))
            .borders(Borders::ALL)
            .border_style(Style::default().fg(color))
            .style(Style::default().bg(theme.background_panel));

        let inner = super::dialog_inner(block.inner(dialog_area));
        frame.render_widget(block, dialog_area);

        let paragraph = Paragraph::new(self.message.clone())
            .style(Style::default().fg(theme.text))
            .wrap(Wrap { trim: false })
            .centered();

        frame.render_widget(
            paragraph,
            Rect {
                x: inner.x,
                y: inner.y + 1,
                width: inner.width,
                height: inner.height.saturating_sub(4),
            },
        );

        let hint = Line::from(vec![
            Span::styled("[Enter] ", Style::default().fg(theme.text_muted)),
            Span::styled("Close", Style::default().fg(theme.text)),
            Span::styled("  [Ctrl+C] ", Style::default().fg(theme.text_muted)),
            Span::styled("Copy", Style::default().fg(theme.text)),
        ]);
        frame.render_widget(
            Paragraph::new(hint).centered(),
            Rect {
                x: inner.x,
                y: inner.y + inner.height.saturating_sub(3),
                width: inner.width,
                height: 1,
            },
        );

        let ok_button = Line::from(vec![Span::styled(
            " [ OK ] ",
            Style::default().fg(theme.text).bg(color),
        )]);

        frame.render_widget(
            Paragraph::new(ok_button).centered(),
            Rect {
                x: inner.x,
                y: inner.y + inner.height.saturating_sub(2),
                width: inner.width,
                height: 1,
            },
        );
    }
}

impl Default for AlertDialog {
    fn default() -> Self {
        Self::info("Message")
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
    fn new_stores_title_message_type_and_starts_closed() {
        let alert = AlertDialog::new("Title", "Message", AlertType::Warning);

        assert_eq!(alert.title, "Title");
        assert_eq!(alert.message(), "Message");
        // AlertType は PartialEq 未実装のため matches! で検証する。
        assert!(matches!(alert.alert_type, AlertType::Warning));
        assert!(!alert.is_open());
    }

    #[test]
    fn default_matches_info_alert_with_message() {
        let alert = AlertDialog::default();

        assert_eq!(alert.title, "Info");
        assert_eq!(alert.message(), "Message");
        assert!(matches!(alert.alert_type, AlertType::Info));
        assert!(!alert.is_open());
    }

    // --- convenience constructors ---

    #[test]
    fn convenience_constructors_set_expected_title_message_and_type() {
        let info = AlertDialog::info("info text");
        assert_eq!(info.title, "Info");
        assert_eq!(info.message(), "info text");
        assert!(matches!(info.alert_type, AlertType::Info));

        let success = AlertDialog::success("success text");
        assert_eq!(success.title, "Success");
        assert_eq!(success.message(), "success text");
        assert!(matches!(success.alert_type, AlertType::Success));

        let warning = AlertDialog::warning("warning text");
        assert_eq!(warning.title, "Warning");
        assert_eq!(warning.message(), "warning text");
        assert!(matches!(warning.alert_type, AlertType::Warning));

        let error = AlertDialog::error("error text");
        assert_eq!(error.title, "Error");
        assert_eq!(error.message(), "error text");
        assert!(matches!(error.alert_type, AlertType::Error));

        for alert in [&info, &success, &warning, &error] {
            assert!(!alert.is_open());
        }
    }

    // --- open / close / is_open ---

    #[test]
    fn open_sets_open_state_and_preserves_data() {
        let mut alert = AlertDialog::new("Title", "Message", AlertType::Error);

        alert.open();

        assert!(alert.is_open());
        assert_eq!(alert.title, "Title");
        assert_eq!(alert.message(), "Message");
        assert!(matches!(alert.alert_type, AlertType::Error));
    }

    #[test]
    fn close_clears_open_state_and_preserves_data() {
        let mut alert = AlertDialog::new("Title", "Message", AlertType::Error);
        alert.open();

        alert.close();

        assert!(!alert.is_open());
        assert_eq!(alert.title, "Title");
        assert_eq!(alert.message(), "Message");
        assert!(matches!(alert.alert_type, AlertType::Error));
    }

    #[test]
    fn open_close_cycles_toggle_state() {
        let mut alert = AlertDialog::info("cycling");

        for _ in 0..3 {
            alert.open();
            assert!(alert.is_open());
            alert.close();
            assert!(!alert.is_open());
        }
        assert_eq!(alert.message(), "cycling");
    }

    #[test]
    fn close_is_idempotent_from_new() {
        let mut alert = AlertDialog::info("message");

        alert.close();

        assert!(!alert.is_open());
        assert_eq!(alert.message(), "message");
    }

    // --- set_message / message ---

    #[test]
    fn set_message_updates_message_only() {
        let mut alert = AlertDialog::new("Title", "Old", AlertType::Warning);

        alert.set_message("New");

        assert_eq!(alert.message(), "New");
        assert_eq!(alert.title, "Title");
        assert!(matches!(alert.alert_type, AlertType::Warning));
    }

    #[test]
    fn set_message_preserves_open_state() {
        let mut alert = AlertDialog::error("before");
        alert.open();

        alert.set_message("after");

        assert!(alert.is_open());
        assert_eq!(alert.message(), "after");
    }

    #[test]
    fn set_message_accepts_empty_and_multiline() {
        let mut alert = AlertDialog::info("initial");

        alert.set_message("");
        assert_eq!(alert.message(), "");

        alert.set_message("line1\nline2");
        assert_eq!(alert.message(), "line1\nline2");
    }

    // --- render ---

    #[test]
    fn render_when_closed_draws_nothing() {
        let alert = AlertDialog::error("invisible");
        let theme = Theme::default();

        let text = render_to_string(80, 24, |frame| {
            alert.render(frame, Rect::new(0, 0, 80, 24), &theme);
        });

        assert!(
            text.trim().is_empty(),
            "closed alert dialog must not draw any content"
        );
    }

    #[test]
    fn render_when_open_shows_icon_title_message_and_hint() {
        let mut alert = AlertDialog::error("Something failed");
        alert.open();
        let theme = Theme::default();

        let text = render_to_string(80, 24, |frame| {
            alert.render(frame, Rect::new(0, 0, 80, 24), &theme);
        });

        assert!(text.contains("✗"), "error icon");
        assert!(text.contains("Error"), "dialog title");
        assert!(text.contains("Something failed"), "message body");
        assert!(text.contains("[Enter]"), "close hint key");
        assert!(text.contains("Close"), "close hint label");
        assert!(text.contains("[Ctrl+C]"), "copy hint key");
        assert!(text.contains("[ OK ]"), "ok button");
    }

    #[test]
    fn render_wraps_long_message_and_shows_full_text() {
        let message = format!("ALERTSTART-{} -ALERTEND", "z".repeat(220));
        let mut alert = AlertDialog::error(&message);
        alert.open();
        let theme = Theme::default();

        let text = render_to_string(80, 24, |frame| {
            alert.render(frame, Rect::new(0, 0, 80, 24), &theme);
        });

        assert!(text.contains("ALERTSTART"), "message head should render");
        assert!(
            text.contains("-ALERTEND"),
            "the end of a long alert must be wrapped and visible"
        );
        assert!(text.contains("[ OK ]"), "ok button must remain visible");
    }

    #[test]
    fn render_wraps_word_separated_message_and_shows_full_text() {
        // 単語境界での折り返しが高さ算出に反映され、末尾が見え続けること。
        let message = format!("alert-start {} -ALERT-END", "word ".repeat(60));
        let mut alert = AlertDialog::error(&message);
        alert.open();
        let theme = Theme::default();

        let text = render_to_string(80, 24, |frame| {
            alert.render(frame, Rect::new(0, 0, 80, 24), &theme);
        });

        assert!(text.contains("alert-start"), "message head should render");
        assert!(
            text.contains("-ALERT-END"),
            "the end of a word-wrapped alert must be wrapped and visible"
        );
        assert!(text.contains("[ OK ]"), "ok button must remain visible");
    }

    #[test]
    fn render_reflects_updated_message() {
        let mut alert = AlertDialog::info("before");
        alert.open();
        alert.set_message("updated body");
        let theme = Theme::default();

        let text = render_to_string(80, 24, |frame| {
            alert.render(frame, Rect::new(0, 0, 80, 24), &theme);
        });

        assert!(text.contains("updated body"));
        assert!(!text.contains("before"));
    }
}

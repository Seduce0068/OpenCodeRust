use ratatui::{
    layout::Rect,
    style::Style,
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};

use crate::context::TodoStatus;

pub struct TodoItem {
    pub content: String,
    pub status: TodoStatus,
}

impl TodoItem {
    pub fn new(content: &str, status: TodoStatus) -> Self {
        Self {
            content: content.to_string(),
            status,
        }
    }

    pub fn render(&self, frame: &mut Frame, area: Rect) {
        let (icon, color) = match &self.status {
            TodoStatus::Pending => ("○", ratatui::style::Color::Gray),
            TodoStatus::InProgress => ("◐", ratatui::style::Color::Yellow),
            TodoStatus::Completed => ("●", ratatui::style::Color::Green),
            TodoStatus::Cancelled => ("○", ratatui::style::Color::DarkGray),
        };

        let line = Line::from(vec![
            Span::styled(icon, Style::default().fg(color)),
            Span::raw(" "),
            Span::styled(&self.content, Style::default()),
        ]);

        let paragraph = Paragraph::new(line);
        frame.render_widget(paragraph, area);
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

    #[test]
    fn new_stores_content_and_status() {
        let item = TodoItem::new("タスク本文", TodoStatus::InProgress);

        assert_eq!(item.content, "タスク本文");
        assert!(matches!(item.status, TodoStatus::InProgress));
    }

    #[test]
    fn new_preserves_every_status_variant() {
        // session_context::TodoStatus は PartialEq/Copy を持たないため matches! で検証する。
        let pending = TodoItem::new("status check", TodoStatus::Pending);
        let in_progress = TodoItem::new("status check", TodoStatus::InProgress);
        let completed = TodoItem::new("status check", TodoStatus::Completed);
        let cancelled = TodoItem::new("status check", TodoStatus::Cancelled);

        assert!(matches!(pending.status, TodoStatus::Pending));
        assert!(matches!(in_progress.status, TodoStatus::InProgress));
        assert!(matches!(completed.status, TodoStatus::Completed));
        assert!(matches!(cancelled.status, TodoStatus::Cancelled));
        assert_eq!(pending.content, "status check");
    }

    #[test]
    fn new_keeps_empty_and_multiline_content_unchanged() {
        let empty = TodoItem::new("", TodoStatus::Pending);
        assert!(empty.content.is_empty());

        let multiline = TodoItem::new("one\ntwo\nthree", TodoStatus::Completed);
        assert_eq!(multiline.content, "one\ntwo\nthree");
    }

    #[test]
    fn render_draws_status_icon_and_content() {
        // Pending と Cancelled は同じアイコン（○）で、色でのみ区別される。
        for (status, icon) in [
            (TodoStatus::Pending, "○"),
            (TodoStatus::InProgress, "◐"),
            (TodoStatus::Completed, "●"),
            (TodoStatus::Cancelled, "○"),
        ] {
            let item = TodoItem::new("render target", status.clone());
            let text = render_to_string(40, 3, |frame| {
                item.render(frame, Rect::new(0, 0, 40, 3));
            });

            assert!(text.contains(icon), "expected icon {icon} for {status:?}");
            assert!(text.contains("render target"));
        }
    }

    #[test]
    fn render_only_writes_inside_the_given_area() {
        let item = TodoItem::new("middle row", TodoStatus::Completed);

        let text = render_to_string(20, 3, |frame| {
            item.render(frame, Rect::new(0, 1, 20, 1));
        });

        let rows: Vec<&str> = text.lines().collect();
        assert_eq!(rows.len(), 3);
        assert!(rows[0].trim().is_empty());
        assert!(rows[1].contains("middle row"));
        assert!(rows[2].trim().is_empty());
    }
}

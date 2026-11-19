use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph},
    Frame,
};

use crate::theme::Theme;

use super::DialogClickMap;

#[derive(Clone, Debug)]
pub enum StatusLineKind {
    Title,
    Normal,
    Muted,
    Success,
    Warning,
    Error,
}

#[derive(Clone, Debug)]
pub struct StatusLine {
    pub text: String,
    pub kind: StatusLineKind,
}

impl StatusLine {
    pub fn title(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            kind: StatusLineKind::Title,
        }
    }

    pub fn normal(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            kind: StatusLineKind::Normal,
        }
    }

    pub fn muted(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            kind: StatusLineKind::Muted,
        }
    }

    pub fn success(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            kind: StatusLineKind::Success,
        }
    }

    pub fn warning(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            kind: StatusLineKind::Warning,
        }
    }

    pub fn error(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            kind: StatusLineKind::Error,
        }
    }
}

pub struct StatusDialog {
    lines: Vec<StatusLine>,
    open: bool,
    click_map: DialogClickMap,
}

impl StatusDialog {
    pub fn new() -> Self {
        Self {
            lines: Vec::new(),
            open: false,
            click_map: DialogClickMap::default(),
        }
    }

    pub fn set_lines(&mut self, lines: Vec<String>) {
        self.lines = lines.into_iter().map(StatusLine::normal).collect();
    }

    pub fn set_status_lines(&mut self, lines: Vec<StatusLine>) {
        self.lines = lines;
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

    /// Return `true` when the click landed on the dialog frame, so the app
    /// keeps the dialog open.
    pub fn handle_click(&self, col: u16, row: u16) -> bool {
        self.click_map.contains(col, row)
    }

    pub fn render(&self, frame: &mut Frame, area: Rect, theme: &Theme) {
        if !self.open {
            return;
        }

        let dialog_area = centered_rect(90, 24, area);
        self.click_map.set_area(dialog_area);
        frame.render_widget(Clear, dialog_area);

        let block = Block::default()
            .title(Span::styled(
                " Status ",
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

        let lines = if self.lines.is_empty() {
            vec![Line::from(Span::styled(
                "No status data available.",
                Style::default().fg(theme.text_muted),
            ))]
        } else {
            self.lines
                .iter()
                .map(|line| {
                    let style = match line.kind {
                        StatusLineKind::Title => Style::default()
                            .fg(theme.primary)
                            .add_modifier(Modifier::BOLD),
                        StatusLineKind::Normal => Style::default().fg(theme.text),
                        StatusLineKind::Muted => Style::default().fg(theme.text_muted),
                        StatusLineKind::Success => Style::default().fg(theme.success),
                        StatusLineKind::Warning => Style::default().fg(theme.warning),
                        StatusLineKind::Error => Style::default().fg(theme.error),
                    };
                    Line::from(Span::styled(&line.text, style))
                })
                .collect()
        };
        frame.render_widget(Paragraph::new(lines), layout[0]);

        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled("Esc", Style::default().fg(theme.primary)),
                Span::styled(" close", Style::default().fg(theme.text_muted)),
            ])),
            layout[1],
        );
    }
}

impl Default for StatusDialog {
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

    #[test]
    fn status_line_constructors_set_text() {
        let cases = [
            (StatusLine::title("title text"), "title text"),
            (StatusLine::normal("normal text"), "normal text"),
            (StatusLine::muted("muted text"), "muted text"),
            (StatusLine::success("success text"), "success text"),
            (StatusLine::warning("warning text"), "warning text"),
            (StatusLine::error("error text"), "error text"),
        ];

        for (line, expected_text) in &cases {
            assert_eq!(line.text.as_str(), *expected_text);
        }
    }

    #[test]
    fn status_line_constructors_set_expected_kind() {
        // StatusLineKind は PartialEq を実装しないため matches! で検証する。
        assert!(matches!(
            &StatusLine::title("x").kind,
            StatusLineKind::Title
        ));
        assert!(matches!(
            &StatusLine::normal("x").kind,
            StatusLineKind::Normal
        ));
        assert!(matches!(
            &StatusLine::muted("x").kind,
            StatusLineKind::Muted
        ));
        assert!(matches!(
            &StatusLine::success("x").kind,
            StatusLineKind::Success
        ));
        assert!(matches!(
            &StatusLine::warning("x").kind,
            StatusLineKind::Warning
        ));
        assert!(matches!(
            &StatusLine::error("x").kind,
            StatusLineKind::Error
        ));
    }

    macro_rules! assert_kind_is_only {
        ($line:expr, $expected:path, [$($other:path),+ $(,)?]) => {{
            let line = $line;
            assert!(matches!(&line.kind, $expected));
            $(
                assert!(!matches!(&line.kind, $other));
            )+
        }};
    }

    #[test]
    fn status_line_kinds_are_distinct_across_constructors() {
        // 各コンストラクタが固有の kind を持ち、他と混同されないことを検証する。
        assert_kind_is_only!(
            StatusLine::title("x"),
            StatusLineKind::Title,
            [
                StatusLineKind::Normal,
                StatusLineKind::Muted,
                StatusLineKind::Success,
                StatusLineKind::Warning,
                StatusLineKind::Error,
            ]
        );
        assert_kind_is_only!(
            StatusLine::normal("x"),
            StatusLineKind::Normal,
            [
                StatusLineKind::Title,
                StatusLineKind::Muted,
                StatusLineKind::Success,
                StatusLineKind::Warning,
                StatusLineKind::Error,
            ]
        );
        assert_kind_is_only!(
            StatusLine::muted("x"),
            StatusLineKind::Muted,
            [
                StatusLineKind::Title,
                StatusLineKind::Normal,
                StatusLineKind::Success,
                StatusLineKind::Warning,
                StatusLineKind::Error,
            ]
        );
        assert_kind_is_only!(
            StatusLine::success("x"),
            StatusLineKind::Success,
            [
                StatusLineKind::Title,
                StatusLineKind::Normal,
                StatusLineKind::Muted,
                StatusLineKind::Warning,
                StatusLineKind::Error,
            ]
        );
        assert_kind_is_only!(
            StatusLine::warning("x"),
            StatusLineKind::Warning,
            [
                StatusLineKind::Title,
                StatusLineKind::Normal,
                StatusLineKind::Muted,
                StatusLineKind::Success,
                StatusLineKind::Error,
            ]
        );
        assert_kind_is_only!(
            StatusLine::error("x"),
            StatusLineKind::Error,
            [
                StatusLineKind::Title,
                StatusLineKind::Normal,
                StatusLineKind::Muted,
                StatusLineKind::Success,
                StatusLineKind::Warning,
            ]
        );
    }
}

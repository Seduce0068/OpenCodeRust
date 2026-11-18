use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph},
    Frame,
};

use crate::theme::Theme;

pub struct HelpDialog {
    open: bool,
}

impl HelpDialog {
    pub fn new() -> Self {
        Self { open: false }
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

    pub fn render(&self, frame: &mut Frame, area: Rect, theme: &Theme) {
        if !self.open {
            return;
        }

        // キー割当の説明行を 2 行追加したため、80x24 でも末尾が見えるよう 20 -> 22 に拡大。
        let dialog_area = centered_rect(74, 22, area);
        frame.render_widget(Clear, dialog_area);

        let block = Block::default()
            .title(Span::styled(
                " Help ",
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

        let lines = vec![
            Line::from(Span::styled(
                "General",
                Style::default()
                    .fg(theme.primary)
                    .add_modifier(Modifier::BOLD),
            )),
            Line::from("  Ctrl+X  Open command list"),
            Line::from("  Ctrl+P  Open command palette"),
            Line::from("  Ctrl+H  Open help"),
            Line::from("  Ctrl+C/q Exit TUI"),
            Line::from(""),
            Line::from(Span::styled(
                "Prompt",
                Style::default()
                    .fg(theme.primary)
                    .add_modifier(Modifier::BOLD),
            )),
            Line::from("  Enter   Submit prompt"),
            Line::from("  Ctrl+U  Clear prompt"),
            Line::from("  Ctrl+V  Paste clipboard"),
            Line::from("  Ctrl+Shift+C  Copy prompt"),
            Line::from("  Ctrl+Shift+X  Cut prompt"),
            Line::from("  Alt+Up/Alt+Down  Prompt history"),
            Line::from(""),
            Line::from(Span::styled(
                "Dialogs",
                Style::default()
                    .fg(theme.primary)
                    .add_modifier(Modifier::BOLD),
            )),
            Line::from("  Ctrl+M  Model list"),
            Line::from("  Alt+V   Cycle model variant"),
            Line::from("  Ctrl+T  Toggle thinking"),
            Line::from("  Alt+T   Toggle tool details"),
            Line::from("  Use /agents to open full agent list"),
            Line::from("  Ctrl+S  Toggle sidebar"),
            Line::from("  Use command palette for session/theme/status/MCP dialogs"),
            Line::from("  Use command palette -> Toggle appearance to switch dark/light"),
        ];

        frame.render_widget(
            Paragraph::new(lines).style(Style::default().fg(theme.text)),
            layout[0],
        );
        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled("Esc", Style::default().fg(theme.primary)),
                Span::styled(" close", Style::default().fg(theme.text_muted)),
            ])),
            layout[1],
        );
    }
}

impl Default for HelpDialog {
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
    fn new_starts_closed() {
        let dialog = HelpDialog::new();

        assert!(!dialog.is_open());
    }

    #[test]
    fn default_starts_closed() {
        let dialog = HelpDialog::default();

        assert!(!dialog.is_open());
    }

    // --- open / close / is_open ---

    #[test]
    fn open_sets_open_state() {
        let mut dialog = HelpDialog::new();
        assert!(!dialog.is_open());

        dialog.open();

        assert!(dialog.is_open());
    }

    #[test]
    fn close_clears_open_state() {
        let mut dialog = HelpDialog::new();
        dialog.open();
        assert!(dialog.is_open());

        dialog.close();

        assert!(!dialog.is_open());
    }

    #[test]
    fn open_close_cycles_toggle_state() {
        let mut dialog = HelpDialog::new();

        for _ in 0..3 {
            dialog.open();
            assert!(dialog.is_open());
            dialog.close();
            assert!(!dialog.is_open());
        }
    }

    #[test]
    fn close_is_idempotent_from_new() {
        let mut dialog = HelpDialog::new();

        dialog.close();
        dialog.close();

        assert!(!dialog.is_open());
    }

    #[test]
    fn open_is_idempotent() {
        let mut dialog = HelpDialog::new();

        dialog.open();
        dialog.open();

        assert!(dialog.is_open());
    }

    // --- render ---

    #[test]
    fn render_when_closed_draws_nothing() {
        let dialog = HelpDialog::new();
        let theme = Theme::default();

        let text = render_to_string(80, 24, |frame| {
            dialog.render(frame, Rect::new(0, 0, 80, 24), &theme);
        });

        assert!(
            text.trim().is_empty(),
            "closed help dialog must not draw any content"
        );
    }

    #[test]
    fn render_when_open_shows_help_sections_and_close_hint() {
        let mut dialog = HelpDialog::new();
        dialog.open();
        let theme = Theme::default();

        let text = render_to_string(80, 24, |frame| {
            dialog.render(frame, Rect::new(0, 0, 80, 24), &theme);
        });

        assert!(text.contains("Help"), "dialog title");
        assert!(text.contains("General"), "general section");
        assert!(text.contains("Ctrl+X"), "command list keybind");
        assert!(text.contains("Prompt"), "prompt section");
        assert!(text.contains("Esc"), "close hint key");
        assert!(text.contains("close"), "close hint label");
    }

    #[test]
    fn render_shows_view_toggle_and_variant_keybinds() {
        let mut dialog = HelpDialog::new();
        dialog.open();
        let theme = Theme::default();

        let text = render_to_string(80, 24, |frame| {
            dialog.render(frame, Rect::new(0, 0, 80, 24), &theme);
        });

        // R5: keybind.rs の割当（alt+v / ctrl+t / alt+t）と表示を一致させる。
        assert!(text.contains("Alt+V"), "variant cycle keybind");
        assert!(text.contains("Ctrl+T"), "toggle thinking keybind");
        assert!(text.contains("Alt+T"), "toggle tool details keybind");
    }
}

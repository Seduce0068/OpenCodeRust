use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph},
    Frame,
};

use crate::theme::Theme;

pub struct SessionExportDialog {
    open: bool,
    session_id: Option<String>,
    filename: String,
    pub include_thinking: bool,
    pub include_tool_details: bool,
    pub include_metadata: bool,
}

impl SessionExportDialog {
    pub fn new() -> Self {
        Self {
            open: false,
            session_id: None,
            filename: String::new(),
            include_thinking: false,
            include_tool_details: true,
            include_metadata: false,
        }
    }

    pub fn open(&mut self, session_id: String, default_filename: String) {
        self.open = true;
        self.session_id = Some(session_id);
        self.filename = default_filename;
    }

    pub fn close(&mut self) {
        self.open = false;
        self.session_id = None;
        self.filename.clear();
    }

    pub fn is_open(&self) -> bool {
        self.open
    }

    pub fn handle_input(&mut self, c: char) {
        match c {
            '1' => self.include_thinking = !self.include_thinking,
            '2' => self.include_tool_details = !self.include_tool_details,
            '3' => self.include_metadata = !self.include_metadata,
            _ => self.filename.push(c),
        }
    }

    pub fn handle_backspace(&mut self) {
        self.filename.pop();
    }

    pub fn session_id(&self) -> Option<&str> {
        self.session_id.as_deref()
    }

    pub fn filename(&self) -> &str {
        &self.filename
    }

    pub fn render(&self, frame: &mut Frame, area: Rect, theme: &Theme) {
        if !self.open {
            return;
        }

        let dialog_area = centered_rect(80, 14, area);
        frame.render_widget(Clear, dialog_area);

        let block = Block::default()
            .title(Span::styled(
                " Export Session ",
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
                Constraint::Length(1), // "Output filename:"
                Constraint::Length(1), // filename input
                Constraint::Length(1), // spacer
                Constraint::Length(1), // "Options:"
                Constraint::Length(1), // [1] thinking
                Constraint::Length(1), // [2] tool details
                Constraint::Length(1), // [3] metadata
                Constraint::Length(1), // spacer
                Constraint::Length(1), // hint
            ])
            .split(inner);

        frame.render_widget(
            Paragraph::new("Output filename:").style(Style::default().fg(theme.text)),
            layout[0],
        );
        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled("> ", Style::default().fg(theme.primary)),
                Span::styled(&self.filename, Style::default().fg(theme.text)),
                Span::styled("▏", Style::default().fg(theme.primary)),
            ])),
            layout[1],
        );

        frame.render_widget(
            Paragraph::new("Options:").style(Style::default().fg(theme.text_muted)),
            layout[3],
        );

        let check = |v: bool| if v { "[x]" } else { "[ ]" };
        frame.render_widget(
            Paragraph::new(format!(
                "  1  {} Include thinking blocks",
                check(self.include_thinking)
            ))
            .style(Style::default().fg(theme.text)),
            layout[4],
        );
        frame.render_widget(
            Paragraph::new(format!(
                "  2  {} Include tool call details",
                check(self.include_tool_details)
            ))
            .style(Style::default().fg(theme.text)),
            layout[5],
        );
        frame.render_widget(
            Paragraph::new(format!(
                "  3  {} Include metadata (tokens, cost)",
                check(self.include_metadata)
            ))
            .style(Style::default().fg(theme.text)),
            layout[6],
        );

        frame.render_widget(
            Paragraph::new(
                "Enter export  Ctrl+C copy transcript  1/2/3 toggle options  Esc cancel",
            )
            .style(Style::default().fg(theme.text_muted)),
            layout[8],
        );
    }
}

impl Default for SessionExportDialog {
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
    fn new_starts_closed_with_no_session_empty_filename_and_default_options() {
        let dialog = SessionExportDialog::new();

        assert!(!dialog.is_open());
        assert_eq!(dialog.session_id(), None);
        assert_eq!(dialog.filename(), "");
        assert!(!dialog.include_thinking);
        assert!(dialog.include_tool_details);
        assert!(!dialog.include_metadata);
    }

    #[test]
    fn default_starts_same_as_new() {
        let dialog = SessionExportDialog::default();

        assert!(!dialog.is_open());
        assert_eq!(dialog.session_id(), None);
        assert_eq!(dialog.filename(), "");
        assert!(!dialog.include_thinking);
        assert!(dialog.include_tool_details);
        assert!(!dialog.include_metadata);
    }

    // --- open / close / is_open ---

    #[test]
    fn open_sets_open_state_and_stores_session_id_and_filename() {
        let mut dialog = SessionExportDialog::new();

        dialog.open("sess-1".to_string(), "export.json".to_string());

        assert!(dialog.is_open());
        assert_eq!(dialog.session_id(), Some("sess-1"));
        assert_eq!(dialog.filename(), "export.json");
    }

    #[test]
    fn open_overwrites_previous_session_id_and_filename() {
        let mut dialog = SessionExportDialog::new();
        dialog.open("sess-1".to_string(), "first.json".to_string());

        dialog.open("sess-2".to_string(), "second.json".to_string());

        assert!(dialog.is_open());
        assert_eq!(dialog.session_id(), Some("sess-2"));
        assert_eq!(dialog.filename(), "second.json");
    }

    #[test]
    fn close_clears_open_state_session_id_and_filename() {
        let mut dialog = SessionExportDialog::new();
        dialog.open("sess-1".to_string(), "export.json".to_string());
        dialog.handle_input('x');
        assert_eq!(dialog.filename(), "export.jsonx");

        dialog.close();

        assert!(!dialog.is_open());
        assert_eq!(dialog.session_id(), None);
        assert_eq!(dialog.filename(), "");
    }

    #[test]
    fn close_is_idempotent_from_new() {
        let mut dialog = SessionExportDialog::new();

        dialog.close();
        dialog.close();

        assert!(!dialog.is_open());
        assert_eq!(dialog.session_id(), None);
        assert_eq!(dialog.filename(), "");
    }

    #[test]
    fn open_close_cycles_toggle_state() {
        let mut dialog = SessionExportDialog::new();

        for _ in 0..3 {
            dialog.open("sess".to_string(), "file.json".to_string());
            assert!(dialog.is_open());
            dialog.close();
            assert!(!dialog.is_open());
        }
        assert_eq!(dialog.session_id(), None);
        assert_eq!(dialog.filename(), "");
    }

    // --- handle_input ---

    #[test]
    fn handle_input_appends_non_digit_chars_to_filename_in_order() {
        let mut dialog = SessionExportDialog::new();
        dialog.open("sess".to_string(), String::new());

        for c in ['a', 'b', 'c'] {
            dialog.handle_input(c);
        }

        assert_eq!(dialog.filename(), "abc");
    }

    #[test]
    fn handle_input_appends_to_existing_filename() {
        let mut dialog = SessionExportDialog::new();
        dialog.open("sess".to_string(), "base".to_string());

        dialog.handle_input('.');
        dialog.handle_input('j');
        dialog.handle_input('s');
        dialog.handle_input('o');
        dialog.handle_input('n');

        assert_eq!(dialog.filename(), "base.json");
    }

    #[test]
    fn handle_input_accepts_whitespace_and_unicode() {
        let mut dialog = SessionExportDialog::new();
        dialog.open("sess".to_string(), String::new());

        dialog.handle_input(' ');
        dialog.handle_input('日');
        dialog.handle_input('本');
        dialog.handle_input(' ');

        assert_eq!(dialog.filename(), " 日本 ");
    }

    #[test]
    fn handle_input_digits_toggle_options_without_touching_filename() {
        let mut dialog = SessionExportDialog::new();
        dialog.open("sess".to_string(), "file".to_string());

        dialog.handle_input('1');
        assert!(dialog.include_thinking);
        assert_eq!(dialog.filename(), "file");

        dialog.handle_input('2');
        assert!(!dialog.include_tool_details);
        assert_eq!(dialog.filename(), "file");

        dialog.handle_input('3');
        assert!(dialog.include_metadata);
        assert_eq!(dialog.filename(), "file");
    }

    #[test]
    fn handle_input_option_toggles_are_independent_and_reversible() {
        let mut dialog = SessionExportDialog::new();
        // 初期値: thinking=false, tool_details=true, metadata=false
        assert!(!dialog.include_thinking);
        assert!(dialog.include_tool_details);
        assert!(!dialog.include_metadata);

        dialog.handle_input('1'); // thinking -> true
        dialog.handle_input('1'); // thinking -> false
        dialog.handle_input('2'); // tool_details -> false
        dialog.handle_input('1'); // thinking -> true ('2' とは独立)

        assert!(dialog.include_thinking);
        assert!(!dialog.include_tool_details);
        assert!(!dialog.include_metadata);
    }

    #[test]
    fn handle_input_preserves_session_id_and_open_state() {
        let mut dialog = SessionExportDialog::new();
        dialog.open("sess-42".to_string(), String::new());

        dialog.handle_input('a');
        dialog.handle_input('1');
        dialog.handle_input('b');

        assert_eq!(dialog.filename(), "ab");
        assert!(dialog.is_open());
        assert_eq!(dialog.session_id(), Some("sess-42"));
    }

    // --- handle_backspace ---

    #[test]
    fn handle_backspace_removes_exactly_one_char_at_a_time() {
        let mut dialog = SessionExportDialog::new();
        dialog.open("sess".to_string(), "abc".to_string());

        dialog.handle_backspace();
        assert_eq!(dialog.filename(), "ab");

        dialog.handle_backspace();
        assert_eq!(dialog.filename(), "a");

        dialog.handle_backspace();
        assert_eq!(dialog.filename(), "");
    }

    #[test]
    fn handle_backspace_removes_one_unicode_scalar_not_one_byte_or_word() {
        // String::pop() は末尾の 1 char (Unicode scalar) を削除する。
        let mut dialog = SessionExportDialog::new();
        dialog.open("sess".to_string(), "日本".to_string());

        dialog.handle_backspace();
        assert_eq!(dialog.filename(), "日");

        dialog.handle_backspace();
        assert_eq!(dialog.filename(), "");
    }

    #[test]
    fn handle_backspace_on_empty_filename_is_noop() {
        let mut dialog = SessionExportDialog::new();
        dialog.open("sess".to_string(), String::new());

        dialog.handle_backspace();
        dialog.handle_backspace();

        assert_eq!(dialog.filename(), "");
        assert!(dialog.is_open());
        assert_eq!(dialog.session_id(), Some("sess"));
    }

    #[test]
    fn handle_backspace_does_not_affect_option_flags() {
        let mut dialog = SessionExportDialog::new();
        dialog.open("sess".to_string(), "abc".to_string());
        dialog.handle_input('1'); // thinking = true

        dialog.handle_backspace();

        assert_eq!(dialog.filename(), "ab");
        assert!(dialog.include_thinking);
        assert!(dialog.include_tool_details);
        assert!(!dialog.include_metadata);
    }

    // --- データ保持 (data retention) ---

    #[test]
    fn option_flags_are_retained_across_close_and_reopen() {
        let mut dialog = SessionExportDialog::new();
        dialog.open("sess-1".to_string(), "first.json".to_string());
        dialog.handle_input('1'); // thinking = true
        dialog.handle_input('2'); // tool_details = false
        dialog.handle_input('3'); // metadata = true

        dialog.close();
        assert!(!dialog.is_open());
        assert_eq!(dialog.session_id(), None);
        assert_eq!(dialog.filename(), "");
        assert!(dialog.include_thinking);
        assert!(!dialog.include_tool_details);
        assert!(dialog.include_metadata);

        dialog.open("sess-2".to_string(), "second.json".to_string());
        assert!(dialog.is_open());
        assert_eq!(dialog.session_id(), Some("sess-2"));
        assert_eq!(dialog.filename(), "second.json");
        // open() はオプションを保持し、リセットしない。
        assert!(dialog.include_thinking);
        assert!(!dialog.include_tool_details);
        assert!(dialog.include_metadata);
    }

    #[test]
    fn filename_edits_and_option_toggles_retain_independent_state() {
        let mut dialog = SessionExportDialog::new();
        dialog.open("sess".to_string(), "report".to_string());

        dialog.handle_input('1'); // thinking = true
        for c in ['.', 't', 'x', 't'] {
            dialog.handle_input(c);
        }
        dialog.handle_backspace();

        assert_eq!(dialog.filename(), "report.tx");
        assert!(dialog.include_thinking);
        assert!(dialog.include_tool_details);
        assert!(!dialog.include_metadata);
        assert!(dialog.is_open());
        assert_eq!(dialog.session_id(), Some("sess"));
    }

    // --- render ---

    #[test]
    fn render_when_closed_draws_nothing() {
        let dialog = SessionExportDialog::new();
        let theme = Theme::default();
        assert!(!dialog.is_open());

        let text = render_to_string(80, 24, |frame| {
            dialog.render(frame, Rect::new(0, 0, 80, 24), &theme);
        });

        assert!(
            text.trim().is_empty(),
            "closed session export dialog must not draw any content"
        );
    }

    #[test]
    fn render_when_open_shows_title_filename_and_default_options() {
        let mut dialog = SessionExportDialog::new();
        dialog.open("sess".to_string(), "export.json".to_string());
        let theme = Theme::default();

        let text = render_to_string(80, 24, |frame| {
            dialog.render(frame, Rect::new(0, 0, 80, 24), &theme);
        });

        assert!(text.contains("Export Session"), "dialog title");
        assert!(text.contains("Output filename:"), "filename label");
        assert!(text.contains("export.json"), "current filename value");
        assert!(text.contains("Options:"), "options label");
        assert!(
            text.contains("[ ] Include thinking blocks"),
            "thinking defaults off"
        );
        assert!(
            text.contains("[x] Include tool call details"),
            "tool details defaults on"
        );
        assert!(
            text.contains("[ ] Include metadata (tokens, cost)"),
            "metadata defaults off"
        );
        assert!(text.contains("Enter export"), "export hint");
        assert!(text.contains("Esc cancel"), "cancel hint");
    }

    #[test]
    fn render_reflects_option_toggles_and_filename_edits() {
        let mut dialog = SessionExportDialog::new();
        dialog.open("sess".to_string(), "old.json".to_string());
        for _ in 0..8 {
            dialog.handle_backspace();
        }
        for c in "new.json".chars() {
            dialog.handle_input(c);
        }
        dialog.handle_input('1'); // thinking -> true
        dialog.handle_input('2'); // tool_details -> false
        dialog.handle_input('3'); // metadata -> true
        let theme = Theme::default();

        let text = render_to_string(80, 24, |frame| {
            dialog.render(frame, Rect::new(0, 0, 80, 24), &theme);
        });

        assert!(text.contains("new.json"), "edited filename rendered");
        assert!(!text.contains("old.json"), "old filename must be gone");
        assert!(text.contains("[x] Include thinking blocks"));
        assert!(text.contains("[ ] Include tool call details"));
        assert!(text.contains("[x] Include metadata (tokens, cost)"));
    }
}

use ratatui::{
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph},
    Frame,
};

use crate::theme::Theme;

#[derive(Clone, Debug, PartialEq)]
pub enum ToolRenderMode {
    Inline,
    Block,
}

#[derive(Clone, Debug)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub arguments: String,
    pub status: ToolCallStatus,
}

impl ToolCall {
    pub fn render_mode(&self) -> ToolRenderMode {
        match self.name.as_str() {
            "glob" | "grep" | "list" | "webfetch" | "websearch" | "skill" | "read" => {
                ToolRenderMode::Inline
            }
            "bash" | "write" | "edit" | "apply_patch" | "task" | "todowrite" | "question" => {
                ToolRenderMode::Block
            }
            _ => ToolRenderMode::Block,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum ToolCallStatus {
    Pending,
    Running,
    Completed,
    Failed,
}

pub struct ToolCallView {
    tool_call: ToolCall,
}

impl ToolCallView {
    pub fn new(tool_call: ToolCall) -> Self {
        Self { tool_call }
    }

    pub fn render(&self, frame: &mut Frame, area: Rect, theme: &Theme) {
        let status_icon = match self.tool_call.status {
            ToolCallStatus::Pending => "◯",
            ToolCallStatus::Running => "◐",
            ToolCallStatus::Completed => "●",
            ToolCallStatus::Failed => "✗",
        };

        let status_color = match self.tool_call.status {
            ToolCallStatus::Pending => theme.text_muted,
            ToolCallStatus::Running => theme.warning,
            ToolCallStatus::Completed => theme.success,
            ToolCallStatus::Failed => theme.error,
        };

        let title_line = Line::from(vec![
            Span::styled(status_icon, Style::default().fg(status_color)),
            Span::raw(" "),
            Span::styled(
                &self.tool_call.name,
                Style::default()
                    .fg(theme.primary)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                match self.tool_call.status {
                    ToolCallStatus::Pending => " (pending)",
                    ToolCallStatus::Running => " (running...)",
                    ToolCallStatus::Completed => " (completed)",
                    ToolCallStatus::Failed => " (failed)",
                },
                Style::default().fg(theme.text_muted),
            ),
        ]);

        let mut lines = vec![title_line];

        if !self.tool_call.arguments.is_empty() {
            lines.push(Line::from(""));
            for line in self.tool_call.arguments.lines().take(5) {
                lines.push(Line::from(Span::styled(
                    format!("  {}", line),
                    Style::default().fg(theme.text_muted),
                )));
            }
            if self.tool_call.arguments.lines().count() > 5 {
                lines.push(Line::from(Span::styled(
                    "  ...",
                    Style::default().fg(theme.text_muted),
                )));
            }
        }

        let paragraph = Paragraph::new(lines).block(
            Block::default()
                .borders(Borders::LEFT)
                .border_style(Style::default().fg(status_color)),
        );

        frame.render_widget(paragraph, area);
    }
}

pub struct ToolResultView {
    pub tool_name: String,
    pub result: String,
    pub is_error: bool,
    pub truncated: bool,
}

impl ToolResultView {
    pub fn new(tool_name: String, result: String, is_error: bool) -> Self {
        let line_count = result.lines().count();
        Self {
            tool_name,
            result,
            is_error,
            truncated: line_count > 20,
        }
    }

    pub fn render(&self, frame: &mut Frame, area: Rect, theme: &Theme) {
        let icon = if self.is_error { "✗" } else { "✓" };
        let color = if self.is_error {
            theme.error
        } else {
            theme.success
        };

        let display_result = if self.truncated {
            self.result.lines().take(20).collect::<Vec<_>>().join("\n")
        } else {
            self.result.clone()
        };

        let mut lines = vec![Line::from(vec![
            Span::styled(icon, Style::default().fg(color)),
            Span::raw(" "),
            Span::styled(
                &self.tool_name,
                Style::default().fg(theme.text).add_modifier(Modifier::BOLD),
            ),
            Span::styled(" result", Style::default().fg(theme.text_muted)),
        ])];

        lines.push(Line::from(""));

        for line in display_result.lines() {
            let styled_line = if self.is_error {
                Span::styled(line.to_string(), Style::default().fg(theme.error))
            } else {
                Span::styled(line.to_string(), Style::default().fg(theme.text))
            };
            lines.push(Line::from(styled_line));
        }

        if self.truncated {
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(
                "... (truncated)",
                Style::default().fg(theme.text_muted),
            )));
        }

        let paragraph = Paragraph::new(lines).block(
            Block::default()
                .borders(Borders::LEFT)
                .border_style(Style::default().fg(color)),
        );

        frame.render_widget(paragraph, area);
    }
}

pub struct BashToolView {
    pub command: String,
    pub output: Option<String>,
    pub exit_code: Option<i32>,
    pub running: bool,
}

impl BashToolView {
    pub fn new(command: String) -> Self {
        Self {
            command,
            output: None,
            exit_code: None,
            running: false,
        }
    }

    pub fn render(&self, frame: &mut Frame, area: Rect, theme: &Theme) {
        let status_color = if self.running {
            theme.warning
        } else if self.exit_code.is_some_and(|c| c != 0) {
            theme.error
        } else {
            theme.success
        };

        let mut lines = vec![Line::from(vec![
            Span::styled(
                "$ ",
                Style::default()
                    .fg(theme.primary)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(&self.command, Style::default().fg(theme.text)),
        ])];

        if let Some(ref output) = self.output {
            lines.push(Line::from(""));
            for line in output.lines().take(15) {
                lines.push(Line::from(Span::styled(
                    line.to_string(),
                    Style::default().fg(theme.text),
                )));
            }
            if output.lines().count() > 15 {
                lines.push(Line::from(Span::styled(
                    "... (output truncated)",
                    Style::default().fg(theme.text_muted),
                )));
            }
        }

        if let Some(code) = self.exit_code {
            lines.push(Line::from(""));
            let status_text = if code == 0 {
                format!("Exit code: {}", code)
            } else {
                format!("Exit code: {} (failed)", code)
            };
            lines.push(Line::from(Span::styled(
                status_text,
                Style::default().fg(status_color),
            )));
        }

        let paragraph = Paragraph::new(lines).block(
            Block::default()
                .borders(Borders::LEFT)
                .border_style(Style::default().fg(status_color)),
        );

        frame.render_widget(paragraph, area);
    }
}

pub struct ReadToolView {
    pub file_path: String,
    pub content: Option<String>,
    pub line_range: Option<(usize, usize)>,
}

impl ReadToolView {
    pub fn new(file_path: String) -> Self {
        Self {
            file_path,
            content: None,
            line_range: None,
        }
    }

    pub fn render(&self, frame: &mut Frame, area: Rect, theme: &Theme) {
        let mut lines = vec![Line::from(vec![
            Span::styled("📖 ", Style::default()),
            Span::styled(&self.file_path, Style::default().fg(theme.primary)),
            if let Some((start, end)) = self.line_range {
                Span::styled(
                    format!(" (lines {}-{})", start, end),
                    Style::default().fg(theme.text_muted),
                )
            } else {
                Span::raw("")
            },
        ])];

        if let Some(ref content) = self.content {
            lines.push(Line::from(""));
            for (i, line) in content.lines().take(20).enumerate() {
                let line_num = if let Some((start, _)) = self.line_range {
                    start + i
                } else {
                    i + 1
                };
                lines.push(Line::from(vec![
                    Span::styled(
                        format!("{:4} ", line_num),
                        Style::default().fg(theme.text_muted),
                    ),
                    Span::styled(line.to_string(), Style::default().fg(theme.text)),
                ]));
            }
            if content.lines().count() > 20 {
                lines.push(Line::from(Span::styled(
                    "... (content truncated)",
                    Style::default().fg(theme.text_muted),
                )));
            }
        }

        let paragraph = Paragraph::new(lines).block(
            Block::default()
                .borders(Borders::LEFT)
                .border_style(Style::default().fg(theme.info)),
        );

        frame.render_widget(paragraph, area);
    }
}

pub struct WriteToolView {
    pub file_path: String,
    pub content_preview: Option<String>,
    pub written: bool,
}

impl WriteToolView {
    pub fn new(file_path: String) -> Self {
        Self {
            file_path,
            content_preview: None,
            written: false,
        }
    }

    pub fn render(&self, frame: &mut Frame, area: Rect, theme: &Theme) {
        let status_icon = if self.written { "✓" } else { "..." };
        let status_color = if self.written {
            theme.success
        } else {
            theme.text_muted
        };

        let mut lines = vec![Line::from(vec![
            Span::styled(status_icon, Style::default().fg(status_color)),
            Span::raw(" "),
            Span::styled(&self.file_path, Style::default().fg(theme.primary)),
        ])];

        if let Some(ref preview) = self.content_preview {
            lines.push(Line::from(""));
            for line in preview.lines().take(5) {
                lines.push(Line::from(Span::styled(
                    format!("  {}", line),
                    Style::default().fg(theme.text_muted),
                )));
            }
        }

        let paragraph = Paragraph::new(lines).block(
            Block::default()
                .borders(Borders::LEFT)
                .border_style(Style::default().fg(status_color)),
        );

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

    fn sample_call(name: &str, status: ToolCallStatus) -> ToolCall {
        ToolCall {
            id: "call-1".to_string(),
            name: name.to_string(),
            arguments: "{\"file_path\":\"src/lib.rs\"}".to_string(),
            status,
        }
    }

    // --- ToolRenderMode / ToolCall::render_mode ---

    #[test]
    fn render_mode_maps_read_only_tools_to_inline() {
        for name in [
            "glob",
            "grep",
            "list",
            "webfetch",
            "websearch",
            "skill",
            "read",
        ] {
            let call = sample_call(name, ToolCallStatus::Pending);
            assert_eq!(call.render_mode(), ToolRenderMode::Inline, "{name}");
        }
    }

    #[test]
    fn render_mode_maps_mutating_and_task_tools_to_block() {
        for name in [
            "bash",
            "write",
            "edit",
            "apply_patch",
            "task",
            "todowrite",
            "question",
        ] {
            let call = sample_call(name, ToolCallStatus::Pending);
            assert_eq!(call.render_mode(), ToolRenderMode::Block, "{name}");
        }
    }

    #[test]
    fn render_mode_defaults_to_block_for_unknown_or_non_exact_names() {
        for name in ["", "unknown", "READ", "Read", "read ", "read_file", "globx"] {
            let call = sample_call(name, ToolCallStatus::Pending);
            assert_eq!(call.render_mode(), ToolRenderMode::Block, "{name:?}");
        }
    }

    #[test]
    fn render_mode_is_independent_of_status_and_arguments() {
        let mut call = sample_call("read", ToolCallStatus::Pending);
        let expected = call.render_mode();

        call.status = ToolCallStatus::Failed;
        call.arguments.clear();
        call.id = "another-id".to_string();

        assert_eq!(call.render_mode(), expected);
        assert_eq!(expected, ToolRenderMode::Inline);
    }

    // --- ToolCallStatus / ToolCall ---

    #[test]
    fn status_variants_are_distinct_and_cloneable() {
        let all = [
            ToolCallStatus::Pending,
            ToolCallStatus::Running,
            ToolCallStatus::Completed,
            ToolCallStatus::Failed,
        ];

        for (i, left) in all.iter().enumerate() {
            assert_eq!(<ToolCallStatus as Clone>::clone(left), *left);
            for (j, right) in all.iter().enumerate() {
                assert_eq!(left == right, i == j, "status pair ({i}, {j})");
            }
        }
    }

    #[test]
    fn tool_call_struct_keeps_all_fields() {
        let call = sample_call("bash", ToolCallStatus::Running);

        assert_eq!(call.id, "call-1");
        assert_eq!(call.name, "bash");
        assert_eq!(call.arguments, "{\"file_path\":\"src/lib.rs\"}");
        assert_eq!(call.status, ToolCallStatus::Running);
        assert_eq!(call.clone().name, "bash");
    }

    // --- ToolCallView ---

    #[test]
    fn tool_call_view_new_stores_the_call() {
        let call = sample_call("edit", ToolCallStatus::Completed);
        let view = ToolCallView::new(call);

        assert_eq!(view.tool_call.id, "call-1");
        assert_eq!(view.tool_call.name, "edit");
        assert_eq!(view.tool_call.arguments, "{\"file_path\":\"src/lib.rs\"}");
        assert_eq!(view.tool_call.status, ToolCallStatus::Completed);
    }

    #[test]
    fn tool_call_view_render_shows_status_name_and_arguments() {
        let view = ToolCallView::new(sample_call("read", ToolCallStatus::Running));
        let theme = Theme::default();

        let text = render_to_string(60, 12, |frame| {
            view.render(frame, Rect::new(0, 0, 60, 12), &theme);
        });

        assert!(text.contains("◐"), "running icon");
        assert!(text.contains("read"));
        assert!(text.contains("(running...)"));
        assert!(text.contains("file_path"));
    }

    #[test]
    fn tool_call_view_render_omits_argument_lines_when_empty() {
        let mut call = sample_call("read", ToolCallStatus::Completed);
        call.arguments.clear();
        let view = ToolCallView::new(call);
        let theme = Theme::default();

        let text = render_to_string(60, 4, |frame| {
            view.render(frame, Rect::new(0, 0, 60, 4), &theme);
        });

        assert!(text.contains("●"));
        assert!(text.contains("(completed)"));
        assert!(!text.contains("file_path"));
    }

    // --- ToolResultView ---

    #[test]
    fn tool_result_view_new_stores_fields() {
        let view = ToolResultView::new("bash".to_string(), "ok".to_string(), true);

        assert_eq!(view.tool_name, "bash");
        assert_eq!(view.result, "ok");
        assert!(view.is_error);
        assert!(!view.truncated);
    }

    #[test]
    fn tool_result_view_truncation_threshold_is_21_lines() {
        let within = (1..=20)
            .map(|i| format!("line {i}"))
            .collect::<Vec<_>>()
            .join("\n");
        let over = (1..=21)
            .map(|i| format!("line {i}"))
            .collect::<Vec<_>>()
            .join("\n");

        assert!(!ToolResultView::new("r".into(), String::new(), false).truncated);
        assert!(!ToolResultView::new("r".into(), within, false).truncated);
        assert!(ToolResultView::new("r".into(), over, false).truncated);
    }

    #[test]
    fn tool_result_view_render_truncates_at_twenty_lines() {
        let result = (1..=21)
            .map(|i| format!("line {i}"))
            .collect::<Vec<_>>()
            .join("\n");
        let view = ToolResultView::new("bash".to_string(), result, true);
        let theme = Theme::default();

        let text = render_to_string(60, 30, |frame| {
            view.render(frame, Rect::new(0, 0, 60, 30), &theme);
        });

        assert!(text.contains("✗"));
        assert!(text.contains("result"));
        assert!(text.contains("line 20"));
        assert!(!text.contains("line 21"));
        assert!(text.contains("... (truncated)"));
    }

    // --- BashToolView ---

    #[test]
    fn bash_tool_view_new_starts_with_no_result() {
        let view = BashToolView::new("cargo test".to_string());

        assert_eq!(view.command, "cargo test");
        assert!(view.output.is_none());
        assert!(view.exit_code.is_none());
        assert!(!view.running);
    }

    #[test]
    fn bash_tool_view_render_shows_command_and_failed_exit_code() {
        let mut view = BashToolView::new("echo hi".to_string());
        view.exit_code = Some(2);
        let theme = Theme::default();

        let text = render_to_string(60, 10, |frame| {
            view.render(frame, Rect::new(0, 0, 60, 10), &theme);
        });

        assert!(text.contains("$ echo hi"));
        assert!(text.contains("Exit code: 2 (failed)"));
    }

    #[test]
    fn bash_tool_view_render_shows_success_exit_code() {
        let mut view = BashToolView::new("true".to_string());
        view.exit_code = Some(0);
        let theme = Theme::default();

        let text = render_to_string(40, 6, |frame| {
            view.render(frame, Rect::new(0, 0, 40, 6), &theme);
        });

        assert!(text.contains("Exit code: 0"));
        assert!(!text.contains("(failed)"));
    }

    // --- ReadToolView / WriteToolView ---

    #[test]
    fn read_tool_view_new_starts_without_content_or_range() {
        let view = ReadToolView::new("src/main.rs".to_string());

        assert_eq!(view.file_path, "src/main.rs");
        assert!(view.content.is_none());
        assert!(view.line_range.is_none());
    }

    #[test]
    fn read_tool_view_render_uses_range_as_start_line_number() {
        let mut view = ReadToolView::new("src/main.rs".to_string());
        view.line_range = Some((10, 12));
        view.content = Some("first\nsecond".to_string());
        let theme = Theme::default();

        let text = render_to_string(60, 10, |frame| {
            view.render(frame, Rect::new(0, 0, 60, 10), &theme);
        });

        assert!(text.contains("src/main.rs"));
        assert!(text.contains("(lines 10-12)"));
        assert!(text.contains("first"));
        assert!(text.contains("second"));
    }

    #[test]
    fn write_tool_view_new_starts_pending_without_preview() {
        let view = WriteToolView::new("out.txt".to_string());

        assert_eq!(view.file_path, "out.txt");
        assert!(view.content_preview.is_none());
        assert!(!view.written);
    }

    #[test]
    fn write_tool_view_render_reflects_written_state() {
        let mut view = WriteToolView::new("out.txt".to_string());
        view.content_preview = Some("hello".to_string());
        let theme = Theme::default();

        let pending = render_to_string(40, 8, |frame| {
            view.render(frame, Rect::new(0, 0, 40, 8), &theme);
        });
        assert!(pending.contains("out.txt"));
        assert!(pending.contains("hello"));
        assert!(!pending.contains("✓"));

        view.written = true;
        let done = render_to_string(40, 8, |frame| {
            view.render(frame, Rect::new(0, 0, 40, 8), &theme);
        });
        assert!(done.contains("✓"));
    }
}

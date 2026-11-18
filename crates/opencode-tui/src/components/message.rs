use ratatui::{
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};

use super::diff::DiffView;
use super::markdown::MarkdownRenderer;
use super::tool_call::ToolCallStatus;
use crate::theme::Theme;

pub enum MessagePart {
    Text {
        content: String,
    },
    ToolCall {
        id: String,
        name: String,
        arguments: String,
        status: ToolCallStatus,
        result: Option<String>,
    },
    Reasoning {
        content: String,
        duration_ms: Option<u64>,
    },
}

pub enum MessageRole {
    User,
    Assistant,
    System,
}

pub struct MessageView {
    pub role: MessageRole,
    pub parts: Vec<MessagePart>,
    pub model_id: Option<String>,
    pub duration_ms: Option<u64>,
    pub error: Option<String>,
    pub show_thinking: bool,
    pub concealed: bool,
}

impl MessageView {
    pub fn new(role: MessageRole) -> Self {
        Self {
            role,
            parts: Vec::new(),
            model_id: None,
            duration_ms: None,
            error: None,
            show_thinking: true,
            concealed: false,
        }
    }

    pub fn render(&self, frame: &mut Frame, area: Rect, theme: &Theme) {
        let mut lines: Vec<Line> = Vec::new();

        for part in &self.parts {
            match part {
                MessagePart::Text { content } => {
                    lines.extend(self.render_text(content, theme));
                }
                MessagePart::ToolCall {
                    name,
                    status,
                    result,
                    arguments,
                    ..
                } => {
                    lines.extend(self.render_tool_call(
                        name,
                        arguments,
                        status,
                        result.as_deref(),
                        theme,
                    ));
                }
                MessagePart::Reasoning {
                    content,
                    duration_ms,
                } => {
                    if self.show_thinking {
                        lines.extend(self.render_reasoning(content, *duration_ms, theme));
                    }
                }
            }
        }

        // Error display (non-abort errors)
        if let Some(err) = &self.error {
            lines.push(Line::from(Span::styled(
                format!("Error: {}", err),
                Style::default().fg(theme.error),
            )));
        }

        // Completion footer for assistant messages
        if matches!(self.role, MessageRole::Assistant) {
            if let Some(model) = &self.model_id {
                let mut footer_spans = vec![Span::styled(
                    model.as_str(),
                    Style::default().fg(theme.text_muted),
                )];
                if let Some(ms) = self.duration_ms {
                    let duration_text = if ms < 1000 {
                        format!("  {}ms", ms)
                    } else {
                        format!("  {:.1}s", ms as f64 / 1000.0)
                    };
                    footer_spans.push(Span::styled(
                        duration_text,
                        Style::default().fg(theme.text_muted),
                    ));
                }
                lines.push(Line::from(footer_spans));
            }
        }

        let paragraph = Paragraph::new(lines);
        frame.render_widget(paragraph, area);
    }

    fn render_text(&self, content: &str, theme: &Theme) -> Vec<Line<'static>> {
        let renderer = MarkdownRenderer::new(theme.clone()).with_concealed(self.concealed);
        renderer.to_lines(content)
    }

    fn render_tool_call(
        &self,
        name: &str,
        arguments: &str,
        status: &ToolCallStatus,
        result: Option<&str>,
        theme: &Theme,
    ) -> Vec<Line<'static>> {
        let mut lines = Vec::new();

        let status_icon = match status {
            ToolCallStatus::Pending => "◯",
            ToolCallStatus::Running => "◐",
            ToolCallStatus::Completed => "●",
            ToolCallStatus::Failed => "✗",
        };

        let status_color = match status {
            ToolCallStatus::Pending => theme.text_muted,
            ToolCallStatus::Running => theme.warning,
            ToolCallStatus::Completed => theme.success,
            ToolCallStatus::Failed => theme.error,
        };

        // Tool header
        lines.push(Line::from(vec![
            Span::styled(status_icon, Style::default().fg(status_color)),
            Span::raw(" "),
            Span::styled(
                name.to_string(),
                Style::default()
                    .fg(theme.primary)
                    .add_modifier(Modifier::BOLD),
            ),
        ]));

        // Dispatch to specialized tool views based on tool name
        match name {
            "bash" => {
                if !arguments.is_empty() {
                    let cmd = arguments.lines().next().unwrap_or(arguments).to_string();
                    lines.push(Line::from(vec![
                        Span::styled(
                            "$ ",
                            Style::default()
                                .fg(theme.primary)
                                .add_modifier(Modifier::BOLD),
                        ),
                        Span::styled(cmd, Style::default().fg(theme.text)),
                    ]));
                }
            }
            "edit" | "apply_patch" => {
                // Show diff if result contains diff content
                if let Some(res) = result {
                    let diff_view = DiffView::new().with_content(res);
                    let diff_lines = diff_view.to_lines(theme);
                    lines.extend(diff_lines);
                } else if !arguments.is_empty() {
                    for line in arguments.lines().take(5) {
                        lines.push(Line::from(Span::styled(
                            format!("  {}", line),
                            Style::default().fg(theme.text_muted),
                        )));
                    }
                }
            }
            "read" => {
                let path = arguments.lines().next().unwrap_or(arguments).to_string();
                lines.push(Line::from(vec![
                    Span::styled("  ", Style::default()),
                    Span::styled(path, Style::default().fg(theme.info)),
                ]));
            }
            "write" => {
                let path = arguments.lines().next().unwrap_or(arguments).to_string();
                lines.push(Line::from(vec![
                    Span::styled("  ", Style::default()),
                    Span::styled(path, Style::default().fg(theme.success)),
                ]));
            }
            "todowrite" => {
                if let Some(res) = result {
                    for line in res.lines().take(10) {
                        lines.push(Line::from(Span::styled(
                            format!("  {}", line),
                            Style::default().fg(theme.text),
                        )));
                    }
                }
            }
            _ => {
                // Generic: show truncated arguments
                if !arguments.is_empty() {
                    for line in arguments.lines().take(3) {
                        lines.push(Line::from(Span::styled(
                            format!("  {}", line),
                            Style::default().fg(theme.text_muted),
                        )));
                    }
                    if arguments.lines().count() > 3 {
                        lines.push(Line::from(Span::styled(
                            "  ...",
                            Style::default().fg(theme.text_muted),
                        )));
                    }
                }
            }
        }

        // Show result summary for completed tools
        if matches!(status, ToolCallStatus::Completed) {
            if let Some(res) = result {
                if !matches!(name, "edit" | "apply_patch" | "todowrite") {
                    let line_count = res.lines().count();
                    if line_count > 0 {
                        lines.push(Line::from(Span::styled(
                            format!("  ({} lines of output)", line_count),
                            Style::default().fg(theme.text_muted),
                        )));
                    }
                }
            }
        }

        // Show error for failed tools
        if matches!(status, ToolCallStatus::Failed) {
            if let Some(res) = result {
                lines.push(Line::from(Span::styled(
                    format!("  Error: {}", res.lines().next().unwrap_or("")),
                    Style::default().fg(theme.error),
                )));
            }
        }

        lines.push(Line::from(""));
        lines
    }

    fn render_reasoning(
        &self,
        content: &str,
        duration_ms: Option<u64>,
        theme: &Theme,
    ) -> Vec<Line<'static>> {
        let mut lines = Vec::new();

        // Strip [REDACTED] markers
        let cleaned = content.replace("[REDACTED]", "").trim().to_string();
        if cleaned.is_empty() {
            return lines;
        }

        let title = if let Some(ms) = duration_ms {
            if ms < 1000 {
                format!("Thinking ({}ms)", ms)
            } else {
                format!("Thinking ({:.1}s)", ms as f64 / 1000.0)
            }
        } else {
            "Thinking".to_string()
        };

        lines.push(Line::from(vec![
            Span::styled("▼ ", Style::default().fg(theme.primary)),
            Span::styled(
                title,
                Style::default()
                    .fg(theme.primary)
                    .add_modifier(Modifier::BOLD),
            ),
        ]));

        for line in cleaned.lines() {
            lines.push(Line::from(Span::styled(
                line.to_string(),
                Style::default().fg(theme.text_muted),
            )));
        }

        lines.push(Line::from(""));
        lines
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::{backend::TestBackend, Terminal};

    fn theme() -> Theme {
        Theme::default()
    }

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

    fn lines_to_text(lines: &[Line<'static>]) -> String {
        let mut text = String::new();
        for line in lines {
            for span in &line.spans {
                text.push_str(span.content.as_ref());
            }
            text.push('\n');
        }
        text
    }

    // --- MessageRole / MessageView::new ---

    #[test]
    fn new_stores_role_and_defaults() {
        let view = MessageView::new(MessageRole::Assistant);

        assert!(matches!(&view.role, MessageRole::Assistant));
        assert!(view.parts.is_empty());
        assert!(view.model_id.is_none());
        assert!(view.duration_ms.is_none());
        assert!(view.error.is_none());
        assert!(view.show_thinking);
        assert!(!view.concealed);
    }

    #[test]
    fn new_preserves_every_role_variant() {
        // MessageRole は PartialEq を持たないため matches! で検証する。
        let user = MessageView::new(MessageRole::User);
        let assistant = MessageView::new(MessageRole::Assistant);
        let system = MessageView::new(MessageRole::System);

        assert!(matches!(&user.role, MessageRole::User));
        assert!(matches!(&assistant.role, MessageRole::Assistant));
        assert!(matches!(&system.role, MessageRole::System));
    }

    // --- データ保持（pub フィールドへ代入した値を assert で読む） ---

    #[test]
    fn view_fields_retain_assigned_values() {
        let view = MessageView {
            role: MessageRole::System,
            parts: Vec::new(),
            model_id: Some("model-x".to_string()),
            duration_ms: Some(2500),
            error: Some("boom".to_string()),
            show_thinking: false,
            concealed: true,
        };

        assert!(matches!(&view.role, MessageRole::System));
        assert!(view.parts.is_empty());
        assert_eq!(view.model_id.as_deref(), Some("model-x"));
        assert_eq!(view.duration_ms, Some(2500));
        assert_eq!(view.error.as_deref(), Some("boom"));
        assert!(!view.show_thinking);
        assert!(view.concealed);
    }

    // --- MessagePart データ保持 ---

    #[test]
    fn text_part_keeps_content() {
        let mut view = MessageView::new(MessageRole::User);
        view.parts.push(MessagePart::Text {
            content: "hello world".to_string(),
        });

        assert_eq!(view.parts.len(), 1);
        assert!(matches!(
            &view.parts[0],
            MessagePart::Text { content } if content.as_str() == "hello world"
        ));
    }

    #[test]
    fn tool_call_part_keeps_all_fields() {
        // ToolCallStatus は PartialEq を持たないためネストした matches! で検証する。
        let mut view = MessageView::new(MessageRole::Assistant);
        view.parts.push(MessagePart::ToolCall {
            id: "call-1".to_string(),
            name: "bash".to_string(),
            arguments: "echo hi".to_string(),
            status: ToolCallStatus::Completed,
            result: Some("hi".to_string()),
        });

        assert!(matches!(
            &view.parts[0],
            MessagePart::ToolCall {
                id,
                name,
                arguments,
                status,
                result,
            } if id.as_str() == "call-1"
                && name.as_str() == "bash"
                && arguments.as_str() == "echo hi"
                && matches!(status, ToolCallStatus::Completed)
                && result.as_deref() == Some("hi")
        ));
    }

    #[test]
    fn reasoning_part_keeps_content_and_duration() {
        let mut view = MessageView::new(MessageRole::Assistant);
        view.parts.push(MessagePart::Reasoning {
            content: "thinking".to_string(),
            duration_ms: Some(1234),
        });
        view.parts.push(MessagePart::Reasoning {
            content: "no duration".to_string(),
            duration_ms: None,
        });

        assert_eq!(view.parts.len(), 2);
        assert!(matches!(
            &view.parts[0],
            MessagePart::Reasoning { content, duration_ms }
                if content.as_str() == "thinking" && *duration_ms == Some(1234)
        ));
        assert!(matches!(
            &view.parts[1],
            MessagePart::Reasoning { content, duration_ms }
                if content.as_str() == "no duration" && duration_ms.is_none()
        ));
    }

    // --- render_text ---

    #[test]
    fn render_text_returns_no_lines_for_empty_input() {
        let view = MessageView::new(MessageRole::User);
        assert!(view.render_text("", &theme()).is_empty());
    }

    #[test]
    fn render_text_keeps_plain_text_and_default_style() {
        let theme = theme();
        let view = MessageView::new(MessageRole::User);

        let lines = view.render_text("hello", &theme);

        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].spans.len(), 1);
        assert_eq!(lines[0].spans[0].content.as_ref(), "hello");
        assert_eq!(lines[0].spans[0].style.fg, Some(theme.text));
    }

    #[test]
    fn render_text_splits_multiline_content() {
        let view = MessageView::new(MessageRole::User);

        let lines = view.render_text("first\nsecond\nthird", &theme());

        assert_eq!(lines.len(), 3);
        assert_eq!(lines[0].spans[0].content.as_ref(), "first");
        assert_eq!(lines[1].spans[0].content.as_ref(), "second");
        assert_eq!(lines[2].spans[0].content.as_ref(), "third");
    }

    #[test]
    fn render_text_keeps_markdown_heading_spans() {
        let theme = theme();
        let view = MessageView::new(MessageRole::User);

        let lines = view.render_text("# Title", &theme);

        assert_eq!(lines.len(), 1);
        let spans = &lines[0].spans;
        assert_eq!(spans.len(), 2);
        assert_eq!(spans[0].content.as_ref(), "# ");
        assert_eq!(spans[0].style.fg, Some(theme.markdown_heading));
        assert_eq!(spans[1].content.as_ref(), "Title");
        assert_eq!(spans[1].style.fg, Some(theme.markdown_heading));
    }

    #[test]
    fn render_text_propagates_concealed_to_code_block() {
        let theme = theme();
        let md = "```rust\nfn main() {}\n```";
        let plain = MessageView::new(MessageRole::User);
        let plain_text = lines_to_text(&plain.render_text(md, &theme));
        assert!(plain_text.contains("fn main()"));

        // concealed は pub フィールド。代入値を読み取るため mut 宣言する。
        let mut concealed_view = MessageView::new(MessageRole::User);
        concealed_view.concealed = true;
        let concealed_text = lines_to_text(&concealed_view.render_text(md, &theme));

        assert!(concealed_text.contains("[rust - 1 lines]"));
        assert!(!concealed_text.contains("fn main()"));
    }

    // --- render（全体描画） ---

    #[test]
    fn render_draws_parts_error_and_assistant_footer() {
        let theme = theme();
        let mut view = MessageView::new(MessageRole::Assistant);
        view.parts.push(MessagePart::Text {
            content: "visible".to_string(),
        });
        view.model_id = Some("m1".to_string());
        view.duration_ms = Some(500);
        view.error = Some("oops".to_string());

        let text = render_to_string(60, 6, |frame| {
            view.render(frame, Rect::new(0, 0, 60, 6), &theme);
        });

        assert!(text.contains("visible"));
        assert!(text.contains("Error: oops"));
        assert!(text.contains("m1"));
        assert!(text.contains("500ms"));
    }
}

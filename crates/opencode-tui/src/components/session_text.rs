use ratatui::{
    style::{Color, Modifier, Style},
    text::{Line, Span},
};

use super::markdown::MarkdownRenderer;
use crate::theme::Theme;

/// Render a text part using markdown rendering.
///
/// `available_width` は行全体の利用可能幅。`▸ ` / `  ` マーカー（2 列）の分を
/// 差し引いて Markdown を描画する。
pub fn render_text_part(
    text: &str,
    theme: &Theme,
    marker_color: Color,
    available_width: usize,
) -> Vec<Line<'static>> {
    const MARKER_WIDTH: usize = 2; // "▸ " / "  "

    let renderer = MarkdownRenderer::new(theme.clone());
    let markdown_width = available_width.saturating_sub(MARKER_WIDTH);
    let mut with_marker = Vec::new();
    for (idx, line) in renderer
        .to_lines_with_width(text, markdown_width)
        .into_iter()
        .enumerate()
    {
        let marker = if idx == 0 { "▸ " } else { "  " };
        let mut spans = vec![Span::styled(marker, Style::default().fg(marker_color))];
        spans.extend(line.spans);
        with_marker.push(Line::from(spans));
    }
    with_marker
}

/// Render a reasoning/thinking part with muted styling and collapsible header.
pub struct ReasoningRender {
    pub lines: Vec<Line<'static>>,
    pub collapsible: bool,
}

pub fn render_reasoning_part(
    text: &str,
    theme: &Theme,
    collapsed: bool,
    preview_lines: usize,
    available_width: usize,
) -> ReasoningRender {
    const MARKER_WIDTH: usize = 2; // "  "

    let cleaned = text.replace("[REDACTED]", "").trim().to_string();
    if cleaned.is_empty() {
        return ReasoningRender {
            lines: Vec::new(),
            collapsible: false,
        };
    }

    let mut lines = Vec::new();
    let renderer = MarkdownRenderer::new(theme.clone()).with_concealed(true);
    let content_lines =
        renderer.to_lines_with_width(&cleaned, available_width.saturating_sub(MARKER_WIDTH));
    let total_content_lines = content_lines.len();
    let collapsible = total_content_lines > preview_lines;

    if collapsible && collapsed {
        lines.push(Line::from(Span::styled(
            format!("▶ Thinking ({} lines)", total_content_lines),
            Style::default()
                .fg(theme.text_muted)
                .add_modifier(Modifier::ITALIC),
        )));
        return ReasoningRender { lines, collapsible };
    }

    lines.push(Line::from(Span::styled(
        if collapsible {
            "▼ Thinking"
        } else {
            "Thinking"
        },
        Style::default()
            .fg(theme.text_muted)
            .add_modifier(Modifier::ITALIC),
    )));

    let visible_count = if collapsible && collapsed {
        preview_lines
    } else {
        total_content_lines
    };

    // Render reasoning with concealed style and muted color.
    for line in content_lines.into_iter().take(visible_count) {
        let mut spans = vec![Span::styled("  ", Style::default().fg(theme.text_muted))];
        spans.extend(
            line.spans
                .into_iter()
                .map(|span| Span::styled(span.content, span.style.fg(theme.text_muted))),
        );
        lines.push(Line::from(spans));
    }

    if collapsible {
        lines.push(Line::from(Span::styled(
            "  [click to collapse]",
            Style::default().fg(theme.text_muted),
        )));
    }

    ReasoningRender { lines, collapsible }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_theme() -> Theme {
        Theme::default()
    }

    #[test]
    fn render_text_part_returns_no_lines_for_empty_input() {
        let lines = render_text_part("", &test_theme(), Color::Cyan, usize::MAX);
        assert!(lines.is_empty());
    }

    #[test]
    fn render_text_part_prepends_marker_and_keeps_markdown_text() {
        let theme = test_theme();
        let marker = Color::Magenta;
        let lines = render_text_part("hello", &theme, marker, usize::MAX);

        assert_eq!(lines.len(), 1);
        let spans = &lines[0].spans;
        assert_eq!(spans.len(), 2);
        assert_eq!(spans[0].content.as_ref(), "▸ ");
        assert_eq!(spans[0].style.fg, Some(marker));
        assert_eq!(spans[1].content.as_ref(), "hello");
        assert_eq!(spans[1].style.fg, Some(theme.text));
    }

    #[test]
    fn render_text_part_uses_indent_marker_after_first_line() {
        let theme = test_theme();
        let marker = Color::Yellow;
        let lines = render_text_part("first\nsecond\nthird", &theme, marker, usize::MAX);

        assert_eq!(lines.len(), 3);
        assert_eq!(lines[0].spans[0].content.as_ref(), "▸ ");
        assert_eq!(lines[1].spans[0].content.as_ref(), "  ");
        assert_eq!(lines[2].spans[0].content.as_ref(), "  ");
        assert_eq!(lines[1].spans[1].content.as_ref(), "second");
        assert_eq!(lines[2].spans[1].content.as_ref(), "third");
        for line in &lines {
            assert_eq!(line.spans[0].style.fg, Some(marker));
        }
    }

    #[test]
    fn render_text_part_keeps_markdown_heading_spans() {
        let theme = test_theme();
        let lines = render_text_part("# Title", &theme, Color::Cyan, usize::MAX);

        assert_eq!(lines.len(), 1);
        let spans = &lines[0].spans;
        assert_eq!(spans.len(), 3);
        assert_eq!(spans[1].content.as_ref(), "# ");
        assert_eq!(spans[1].style.fg, Some(theme.markdown_heading));
        assert!(spans[1].style.add_modifier.contains(Modifier::BOLD));
        assert_eq!(spans[2].content.as_ref(), "Title");
        assert_eq!(spans[2].style.fg, Some(theme.markdown_heading));
    }

    #[test]
    fn render_reasoning_part_returns_empty_for_blank_or_redacted_text() {
        let theme = test_theme();
        for input in ["", "   ", "[REDACTED]", " \n [REDACTED] \n "] {
            let render = render_reasoning_part(input, &theme, false, 2, usize::MAX);
            assert!(render.lines.is_empty(), "input: {input:?}");
            assert!(!render.collapsible, "input: {input:?}");
        }
    }

    #[test]
    fn render_reasoning_part_builds_header_and_muted_body_for_short_text() {
        let theme = test_theme();
        let render = render_reasoning_part("alpha", &theme, false, 3, usize::MAX);

        assert!(!render.collapsible);
        assert_eq!(render.lines.len(), 2);

        let header = &render.lines[0].spans;
        assert_eq!(header.len(), 1);
        assert_eq!(header[0].content.as_ref(), "Thinking");
        assert_eq!(header[0].style.fg, Some(theme.text_muted));
        assert!(header[0].style.add_modifier.contains(Modifier::ITALIC));

        let body = &render.lines[1].spans;
        assert_eq!(body.len(), 2);
        assert_eq!(body[0].content.as_ref(), "  ");
        assert_eq!(body[0].style.fg, Some(theme.text_muted));
        assert_eq!(body[1].content.as_ref(), "alpha");
        assert_eq!(body[1].style.fg, Some(theme.text_muted));
    }

    #[test]
    fn render_reasoning_part_collapsed_shows_only_count_header() {
        let theme = test_theme();
        let render = render_reasoning_part("one\ntwo\nthree", &theme, true, 2, usize::MAX);

        assert!(render.collapsible);
        assert_eq!(render.lines.len(), 1);
        let header = &render.lines[0].spans;
        assert_eq!(header.len(), 1);
        assert_eq!(header[0].content.as_ref(), "▶ Thinking (3 lines)");
        assert_eq!(header[0].style.fg, Some(theme.text_muted));
        assert!(header[0].style.add_modifier.contains(Modifier::ITALIC));
    }

    #[test]
    fn render_reasoning_part_expanded_shows_all_lines_and_footer() {
        let theme = test_theme();
        let render = render_reasoning_part("one\ntwo\nthree", &theme, false, 2, usize::MAX);

        assert!(render.collapsible);
        assert_eq!(render.lines.len(), 5);
        assert_eq!(render.lines[0].spans[0].content.as_ref(), "▼ Thinking");
        assert_eq!(render.lines[1].spans[1].content.as_ref(), "one");
        assert_eq!(render.lines[2].spans[1].content.as_ref(), "two");
        assert_eq!(render.lines[3].spans[1].content.as_ref(), "three");
        let footer = &render.lines[4].spans;
        assert_eq!(footer.len(), 1);
        assert_eq!(footer[0].content.as_ref(), "  [click to collapse]");
        assert_eq!(footer[0].style.fg, Some(theme.text_muted));
    }

    #[test]
    fn render_reasoning_part_removes_redacted_marker_from_body() {
        let theme = test_theme();
        let render = render_reasoning_part("before [REDACTED] after", &theme, false, 3, usize::MAX);

        assert!(!render.collapsible);
        assert_eq!(render.lines.len(), 2);
        assert_eq!(render.lines[1].spans[1].content.as_ref(), "before  after");
    }

    #[test]
    fn render_reasoning_part_conceals_and_mutes_code_blocks() {
        let theme = test_theme();
        let render = render_reasoning_part(
            "```rust\nfn main() {}\nlet x = 1;\n```",
            &theme,
            false,
            4,
            usize::MAX,
        );

        assert!(!render.collapsible);
        assert_eq!(render.lines.len(), 2);
        let body = &render.lines[1].spans;
        assert_eq!(body.len(), 3);
        assert_eq!(body[0].content.as_ref(), "  ");
        assert_eq!(body[1].content.as_ref(), "▶ ");
        assert_eq!(body[2].content.as_ref(), "[rust - 2 lines]");
        assert_eq!(body[1].style.fg, Some(theme.text_muted));
        assert_eq!(body[2].style.fg, Some(theme.text_muted));
    }
}

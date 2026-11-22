use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

use ratatui::{
    style::Color,
    style::{Modifier, Style},
    text::{Line, Span},
};

use super::markdown::MarkdownRenderer;
use crate::context::{Message, MessagePart};
use crate::theme::Theme;

/// Render a user message with shared left gutter shape.
///
/// `available_width` は行全体の利用可能幅。`┃ ` ガターの分を差し引いて
/// Markdown を描画し、表が端末幅を超えないようにする。
pub fn render_user_message(
    msg: &Message,
    theme: &Theme,
    show_timestamps: bool,
    agent: Option<&str>,
    available_width: usize,
) -> Vec<Line<'static>> {
    const GUTTER_WIDTH: usize = 2; // "┃ "

    let mut lines = Vec::new();
    let border_char = "┃ ";
    let markdown_width = available_width.saturating_sub(GUTTER_WIDTH);
    let border_style = Style::default().fg(user_border_color_for_agent(agent, theme));

    if msg.parts.is_empty() {
        for line_text in msg.content.lines() {
            lines.push(Line::from(vec![
                Span::styled(border_char, border_style),
                Span::styled(line_text.to_string(), Style::default().fg(theme.text)),
            ]));
        }
    } else {
        for part in &msg.parts {
            match part {
                MessagePart::Text { text } => {
                    let md_renderer = MarkdownRenderer::new(theme.clone());
                    let md_lines = md_renderer.to_lines_with_width(text, markdown_width);
                    for md_line in md_lines {
                        let mut spans = vec![Span::styled(border_char, border_style)];
                        spans.extend(md_line.spans);
                        lines.push(Line::from(spans));
                    }
                }
                MessagePart::File { path, mime } => {
                    lines.push(Line::from(vec![
                        Span::styled(border_char, border_style),
                        Span::styled(
                            mime_badge(mime),
                            Style::default().fg(theme.info).add_modifier(Modifier::BOLD),
                        ),
                        Span::raw(" "),
                        Span::styled(path.clone(), Style::default().fg(theme.text)),
                    ]));
                }
                MessagePart::Image { url } => {
                    lines.push(Line::from(vec![
                        Span::styled(border_char, border_style),
                        Span::styled("[image] ", Style::default().fg(theme.info)),
                        Span::styled(url.clone(), Style::default().fg(theme.text_muted)),
                    ]));
                }
                _ => {}
            }
        }
    }

    if show_timestamps {
        // wire/DB の UTC 値はそのままに、表示時だけローカルタイムゾーンへ変換する。
        // session_list / prompt_stash と同じ方針（最大で offset 分のズレを解消）。
        let ts = msg
            .created_at
            .with_timezone(&chrono::Local)
            .format("%H:%M")
            .to_string();
        if !lines.is_empty() {
            lines.push(Line::from(vec![
                Span::styled(border_char, border_style),
                Span::styled(ts, Style::default().fg(theme.text_muted)),
            ]));
        }
    }

    lines
}

fn mime_badge(mime: &str) -> String {
    let short = if let Some(sub) = mime.strip_prefix("image/") {
        sub.to_uppercase()
    } else if let Some(sub) = mime.strip_prefix("text/") {
        sub.to_uppercase()
    } else if let Some(sub) = mime.strip_prefix("application/") {
        sub.to_uppercase()
    } else {
        mime.to_uppercase()
    };
    format!("[{}]", short)
}

fn user_border_color_for_agent(agent: Option<&str>, theme: &Theme) -> Color {
    let Some(agent) = agent else {
        return theme.primary;
    };
    if theme.agent_colors.is_empty() {
        return theme.primary;
    }
    let mut hasher = DefaultHasher::new();
    agent.hash(&mut hasher);
    let idx = (hasher.finish() as usize) % theme.agent_colors.len();
    theme.agent_colors[idx]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mime_badge_strips_prefix_and_uppercases() {
        assert_eq!(mime_badge("image/png"), "[PNG]");
        assert_eq!(mime_badge("text/plain"), "[PLAIN]");
        assert_eq!(mime_badge("application/pdf"), "[PDF]");
        assert_eq!(mime_badge("custom"), "[CUSTOM]");
    }

    #[test]
    fn border_color_defaults_to_primary() {
        let theme = Theme::default();
        assert_eq!(user_border_color_for_agent(None, &theme), theme.primary);
    }

    #[test]
    fn border_color_is_stable_for_agent() {
        let theme = Theme::default();
        if theme.agent_colors.is_empty() {
            return;
        }
        let first = user_border_color_for_agent(Some("build"), &theme);
        let second = user_border_color_for_agent(Some("build"), &theme);
        assert_eq!(first, second);
        assert!(theme.agent_colors.contains(&first));
    }

    #[test]
    fn user_message_timestamp_uses_local_timezone() {
        use crate::context::{MessageRole, TokenUsage};
        use chrono::{TimeZone, Utc};

        let created_at = Utc
            .with_ymd_and_hms(2026, 1, 2, 3, 4, 5)
            .single()
            .expect("valid UTC datetime");
        let msg = Message {
            id: "m1".to_string(),
            role: MessageRole::User,
            content: "hi".to_string(),
            created_at,
            agent: None,
            model: None,
            mode: None,
            finish: None,
            error: None,
            completed_at: None,
            cost: 0.0,
            tokens: TokenUsage::default(),
            parts: Vec::new(),
        };

        let lines = render_user_message(&msg, &Theme::default(), true, None, 80);
        let rendered = lines
            .last()
            .and_then(|line| line.spans.iter().find(|span| span.content.contains(':')))
            .map(|span| span.content.to_string())
            .expect("timestamp span must be rendered");

        let expected = created_at
            .with_timezone(&chrono::Local)
            .format("%H:%M")
            .to_string();
        assert_eq!(rendered, expected, "timestamp must use local timezone");

        // オフセットが 0 のホストでは UTC と一致するため、差分がある場合のみ検証する。
        let raw_utc = created_at.format("%H:%M").to_string();
        if expected != raw_utc {
            assert_ne!(rendered, raw_utc, "timestamp must not be raw UTC");
        }
    }
}

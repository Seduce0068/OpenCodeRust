use std::sync::Arc;

use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::Style,
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};

use crate::branding::{APP_SHORT_NAME, APP_TAGLINE, APP_VERSION_DATE};
use crate::components::{Logo, Prompt};
use crate::context::{AppContext, McpConnectionStatus};

const HOME_TIPS: &[&str] = &[
    "Press {highlight}Tab{/highlight} to cycle agents",
    "Press {highlight}Shift+Tab{/highlight} to cycle agents backward",
    "Press {highlight}Ctrl+P{/highlight} to open command palette",
    "Type {highlight}/help{/highlight} to browse all commands",
    "Use {highlight}/themes{/highlight} to switch visual themes",
    "Use {highlight}/sessions{/highlight} to resume older work",
    "Use {highlight}/timeline{/highlight} to jump to any message",
    "Use {highlight}/status{/highlight} to inspect runtime status",
    "Use {highlight}/mcps{/highlight} to inspect MCP connections",
    "Use {highlight}/editor{/highlight} to write prompts externally",
    "Use {highlight}/compact{/highlight} when context gets too long",
    "Use {highlight}/new{/highlight} to start a clean session",
    "Use {highlight}/copy{/highlight} to copy current session summary",
    "Use {highlight}/fork{/highlight} to branch from a message",
    "Use {highlight}/rename{/highlight} to rename this session",
    "Use {highlight}/share{/highlight} to generate share links",
    "Use {highlight}/unshare{/highlight} to revoke share links",
    "Use {highlight}/timestamps{/highlight} to show or hide times",
    "Use {highlight}/thinking{/highlight} to toggle reasoning blocks",
    "Use {highlight}/density{/highlight} for cozy or compact layout",
    "Use {highlight}/highlight{/highlight} to toggle semantic styling",
    "Use {highlight}/sidebar{/highlight} to toggle sidebar visibility",
    "Use {highlight}/header{/highlight} to toggle session header",
    "Use {highlight}/scrollbar{/highlight} to toggle scrollbar",
    "Use {highlight}/tips.toggle{/highlight} to hide or show tips",
    "Use {highlight}/stash{/highlight} to save current draft",
    "Use {highlight}/export{/highlight} to export chat transcript",
    "Use {highlight}Esc{/highlight} twice to interrupt running tasks",
    "Use {highlight}Alt+Up{/highlight} and {highlight}Alt+Down{/highlight} for prompt history",
    "Use {highlight}Ctrl+V{/highlight} to paste into the prompt",
    "Use {highlight}Ctrl+Shift+C{/highlight} to copy selected text",
    "Use {highlight}@path{/highlight} to reference files in prompt",
    "Use {highlight}/connect{/highlight} to add a new provider",
    "Use {highlight}/models{/highlight} to switch active model",
    "Use {highlight}/agents{/highlight} to switch active agent",
    "Use {highlight}/skills{/highlight} to inspect installed skills",
];
const TIP_ROTATE_SECONDS: i64 = 12;
const HOME_MAX_CONTENT_WIDTH: u16 = 75;
const HOME_OUTER_H_PADDING: u16 = 2;
const HOME_OUTER_V_PADDING: u16 = 1;

pub struct HomeView {
    context: Arc<AppContext>,
}

impl HomeView {
    pub fn new(context: Arc<AppContext>) -> Self {
        Self { context }
    }

    pub fn render(&self, frame: &mut Frame, area: Rect) {
        let prompt = Prompt::new(self.context.clone())
            .with_placeholder("Ask anything... \"Fix a TODO in the codebase\"");
        self.render_with_prompt(frame, area, &prompt);
    }

    pub fn render_with_prompt(&self, frame: &mut Frame, area: Rect, prompt: &Prompt) {
        let area = Rect {
            x: area.x.saturating_add(HOME_OUTER_H_PADDING),
            y: area.y.saturating_add(HOME_OUTER_V_PADDING),
            width: area
                .width
                .saturating_sub(HOME_OUTER_H_PADDING.saturating_mul(2)),
            height: area
                .height
                .saturating_sub(HOME_OUTER_V_PADDING.saturating_mul(2)),
        };
        if area.width == 0 || area.height == 0 {
            return;
        }
        let theme = self.context.theme.read();

        let layout = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Min(1),
                Constraint::Length(6),
                Constraint::Length(1),
                Constraint::Length(8),
                Constraint::Length(4),
                Constraint::Length(3),
            ])
            .split(area);

        let logo = Logo::new(theme.text, theme.text_muted, theme.background);
        logo.render(frame, layout[1]);
        self.render_tagline(frame, layout[2]);

        let prompt_width = layout[3].width.min(HOME_MAX_CONTENT_WIDTH);
        let left_pad = (layout[3].width.saturating_sub(prompt_width)) / 2;
        let prompt_area = Rect {
            x: layout[3].x + left_pad,
            y: layout[3].y,
            width: prompt_width,
            height: layout[3].height,
        };

        prompt.render(frame, prompt_area);

        if self.should_show_tips() {
            self.render_tips(frame, layout[4]);
        }

        self.render_footer(frame, layout[5]);
    }

    fn render_tagline(&self, frame: &mut Frame, area: Rect) {
        if area.width == 0 || area.height == 0 {
            return;
        }
        let theme = self.context.theme.read();
        let line = Paragraph::new(Line::from(Span::styled(
            APP_TAGLINE,
            Style::default().fg(theme.text_muted),
        )))
        .alignment(ratatui::layout::Alignment::Center);
        frame.render_widget(line, area);
    }

    fn render_tips(&self, frame: &mut Frame, area: Rect) {
        if area.width == 0 || area.height == 0 {
            return;
        }
        let theme = self.context.theme.read();
        let tip_width = area.width.min(HOME_MAX_CONTENT_WIDTH);
        let left_pad = (area.width.saturating_sub(tip_width)) / 2;
        let tip_area = Rect {
            x: area.x + left_pad,
            y: area.y,
            width: tip_width,
            height: area.height,
        };
        if tip_area.height == 0 || tip_area.width == 0 {
            return;
        }

        let top_padding = 3u16.min(tip_area.height.saturating_sub(1));
        let tip_render_area = Rect {
            x: tip_area.x,
            y: tip_area.y.saturating_add(top_padding),
            width: tip_area.width,
            height: tip_area.height.saturating_sub(top_padding).max(1),
        };

        let slot = chrono::Utc::now()
            .timestamp()
            .div_euclid(TIP_ROTATE_SECONDS);
        let tip_idx = slot.rem_euclid(HOME_TIPS.len() as i64) as usize;
        let tip = HOME_TIPS
            .get(tip_idx)
            .copied()
            .unwrap_or("Use /help to open command guide");
        let mut spans = vec![Span::styled("● Tip ", Style::default().fg(theme.warning))];
        spans.extend(parse_tip_highlights(tip, &theme));
        let paragraph = Paragraph::new(Line::from(spans));

        frame.render_widget(paragraph, tip_render_area);
    }

    fn render_footer(&self, frame: &mut Frame, area: Rect) {
        if area.width == 0 || area.height == 0 {
            return;
        }
        let theme = self.context.theme.read();
        let directory = self.context.directory.read();
        let mcp_servers = self.context.mcp_servers.read();

        let horizontal_padding = 2u16.min(area.width / 2);
        let vertical_padding = if area.height >= 3 { 1 } else { 0 };
        let content_area = Rect {
            x: area.x.saturating_add(horizontal_padding),
            y: area.y.saturating_add(vertical_padding),
            width: area
                .width
                .saturating_sub(horizontal_padding.saturating_mul(2)),
            height: area
                .height
                .saturating_sub(vertical_padding.saturating_mul(2)),
        };
        if content_area.width == 0 || content_area.height == 0 {
            return;
        }

        let connected_count = mcp_servers
            .iter()
            .filter(|s| matches!(s.status, McpConnectionStatus::Connected))
            .count();

        let mut spans: Vec<Span> = Vec::new();

        // Left: directory
        let dir_text = directory.to_string();
        spans.push(Span::styled(
            dir_text.clone(),
            Style::default().fg(theme.text_muted),
        ));

        // Middle: MCP status (only when servers exist)
        let mcp_text = if !mcp_servers.is_empty() {
            let has_errors = mcp_servers.iter().any(|s| {
                matches!(
                    s.status,
                    McpConnectionStatus::Failed | McpConnectionStatus::NeedsClientRegistration
                )
            });
            let dot_color = if has_errors {
                theme.error
            } else if connected_count > 0 {
                theme.success
            } else {
                theme.text_muted
            };
            let label = format!("{} MCP", connected_count);
            Some((dot_color, label))
        } else {
            None
        };

        // Right: branding + date version
        let version_text = format!("{} {}", APP_SHORT_NAME, APP_VERSION_DATE);

        // Calculate padding
        let left_len = dir_text.len();
        let mid_len = mcp_text.as_ref().map(|(_, l)| l.len() + 4).unwrap_or(0);
        let right_len = version_text.len();
        let total_content = left_len + mid_len + right_len;
        let available = content_area.width as usize;

        if let Some((dot_color, ref label)) = mcp_text {
            let left_padding = if available > total_content {
                (available - total_content) / 2
            } else if available > right_len + left_len + 1 {
                1
            } else {
                0
            };
            spans.push(Span::raw(" ".repeat(left_padding)));
            spans.push(Span::styled(
                "⊙ ".to_string(),
                Style::default().fg(dot_color),
            ));
            spans.push(Span::styled(
                label.clone(),
                Style::default().fg(theme.text_muted),
            ));
        }

        // Right-align version
        let used: usize = spans.iter().map(|s| s.content.len()).sum();
        let right_padding = available.saturating_sub(used + right_len);
        spans.push(Span::raw(" ".repeat(right_padding)));
        spans.push(Span::styled(
            version_text,
            Style::default().fg(theme.text_muted),
        ));

        let line = Line::from(spans);
        let paragraph = Paragraph::new(line);
        frame.render_widget(paragraph, content_area);
    }

    fn should_show_tips(&self) -> bool {
        let is_first_time_user = self.context.session.read().sessions.is_empty();
        let tips_hidden = *self.context.tips_hidden.read();
        !is_first_time_user && !tips_hidden
    }
}

fn parse_tip_highlights(tip: &str, theme: &crate::theme::Theme) -> Vec<Span<'static>> {
    let mut spans = Vec::new();
    let mut remaining = tip;
    const START: &str = "{highlight}";
    const END: &str = "{/highlight}";

    loop {
        let Some(start_idx) = remaining.find(START) else {
            if !remaining.is_empty() {
                spans.push(Span::styled(
                    remaining.to_string(),
                    Style::default().fg(theme.text_muted),
                ));
            }
            break;
        };

        let (plain, after_start_marker) = remaining.split_at(start_idx);
        if !plain.is_empty() {
            spans.push(Span::styled(
                plain.to_string(),
                Style::default().fg(theme.text_muted),
            ));
        }

        let highlighted_tail = &after_start_marker[START.len()..];
        let Some(end_idx) = highlighted_tail.find(END) else {
            spans.push(Span::styled(
                START.to_string(),
                Style::default().fg(theme.text_muted),
            ));
            if !highlighted_tail.is_empty() {
                spans.push(Span::styled(
                    highlighted_tail.to_string(),
                    Style::default().fg(theme.text_muted),
                ));
            }
            break;
        };

        let (highlighted, after_end_marker) = highlighted_tail.split_at(end_idx);
        if !highlighted.is_empty() {
            spans.push(Span::styled(
                highlighted.to_string(),
                Style::default().fg(theme.text),
            ));
        }
        remaining = &after_end_marker[END.len()..];
    }

    spans
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context::{McpServerStatus, Session};
    use crate::theme::Theme;
    use once_cell::sync::Lazy;
    use ratatui::{backend::TestBackend, Terminal};
    use std::sync::Mutex;

    static ENV_LOCK: Lazy<Mutex<()>> = Lazy::new(|| Mutex::new(()));

    /// OPENCODE_STATE_DIR を一時ディレクトリへ切り替えて AppContext を構築する。
    /// prompt.rs のテストと同じ方式で、履歴・frecency・stash の実状態との接触を避ける。
    /// ネットワークアクセスや外部サービスは行わない。
    fn with_isolated_context<T>(f: impl FnOnce(Arc<AppContext>) -> T) -> T {
        let _guard = ENV_LOCK.lock().expect("lock env");
        let state_dir =
            std::env::temp_dir().join(format!("opencode-tui-home-{}", uuid::Uuid::new_v4()));
        let previous = std::env::var("OPENCODE_STATE_DIR").ok();
        std::env::set_var("OPENCODE_STATE_DIR", &state_dir);

        let context = Arc::new(AppContext::new());
        let result = f(context);

        if let Some(prev) = previous {
            std::env::set_var("OPENCODE_STATE_DIR", prev);
        } else {
            std::env::remove_var("OPENCODE_STATE_DIR");
        }
        let _ = std::fs::remove_dir_all(&state_dir);
        result
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

    #[test]
    fn new_stores_the_given_context() {
        with_isolated_context(|context| {
            let expected = Arc::clone(&context);
            let view = HomeView::new(context);

            assert!(Arc::ptr_eq(&view.context, &expected));
        });
    }

    #[test]
    fn render_draws_tagline_prompt_placeholder_and_branding() {
        with_isolated_context(|context| {
            let view = HomeView::new(context);
            let text = render_to_string(100, 30, |frame| {
                view.render(frame, Rect::new(0, 0, 100, 30));
            });

            assert!(text.contains(APP_TAGLINE), "tagline should be drawn");
            assert!(
                text.contains("Ask anything..."),
                "default placeholder missing"
            );
            assert!(
                text.contains("Fix a TODO in the codebase"),
                "prompt example missing"
            );
            assert!(text.contains(APP_SHORT_NAME), "short name missing");
            assert!(text.contains(APP_VERSION_DATE), "version date missing");
            // セッションが空（新規ユーザー）のときは Tips を出さない。
            assert!(!text.contains("● Tip"), "tips should be hidden initially");
        });
    }

    #[test]
    fn render_applies_outer_padding() {
        with_isolated_context(|context| {
            let view = HomeView::new(context);
            let area = Rect::new(10, 5, 100, 30);
            let text = render_to_string(120, 40, |frame| {
                view.render(frame, area);
            });
            let rows: Vec<&str> = text.lines().collect();

            assert_eq!(rows.len(), 40);
            assert!(
                rows[..5].iter().all(|row| row.trim().is_empty()),
                "rows above the area must stay blank"
            );
            assert!(
                rows.iter()
                    .all(|row| row.chars().take(10).all(|c| c == ' ')),
                "columns left of the area must stay blank"
            );
            assert!(text.contains(APP_TAGLINE));
        });
    }

    #[test]
    fn render_with_prompt_uses_the_supplied_prompt() {
        with_isolated_context(|context| {
            let view = HomeView::new(Arc::clone(&context));
            const CUSTOM_PLACEHOLDER: &str = "Custom home prompt >";
            let prompt = Prompt::new(Arc::clone(&context)).with_placeholder(CUSTOM_PLACEHOLDER);

            let text = render_to_string(100, 30, |frame| {
                view.render_with_prompt(frame, Rect::new(0, 0, 100, 30), &prompt);
            });

            assert!(text.contains(CUSTOM_PLACEHOLDER), "custom prompt missing");
            assert!(
                !text.contains("Ask anything..."),
                "default placeholder leaked"
            );
            assert!(text.contains(APP_TAGLINE), "home chrome missing");
        });
    }

    #[test]
    fn render_with_prompt_is_noop_for_zero_sized_inner_area() {
        with_isolated_context(|context| {
            let view = HomeView::new(Arc::clone(&context));
            let prompt = Prompt::new(context);
            for area in [
                Rect::new(0, 0, 0, 30),
                Rect::new(0, 0, 100, 0),
                Rect::new(0, 0, 4, 30),
                Rect::new(0, 0, 100, 2),
            ] {
                let text = render_to_string(100, 30, |frame| {
                    view.render_with_prompt(frame, area, &prompt);
                });
                assert!(
                    text.chars().all(|c| c == ' ' || c == '\n'),
                    "expected blank buffer for {area:?}"
                );
            }
        });
    }

    #[test]
    fn tips_render_only_for_returning_user_when_enabled() {
        with_isolated_context(|context| {
            {
                let mut session = context.session.write();
                let now = chrono::Utc::now();
                session.sessions.insert(
                    "seed-session".to_string(),
                    Session {
                        id: "seed-session".to_string(),
                        title: "seed".to_string(),
                        created_at: now,
                        updated_at: now,
                        parent_id: None,
                        share: None,
                    },
                );
            }
            let view = HomeView::new(Arc::clone(&context));

            *context.tips_hidden.write() = true;
            let hidden = render_to_string(100, 30, |frame| {
                view.render(frame, Rect::new(0, 0, 100, 30));
            });
            assert!(!hidden.contains("● Tip"), "tips must respect tips_hidden");

            *context.tips_hidden.write() = false;
            let shown = render_to_string(100, 30, |frame| {
                view.render(frame, Rect::new(0, 0, 100, 30));
            });
            assert!(
                shown.contains("● Tip"),
                "tips should render for returning user"
            );
        });
    }

    #[test]
    fn footer_reports_connected_mcp_count() {
        with_isolated_context(|context| {
            context.mcp_servers.write().push(McpServerStatus {
                name: "demo".to_string(),
                status: McpConnectionStatus::Connected,
                error: None,
            });
            let view = HomeView::new(context);

            let text = render_to_string(100, 30, |frame| {
                view.render(frame, Rect::new(0, 0, 100, 30));
            });

            assert!(text.contains("⊙"), "MCP status dot missing");
            assert!(text.contains("1 MCP"), "connected count missing");
        });
    }

    #[test]
    fn parse_tip_highlights_splits_marked_segments() {
        let theme = Theme::dark();
        let spans = parse_tip_highlights("Press {highlight}Tab{/highlight} now", &theme);
        let texts: Vec<&str> = spans.iter().map(|span| span.content.as_ref()).collect();

        assert_eq!(texts, vec!["Press ", "Tab", " now"]);
    }

    #[test]
    fn parse_tip_highlights_handles_plain_and_unclosed_markers() {
        let theme = Theme::dark();
        let plain = parse_tip_highlights("plain tip", &theme);
        let plain_texts: Vec<&str> = plain.iter().map(|span| span.content.as_ref()).collect();
        assert_eq!(plain_texts, vec!["plain tip"]);

        let unclosed = parse_tip_highlights("{highlight}Tab", &theme);
        let unclosed_texts: Vec<&str> = unclosed.iter().map(|span| span.content.as_ref()).collect();
        assert_eq!(unclosed_texts, vec!["{highlight}", "Tab"]);
    }
}

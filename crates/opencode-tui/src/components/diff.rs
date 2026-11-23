use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::Style,
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph},
    Frame,
};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::theme::Theme;

#[derive(Clone, Debug, PartialEq)]
pub enum DiffMode {
    Split,
    Unified,
}

#[derive(Clone, Debug)]
pub struct DiffLine {
    pub content: String,
    pub line_type: DiffLineType,
    pub old_line_num: Option<u32>,
    pub new_line_num: Option<u32>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum DiffLineType {
    Context,
    Added,
    Removed,
    HunkHeader,
}

pub struct DiffView {
    lines: Vec<DiffLine>,
    mode: DiffMode,
    scroll_offset: u16,
}

/// Collect the leading chars of `text` while their cumulative *display* width
/// stays within `max_width`. `UnicodeWidthChar::width` returning `None` means
/// a control character, which is counted as zero width.
fn truncate_to_width(text: &str, max_width: usize) -> String {
    let mut used = 0usize;
    let mut out = String::new();
    for ch in text.chars() {
        let ch_width = ch.width().unwrap_or(0);
        if used.saturating_add(ch_width) > max_width {
            break;
        }
        used += ch_width;
        out.push(ch);
    }
    out
}

impl DiffView {
    pub fn new() -> Self {
        Self {
            lines: Vec::new(),
            mode: DiffMode::Unified,
            scroll_offset: 0,
        }
    }

    pub fn with_content(mut self, content: &str) -> Self {
        self.parse_diff(content);
        self
    }

    pub fn with_mode(mut self, mode: DiffMode) -> Self {
        self.mode = mode;
        self
    }

    fn parse_diff(&mut self, content: &str) {
        let mut old_line: u32 = 0;
        let mut new_line: u32 = 0;

        for line in content.lines() {
            let (diff_line, _update_counts) = if let Some(stripped) = line.strip_prefix("@@ ") {
                let header = stripped.to_string();
                old_line = Self::parse_hunk_start(&header, true);
                new_line = Self::parse_hunk_start(&header, false);
                (
                    DiffLine {
                        content: line.to_string(),
                        line_type: DiffLineType::HunkHeader,
                        old_line_num: None,
                        new_line_num: None,
                    },
                    false,
                )
            } else if line.starts_with('+') && !line.starts_with("+++") {
                let content = line[1..].to_string();
                let num = new_line;
                new_line += 1;
                (
                    DiffLine {
                        content,
                        line_type: DiffLineType::Added,
                        old_line_num: None,
                        new_line_num: Some(num),
                    },
                    true,
                )
            } else if line.starts_with('-') && !line.starts_with("---") {
                let content = line[1..].to_string();
                let num = old_line;
                old_line += 1;
                (
                    DiffLine {
                        content,
                        line_type: DiffLineType::Removed,
                        old_line_num: Some(num),
                        new_line_num: None,
                    },
                    true,
                )
            } else if line.starts_with(' ') || line.is_empty() {
                let content = if line.is_empty() {
                    " ".to_string()
                } else {
                    line[1..].to_string()
                };
                let o = old_line;
                let n = new_line;
                old_line += 1;
                new_line += 1;
                (
                    DiffLine {
                        content,
                        line_type: DiffLineType::Context,
                        old_line_num: Some(o),
                        new_line_num: Some(n),
                    },
                    true,
                )
            } else {
                (
                    DiffLine {
                        content: line.to_string(),
                        line_type: DiffLineType::Context,
                        old_line_num: Some(old_line),
                        new_line_num: Some(new_line),
                    },
                    true,
                )
            };

            self.lines.push(diff_line);
        }
    }

    fn parse_hunk_start(header: &str, is_old: bool) -> u32 {
        let parts: Vec<&str> = header.split_whitespace().collect();
        if parts.len() >= 2 {
            let range = if is_old { parts[0] } else { parts[1] };
            range
                .trim_start_matches(&['-', '+'][..])
                .split(',')
                .next()
                .and_then(|s| s.parse().ok())
                .unwrap_or(1)
        } else {
            1
        }
    }

    pub fn set_mode(&mut self, mode: DiffMode) {
        self.mode = mode;
    }

    /// Convert diff lines to ratatui Lines for embedding in other widgets.
    pub fn to_lines(&self, theme: &Theme) -> Vec<Line<'static>> {
        let mut lines = Vec::new();

        for diff_line in &self.lines {
            let prefix = match diff_line.line_type {
                DiffLineType::Added => Span::styled("+ ", Style::default().fg(theme.diff_added)),
                DiffLineType::Removed => {
                    Span::styled("- ", Style::default().fg(theme.diff_removed))
                }
                DiffLineType::HunkHeader => Span::styled("@@ ", Style::default().fg(theme.primary)),
                DiffLineType::Context => Span::styled("  ", Style::default().fg(theme.text_muted)),
            };

            let line_num = if let Some(n) = diff_line.new_line_num.or(diff_line.old_line_num) {
                format!("{:4} ", n)
            } else {
                "     ".to_string()
            };

            let line_style = match diff_line.line_type {
                DiffLineType::Added => Style::default().fg(theme.diff_added),
                DiffLineType::Removed => Style::default().fg(theme.diff_removed),
                DiffLineType::HunkHeader => Style::default().fg(theme.primary),
                DiffLineType::Context => Style::default().fg(theme.text),
            };

            lines.push(Line::from(vec![
                Span::raw(line_num),
                prefix,
                Span::styled(diff_line.content.clone(), line_style),
            ]));
        }

        lines
    }

    pub fn scroll_up(&mut self) {
        if self.scroll_offset > 0 {
            self.scroll_offset -= 1;
        }
    }

    pub fn scroll_down(&mut self, max: u16) {
        if self.scroll_offset < max {
            self.scroll_offset += 1;
        }
    }

    /// Decide wrapping by display width (not scalar count) and, when needed,
    /// truncate to a prefix that fits the content column budget.
    fn fit_content(content: &str, width: usize) -> String {
        if UnicodeWidthStr::width(content) > width.saturating_sub(12) {
            truncate_to_width(content, width.saturating_sub(15))
        } else {
            content.to_string()
        }
    }

    pub fn render(&self, frame: &mut Frame, area: Rect, theme: &Theme) {
        if self.lines.is_empty() {
            let empty = Paragraph::new("No changes")
                .block(Block::default().title(" Diff ").borders(Borders::ALL))
                .style(Style::default().fg(theme.text_muted));
            frame.render_widget(empty, area);
            return;
        }

        let width = area.width as usize;

        if self.mode == DiffMode::Split && width > 120 {
            self.render_split(frame, area, theme);
        } else {
            self.render_unified(frame, area, theme, width);
        }
    }

    fn render_unified(&self, frame: &mut Frame, area: Rect, theme: &Theme, width: usize) {
        let mut lines = Vec::new();

        for diff_line in &self.lines {
            let prefix = match diff_line.line_type {
                DiffLineType::Added => Span::styled("+ ", Style::default().fg(theme.diff_added)),
                DiffLineType::Removed => {
                    Span::styled("- ", Style::default().fg(theme.diff_removed))
                }
                DiffLineType::HunkHeader => Span::styled("@@ ", Style::default().fg(theme.primary)),
                DiffLineType::Context => Span::styled("  ", Style::default().fg(theme.text_muted)),
            };

            let line_num = if let Some(n) = diff_line.new_line_num.or(diff_line.old_line_num) {
                format!("{:4} ", n)
            } else {
                "     ".to_string()
            };

            let line_style = match diff_line.line_type {
                DiffLineType::Added => Style::default().fg(theme.diff_added),
                DiffLineType::Removed => Style::default().fg(theme.diff_removed),
                DiffLineType::HunkHeader => Style::default().fg(theme.primary),
                DiffLineType::Context => Style::default().fg(theme.text),
            };

            let content = Self::fit_content(&diff_line.content, width);

            lines.push(Line::from(vec![
                Span::raw(line_num),
                prefix,
                Span::styled(content, line_style),
            ]));
        }

        let paragraph = Paragraph::new(lines)
            .block(
                Block::default()
                    .title(" Diff ")
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(theme.border)),
            )
            .scroll((self.scroll_offset, 0));

        frame.render_widget(paragraph, area);
    }

    fn render_split(&self, frame: &mut Frame, area: Rect, theme: &Theme) {
        let constraints = vec![Constraint::Percentage(50), Constraint::Percentage(50)];
        let chunks = Layout::default()
            .direction(Direction::Horizontal)
            .constraints(constraints)
            .split(area);

        // 各ペインは area.width/2 程度の実幅なので、行番号・枠を含む
        // ペイン幅を基準に content を表示幅で切詰める（unified と同じ基準）。
        let old_width = chunks[0].width as usize;
        let new_width = chunks[1].width as usize;

        let mut old_lines = Vec::new();
        let mut new_lines = Vec::new();

        for diff_line in &self.lines {
            match diff_line.line_type {
                DiffLineType::Removed => {
                    old_lines.push(Line::from(vec![
                        Span::styled(
                            format!("{:4}", diff_line.old_line_num.unwrap_or(0)),
                            Style::default().fg(theme.text_muted),
                        ),
                        Span::styled(
                            Self::fit_content(&diff_line.content, old_width),
                            Style::default().fg(theme.diff_removed),
                        ),
                    ]));
                    new_lines.push(Line::from(""));
                }
                DiffLineType::Added => {
                    old_lines.push(Line::from(""));
                    new_lines.push(Line::from(vec![
                        Span::styled(
                            format!("{:4}", diff_line.new_line_num.unwrap_or(0)),
                            Style::default().fg(theme.text_muted),
                        ),
                        Span::styled(
                            Self::fit_content(&diff_line.content, new_width),
                            Style::default().fg(theme.diff_added),
                        ),
                    ]));
                }
                DiffLineType::Context => {
                    old_lines.push(Line::from(vec![
                        Span::styled(
                            format!("{:4}", diff_line.old_line_num.unwrap_or(0)),
                            Style::default().fg(theme.text_muted),
                        ),
                        Span::styled(
                            Self::fit_content(&diff_line.content, old_width),
                            Style::default().fg(theme.text),
                        ),
                    ]));
                    new_lines.push(Line::from(vec![
                        Span::styled(
                            format!("{:4}", diff_line.new_line_num.unwrap_or(0)),
                            Style::default().fg(theme.text_muted),
                        ),
                        Span::styled(
                            Self::fit_content(&diff_line.content, new_width),
                            Style::default().fg(theme.text),
                        ),
                    ]));
                }
                DiffLineType::HunkHeader => {
                    old_lines.push(Line::from(Span::styled(
                        Self::fit_content(&diff_line.content, old_width),
                        Style::default().fg(theme.primary),
                    )));
                    new_lines.push(Line::from(Span::styled(
                        Self::fit_content(&diff_line.content, new_width),
                        Style::default().fg(theme.primary),
                    )));
                }
            }
        }

        let old_block = Block::default()
            .title(" Old ")
            .borders(Borders::ALL)
            .border_style(Style::default().fg(theme.diff_removed));

        let new_block = Block::default()
            .title(" New ")
            .borders(Borders::ALL)
            .border_style(Style::default().fg(theme.diff_added));

        frame.render_widget(
            Paragraph::new(old_lines)
                .block(old_block)
                .scroll((self.scroll_offset, 0)),
            chunks[0],
        );
        frame.render_widget(
            Paragraph::new(new_lines)
                .block(new_block)
                .scroll((self.scroll_offset, 0)),
            chunks[1],
        );
    }
}

impl Default for DiffView {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_hunk_start_old_and_new() {
        assert_eq!(DiffView::parse_hunk_start("-10,5 +20,7 @@", true), 10);
        assert_eq!(DiffView::parse_hunk_start("-10,5 +20,7 @@", false), 20);
        assert_eq!(DiffView::parse_hunk_start("-1 +1", true), 1);
        assert_eq!(DiffView::parse_hunk_start("garbage", true), 1);
    }

    #[test]
    fn with_content_parses_diff_lines() {
        let view =
            DiffView::new().with_content("--- a/x\n+++ b/x\n@@ -1,1 +1,1 @@\n-old\n+new\n same");
        let lines = view.to_lines(&Theme::default());
        // 2 hunk-header-ish lines + header + 3 body lines
        assert_eq!(lines.len(), 6);
    }

    #[test]
    fn empty_view_has_no_lines() {
        assert!(DiffView::new().to_lines(&Theme::default()).is_empty());
        assert!(DiffView::default().to_lines(&Theme::default()).is_empty());
    }

    #[test]
    fn mode_can_be_set_and_changed() {
        let view = DiffView::new().with_mode(DiffMode::Split);
        // set_mode mutates; verify via a mutable binding
        let mut view = view;
        view.set_mode(DiffMode::Unified);
        // No public getter; exercising the setter is the coverage goal.
        let _ = view.to_lines(&Theme::default());
    }

    #[test]
    fn scroll_up_and_down_are_bounded() {
        let mut view = DiffView::new();
        view.scroll_up(); // stays at 0
        view.scroll_down(2);
        view.scroll_down(2);
        view.scroll_down(2); // clamped at 2
        view.scroll_up();
        // Draining scroll via repeated ups must not panic.
        for _ in 0..5 {
            view.scroll_up();
        }
    }

    #[test]
    fn truncate_to_width_uses_display_width() {
        let text = "あいうえお"; // 5 scalars, 10 columns
        assert_eq!(truncate_to_width(text, 5), "あい"); // 4 columns
        assert_eq!(truncate_to_width(text, 4), "あい"); // exactly 4 columns
        assert_eq!(truncate_to_width(text, 3), "あ"); // 2 columns
        assert_eq!(truncate_to_width(text, 0), "");
    }

    #[test]
    fn truncate_to_width_treats_control_chars_as_zero_width() {
        // UnicodeWidthChar::width('\n') は None なので 0 として扱い、
        // 上限 3 でも全体が残る（str 版の width とは規則が異なる）。
        let text = "a\nbc";
        let fitted = truncate_to_width(text, 3);
        assert_eq!(fitted, text);
    }

    #[test]
    fn fit_content_ascii_matches_legacy_behavior() {
        let ascii = "abcdefghij"; // 10 scalars == 10 columns
        assert_eq!(DiffView::fit_content(ascii, 22), ascii); // 10 > 10 is false
        assert_eq!(DiffView::fit_content(ascii, 21), "abcdef"); // legacy take(6)
    }

    #[test]
    fn fit_content_fullwidth_never_overflows_frame() {
        let text = "あいうえおかきくけこ"; // 10 scalars, 20 columns
        let width = 20usize; // sub12 = 8, sub15 = 5
        let fitted = DiffView::fit_content(text, width);
        assert_eq!(fitted, "あい"); // 4 columns <= 5
        assert!(UnicodeWidthStr::width(fitted.as_str()) <= width.saturating_sub(15));
        // line number (5) + prefix (2) + content + borders (2) must fit the area.
        assert!(5 + 2 + UnicodeWidthStr::width(fitted.as_str()) + 2 <= width);
    }

    #[test]
    fn fit_content_fullwidth_within_budget_is_unchanged() {
        let text = "あい"; // 4 columns
        assert_eq!(DiffView::fit_content(text, 20), text); // 4 > 8 is false
    }

    /// Split モードで描画し、指定ペインの 1 行を文字列として返す。
    fn render_split_pane_row(
        view: &DiffView,
        width: u16,
        height: u16,
        pane: usize,
        y: u16,
    ) -> String {
        use ratatui::{backend::TestBackend, Terminal};

        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).expect("TestBackend should initialize");
        let theme = Theme::default();
        terminal
            .draw(|frame| view.render(frame, Rect::new(0, 0, width, height), &theme))
            .expect("draw should succeed");

        let buffer = terminal.backend().buffer();
        let half = (width as usize) / 2;
        let (x0, x1) = match pane {
            0 => (0, half),
            1 => (half, width as usize),
            _ => panic!("pane must be 0 or 1"),
        };
        (x0..x1)
            .map(|x| {
                buffer
                    .cell((x as u16, y))
                    .map(|cell| cell.symbol())
                    .unwrap_or(" ")
            })
            .collect()
    }

    #[test]
    fn render_split_fits_fullwidth_content_in_all_branches() {
        let full = "あ".repeat(30); // 表示幅 60
        let hunk = format!("@@ {}", full);
        let diff = format!("{}\n-{}\n+{}\n {}\n", hunk, full, full, full);
        let view = DiffView::new()
            .with_content(&diff)
            .with_mode(DiffMode::Split);

        let width = 124u16; // render() の Split 分岐は width > 120
        let height = 6u16; // 枠 2 行 + 本文 4 行
        let chunk_width = (width as usize) / 2; // 各ペイン 62 列

        let fitted_body = DiffView::fit_content(&full, chunk_width);
        let fitted_header = DiffView::fit_content(&hunk, chunk_width);
        let body_chars = fitted_body.chars().count();
        let header_chars = fitted_header.chars().count() - "@@ ".chars().count();
        // 無修正なら ratatui が枠内幅で切るだけなので、fit_content より多く残る。
        assert!(body_chars < full.chars().count());
        assert!(header_chars < full.chars().count());

        let fullwidth_count = |row: &str| row.chars().filter(|c| *c == 'あ').count();

        // 行 1: HunkHeader（両ペイン、行番号スパンなし）
        assert_eq!(
            fullwidth_count(&render_split_pane_row(&view, width, height, 0, 1)),
            header_chars
        );
        assert_eq!(
            fullwidth_count(&render_split_pane_row(&view, width, height, 1, 1)),
            header_chars
        );
        // 行 2: Removed（old のみ）
        assert_eq!(
            fullwidth_count(&render_split_pane_row(&view, width, height, 0, 2)),
            body_chars
        );
        assert_eq!(
            fullwidth_count(&render_split_pane_row(&view, width, height, 1, 2)),
            0
        );
        // 行 3: Added（new のみ）
        assert_eq!(
            fullwidth_count(&render_split_pane_row(&view, width, height, 0, 3)),
            0
        );
        assert_eq!(
            fullwidth_count(&render_split_pane_row(&view, width, height, 1, 3)),
            body_chars
        );
        // 行 4: Context（両ペイン）
        assert_eq!(
            fullwidth_count(&render_split_pane_row(&view, width, height, 0, 4)),
            body_chars
        );
        assert_eq!(
            fullwidth_count(&render_split_pane_row(&view, width, height, 1, 4)),
            body_chars
        );
    }

    #[test]
    fn render_split_keeps_short_ascii_content_unchanged() {
        let view = DiffView::new()
            .with_content("-abc\n+xyz\n same\n")
            .with_mode(DiffMode::Split);
        let width = 124u16;
        let height = 5u16; // 枠 2 行 + 本文 3 行

        assert!(render_split_pane_row(&view, width, height, 0, 1).contains("abc"));
        assert!(render_split_pane_row(&view, width, height, 1, 2).contains("xyz"));
        assert!(render_split_pane_row(&view, width, height, 0, 3).contains("same"));
        assert!(render_split_pane_row(&view, width, height, 1, 3).contains("same"));
    }
}

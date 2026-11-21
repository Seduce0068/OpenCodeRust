use pulldown_cmark::{Alignment, CodeBlockKind, Event, HeadingLevel, Options, Parser, Tag, TagEnd};
use ratatui::{
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Paragraph, Wrap},
    Frame,
};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use super::code_block::{CodeBlock, CodeTheme};
use crate::theme::Theme;

pub struct MarkdownRenderer {
    theme: Theme,
    code_theme: CodeTheme,
    concealed: bool,
}

#[derive(Clone, Copy)]
struct ListState {
    ordered: bool,
    next: u64,
}

struct TableRow {
    is_header: bool,
    cells: Vec<Vec<Span<'static>>>,
}

struct TableBuilder {
    alignments: Vec<Alignment>,
    rows: Vec<TableRow>,
    current_row: Vec<Vec<Span<'static>>>,
    in_header: bool,
}

impl TableBuilder {
    fn new(alignments: Vec<Alignment>) -> Self {
        Self {
            alignments,
            rows: Vec::new(),
            current_row: Vec::new(),
            in_header: false,
        }
    }

    fn set_header(&mut self, in_header: bool) {
        self.in_header = in_header;
    }

    fn push_cell(&mut self, cell: Vec<Span<'static>>) {
        self.current_row.push(cell);
    }

    fn finish_row(&mut self) {
        if self.current_row.is_empty() {
            return;
        }
        self.rows.push(TableRow {
            is_header: self.in_header,
            cells: std::mem::take(&mut self.current_row),
        });
    }

    /// 利用可能幅に収まるように列幅を決定して表を描画する。
    ///
    /// 枠線 (左右 2 + 列間 `col_count - 1` + 各列のパディング `2 * col_count`) の分を
    /// 差し引いた幅がセル本文の上限。超過時は元の幅に比例してクランプし、
    /// 1 列 1 文字でも確保できない場合はコードブロック風の平坦表示へフォールバックする。
    fn render(mut self, lines: &mut Vec<Line<'static>>, theme: &Theme, available_width: usize) {
        self.finish_row();
        if self.rows.is_empty() {
            return;
        }

        let col_count = self
            .rows
            .iter()
            .map(|row| row.cells.len())
            .max()
            .unwrap_or(0)
            .max(self.alignments.len());
        if col_count == 0 {
            return;
        }

        let mut widths = vec![1usize; col_count];
        for row in &self.rows {
            for (idx, cell) in row.cells.iter().enumerate() {
                if idx < widths.len() {
                    widths[idx] = widths[idx].max(cell_span_width(cell));
                }
            }
        }

        let frame_overhead = col_count.saturating_mul(3).saturating_add(1);
        let max_content = available_width.saturating_sub(frame_overhead);
        if max_content < col_count {
            self.render_flat(lines, theme, available_width);
            return;
        }
        let widths = clamp_column_widths(&widths, max_content);

        lines.push(table_border_line(
            '┌',
            '┬',
            '┐',
            &widths,
            theme.markdown_horizontal_rule,
        ));

        for (idx, row) in self.rows.iter().enumerate() {
            lines.push(table_row_line(row, &widths, &self.alignments, theme));

            let is_header_break =
                row.is_header && self.rows.get(idx + 1).is_some_and(|next| !next.is_header);
            if is_header_break {
                lines.push(table_border_line(
                    '├',
                    '┼',
                    '┤',
                    &widths,
                    theme.markdown_horizontal_rule,
                ));
            }
        }

        lines.push(table_border_line(
            '└',
            '┴',
            '┘',
            &widths,
            theme.markdown_horizontal_rule,
        ));
    }

    /// 枠線を描けないほど狭い端末向けの平坦表示。
    /// コードブロックと同じ `│ ` のガターを付け、セルは ` │ ` で区切る。
    /// `available_width` を守るため、各セルは残り幅へ切り詰める。
    fn render_flat(self, lines: &mut Vec<Line<'static>>, theme: &Theme, available_width: usize) {
        const GUTTER_WIDTH: usize = 2; // "│ "
        const SEPARATOR_WIDTH: usize = 3; // " │ "

        let border_style = Style::default().fg(theme.markdown_horizontal_rule);
        for row in self.rows {
            let cell_count = row.cells.len();
            let overhead = GUTTER_WIDTH
                .saturating_add(SEPARATOR_WIDTH.saturating_mul(cell_count.saturating_sub(1)));
            let budget = available_width.saturating_sub(overhead);
            let original_widths: Vec<usize> =
                row.cells.iter().map(|cell| cell_span_width(cell)).collect();
            let widths = distribute_flat_widths(&original_widths, budget);

            let mut spans = vec![Span::styled("│ ", border_style)];
            for (idx, (cell, width)) in row.cells.into_iter().zip(widths).enumerate() {
                if idx > 0 {
                    spans.push(Span::styled(" │ ", border_style));
                }
                spans.extend(truncate_spans_to_width(cell, width));
            }
            // ガターや区切り文字自体が幅を食い潰す極端に狭い場合も行幅を守る。
            spans = truncate_spans_to_width(spans, available_width);
            lines.push(Line::from(spans));
        }
    }
}

/// 平坦表示のセル本文に割り当てる幅を、元の幅に比例して配分する。
/// 各列は 0 幅を許し、合計が `max_total` を超えないようにする。
fn distribute_flat_widths(original: &[usize], max_total: usize) -> Vec<usize> {
    let total: usize = original
        .iter()
        .fold(0usize, |acc, &w| acc.saturating_add(w));
    if total <= max_total {
        return original.to_vec();
    }
    if max_total == 0 {
        return vec![0; original.len()];
    }

    let total_u128 = total as u128;
    let max_u128 = max_total as u128;
    let mut widths: Vec<usize> = original
        .iter()
        .map(|&w| (w as u128 * max_u128 / total_u128) as usize)
        .collect();
    let mut sum: usize = widths.iter().fold(0usize, |acc, &w| acc.saturating_add(w));

    // 切り捨てで余った分を、元の幅の剰余が大きい列から順に配る。
    let mut order: Vec<usize> = (0..original.len()).collect();
    order.sort_by_key(|&idx| std::cmp::Reverse((original[idx] as u128 * max_u128) % total_u128));
    let mut cursor = 0usize;
    while sum < max_total && !order.is_empty() {
        widths[order[cursor % order.len()]] += 1;
        sum += 1;
        cursor += 1;
    }

    debug_assert_eq!(widths.iter().sum::<usize>(), max_total);
    widths
}

/// セル本文の幅合計が `max_total` を超えないよう、元の幅に比例して列幅を配分する。
/// 各列は最低 1 文字を確保し、端数は元の幅の剰余が大きい列から順に配る。
fn clamp_column_widths(original: &[usize], max_total: usize) -> Vec<usize> {
    let count = original.len();
    if count == 0 {
        return Vec::new();
    }
    let total = original
        .iter()
        .fold(0usize, |acc, &width| acc.saturating_add(width));
    if total <= max_total {
        return original.to_vec();
    }

    let total_u128 = total as u128;
    let max_u128 = max_total as u128;
    let mut widths: Vec<usize> = original
        .iter()
        .map(|&width| {
            let scaled = (width as u128 * max_u128 / total_u128) as usize;
            scaled.max(1)
        })
        .collect();

    // 切り捨ての逆で上限を超えた分を、幅の大きい列から削る。
    let mut sum: usize = widths.iter().fold(0usize, |acc, &w| acc.saturating_add(w));
    while sum > max_total {
        let target = widths
            .iter()
            .enumerate()
            .filter(|(_, &w)| w > 1)
            .max_by_key(|(_, &w)| w)
            .map(|(idx, _)| idx);
        match target {
            Some(idx) => {
                widths[idx] -= 1;
                sum -= 1;
            }
            None => break,
        }
    }

    let mut order: Vec<usize> = (0..count).collect();
    order.sort_by_key(|&idx| std::cmp::Reverse((original[idx] as u128 * max_u128) % total_u128));
    let mut remaining = max_total.saturating_sub(sum);
    let mut cursor = 0usize;
    while remaining > 0 {
        widths[order[cursor % count]] += 1;
        remaining -= 1;
        cursor += 1;
    }

    debug_assert_eq!(widths.iter().sum::<usize>(), max_total);
    widths
}

impl MarkdownRenderer {
    pub fn new(theme: Theme) -> Self {
        let code_theme = CodeTheme::from_app_theme(&theme);
        Self {
            theme,
            code_theme,
            concealed: false,
        }
    }

    pub fn with_concealed(mut self, concealed: bool) -> Self {
        self.concealed = concealed;
        self
    }

    pub fn render(&self, text: &str, frame: &mut Frame, area: Rect) {
        let lines = self.to_lines_with_width(text, usize::from(area.width));
        let paragraph = Paragraph::new(lines).wrap(Wrap { trim: false });
        frame.render_widget(paragraph, area);
    }

    /// 既存呼び出し互換のエントリポイント。利用可能幅を指定できないため表はクランプしない。
    pub fn to_lines(&self, text: &str) -> Vec<Line<'static>> {
        self.to_lines_with_width(text, usize::MAX)
    }

    /// 利用可能幅を考虑到して Markdown を行列へ変換する。
    /// 表は `available_width` に収まるよう列幅がクランプされる。
    pub fn to_lines_with_width(&self, text: &str, available_width: usize) -> Vec<Line<'static>> {
        let mut options = Options::empty();
        options.insert(Options::ENABLE_STRIKETHROUGH);
        options.insert(Options::ENABLE_TABLES);
        options.insert(Options::ENABLE_TASKLISTS);
        options.insert(Options::ENABLE_FOOTNOTES);

        let parser = Parser::new_ext(text, options);

        let mut lines: Vec<Line<'static>> = Vec::new();
        let mut current: Vec<Span<'static>> = Vec::new();
        let mut styles = vec![Style::default().fg(self.theme.text)];
        let mut list_stack: Vec<ListState> = Vec::new();
        let mut blockquote_depth = 0usize;
        let mut link_urls: Vec<String> = Vec::new();
        let mut code_block: Option<(Option<String>, String)> = None;
        let mut table: Option<TableBuilder> = None;

        for event in parser {
            if let Some((_, code)) = code_block.as_mut() {
                match event {
                    Event::Text(text) | Event::Code(text) => {
                        code.push_str(&text);
                    }
                    Event::SoftBreak | Event::HardBreak => code.push('\n'),
                    Event::End(TagEnd::CodeBlock) => {
                        if let Some((language, content)) = code_block.take() {
                            self.render_code_block(language, &content, &mut lines);
                        }
                        push_blank_line(&mut lines);
                    }
                    _ => {}
                }
                continue;
            }

            match event {
                Event::Start(tag) => match tag {
                    Tag::Paragraph => {}
                    Tag::Heading { level, .. } => {
                        flush_line(&mut lines, &mut current);
                        let style = heading_style(level, &self.theme);
                        styles.push(style);
                        ensure_prefix(&mut current, blockquote_depth, &self.theme);
                        current.push(Span::styled(
                            format!("{} ", "#".repeat(heading_level_to_u8(level) as usize)),
                            style,
                        ));
                    }
                    Tag::BlockQuote(_) => {
                        flush_line(&mut lines, &mut current);
                        blockquote_depth += 1;
                    }
                    Tag::CodeBlock(kind) => {
                        flush_line(&mut lines, &mut current);
                        let language = match kind {
                            CodeBlockKind::Fenced(lang) => {
                                let trimmed = lang.trim();
                                if trimmed.is_empty() {
                                    None
                                } else {
                                    Some(trimmed.to_string())
                                }
                            }
                            CodeBlockKind::Indented => None,
                        };
                        code_block = Some((language, String::new()));
                    }
                    Tag::List(start) => {
                        flush_line(&mut lines, &mut current);
                        list_stack.push(ListState {
                            ordered: start.is_some(),
                            next: start.unwrap_or(1),
                        });
                    }
                    Tag::Item => {
                        flush_line(&mut lines, &mut current);
                        ensure_prefix(&mut current, blockquote_depth, &self.theme);
                        let (prefix, color) = if let Some(state) = list_stack.last_mut() {
                            if state.ordered {
                                let out = format!("{}. ", state.next);
                                state.next += 1;
                                (out, self.theme.markdown_list_enumeration)
                            } else {
                                ("• ".to_string(), self.theme.markdown_list_item)
                            }
                        } else {
                            ("• ".to_string(), self.theme.markdown_list_item)
                        };
                        current.push(Span::styled(prefix, Style::default().fg(color)));
                    }
                    Tag::Emphasis => {
                        let style = current_style(&styles)
                            .fg(self.theme.markdown_emph)
                            .add_modifier(Modifier::ITALIC);
                        styles.push(style);
                    }
                    Tag::Strong => {
                        let style = current_style(&styles)
                            .fg(self.theme.markdown_strong)
                            .add_modifier(Modifier::BOLD);
                        styles.push(style);
                    }
                    Tag::Strikethrough => {
                        let style = current_style(&styles).add_modifier(Modifier::CROSSED_OUT);
                        styles.push(style);
                    }
                    Tag::Link { dest_url, .. } => {
                        link_urls.push(dest_url.to_string());
                        let style = current_style(&styles)
                            .fg(self.theme.markdown_link_text)
                            .add_modifier(Modifier::UNDERLINED);
                        styles.push(style);
                    }
                    Tag::Image { dest_url, .. } => {
                        link_urls.push(dest_url.to_string());
                        let style = current_style(&styles).fg(self.theme.markdown_image);
                        styles.push(style);
                        if table.is_none() {
                            ensure_prefix(&mut current, blockquote_depth, &self.theme);
                        }
                        current.push(Span::styled("[image] ", style));
                    }
                    Tag::Table(alignments) => {
                        flush_line(&mut lines, &mut current);
                        table = Some(TableBuilder::new(alignments));
                    }
                    Tag::TableHead => {
                        if let Some(tbl) = table.as_mut() {
                            tbl.set_header(true);
                        } else {
                            flush_line(&mut lines, &mut current);
                        }
                    }
                    Tag::TableRow if table.is_none() => {
                        flush_line(&mut lines, &mut current);
                    }
                    Tag::TableCell => {
                        if table.is_some() {
                            current.clear();
                        } else {
                            ensure_prefix(&mut current, blockquote_depth, &self.theme);
                            if !current.is_empty() {
                                current.push(Span::styled(
                                    " │ ",
                                    Style::default().fg(self.theme.text_muted),
                                ));
                            }
                        }
                    }
                    _ => {}
                },
                Event::End(end) => match end {
                    TagEnd::Paragraph => {
                        flush_line(&mut lines, &mut current);
                        push_blank_line(&mut lines);
                    }
                    TagEnd::Heading(_) => {
                        if styles.len() > 1 {
                            styles.pop();
                        }
                        flush_line(&mut lines, &mut current);
                        push_blank_line(&mut lines);
                    }
                    TagEnd::BlockQuote(_) => {
                        flush_line(&mut lines, &mut current);
                        blockquote_depth = blockquote_depth.saturating_sub(1);
                        push_blank_line(&mut lines);
                    }
                    TagEnd::List(_) => {
                        flush_line(&mut lines, &mut current);
                        list_stack.pop();
                        push_blank_line(&mut lines);
                    }
                    TagEnd::Item => {
                        flush_line(&mut lines, &mut current);
                    }
                    TagEnd::Emphasis | TagEnd::Strong | TagEnd::Strikethrough
                        if styles.len() > 1 =>
                    {
                        styles.pop();
                    }
                    TagEnd::Link => {
                        if styles.len() > 1 {
                            styles.pop();
                        }
                        if let Some(url) = link_urls.pop() {
                            current.push(Span::styled(
                                format!(" ({url})"),
                                Style::default().fg(self.theme.markdown_link),
                            ));
                        }
                    }
                    TagEnd::Image => {
                        if styles.len() > 1 {
                            styles.pop();
                        }
                        if let Some(url) = link_urls.pop() {
                            current.push(Span::styled(
                                format!(" ({url})"),
                                Style::default().fg(self.theme.markdown_image_text),
                            ));
                        }
                    }
                    TagEnd::Table => {
                        if let Some(tbl) = table.take() {
                            tbl.render(&mut lines, &self.theme, available_width);
                        } else {
                            flush_line(&mut lines, &mut current);
                        }
                        push_blank_line(&mut lines);
                    }
                    TagEnd::TableHead => {
                        if let Some(tbl) = table.as_mut() {
                            tbl.finish_row();
                            tbl.set_header(false);
                        } else {
                            flush_line(&mut lines, &mut current);
                        }
                    }
                    TagEnd::TableRow => {
                        if let Some(tbl) = table.as_mut() {
                            tbl.finish_row();
                        } else {
                            flush_line(&mut lines, &mut current);
                        }
                    }
                    TagEnd::TableCell => {
                        if let Some(tbl) = table.as_mut() {
                            tbl.push_cell(std::mem::take(&mut current));
                        }
                    }
                    _ => {}
                },
                Event::Text(text) => {
                    if table.is_none() {
                        ensure_prefix(&mut current, blockquote_depth, &self.theme);
                    }
                    current.push(Span::styled(text.to_string(), current_style(&styles)));
                }
                Event::Code(text) => {
                    if table.is_none() {
                        ensure_prefix(&mut current, blockquote_depth, &self.theme);
                    }
                    current.push(Span::styled(
                        text.to_string(),
                        Style::default()
                            .fg(self.theme.markdown_code)
                            .bg(self.theme.markdown_code_bg),
                    ));
                }
                Event::SoftBreak => {
                    if table.is_some() {
                        current.push(Span::raw(" "));
                    } else {
                        flush_line(&mut lines, &mut current);
                    }
                }
                Event::HardBreak => {
                    if table.is_some() {
                        current.push(Span::raw(" "));
                    } else {
                        flush_line(&mut lines, &mut current);
                    }
                }
                Event::Rule => {
                    flush_line(&mut lines, &mut current);
                    lines.push(Line::from(Span::styled(
                        "─".repeat(40),
                        Style::default().fg(self.theme.markdown_horizontal_rule),
                    )));
                    push_blank_line(&mut lines);
                }
                Event::Html(html) | Event::InlineHtml(html) => {
                    if table.is_none() {
                        ensure_prefix(&mut current, blockquote_depth, &self.theme);
                    }
                    current.push(Span::styled(
                        html.to_string(),
                        Style::default().fg(self.theme.text_muted),
                    ));
                }
                Event::TaskListMarker(checked) => {
                    if table.is_none() {
                        ensure_prefix(&mut current, blockquote_depth, &self.theme);
                    }
                    let mark = if checked { "[x] " } else { "[ ] " };
                    current.push(Span::styled(
                        mark,
                        Style::default().fg(self.theme.markdown_list_item),
                    ));
                }
                Event::FootnoteReference(label) => {
                    if table.is_none() {
                        ensure_prefix(&mut current, blockquote_depth, &self.theme);
                    }
                    current.push(Span::styled(
                        format!("[^{label}]"),
                        Style::default().fg(self.theme.text_muted),
                    ));
                }
                _ => {}
            }
        }

        flush_line(&mut lines, &mut current);
        while lines.last().is_some_and(line_is_blank) {
            lines.pop();
        }
        lines
    }

    fn render_code_block(
        &self,
        language: Option<String>,
        code: &str,
        lines: &mut Vec<Line<'static>>,
    ) {
        if self.concealed {
            lines.push(Line::from(vec![
                Span::styled("▶ ", Style::default().fg(self.theme.primary)),
                Span::styled(
                    format!(
                        "[{} - {} lines]",
                        language.as_deref().unwrap_or("code"),
                        code.lines().count()
                    ),
                    Style::default().fg(self.theme.text_muted),
                ),
            ]));
            return;
        }

        let code_block = CodeBlock::new(language.clone(), code.to_string());

        lines.push(Line::from(vec![
            Span::styled(
                "╭",
                Style::default().fg(self.theme.markdown_horizontal_rule),
            ),
            Span::styled(
                format!(" {} ", language.as_deref().unwrap_or("code")),
                Style::default().fg(self.theme.markdown_code_block),
            ),
            Span::styled(
                "─".repeat(20),
                Style::default().fg(self.theme.markdown_horizontal_rule),
            ),
        ]));

        for code_spans in code_block.to_lines(&self.code_theme) {
            let mut line_spans = vec![Span::styled(
                "│ ",
                Style::default().fg(self.theme.markdown_horizontal_rule),
            )];
            line_spans.extend(code_spans);
            lines.push(Line::from(line_spans));
        }

        lines.push(Line::from(Span::styled(
            "╰───",
            Style::default().fg(self.theme.markdown_horizontal_rule),
        )));
    }
}

fn table_border_line(
    left: char,
    middle: char,
    right: char,
    widths: &[usize],
    color: ratatui::style::Color,
) -> Line<'static> {
    let mut text = String::new();
    text.push(left);
    for (idx, width) in widths.iter().enumerate() {
        text.push_str(&"─".repeat(width + 2));
        if idx + 1 < widths.len() {
            text.push(middle);
        }
    }
    text.push(right);
    Line::from(Span::styled(text, Style::default().fg(color)))
}

fn table_row_line(
    row: &TableRow,
    widths: &[usize],
    alignments: &[Alignment],
    theme: &Theme,
) -> Line<'static> {
    let mut spans: Vec<Span<'static>> = Vec::new();
    let border_style = Style::default().fg(theme.markdown_horizontal_rule);
    let content_style = if row.is_header {
        Style::default()
            .fg(theme.markdown_heading)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(theme.text)
    };

    spans.push(Span::styled("│", border_style));
    for (idx, width) in widths.iter().enumerate() {
        // 列幅をクランプした結果、元のセル本文が width を超える場合は
        // 表示幅いっぱいで切り詰める（枠線が折り返されないようにする）。
        let cell = truncate_spans_to_width(row.cells.get(idx).cloned().unwrap_or_default(), *width);
        let cell_width = cell_span_width(&cell);
        let padding = width.saturating_sub(cell_width);
        let alignment = alignments.get(idx).copied().unwrap_or(Alignment::None);
        let (left_pad, right_pad) = match alignment {
            Alignment::Right => (padding, 0),
            Alignment::Center => (padding / 2, padding - padding / 2),
            _ => (0, padding),
        };

        spans.push(Span::styled(" ", content_style));
        if left_pad > 0 {
            spans.push(Span::styled(" ".repeat(left_pad), content_style));
        }
        if row.is_header {
            for span in cell {
                spans.push(Span::styled(
                    span.content.to_string(),
                    span.style
                        .fg(theme.markdown_heading)
                        .add_modifier(Modifier::BOLD),
                ));
            }
        } else {
            spans.extend(cell);
        }
        if right_pad > 0 {
            spans.push(Span::styled(" ".repeat(right_pad), content_style));
        }
        spans.push(Span::styled(" ", content_style));
        spans.push(Span::styled("│", border_style));
    }

    Line::from(spans)
}

/// Span 列を表示幅 `max_width` に収まるよう切り詰める（スタイルは保持）。
fn truncate_spans_to_width(spans: Vec<Span<'static>>, max_width: usize) -> Vec<Span<'static>> {
    let mut result = Vec::new();
    let mut used = 0usize;
    for span in spans {
        if used >= max_width {
            break;
        }
        let mut text = String::new();
        for ch in span.content.chars() {
            let w = UnicodeWidthChar::width(ch).unwrap_or(1);
            if used + w > max_width {
                break;
            }
            text.push(ch);
            used += w;
        }
        if !text.is_empty() {
            result.push(span.content(std::borrow::Cow::Owned(text)));
        }
    }
    result
}

fn cell_span_width(spans: &[Span<'static>]) -> usize {
    spans
        .iter()
        .map(|span| UnicodeWidthStr::width(span.content.as_ref()))
        .sum::<usize>()
}

fn heading_level_to_u8(level: HeadingLevel) -> u8 {
    match level {
        HeadingLevel::H1 => 1,
        HeadingLevel::H2 => 2,
        HeadingLevel::H3 => 3,
        HeadingLevel::H4 => 4,
        HeadingLevel::H5 => 5,
        HeadingLevel::H6 => 6,
    }
}

fn heading_style(level: HeadingLevel, theme: &Theme) -> Style {
    match level {
        HeadingLevel::H1 | HeadingLevel::H2 => Style::default()
            .fg(theme.markdown_heading)
            .add_modifier(Modifier::BOLD),
        _ => Style::default()
            .fg(theme.markdown_heading)
            .add_modifier(Modifier::BOLD),
    }
}

fn ensure_prefix(spans: &mut Vec<Span<'static>>, depth: usize, theme: &Theme) {
    if !spans.is_empty() {
        return;
    }
    for _ in 0..depth {
        spans.push(Span::styled(
            "│ ",
            Style::default().fg(theme.markdown_block_quote),
        ));
    }
}

fn current_style(styles: &[Style]) -> Style {
    styles
        .last()
        .copied()
        .unwrap_or_else(|| Style::default().fg(ratatui::style::Color::White))
}

fn flush_line(lines: &mut Vec<Line<'static>>, current: &mut Vec<Span<'static>>) {
    if current.is_empty() {
        return;
    }
    lines.push(Line::from(std::mem::take(current)));
}

fn push_blank_line(lines: &mut Vec<Line<'static>>) {
    if lines.last().is_some_and(line_is_blank) {
        return;
    }
    lines.push(Line::from(""));
}

fn line_is_blank(line: &Line<'_>) -> bool {
    line.spans
        .iter()
        .all(|span| span.content.as_ref().trim().is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_theme() -> Theme {
        Theme::default()
    }

    fn line_text(line: &Line<'static>) -> String {
        line.spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect()
    }

    #[test]
    fn new_retains_theme_and_derives_code_theme() {
        let theme = test_theme();
        let renderer = MarkdownRenderer::new(theme.clone());

        // Theme / CodeTheme は PartialEq 未実装のためフィールドで比較する。
        assert_eq!(renderer.theme.text, theme.text);
        assert_eq!(renderer.theme.markdown_heading, theme.markdown_heading);
        assert_eq!(renderer.theme.primary, theme.primary);
        assert_eq!(renderer.theme.agent_colors, theme.agent_colors);
        assert!(!renderer.concealed);

        // CodeTheme::from_app_theme の写像を確認する。
        assert_eq!(renderer.code_theme.text, theme.text);
        assert_eq!(renderer.code_theme.keyword, theme.primary);
        assert_eq!(renderer.code_theme.string, theme.success);
        assert_eq!(renderer.code_theme.number, theme.warning);
        assert_eq!(renderer.code_theme.comment, theme.text_muted);
        assert_eq!(renderer.code_theme.punctuation, theme.border);
        assert_eq!(renderer.code_theme.function, theme.info);
    }

    #[test]
    fn to_lines_empty_text_returns_no_lines() {
        let renderer = MarkdownRenderer::new(test_theme());
        assert!(renderer.to_lines("").is_empty());
    }

    #[test]
    fn to_lines_plain_paragraph_uses_theme_text() {
        let theme = test_theme();
        let renderer = MarkdownRenderer::new(theme.clone());

        let lines = renderer.to_lines("hello world");
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].spans.len(), 1);
        assert_eq!(lines[0].spans[0].content.as_ref(), "hello world");
        assert_eq!(lines[0].spans[0].style.fg, Some(theme.text));
    }

    #[test]
    fn to_lines_heading_uses_hash_prefix_and_heading_style() {
        let theme = test_theme();
        let renderer = MarkdownRenderer::new(theme.clone());

        let lines = renderer.to_lines("# Title");
        assert_eq!(lines.len(), 1);
        let spans = &lines[0].spans;
        assert_eq!(spans.len(), 2);
        assert_eq!(spans[0].content.as_ref(), "# ");
        assert_eq!(spans[0].style.fg, Some(theme.markdown_heading));
        assert!(spans[0].style.add_modifier.contains(Modifier::BOLD));
        assert_eq!(spans[1].content.as_ref(), "Title");
        assert_eq!(spans[1].style.fg, Some(theme.markdown_heading));
        assert!(spans[1].style.add_modifier.contains(Modifier::BOLD));
    }

    #[test]
    fn to_lines_heading_level_controls_hash_count() {
        let renderer = MarkdownRenderer::new(test_theme());
        for (markdown, prefix) in [
            ("# one", "# "),
            ("## two", "## "),
            ("### three", "### "),
            ("###### six", "###### "),
        ] {
            let lines = renderer.to_lines(markdown);
            assert_eq!(lines.len(), 1, "input: {markdown}");
            assert_eq!(
                lines[0].spans[0].content.as_ref(),
                prefix,
                "input: {markdown}"
            );
        }
    }

    #[test]
    fn to_lines_unordered_list_uses_bullet_prefix() {
        let theme = test_theme();
        let renderer = MarkdownRenderer::new(theme.clone());

        let lines = renderer.to_lines("- alpha\n- beta");
        assert_eq!(lines.len(), 2);
        for (line, expected) in lines.iter().zip(["alpha", "beta"]) {
            assert_eq!(line.spans.len(), 2);
            assert_eq!(line.spans[0].content.as_ref(), "• ");
            assert_eq!(line.spans[0].style.fg, Some(theme.markdown_list_item));
            assert_eq!(line.spans[1].content.as_ref(), expected);
            assert_eq!(line.spans[1].style.fg, Some(theme.text));
        }
    }

    #[test]
    fn to_lines_ordered_list_increments_prefix() {
        let theme = test_theme();
        let renderer = MarkdownRenderer::new(theme.clone());

        let lines = renderer.to_lines("1. first\n2. second\n3. third");
        assert_eq!(lines.len(), 3);
        for (line, (prefix, text)) in
            lines
                .iter()
                .zip([("1. ", "first"), ("2. ", "second"), ("3. ", "third")])
        {
            assert_eq!(line.spans.len(), 2);
            assert_eq!(line.spans[0].content.as_ref(), prefix);
            assert_eq!(
                line.spans[0].style.fg,
                Some(theme.markdown_list_enumeration)
            );
            assert_eq!(line.spans[1].content.as_ref(), text);
            assert_eq!(line.spans[1].style.fg, Some(theme.text));
        }
    }

    #[test]
    fn to_lines_table_renders_borders_header_and_rows() {
        let theme = test_theme();
        let renderer = MarkdownRenderer::new(theme.clone());

        let lines = renderer.to_lines("| Name | Age |\n| --- | --- |\n| Alice | 30 |");
        assert_eq!(lines.len(), 5);

        assert_eq!(line_text(&lines[0]), "┌───────┬─────┐");
        assert_eq!(line_text(&lines[1]), "│ Name  │ Age │");
        assert_eq!(line_text(&lines[2]), "├───────┼─────┤");
        assert_eq!(line_text(&lines[3]), "│ Alice │ 30  │");
        assert_eq!(line_text(&lines[4]), "└───────┴─────┘");

        for index in [0, 2, 4] {
            assert_eq!(lines[index].spans.len(), 1, "line: {index}");
            assert_eq!(
                lines[index].spans[0].style.fg,
                Some(theme.markdown_horizontal_rule),
                "line: {index}"
            );
        }

        let name = lines[1]
            .spans
            .iter()
            .find(|span| span.content.as_ref() == "Name")
            .expect("header cell");
        assert_eq!(name.style.fg, Some(theme.markdown_heading));
        assert!(name.style.add_modifier.contains(Modifier::BOLD));

        let alice = lines[3]
            .spans
            .iter()
            .find(|span| span.content.as_ref() == "Alice")
            .expect("body cell");
        assert_eq!(alice.style.fg, Some(theme.text));
        assert!(!alice.style.add_modifier.contains(Modifier::BOLD));
    }

    #[test]
    fn to_lines_table_alignment_controls_padding() {
        let renderer = MarkdownRenderer::new(test_theme());

        let lines = renderer
            .to_lines("| a | b | c |\n| :--- | :---: | ---: |\n| longer | longer | longer |");
        assert_eq!(lines.len(), 5);
        assert_eq!(line_text(&lines[0]), "┌────────┬────────┬────────┐");
        assert_eq!(line_text(&lines[1]), "│ a      │   b    │      c │");
        assert_eq!(line_text(&lines[2]), "├────────┼────────┼────────┤");
        assert_eq!(line_text(&lines[3]), "│ longer │ longer │ longer │");
        assert_eq!(line_text(&lines[4]), "└────────┴────────┴────────┘");
    }

    #[test]
    fn to_lines_with_width_clamps_wide_table_without_breaking_frame() {
        let renderer = MarkdownRenderer::new(test_theme());
        let markdown = concat!(
            "| a very long first header | second long header |\n",
            "| --- | --- |\n",
            "| 0123456789012345678901234567890 | short |\n",
            "| another fairly long body cell | tail |\n",
        );
        let available = 30usize;

        let lines = renderer.to_lines_with_width(markdown, available);
        let table_lines: Vec<&Line<'static>> =
            lines.iter().filter(|line| !line_is_blank(line)).collect();
        assert_eq!(table_lines.len(), 6);

        for line in &table_lines {
            let text = line_text(line);
            let width = UnicodeWidthStr::width(text.as_str());
            assert!(
                width <= available,
                "table line must fit in available width: {text} ({width} > {available})"
            );
            // 枠線文字が途中で折り返されず、行頭と行末に罫線が残っていることを固定する。
            assert!(
                matches!(text.chars().next(), Some('┌' | '│' | '├' | '└')),
                "table line must start with a frame character: {text}"
            );
            assert!(
                matches!(text.chars().last(), Some('┐' | '│' | '┤' | '┘')),
                "table line must end with a frame character: {text}"
            );
        }
        assert_eq!(table_lines[0].spans.len(), 1);
        assert_eq!(table_lines[5].spans.len(), 1);
    }

    #[test]
    fn to_lines_with_width_narrow_table_falls_back_to_flat_rows() {
        let renderer = MarkdownRenderer::new(test_theme());
        let markdown = "| Name | Age |\n| --- | --- |\n| Alice | 30 |";
        let available = 6usize;

        let lines = renderer.to_lines_with_width(markdown, available);
        assert!(!lines.is_empty());
        for line in &lines {
            let text = line_text(line);
            let width = UnicodeWidthStr::width(text.as_str());
            assert!(
                width <= available,
                "flat table line must fit available width: {text} ({width} > {available})"
            );
            assert!(
                !text.contains('┌') && !text.contains('└'),
                "too-narrow table must not emit a broken frame: {text}"
            );
            assert!(
                text.starts_with('│'),
                "flat table line must keep its gutter: {text}"
            );
        }
        // 先頭セルから順に切り詰められ、末尾のセルは 0 幅になり得る。
        assert_eq!(line_text(&lines[0]), "│ N │ ");
        assert_eq!(line_text(&lines[1]), "│ A │ ");
    }

    #[test]
    fn to_lines_code_block_renders_frame_and_content() {
        let theme = test_theme();
        let renderer = MarkdownRenderer::new(theme.clone());

        let lines = renderer.to_lines("```unknown_language_token_xyz\nalpha\nbeta\n```");
        assert_eq!(lines.len(), 4);

        let dashes = "─".repeat(20);
        let header = &lines[0].spans;
        assert_eq!(header.len(), 3);
        assert_eq!(header[0].content.as_ref(), "╭");
        assert_eq!(header[0].style.fg, Some(theme.markdown_horizontal_rule));
        assert_eq!(header[1].content.as_ref(), " unknown_language_token_xyz ");
        assert_eq!(header[1].style.fg, Some(theme.markdown_code_block));
        assert_eq!(header[2].content.as_ref(), dashes.as_str());
        assert_eq!(header[2].style.fg, Some(theme.markdown_horizontal_rule));

        for (line, expected) in lines[1..3].iter().zip(["alpha", "beta"]) {
            assert_eq!(line.spans.len(), 2);
            assert_eq!(line.spans[0].content.as_ref(), "│ ");
            assert_eq!(line.spans[0].style.fg, Some(theme.markdown_horizontal_rule));
            assert_eq!(line.spans[1].content.as_ref(), expected);
            assert_eq!(line.spans[1].style.fg, Some(theme.text));
        }

        assert_eq!(lines[3].spans.len(), 1);
        assert_eq!(lines[3].spans[0].content.as_ref(), "╰───");
        assert_eq!(
            lines[3].spans[0].style.fg,
            Some(theme.markdown_horizontal_rule)
        );
    }

    #[test]
    fn to_lines_code_block_without_language_uses_code_label() {
        let renderer = MarkdownRenderer::new(test_theme());

        let lines = renderer.to_lines("```\nx\n```");
        assert_eq!(lines.len(), 3);
        assert_eq!(lines[0].spans[1].content.as_ref(), " code ");
        assert_eq!(lines[1].spans[1].content.as_ref(), "x");
        assert_eq!(lines[2].spans[0].content.as_ref(), "╰───");
    }

    #[test]
    fn to_lines_concealed_code_block_shows_summary() {
        let theme = test_theme();
        let renderer = MarkdownRenderer::new(theme.clone()).with_concealed(true);

        let lines = renderer.to_lines("```rust\na\nb\n```");
        assert_eq!(lines.len(), 1);
        let spans = &lines[0].spans;
        assert_eq!(spans.len(), 2);
        assert_eq!(spans[0].content.as_ref(), "▶ ");
        assert_eq!(spans[0].style.fg, Some(theme.primary));
        assert_eq!(spans[1].content.as_ref(), "[rust - 2 lines]");
        assert_eq!(spans[1].style.fg, Some(theme.text_muted));
    }
}

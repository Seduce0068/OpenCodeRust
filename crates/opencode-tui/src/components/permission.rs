use std::cell::Cell;
use std::collections::VecDeque;

use ratatui::{
    layout::Rect,
    style::Style,
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Wrap},
    Frame,
};

use crate::theme::Theme;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

/// ratatui の `WordWrapper { trim: false }` と同じ規則で、1 行が
/// `content_width` 内で占める行数を数える。
///
/// 単語（空白以外の連続）を単位に折り返し、行末の空白は残す。
/// 単語自体が行幅より長い場合は幅単位で分割する。
/// 幅を超えるシンボルは ratatui と同じく無視する。
pub(crate) fn wrapped_text_rows(text: &str, content_width: u16) -> usize {
    let width = usize::from(content_width);
    if width == 0 {
        return 0;
    }

    // pending_line / pending_word / pending_whitespace の幅だけを追跡する。
    // grapheme 単位の実体は不要で、幅と空白の有無だけ行数に影響する。
    let mut rows = 0usize;
    let mut line_width = 0usize;
    let mut word_width = 0usize;
    let mut whitespace: VecDeque<usize> = VecDeque::new();
    let mut whitespace_width = 0usize;
    let mut previous_non_whitespace = false;

    for c in text.chars() {
        let is_whitespace = c.is_whitespace();
        let symbol_width = UnicodeWidthChar::width(c).unwrap_or(0);
        if symbol_width > width {
            continue;
        }

        let word_found = previous_non_whitespace && is_whitespace;
        // trim = false なので、単語と空白の合計が行幅を超えた時点で
        // 保留中の単語を現在行に確定させる。
        let untrimmed_overflow =
            line_width == 0 && word_width + whitespace_width + symbol_width > width;

        if word_found || untrimmed_overflow {
            line_width += whitespace_width;
            whitespace.clear();
            whitespace_width = 0;
            line_width += word_width;
            word_width = 0;
        }

        let line_full = line_width >= width;
        let pending_word_overflow =
            symbol_width > 0 && line_width + whitespace_width + word_width >= width;

        if line_full || pending_word_overflow {
            let mut remaining = width.saturating_sub(line_width);
            rows += 1;
            line_width = 0;

            // 次の行に回る際、収まる分の空白だけを現在行に残す。
            loop {
                match whitespace.front().copied() {
                    Some(ws) if ws <= remaining => {
                        whitespace.pop_front();
                        whitespace_width -= ws;
                        remaining -= ws;
                    }
                    _ => break,
                }
            }

            // 行頭に回った空白は破棄する（ratatui の trim = false と同じ）。
            if is_whitespace && whitespace.is_empty() {
                continue;
            }
        }

        if is_whitespace {
            whitespace_width += symbol_width;
            whitespace.push_back(symbol_width);
        } else {
            word_width += symbol_width;
        }
        previous_non_whitespace = !is_whitespace;
    }

    if line_width > 0 || word_width > 0 || whitespace_width > 0 {
        rows += 1;
    }
    rows.max(1)
}

/// `Wrap { trim: false }` の Paragraph が `content_width` 内で占める行数を返す。
///
/// 空行も 1 行として数える。単語境界を考慮した折り返し行数と、従来の
/// 幅のみからの見積もりの大きい方を採用する。後者を下限に残す理由は、
/// 全角文字などで ratatui が幅をはみ出して 1 行に描画するケースでも
/// 高さを過小評価して末尾が見えなくなることを避けるため。
pub(crate) fn wrapped_row_count(lines: &[Line<'_>], content_width: u16) -> usize {
    let width_u16 = content_width.max(1);
    let width = usize::from(width_u16);
    lines
        .iter()
        .map(|line| {
            let text: String = line
                .spans
                .iter()
                .map(|span| span.content.as_ref())
                .collect();
            let word_rows = wrapped_text_rows(&text, width_u16);
            let width_rows = UnicodeWidthStr::width(text.as_str()).div_ceil(width).max(1);
            word_rows.max(width_rows)
        })
        .sum()
}

#[derive(Clone, Debug, PartialEq)]
pub enum PermissionType {
    ReadFile,
    WriteFile,
    Edit,
    ExecuteCommand,
    Bash,
    NetworkRequest,
    Glob,
    Grep,
    List,
    Task,
    WebFetch,
    WebSearch,
    CodeSearch,
    ExternalDirectory,
    Other,
}

impl PermissionType {
    /// Map a tool permission identifier (for example `bash` or
    /// `external_directory`) to the prompt representation.
    pub fn from_permission_str(value: &str) -> Self {
        match value {
            "read" => PermissionType::ReadFile,
            "write" => PermissionType::WriteFile,
            "edit" => PermissionType::Edit,
            "bash" => PermissionType::Bash,
            "glob" => PermissionType::Glob,
            "grep" => PermissionType::Grep,
            "list" => PermissionType::List,
            "task" => PermissionType::Task,
            "webfetch" => PermissionType::WebFetch,
            "websearch" => PermissionType::WebSearch,
            "codesearch" => PermissionType::CodeSearch,
            "external_directory" => PermissionType::ExternalDirectory,
            _ => PermissionType::Other,
        }
    }

    pub fn label(&self) -> &'static str {
        match self {
            PermissionType::ReadFile => "Read file",
            PermissionType::WriteFile => "Write file",
            PermissionType::Edit => "Edit file",
            PermissionType::ExecuteCommand => "Execute command",
            PermissionType::Bash => "Run shell command",
            PermissionType::NetworkRequest => "Network request",
            PermissionType::Glob => "Glob search",
            PermissionType::Grep => "Grep search",
            PermissionType::List => "List directory",
            PermissionType::Task => "Task operation",
            PermissionType::WebFetch => "Fetch web content",
            PermissionType::WebSearch => "Web search",
            PermissionType::CodeSearch => "Code search",
            PermissionType::ExternalDirectory => "External directory access",
            PermissionType::Other => "Tool execution",
        }
    }

    pub fn icon(&self) -> &'static str {
        match self {
            PermissionType::ReadFile => "[R]",
            PermissionType::WriteFile => "[W]",
            PermissionType::Edit => "[E]",
            PermissionType::ExecuteCommand => "[X]",
            PermissionType::Bash => "[!]",
            PermissionType::NetworkRequest => "[N]",
            PermissionType::Glob => "[G]",
            PermissionType::Grep => "[S]",
            PermissionType::List => "[L]",
            PermissionType::Task => "[T]",
            PermissionType::WebFetch => "[F]",
            PermissionType::WebSearch => "[Q]",
            PermissionType::CodeSearch => "[C]",
            PermissionType::ExternalDirectory => "[D]",
            PermissionType::Other => "[?]",
        }
    }
}

#[derive(Clone, Debug)]
pub struct PermissionRequest {
    pub id: String,
    pub permission_type: PermissionType,
    pub resource: String,
    pub tool_name: String,
}

#[derive(Clone, Debug, PartialEq)]
pub enum PermissionAction {
    Approve,
    Deny,
    ApproveAlways,
}

/// 権限プロンプトのキーヒント。Enter は暗黙 approve せず既定で Deny にする。
pub const PERMISSION_KEY_HINT: &str = "Enter: Deny (default)  Esc: Deny";

pub struct PermissionPrompt {
    requests: Vec<PermissionRequest>,
    current_index: usize,
    pub is_open: bool,
    last_rendered_area: Cell<Option<Rect>>,
    pending_action: Option<PermissionAction>,
}

impl PermissionPrompt {
    pub fn new() -> Self {
        Self {
            requests: Vec::new(),
            current_index: 0,
            is_open: false,
            last_rendered_area: Cell::new(None),
            pending_action: None,
        }
    }

    pub fn add_request(&mut self, request: PermissionRequest) {
        self.requests.push(request);
        self.is_open = !self.requests.is_empty();
    }

    pub fn current_request(&self) -> Option<&PermissionRequest> {
        self.requests.get(self.current_index)
    }

    pub fn approve(&mut self) -> Option<PermissionRequest> {
        if self.current_index < self.requests.len() {
            let request = self.requests.remove(self.current_index);
            if self.requests.is_empty() {
                self.is_open = false;
            }
            Some(request)
        } else {
            None
        }
    }

    pub fn deny(&mut self) -> Option<PermissionRequest> {
        if self.current_index < self.requests.len() {
            let request = self.requests.remove(self.current_index);
            if self.requests.is_empty() {
                self.is_open = false;
            }
            Some(request)
        } else {
            None
        }
    }

    pub fn approve_always(&mut self) -> Option<PermissionRequest> {
        // 「常に許可」の意志は reply 文字列 "always" で表現する。
        // サーバは permission id の完全一致で pending を解決するため、
        // ここで id にサフィックスを付与すると reply が 404 になる。
        self.approve()
    }

    pub fn close(&mut self) {
        self.requests.clear();
        self.current_index = 0;
        self.is_open = false;
    }

    pub fn is_empty(&self) -> bool {
        self.requests.is_empty()
    }

    pub fn pending_count(&self) -> usize {
        self.requests.len()
    }

    pub fn handle_click(&mut self, col: u16, row: u16) {
        if !self.is_open || self.requests.is_empty() {
            return;
        }
        // Button row is the last content line inside the border.
        // We store the last rendered area to check clicks.
        // For simplicity, check if the click is on the button hints row
        // and map x-position to the three buttons.
        if let Some(area) = self.last_rendered_area.get() {
            if row < area.y
                || row >= area.y + area.height
                || col < area.x
                || col >= area.x + area.width
            {
                return;
            }
            // The button line is at the bottom of the content (area.y + area.height - 2 for border)
            let button_row = area.y.saturating_add(area.height.saturating_sub(2));
            if row == button_row {
                // "[y] Allow  [n] Deny  [a] Always allow"
                // Rough column ranges inside the border:
                let inner_col = col.saturating_sub(area.x + 1);
                if inner_col < 11 {
                    // [y] Allow
                    self.pending_action = Some(PermissionAction::Approve);
                } else if inner_col < 21 {
                    // [n] Deny
                    self.pending_action = Some(PermissionAction::Deny);
                } else {
                    // [a] Always allow
                    self.pending_action = Some(PermissionAction::ApproveAlways);
                }
            }
        }
    }

    pub fn take_pending_action(&mut self) -> Option<PermissionAction> {
        self.pending_action.take()
    }

    /// テスト専用: `render` を経由せずにクリック判定の基準領域を設定する。
    #[cfg(test)]
    pub(crate) fn set_last_rendered_area_for_test(&self, area: Rect) {
        self.last_rendered_area.set(Some(area));
    }

    pub fn render(&self, frame: &mut Frame, area: Rect, theme: &Theme) {
        if !self.is_open || self.requests.is_empty() {
            return;
        }

        let request = match self.current_request() {
            Some(r) => r,
            None => return,
        };

        let width = area.width.saturating_sub(2).min(80);
        // 枠 2 行分を除いた幅が Paragraph の折り返し幅。
        let content_width = width.saturating_sub(2).max(1);

        let title = format!(
            "{} {} - Permission Request",
            request.permission_type.icon(),
            request.permission_type.label()
        );

        let content = vec![
            Line::from(Span::styled(
                &title,
                Style::default().fg(theme.primary).bold(),
            )),
            Line::from(""),
            Line::from(vec![
                Span::styled("Tool: ", Style::default().fg(theme.text_muted)),
                Span::styled(&request.tool_name, Style::default().fg(theme.text)),
            ]),
            Line::from(vec![
                Span::styled("Resource: ", Style::default().fg(theme.text_muted)),
                Span::styled(&request.resource, Style::default().fg(theme.text)),
            ]),
            Line::from(""),
            Line::from(vec![
                Span::styled("[y] Allow  ", Style::default().fg(theme.success)),
                Span::styled("[n] Deny  ", Style::default().fg(theme.error)),
                Span::styled("[a] Always allow", Style::default().fg(theme.primary)),
            ]),
            Line::from(Span::styled(
                PERMISSION_KEY_HINT,
                Style::default().fg(theme.text_muted),
            )),
        ];

        // 固定 9 行ではなく、折り返し後の内容行数 + 枠 2 行で高さを決める。
        // 長い resource やヒント行が折り返しても末尾まで読める。
        let wrapped_rows = wrapped_row_count(&content, content_width);
        let max_height = area.height.saturating_sub(1).max(1);
        let height = (wrapped_rows as u16)
            .saturating_add(2)
            .min(max_height)
            .max(1);

        // Render inline at the bottom of the session area
        let popup_area = Rect::new(
            area.x + 1,
            area.y + area.height.saturating_sub(height + 1),
            width,
            height,
        );

        self.last_rendered_area.set(Some(popup_area));

        let paragraph = Paragraph::new(content)
            .block(
                Block::default()
                    .title(" Permission ")
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(theme.warning)),
            )
            .style(Style::default().bg(theme.background_panel))
            .wrap(Wrap { trim: false });

        frame.render_widget(paragraph, popup_area);
    }
}

impl Default for PermissionPrompt {
    fn default() -> Self {
        Self::new()
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

    fn sample_request() -> PermissionRequest {
        PermissionRequest {
            id: "perm-1".to_string(),
            permission_type: PermissionType::Bash,
            resource: "ls -la".to_string(),
            tool_name: "bash".to_string(),
        }
    }

    #[test]
    fn from_permission_str_maps_every_known_identifier() {
        // Expected values are taken from the implementation table in
        // PermissionType::from_permission_str, not inferred from variant names.
        let cases = [
            ("read", PermissionType::ReadFile),
            ("write", PermissionType::WriteFile),
            ("edit", PermissionType::Edit),
            ("bash", PermissionType::Bash),
            ("glob", PermissionType::Glob),
            ("grep", PermissionType::Grep),
            ("list", PermissionType::List),
            ("task", PermissionType::Task),
            ("webfetch", PermissionType::WebFetch),
            ("websearch", PermissionType::WebSearch),
            ("codesearch", PermissionType::CodeSearch),
            ("external_directory", PermissionType::ExternalDirectory),
        ];
        for (input, expected) in cases {
            assert_eq!(
                PermissionType::from_permission_str(input),
                expected,
                "input: {input:?}"
            );
        }
    }

    #[test]
    fn from_permission_str_falls_back_to_other_for_unmapped_values() {
        // The implementation has no arm for "execute_command" or
        // "network_request", so those variants are not reachable from a string.
        for input in [
            "",
            "unknown",
            "READ",
            "Bash",
            "execute_command",
            "network_request",
            "external-directory",
        ] {
            assert_eq!(
                PermissionType::from_permission_str(input),
                PermissionType::Other,
                "input: {input:?}"
            );
        }
    }

    #[test]
    fn label_returns_documented_text_for_every_variant() {
        let cases = [
            (PermissionType::ReadFile, "Read file"),
            (PermissionType::WriteFile, "Write file"),
            (PermissionType::Edit, "Edit file"),
            (PermissionType::ExecuteCommand, "Execute command"),
            (PermissionType::Bash, "Run shell command"),
            (PermissionType::NetworkRequest, "Network request"),
            (PermissionType::Glob, "Glob search"),
            (PermissionType::Grep, "Grep search"),
            (PermissionType::List, "List directory"),
            (PermissionType::Task, "Task operation"),
            (PermissionType::WebFetch, "Fetch web content"),
            (PermissionType::WebSearch, "Web search"),
            (PermissionType::CodeSearch, "Code search"),
            (
                PermissionType::ExternalDirectory,
                "External directory access",
            ),
            (PermissionType::Other, "Tool execution"),
        ];
        for (variant, expected) in cases {
            assert_eq!(variant.label(), expected, "variant: {variant:?}");
        }
    }

    #[test]
    fn icon_returns_documented_marker_for_every_variant() {
        let cases = [
            (PermissionType::ReadFile, "[R]"),
            (PermissionType::WriteFile, "[W]"),
            (PermissionType::Edit, "[E]"),
            (PermissionType::ExecuteCommand, "[X]"),
            (PermissionType::Bash, "[!]"),
            (PermissionType::NetworkRequest, "[N]"),
            (PermissionType::Glob, "[G]"),
            (PermissionType::Grep, "[S]"),
            (PermissionType::List, "[L]"),
            (PermissionType::Task, "[T]"),
            (PermissionType::WebFetch, "[F]"),
            (PermissionType::WebSearch, "[Q]"),
            (PermissionType::CodeSearch, "[C]"),
            (PermissionType::ExternalDirectory, "[D]"),
            (PermissionType::Other, "[?]"),
        ];
        for (variant, expected) in cases {
            assert_eq!(variant.icon(), expected, "variant: {variant:?}");
        }
    }

    #[test]
    fn permission_request_retains_all_fields() {
        // PermissionRequest has no PartialEq: assert on each assigned value.
        let request = sample_request();
        assert_eq!(request.id, "perm-1");
        assert_eq!(request.permission_type, PermissionType::Bash);
        assert_eq!(request.resource, "ls -la");
        assert_eq!(request.tool_name, "bash");
    }

    #[test]
    fn permission_request_clone_retains_field_values() {
        let request = sample_request();
        let cloned = request.clone();
        assert_eq!(cloned.id, request.id);
        assert_eq!(cloned.permission_type, request.permission_type);
        assert_eq!(cloned.resource, request.resource);
        assert_eq!(cloned.tool_name, request.tool_name);
    }

    #[test]
    fn permission_action_supports_equality_and_inequality() {
        assert_eq!(PermissionAction::Approve, PermissionAction::Approve);
        assert_eq!(PermissionAction::Deny, PermissionAction::Deny);
        assert_eq!(
            PermissionAction::ApproveAlways,
            PermissionAction::ApproveAlways
        );
        assert_ne!(PermissionAction::Approve, PermissionAction::Deny);
        assert_ne!(PermissionAction::Deny, PermissionAction::ApproveAlways);
        assert_ne!(PermissionAction::ApproveAlways, PermissionAction::Approve);
    }

    #[test]
    fn new_prompt_is_closed_empty_and_has_no_pending_state() {
        let mut prompt = PermissionPrompt::new();
        assert!(!prompt.is_open);
        assert!(prompt.is_empty());
        assert_eq!(prompt.pending_count(), 0);
        assert!(prompt.current_request().is_none());
        assert_eq!(prompt.current_index, 0);
        assert!(prompt.take_pending_action().is_none());
    }

    #[test]
    fn default_prompt_matches_new_prompt() {
        let prompt = PermissionPrompt::default();
        assert!(!prompt.is_open);
        assert!(prompt.is_empty());
        assert_eq!(prompt.pending_count(), 0);
        assert!(prompt.current_request().is_none());
    }

    #[test]
    fn add_request_opens_prompt_and_exposes_first_as_current() {
        let mut prompt = PermissionPrompt::new();
        prompt.add_request(sample_request());
        assert!(prompt.is_open);
        assert!(!prompt.is_empty());
        assert_eq!(prompt.pending_count(), 1);
        let current = prompt.current_request().expect("request was added");
        assert_eq!(current.id, "perm-1");
        assert_eq!(current.tool_name, "bash");
    }

    #[test]
    fn add_request_queues_multiple_without_changing_current() {
        let mut prompt = PermissionPrompt::new();
        prompt.add_request(sample_request());
        let mut second = sample_request();
        second.id = "perm-2".to_string();
        second.tool_name = "edit".to_string();
        prompt.add_request(second);
        assert_eq!(prompt.pending_count(), 2);
        assert_eq!(
            prompt.current_request().map(|request| request.id.as_str()),
            Some("perm-1")
        );
    }

    #[test]
    fn approve_removes_current_and_closes_when_last_request_is_taken() {
        let mut prompt = PermissionPrompt::new();
        prompt.add_request(sample_request());
        let approved = prompt.approve().expect("pending request");
        assert_eq!(approved.id, "perm-1");
        assert!(!prompt.is_open);
        assert!(prompt.is_empty());
        assert_eq!(prompt.pending_count(), 0);
        assert!(prompt.approve().is_none());
    }

    #[test]
    fn approve_advances_to_next_queued_request() {
        let mut prompt = PermissionPrompt::new();
        prompt.add_request(sample_request());
        let mut second = sample_request();
        second.id = "perm-2".to_string();
        prompt.add_request(second);

        let first = prompt.approve().expect("first request");
        assert_eq!(first.id, "perm-1");
        assert!(prompt.is_open);
        assert_eq!(prompt.pending_count(), 1);
        let current = prompt.current_request().expect("second is now current");
        assert_eq!(current.id, "perm-2");
    }

    #[test]
    fn deny_removes_current_request_and_closes_when_empty() {
        // Current implementation lets deny() remove the request just like approve();
        // the caller decides the response. This test pins that behavior.
        let mut prompt = PermissionPrompt::new();
        prompt.add_request(sample_request());
        let denied = prompt.deny().expect("pending request");
        assert_eq!(denied.id, "perm-1");
        assert_eq!(denied.resource, "ls -la");
        assert!(prompt.is_empty());
        assert!(!prompt.is_open);
        assert!(prompt.deny().is_none());
    }

    #[test]
    fn approve_always_keeps_original_request_id() {
        // サーバは permission id の完全一致で pending を解決するため、
        // "always" の意志に関わらず id は書き換えない（reply 文字列で表現する）。
        let mut prompt = PermissionPrompt::new();
        prompt.add_request(sample_request());
        let approved = prompt.approve_always().expect("pending request");
        assert_eq!(approved.id, "perm-1");
        assert!(!approved.id.ends_with("_always"));
        assert!(prompt.is_empty());
        assert!(!prompt.is_open);
    }

    #[test]
    fn approve_always_on_empty_prompt_returns_none() {
        let mut prompt = PermissionPrompt::new();
        assert!(prompt.approve_always().is_none());
    }

    #[test]
    fn close_clears_requests_and_resets_state() {
        let mut prompt = PermissionPrompt::new();
        prompt.add_request(sample_request());
        let mut second = sample_request();
        second.id = "perm-2".to_string();
        prompt.add_request(second);
        prompt.close();
        assert!(prompt.is_empty());
        assert_eq!(prompt.pending_count(), 0);
        assert_eq!(prompt.current_index, 0);
        assert!(!prompt.is_open);
        assert!(prompt.current_request().is_none());
        assert!(prompt.approve().is_none());
    }

    #[test]
    fn handle_click_maps_button_columns_to_actions() {
        let mut prompt = PermissionPrompt::new();
        prompt.add_request(sample_request());
        // Simulate the area that render() would record, without creating a Frame.
        // x=10, y=20, width=40, height=8 -> button row 26, inner origin 11.
        prompt
            .last_rendered_area
            .set(Some(Rect::new(10, 20, 40, 8)));

        let cases = [
            (11, PermissionAction::Approve),
            (25, PermissionAction::Deny),
            (40, PermissionAction::ApproveAlways),
        ];
        for (col, expected) in cases {
            prompt.handle_click(col, 26);
            let action = prompt
                .take_pending_action()
                .expect("click produced an action");
            assert_eq!(action, expected, "col: {col}");
        }
    }

    #[test]
    fn handle_click_ignores_outside_area_and_non_button_rows() {
        let mut prompt = PermissionPrompt::new();
        prompt.add_request(sample_request());
        prompt
            .last_rendered_area
            .set(Some(Rect::new(10, 20, 40, 8)));

        prompt.handle_click(9, 26);
        prompt.handle_click(50, 26);
        prompt.handle_click(11, 19);
        prompt.handle_click(11, 28);
        prompt.handle_click(11, 21);
        assert!(prompt.take_pending_action().is_none());
    }

    #[test]
    fn handle_click_is_noop_when_closed_or_without_rendered_area() {
        let mut prompt = PermissionPrompt::new();
        prompt.handle_click(11, 26);
        assert!(prompt.take_pending_action().is_none());

        prompt.add_request(sample_request());
        prompt.handle_click(11, 26);
        assert!(prompt.take_pending_action().is_none());

        prompt.close();
        prompt
            .last_rendered_area
            .set(Some(Rect::new(10, 20, 40, 8)));
        prompt.handle_click(11, 26);
        assert!(prompt.take_pending_action().is_none());
    }

    #[test]
    fn permission_key_hint_states_enter_and_esc_default_to_deny() {
        assert!(PERMISSION_KEY_HINT.contains("Enter: Deny"));
        assert!(PERMISSION_KEY_HINT.contains("Esc: Deny"));
        assert!(!PERMISSION_KEY_HINT.contains("Enter: Allow"));
    }

    #[test]
    fn wrapped_row_count_counts_wrapped_and_empty_lines() {
        let lines = vec![
            Line::from("a".repeat(10)),
            Line::from(""),
            Line::from("日本語"),
        ];
        assert_eq!(wrapped_row_count(&lines, 5), 2 + 1 + 2);
    }

    #[test]
    fn wrapped_row_count_respects_word_boundaries() {
        // 幅だけの計算だと ceil(7/4) = 2 だが、ratatui は単語境界で
        // "a " / "bcd " / "e" の 3 行に折り返す。
        let lines = vec![Line::from("a bcd e")];
        assert!(wrapped_row_count(&lines, 4) >= 3);
        assert_eq!(wrapped_text_rows("a bcd e", 4), 3);
        // 短い行は従来どおり 1 行（クリック座標の基準を変えない）。
        assert_eq!(wrapped_text_rows("Continue?", 76), 1);
        assert_eq!(wrapped_row_count(&[Line::from("Continue?")], 76), 1);
    }

    #[test]
    fn render_wraps_long_resource_and_keeps_the_tail_visible() {
        let mut prompt = PermissionPrompt::new();
        let request = PermissionRequest {
            id: "perm-long".to_string(),
            permission_type: PermissionType::Bash,
            resource: format!("HEAD-{} -TAIL", "x".repeat(150)),
            tool_name: "bash".to_string(),
        };
        prompt.add_request(request);
        let theme = Theme::default();

        let text = render_to_string(80, 24, |frame| {
            prompt.render(frame, Rect::new(0, 0, 80, 24), &theme);
        });

        assert!(text.contains("HEAD-"), "resource head should render");
        assert!(
            text.contains("-TAIL"),
            "the end of a long resource must be wrapped and visible"
        );
        assert!(
            text.contains("[a] Always allow"),
            "approval buttons must still be rendered"
        );
    }

    #[test]
    fn handle_click_keeps_button_mapping_with_extra_hint_row() {
        // ヒント行を追加してもボタン行（area.y + height - 2）の割り当ては不変。
        let mut prompt = PermissionPrompt::new();
        prompt.add_request(sample_request());
        prompt
            .last_rendered_area
            .set(Some(Rect::new(10, 20, 40, 9)));

        prompt.handle_click(11, 27);
        assert_eq!(
            prompt.take_pending_action(),
            Some(PermissionAction::Approve)
        );
        prompt.handle_click(25, 27);
        assert_eq!(prompt.take_pending_action(), Some(PermissionAction::Deny));
        prompt.handle_click(40, 27);
        assert_eq!(
            prompt.take_pending_action(),
            Some(PermissionAction::ApproveAlways)
        );
    }
}

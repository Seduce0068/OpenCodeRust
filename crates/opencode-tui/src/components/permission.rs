use std::cell::Cell;

use ratatui::{
    layout::Rect,
    style::Style,
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph},
    Frame,
};

use crate::theme::Theme;

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
        let mut request = self.approve()?;
        request.id = format!("{}_always", request.id);
        Some(request)
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
            let button_row = area.y + area.height - 2;
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

    pub fn render(&self, frame: &mut Frame, area: Rect, theme: &Theme) {
        if !self.is_open || self.requests.is_empty() {
            return;
        }

        let request = match self.current_request() {
            Some(r) => r,
            None => return,
        };

        // キーヒント行を追加したため 9 行（枠 2 行 + 内容 7 行）。
        let height = 9u16;
        let width = area.width.saturating_sub(2).min(80);

        // Render inline at the bottom of the session area
        let popup_area = Rect::new(
            area.x + 1,
            area.y + area.height.saturating_sub(height + 1),
            width,
            height,
        );

        self.last_rendered_area.set(Some(popup_area));

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

        let paragraph = Paragraph::new(content)
            .block(
                Block::default()
                    .title(" Permission ")
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(theme.warning)),
            )
            .style(Style::default().bg(theme.background_panel));

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
    fn approve_always_appends_suffix_to_taken_request_id() {
        let mut prompt = PermissionPrompt::new();
        prompt.add_request(sample_request());
        let approved = prompt.approve_always().expect("pending request");
        assert_eq!(approved.id, "perm-1_always");
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

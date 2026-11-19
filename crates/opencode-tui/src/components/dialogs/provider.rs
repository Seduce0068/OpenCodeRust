use ratatui::{
    layout::Rect,
    style::Style,
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, ListState, Paragraph},
    Frame,
};

use crate::theme::Theme;

use super::DialogClickMap;

#[derive(Clone, Debug)]
pub struct Provider {
    pub id: String,
    pub name: String,
    pub status: ProviderStatus,
}

#[derive(Clone, Debug, PartialEq)]
pub enum ProviderStatus {
    Connected,
    Disconnected,
    Error,
}

pub struct ProviderDialog {
    pub providers: Vec<Provider>,
    pub state: ListState,
    pub open: bool,
    pub selected_provider: Option<Provider>,
    pub api_key_input: String,
    pub input_mode: bool,
    click_map: DialogClickMap,
}

impl ProviderDialog {
    pub fn new() -> Self {
        Self {
            providers: Vec::new(),
            state: ListState::default(),
            open: false,
            selected_provider: None,
            api_key_input: String::new(),
            input_mode: false,
            click_map: DialogClickMap::default(),
        }
    }

    pub fn open(&mut self) {
        self.open = true;
        self.state.select(Some(0));
    }

    pub fn close(&mut self) {
        self.open = false;
        self.input_mode = false;
        self.api_key_input.clear();
    }

    pub fn is_open(&self) -> bool {
        self.open
    }

    pub fn set_providers(&mut self, providers: Vec<Provider>) {
        self.providers = providers;
    }

    pub fn move_up(&mut self) {
        if let Some(selected) = self.state.selected() {
            let new = selected.saturating_sub(1);
            self.state.select(Some(new));
        }
    }

    pub fn move_down(&mut self) {
        if let Some(selected) = self.state.selected() {
            let new = (selected + 1).min(self.providers.len().saturating_sub(1));
            self.state.select(Some(new));
        }
    }

    pub fn selected_provider(&self) -> Option<&Provider> {
        self.state.selected().and_then(|i| self.providers.get(i))
    }

    /// Select the item under the pointer. Returns `true` when the click landed
    /// on the dialog frame (so the app must not close it).
    pub fn handle_click(&mut self, col: u16, row: u16) -> bool {
        if !self.input_mode {
            if let Some(index) = self.click_map.item_at(col, row) {
                self.state.select(Some(index));
            }
        }
        self.click_map.contains(col, row)
    }

    pub fn render(&self, frame: &mut Frame, area: Rect, theme: &Theme) {
        if !self.open {
            return;
        }

        let height = 15u16;
        let width = 50u16;
        let popup_area = super::centered_rect(width, height, area);
        self.click_map.set_area(popup_area);
        let block = Block::default()
            .title(" Connect Provider ")
            .borders(Borders::ALL)
            .border_style(Style::default().fg(theme.border));
        let content_area = super::dialog_inner(block.inner(popup_area));

        if self.input_mode {
            self.click_map.set_list(content_area, 0, 0);
            let content = vec![
                Line::from(Span::styled(
                    "Connect Provider",
                    Style::default().fg(theme.primary).bold(),
                )),
                Line::from(""),
                Line::from(vec![
                    Span::styled("Provider: ", Style::default().fg(theme.text_muted)),
                    Span::styled(
                        self.selected_provider()
                            .map(|p| p.name.as_str())
                            .unwrap_or(""),
                        Style::default().fg(theme.text),
                    ),
                ]),
                Line::from(""),
                Line::from("Enter API Key:"),
                Line::from(Span::styled(
                    format!("> {}", self.api_key_input),
                    Style::default().fg(theme.primary),
                )),
                Line::from(""),
                Line::from(vec![
                    Span::styled("[Enter] Connect  ", Style::default().fg(theme.success)),
                    Span::styled("[Esc] Cancel", Style::default().fg(theme.text_muted)),
                ]),
            ];

            frame.render_widget(
                block
                    .clone()
                    .style(Style::default().bg(theme.background_panel)),
                popup_area,
            );
            let paragraph =
                Paragraph::new(content).style(Style::default().bg(theme.background_panel));
            frame.render_widget(paragraph, content_area);
        } else {
            let items: Vec<ListItem> = self
                .providers
                .iter()
                .enumerate()
                .map(|(i, p)| {
                    let status_icon = match p.status {
                        ProviderStatus::Connected => "●",
                        ProviderStatus::Disconnected => "◯",
                        ProviderStatus::Error => "✗",
                    };
                    let status_color = match p.status {
                        ProviderStatus::Connected => theme.success,
                        ProviderStatus::Disconnected => theme.text_muted,
                        ProviderStatus::Error => theme.error,
                    };
                    let is_selected = self.state.selected() == Some(i);
                    let style = if is_selected {
                        Style::default()
                            .fg(theme.primary)
                            .bg(theme.background_element)
                    } else {
                        Style::default().fg(theme.text)
                    };
                    ListItem::new(Line::from(vec![
                        Span::styled(status_icon, Style::default().fg(status_color)),
                        Span::raw(" "),
                        Span::styled(&p.name, style),
                    ]))
                })
                .collect();

            frame.render_widget(
                block.style(Style::default().bg(theme.background_panel)),
                popup_area,
            );
            let list = List::new(items).highlight_style(Style::default().fg(theme.primary));

            frame.render_widget(list, content_area);
            self.click_map
                .set_list(content_area, 0, self.providers.len());
        }
    }
}

impl Default for ProviderDialog {
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

    fn provider(id: &str, name: &str, status: ProviderStatus) -> Provider {
        Provider {
            id: id.to_string(),
            name: name.to_string(),
            status,
        }
    }

    fn sample_providers() -> Vec<Provider> {
        vec![
            provider("anthropic", "Anthropic", ProviderStatus::Connected),
            provider("openai", "OpenAI", ProviderStatus::Disconnected),
            provider("google", "Google", ProviderStatus::Error),
        ]
    }

    // --- Provider / ProviderStatus データ保持 ---

    #[test]
    fn provider_stores_id_name_and_status() {
        let p = provider("anthropic", "Anthropic", ProviderStatus::Connected);

        assert_eq!(p.id, "anthropic");
        assert_eq!(p.name, "Anthropic");
        assert_eq!(p.status, ProviderStatus::Connected);
    }

    #[test]
    fn provider_clone_preserves_all_fields() {
        let original = provider("google", "Google", ProviderStatus::Error);

        let cloned = original.clone();

        assert_eq!(cloned.id, original.id);
        assert_eq!(cloned.name, original.name);
        assert_eq!(cloned.status, original.status);
    }

    #[test]
    fn provider_status_variants_are_distinct_and_cloneable() {
        // ProviderStatus は PartialEq を実装しているため assert_eq! / assert_ne! が使える。
        assert_ne!(ProviderStatus::Connected, ProviderStatus::Disconnected);
        assert_ne!(ProviderStatus::Disconnected, ProviderStatus::Error);
        assert_ne!(ProviderStatus::Error, ProviderStatus::Connected);
        assert_eq!(ProviderStatus::Error.clone(), ProviderStatus::Error);
    }

    // --- new / Default ---

    #[test]
    fn new_starts_empty_and_closed() {
        let dialog = ProviderDialog::new();

        assert!(dialog.providers.is_empty());
        // ListState::default() は未選択のため selected() は None。
        assert_eq!(dialog.state.selected(), None);
        assert!(!dialog.is_open());
        assert!(dialog.selected_provider.is_none());
        assert_eq!(dialog.api_key_input, "");
        assert!(!dialog.input_mode);
    }

    #[test]
    fn default_matches_new_initial_state() {
        let dialog = ProviderDialog::default();

        assert!(dialog.providers.is_empty());
        assert_eq!(dialog.state.selected(), None);
        assert!(!dialog.is_open());
        assert!(dialog.selected_provider().is_none());
    }

    // --- set_providers ---

    #[test]
    fn set_providers_replaces_existing_providers() {
        let mut dialog = ProviderDialog::new();
        dialog.set_providers(sample_providers());

        assert_eq!(dialog.providers.len(), 3);
        assert_eq!(dialog.providers[0].name, "Anthropic");
        assert_eq!(dialog.providers[1].id, "openai");
        assert_eq!(dialog.providers[2].status, ProviderStatus::Error);
    }

    #[test]
    fn set_providers_can_clear_providers() {
        let mut dialog = ProviderDialog::new();
        dialog.set_providers(sample_providers());

        dialog.set_providers(Vec::new());

        assert!(dialog.providers.is_empty());
    }

    #[test]
    fn set_providers_keeps_current_selection() {
        let mut dialog = ProviderDialog::new();
        dialog.set_providers(sample_providers());
        dialog.open();
        dialog.move_down();
        assert_eq!(dialog.state.selected(), Some(1));

        dialog.set_providers(vec![provider("solo", "Solo", ProviderStatus::Connected)]);

        // set_providers は選択インデックスをリセットしないため範囲外になる。
        assert_eq!(dialog.state.selected(), Some(1));
        assert!(dialog.selected_provider().is_none());
    }

    // --- open / close / is_open ---

    #[test]
    fn open_sets_open_state_and_selects_first() {
        let mut dialog = ProviderDialog::new();
        assert!(!dialog.is_open());
        assert_eq!(dialog.state.selected(), None);

        dialog.open();

        assert!(dialog.is_open());
        assert_eq!(dialog.state.selected(), Some(0));
    }

    #[test]
    fn open_without_providers_selects_index_zero_but_no_selected_provider() {
        let mut dialog = ProviderDialog::new();

        dialog.open();

        assert!(dialog.is_open());
        assert_eq!(dialog.state.selected(), Some(0));
        assert!(dialog.selected_provider().is_none());
    }

    #[test]
    fn open_selects_first_provider_after_set_providers() {
        let mut dialog = ProviderDialog::new();
        dialog.set_providers(sample_providers());

        dialog.open();

        assert!(dialog.is_open());
        assert_eq!(dialog.state.selected(), Some(0));
        let selected = dialog.selected_provider().expect("first provider");
        assert_eq!(selected.id, "anthropic");
        assert_eq!(selected.name, "Anthropic");
        assert_eq!(selected.status, ProviderStatus::Connected);
    }

    #[test]
    fn close_clears_open_input_mode_and_api_key_but_preserves_data() {
        let mut dialog = ProviderDialog::new();
        dialog.set_providers(sample_providers());
        dialog.open();
        dialog.move_down();
        dialog.input_mode = true;
        dialog.api_key_input = "test-key".to_string();
        assert_eq!(dialog.state.selected(), Some(1));

        dialog.close();

        assert!(!dialog.is_open());
        assert!(!dialog.input_mode);
        assert_eq!(dialog.api_key_input, "");
        assert_eq!(dialog.providers.len(), 3);
        assert_eq!(dialog.state.selected(), Some(1));
        let selected = dialog.selected_provider().expect("second provider");
        assert_eq!(selected.name, "OpenAI");
        assert_eq!(selected.status, ProviderStatus::Disconnected);
    }

    #[test]
    fn open_close_cycles_toggle_state() {
        let mut dialog = ProviderDialog::new();

        for _ in 0..3 {
            dialog.open();
            assert!(dialog.is_open());
            dialog.close();
            assert!(!dialog.is_open());
        }
    }

    #[test]
    fn close_is_idempotent_from_new() {
        let mut dialog = ProviderDialog::new();

        dialog.close();
        dialog.close();

        assert!(!dialog.is_open());
        assert!(dialog.providers.is_empty());
    }

    #[test]
    fn open_is_idempotent_and_resets_selection() {
        let mut dialog = ProviderDialog::new();
        dialog.set_providers(sample_providers());
        dialog.open();
        dialog.move_down();
        assert_eq!(dialog.state.selected(), Some(1));

        dialog.open();

        assert!(dialog.is_open());
        assert_eq!(dialog.state.selected(), Some(0));
    }

    // --- 選択移動 ---

    #[test]
    fn move_down_advances_selection_and_selected_provider() {
        let mut dialog = ProviderDialog::new();
        dialog.set_providers(sample_providers());
        dialog.open();
        assert_eq!(dialog.state.selected(), Some(0));

        dialog.move_down();

        assert_eq!(dialog.state.selected(), Some(1));
        let selected = dialog.selected_provider().expect("second provider");
        assert_eq!(selected.id, "openai");
        assert_eq!(selected.name, "OpenAI");
        assert_eq!(selected.status, ProviderStatus::Disconnected);
    }

    #[test]
    fn move_down_clamps_at_last_item() {
        let mut dialog = ProviderDialog::new();
        dialog.set_providers(sample_providers());
        dialog.open();

        for _ in 0..10 {
            dialog.move_down();
        }

        assert_eq!(dialog.state.selected(), Some(2));
        let selected = dialog.selected_provider().expect("last provider");
        assert_eq!(selected.name, "Google");
        assert_eq!(selected.status, ProviderStatus::Error);
    }

    #[test]
    fn move_up_moves_back_and_clamps_at_zero() {
        let mut dialog = ProviderDialog::new();
        dialog.set_providers(sample_providers());
        dialog.open();
        dialog.move_down();
        dialog.move_down();
        assert_eq!(dialog.state.selected(), Some(2));

        dialog.move_up();
        assert_eq!(dialog.state.selected(), Some(1));
        dialog.move_up();
        assert_eq!(dialog.state.selected(), Some(0));
        dialog.move_up();
        assert_eq!(dialog.state.selected(), Some(0));
    }

    #[test]
    fn move_down_with_empty_providers_stays_clamped() {
        let mut dialog = ProviderDialog::new();
        dialog.open();
        assert_eq!(dialog.state.selected(), Some(0));
        assert!(dialog.selected_provider().is_none());

        dialog.move_down();

        // 空 providers では len().saturating_sub(1) == 0 のため進まない。
        assert_eq!(dialog.state.selected(), Some(0));
        assert!(dialog.selected_provider().is_none());
    }

    #[test]
    fn move_down_before_set_providers_is_noop() {
        let mut dialog = ProviderDialog::new();
        assert_eq!(dialog.state.selected(), None);

        dialog.move_down();

        // new() 直後は未選択（None）のため move_down は何もしない。
        assert_eq!(dialog.state.selected(), None);
        assert!(dialog.selected_provider().is_none());
    }

    #[test]
    fn move_up_and_move_down_are_noop_without_selection() {
        let mut dialog = ProviderDialog::new();
        dialog.set_providers(sample_providers());
        dialog.state.select(None);
        assert_eq!(dialog.state.selected(), None);

        dialog.move_up();
        dialog.move_down();

        assert_eq!(dialog.state.selected(), None);
        assert!(dialog.selected_provider().is_none());
    }

    // --- render ---

    #[test]
    fn render_when_closed_draws_nothing() {
        let dialog = ProviderDialog::new();
        let theme = Theme::default();

        let text = render_to_string(80, 24, |frame| {
            dialog.render(frame, Rect::new(0, 0, 80, 24), &theme);
        });

        assert!(
            text.trim().is_empty(),
            "closed provider dialog must not draw any content"
        );
    }

    #[test]
    fn render_when_open_shows_title_and_provider_names() {
        let mut dialog = ProviderDialog::new();
        dialog.set_providers(sample_providers());
        dialog.open();
        let theme = Theme::default();

        let text = render_to_string(80, 24, |frame| {
            dialog.render(frame, Rect::new(0, 0, 80, 24), &theme);
        });

        assert!(text.contains("Connect Provider"), "dialog title");
        assert!(text.contains("Anthropic"), "first provider name");
        assert!(text.contains("OpenAI"), "second provider name");
        assert!(text.contains("Google"), "third provider name");
    }

    #[test]
    fn render_input_mode_shows_api_key_prompt_and_current_value() {
        let mut dialog = ProviderDialog::new();
        dialog.set_providers(sample_providers());
        dialog.open();
        dialog.input_mode = true;
        dialog.api_key_input = "test-key".to_string();
        let theme = Theme::default();

        let text = render_to_string(80, 24, |frame| {
            dialog.render(frame, Rect::new(0, 0, 80, 24), &theme);
        });

        assert!(text.contains("Connect Provider"), "input mode heading");
        assert!(text.contains("Enter API Key:"), "api key prompt");
        assert!(text.contains("test-key"), "current api key value");
        assert!(text.contains("Connect"), "connect hint");
        assert!(text.contains("Cancel"), "cancel hint");
    }
}

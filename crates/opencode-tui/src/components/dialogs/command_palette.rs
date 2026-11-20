use ratatui::{
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph},
    Frame,
};

use crate::command::{fuzzy_match, CommandAction};
use crate::context::MessageDensity;
use crate::theme::Theme;
use unicode_width::UnicodeWidthStr;

use super::DialogClickMap;

/// 検索行の `"> "` とカーソル `"▏"` の表示幅。
const SEARCH_PREFIX_WIDTH: usize = 2;
const SEARCH_CURSOR_WIDTH: usize = 1;

/// 検索/入力行の先頭を省略し、末尾（カーソル位置）が見える入力文字列を返す。
///
/// `"> " + query + "▏"` の幅が `inner_width` を超えた場合だけ先頭を切り詰める。
/// 幅は `UnicodeWidthStr` で測り、`char_indices` で Unicode 境界のみを省略するため
/// 全角文字の途中で切れない。担当パス内に置く共通ヘルパとして各ダイアログから利用する。
pub(crate) fn visible_query_tail(query: &str, inner_width: u16) -> &str {
    let width = usize::from(inner_width);
    let total_width = SEARCH_PREFIX_WIDTH + query.width() + SEARCH_CURSOR_WIDTH;
    if total_width <= width {
        return query;
    }

    let max_query_width = width.saturating_sub(SEARCH_PREFIX_WIDTH + SEARCH_CURSOR_WIDTH);
    let mut skip = 0usize;
    for (idx, ch) in query.char_indices() {
        if query[idx..].width() <= max_query_width {
            break;
        }
        skip = idx + ch.len_utf8();
    }
    &query[skip..]
}

#[derive(Clone, Debug)]
pub struct Command {
    pub action: CommandAction,
    pub title: String,
    pub keybind: Option<String>,
    pub category: String,
}

pub struct CommandPalette {
    commands: Vec<Command>,
    filtered: Vec<usize>,
    query: String,
    state: ListState,
    open: bool,
    click_map: DialogClickMap,
}

impl CommandPalette {
    pub fn new() -> Self {
        let commands = vec![
            Command {
                action: CommandAction::SubmitPrompt,
                title: "Submit prompt".to_string(),
                keybind: Some("enter".to_string()),
                category: "Prompt".to_string(),
            },
            Command {
                action: CommandAction::ClearPrompt,
                title: "Clear prompt".to_string(),
                keybind: Some("ctrl+u".to_string()),
                category: "Prompt".to_string(),
            },
            Command {
                action: CommandAction::PasteClipboard,
                title: "Paste from clipboard".to_string(),
                keybind: Some("ctrl+v".to_string()),
                category: "Prompt".to_string(),
            },
            Command {
                action: CommandAction::CopyPrompt,
                title: "Copy prompt to clipboard".to_string(),
                keybind: Some("ctrl+shift+c".to_string()),
                category: "Prompt".to_string(),
            },
            Command {
                action: CommandAction::CutPrompt,
                title: "Cut prompt to clipboard".to_string(),
                keybind: Some("ctrl+shift+x".to_string()),
                category: "Prompt".to_string(),
            },
            Command {
                action: CommandAction::HistoryPrevious,
                title: "Prompt history previous".to_string(),
                keybind: Some("alt+up".to_string()),
                category: "Prompt".to_string(),
            },
            Command {
                action: CommandAction::HistoryNext,
                title: "Prompt history next".to_string(),
                keybind: Some("alt+down".to_string()),
                category: "Prompt".to_string(),
            },
            Command {
                action: CommandAction::PromptStashPush,
                title: "Stash prompt".to_string(),
                keybind: None,
                category: "Prompt".to_string(),
            },
            Command {
                action: CommandAction::PromptStashList,
                title: "Open prompt stash".to_string(),
                keybind: None,
                category: "Prompt".to_string(),
            },
            Command {
                action: CommandAction::PromptSkillList,
                title: "Insert skill command".to_string(),
                keybind: None,
                category: "Prompt".to_string(),
            },
            Command {
                action: CommandAction::ToggleSidebar,
                title: "Toggle sidebar".to_string(),
                keybind: Some("ctrl+s".to_string()),
                category: "View".to_string(),
            },
            Command {
                action: CommandAction::ToggleHeader,
                title: "Hide header".to_string(),
                keybind: None,
                category: "View".to_string(),
            },
            Command {
                action: CommandAction::ToggleScrollbar,
                title: "Show scrollbar".to_string(),
                keybind: None,
                category: "View".to_string(),
            },
            Command {
                action: CommandAction::ToggleTips,
                title: "Hide tips".to_string(),
                keybind: None,
                category: "View".to_string(),
            },
            Command {
                action: CommandAction::ToggleThinking,
                title: "Toggle thinking".to_string(),
                keybind: Some("ctrl+t".to_string()),
                category: "View".to_string(),
            },
            Command {
                action: CommandAction::ToggleToolDetails,
                title: "Toggle tool details".to_string(),
                keybind: Some("alt+t".to_string()),
                category: "View".to_string(),
            },
            Command {
                action: CommandAction::ToggleDensity,
                title: "Switch to cozy density".to_string(),
                keybind: None,
                category: "View".to_string(),
            },
            Command {
                action: CommandAction::ToggleSemanticHighlight,
                title: "Disable semantic highlight".to_string(),
                keybind: None,
                category: "View".to_string(),
            },
            Command {
                action: CommandAction::SwitchSession,
                title: "Switch session".to_string(),
                keybind: None,
                category: "Session".to_string(),
            },
            Command {
                action: CommandAction::RenameSession,
                title: "Rename current session".to_string(),
                keybind: None,
                category: "Session".to_string(),
            },
            Command {
                action: CommandAction::ExportSession,
                title: "Export current session".to_string(),
                keybind: None,
                category: "Session".to_string(),
            },
            Command {
                action: CommandAction::SwitchModel,
                title: "Switch model".to_string(),
                keybind: Some("ctrl+m".to_string()),
                category: "Session".to_string(),
            },
            Command {
                action: CommandAction::SwitchAgent,
                title: "Open agent list".to_string(),
                keybind: None,
                category: "Session".to_string(),
            },
            Command {
                action: CommandAction::CycleVariant,
                title: "Cycle model variant".to_string(),
                keybind: Some("alt+v".to_string()),
                category: "Session".to_string(),
            },
            Command {
                action: CommandAction::NewSession,
                title: "New session".to_string(),
                keybind: Some("ctrl+n".to_string()),
                category: "Session".to_string(),
            },
            Command {
                action: CommandAction::SwitchTheme,
                title: "Switch theme".to_string(),
                keybind: None,
                category: "System".to_string(),
            },
            Command {
                action: CommandAction::ToggleAppearance,
                title: "Toggle appearance".to_string(),
                keybind: None,
                category: "System".to_string(),
            },
            Command {
                action: CommandAction::ViewStatus,
                title: "View status".to_string(),
                keybind: None,
                category: "System".to_string(),
            },
            Command {
                action: CommandAction::ToggleMcp,
                title: "Manage MCP servers".to_string(),
                keybind: None,
                category: "System".to_string(),
            },
            Command {
                action: CommandAction::ShowHelp,
                title: "Show help".to_string(),
                keybind: Some("ctrl+h".to_string()),
                category: "Help".to_string(),
            },
            Command {
                action: CommandAction::Exit,
                title: "Exit".to_string(),
                keybind: Some("ctrl+c".to_string()),
                category: "App".to_string(),
            },
        ];

        let filtered = (0..commands.len()).collect();
        let mut state = ListState::default();
        state.select(Some(0));

        Self {
            commands,
            filtered,
            query: String::new(),
            state,
            open: false,
            click_map: DialogClickMap::default(),
        }
    }

    pub fn open(&mut self) {
        self.open = true;
        self.query.clear();
        self.filtered = (0..self.commands.len()).collect();
        self.state.select(Some(0));
    }

    pub fn close(&mut self) {
        self.open = false;
    }

    pub fn is_open(&self) -> bool {
        self.open
    }

    pub fn handle_input(&mut self, c: char) {
        self.query.push(c);
        self.filter_commands();
    }

    pub fn handle_backspace(&mut self) {
        self.query.pop();
        self.filter_commands();
    }

    pub fn move_up(&mut self) {
        if let Some(selected) = self.state.selected() {
            if selected > 0 {
                self.state.select(Some(selected - 1));
            }
        }
    }

    pub fn move_down(&mut self) {
        if let Some(selected) = self.state.selected() {
            if selected < self.filtered.len().saturating_sub(1) {
                self.state.select(Some(selected + 1));
            }
        }
    }

    pub fn selected_command(&self) -> Option<&Command> {
        self.state
            .selected()
            .and_then(|i| self.filtered.get(i))
            .and_then(|&idx| self.commands.get(idx))
    }

    pub fn selected_action(&self) -> Option<CommandAction> {
        self.selected_command().map(|cmd| cmd.action.clone())
    }

    /// Select the item under the pointer. Returns `true` when the click landed
    /// on the dialog frame (so the app must not close it).
    pub fn handle_click(&mut self, col: u16, row: u16) -> bool {
        if let Some(index) = self.click_map.item_at(col, row) {
            self.state.select(Some(index));
        }
        self.click_map.contains(col, row)
    }

    pub fn sync_visibility_labels(
        &mut self,
        show_thinking: bool,
        show_tool_details: bool,
        density: MessageDensity,
        semantic_highlight: bool,
        show_header: bool,
        show_scrollbar: bool,
        tips_hidden: bool,
    ) {
        for command in &mut self.commands {
            if matches!(&command.action, CommandAction::ToggleHeader) {
                command.title = if show_header {
                    "Hide header".to_string()
                } else {
                    "Show header".to_string()
                };
            }
            if matches!(&command.action, CommandAction::ToggleScrollbar) {
                command.title = if show_scrollbar {
                    "Hide scrollbar".to_string()
                } else {
                    "Show scrollbar".to_string()
                };
            }
            if matches!(&command.action, CommandAction::ToggleTips) {
                command.title = if tips_hidden {
                    "Show tips".to_string()
                } else {
                    "Hide tips".to_string()
                };
            }
            if matches!(&command.action, CommandAction::ToggleThinking) {
                command.title = if show_thinking {
                    "Hide thinking".to_string()
                } else {
                    "Show thinking".to_string()
                };
            }
            if matches!(&command.action, CommandAction::ToggleToolDetails) {
                command.title = if show_tool_details {
                    "Hide tool details".to_string()
                } else {
                    "Show tool details".to_string()
                };
            }
            if matches!(&command.action, CommandAction::ToggleDensity) {
                command.title = match density {
                    MessageDensity::Compact => "Switch to cozy density".to_string(),
                    MessageDensity::Cozy => "Switch to compact density".to_string(),
                };
            }
            if matches!(&command.action, CommandAction::ToggleSemanticHighlight) {
                command.title = if semantic_highlight {
                    "Disable semantic highlight".to_string()
                } else {
                    "Enable semantic highlight".to_string()
                };
            }
        }

        self.filter_commands();
    }

    fn filter_commands(&mut self) {
        if self.query.is_empty() {
            self.filtered = (0..self.commands.len()).collect();
        } else {
            let mut scored: Vec<(usize, i32)> = self
                .commands
                .iter()
                .enumerate()
                .filter_map(|(i, cmd)| {
                    let title_score = fuzzy_match(&self.query, &cmd.title);
                    let cat_score = fuzzy_match(&self.query, &cmd.category);
                    let best = title_score.into_iter().chain(cat_score).max();
                    best.map(|s| (i, s))
                })
                .collect();
            scored.sort_by(|a, b| b.1.cmp(&a.1));
            self.filtered = scored.into_iter().map(|(i, _)| i).collect();
        }

        if self.filtered.is_empty() {
            self.state.select(None);
        } else {
            self.state.select(Some(0));
        }
    }

    pub fn render(&self, frame: &mut Frame, area: Rect, theme: &Theme) {
        if !self.open {
            return;
        }

        let dialog_width = 60;
        let dialog_height = (self.filtered.len() + 4).min(20) as u16;

        let dialog_area = centered_rect(dialog_width, dialog_height, area);
        self.click_map.set_area(dialog_area);

        frame.render_widget(Clear, dialog_area);

        let block = Block::default()
            .title(Span::styled(
                " Commands ",
                Style::default()
                    .fg(theme.primary)
                    .add_modifier(Modifier::BOLD),
            ))
            .borders(Borders::ALL)
            .border_style(Style::default().fg(theme.border))
            .style(Style::default().bg(theme.background_panel));

        let inner_area = super::dialog_inner(block.inner(dialog_area));
        frame.render_widget(block, dialog_area);

        let search_line = Line::from(vec![
            Span::styled("> ", Style::default().fg(theme.primary)),
            Span::styled(
                visible_query_tail(&self.query, inner_area.width),
                Style::default().fg(theme.text),
            ),
            Span::styled("▏", Style::default().fg(theme.primary)),
        ]);

        let search_paragraph = Paragraph::new(search_line);
        frame.render_widget(
            search_paragraph,
            Rect {
                x: inner_area.x,
                y: inner_area.y,
                width: inner_area.width,
                height: 1,
            },
        );

        let items: Vec<ListItem> = self
            .filtered
            .iter()
            .filter_map(|&idx| {
                self.commands.get(idx).map(|cmd| {
                    let style = if Some(idx)
                        == self
                            .state
                            .selected()
                            .and_then(|s| self.filtered.get(s))
                            .copied()
                    {
                        Style::default()
                            .fg(theme.text)
                            .bg(theme.background_element)
                            .add_modifier(Modifier::BOLD)
                    } else {
                        Style::default().fg(theme.text)
                    };

                    let mut spans = vec![Span::styled(&cmd.title, style)];

                    if let Some(ref keybind) = cmd.keybind {
                        spans.push(Span::raw("  "));
                        spans.push(Span::styled(
                            keybind.clone(),
                            Style::default().fg(theme.text_muted),
                        ));
                    }

                    ListItem::new(Line::from(spans))
                })
            })
            .collect();

        let list = List::new(items).style(Style::default().fg(theme.text));

        let list_area = Rect {
            x: inner_area.x,
            y: inner_area.y + 2,
            width: inner_area.width,
            height: inner_area.height.saturating_sub(2),
        };

        let mut state = self.state;
        frame.render_stateful_widget(list, list_area, &mut state);
        self.click_map
            .set_list(list_area, state.offset(), self.filtered.len());
    }
}

impl Default for CommandPalette {
    fn default() -> Self {
        Self::new()
    }
}

fn centered_rect(width: u16, height: u16, area: Rect) -> Rect {
    super::centered_rect(width, height, area)
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

    /// テスト用 Command を構築するヘルパー。
    fn command(
        action: CommandAction,
        title: &str,
        keybind: Option<&str>,
        category: &str,
    ) -> Command {
        Command {
            action,
            title: title.to_string(),
            keybind: keybind.map(str::to_string),
            category: category.to_string(),
        }
    }

    /// 確実に一致しないクエリ（fuzzy_match が None を返す）を順に入力するヘルパー。
    fn type_no_match(palette: &mut CommandPalette) {
        for c in "zzz_qqq_xxx".chars() {
            palette.handle_input(c);
        }
    }

    // --- Command データ保持 ---

    #[test]
    fn command_stores_all_fields() {
        let cmd = command(
            CommandAction::SubmitPrompt,
            "Submit prompt",
            Some("enter"),
            "Prompt",
        );

        assert_eq!(cmd.title, "Submit prompt");
        assert_eq!(cmd.keybind.as_deref(), Some("enter"));
        assert_eq!(cmd.category, "Prompt");
        assert!(matches!(&cmd.action, CommandAction::SubmitPrompt));
    }

    #[test]
    fn command_allows_missing_keybind() {
        let cmd = command(
            CommandAction::PromptStashPush,
            "Stash prompt",
            None,
            "Prompt",
        );

        assert!(cmd.keybind.is_none());
        assert_eq!(cmd.title, "Stash prompt");
        assert!(matches!(&cmd.action, CommandAction::PromptStashPush));
    }

    #[test]
    fn command_clone_preserves_all_fields() {
        let original = command(CommandAction::Exit, "Exit", Some("ctrl+c"), "App");

        let cloned = original.clone();

        assert_eq!(cloned.title, original.title);
        assert_eq!(cloned.keybind, original.keybind);
        assert_eq!(cloned.category, original.category);
        assert!(matches!(&cloned.action, CommandAction::Exit));
    }

    #[test]
    fn new_populates_expected_default_commands() {
        let palette = CommandPalette::new();

        assert!(palette.commands.len() > 20);
        assert!(palette.commands.iter().any(|c| {
            matches!(&c.action, CommandAction::SubmitPrompt) && c.title == "Submit prompt"
        }));
        assert!(palette
            .commands
            .iter()
            .any(|c| matches!(&c.action, CommandAction::Exit) && c.title == "Exit"));
        assert!(palette
            .commands
            .iter()
            .any(|c| matches!(&c.action, CommandAction::ShowHelp) && c.title == "Show help"));
    }

    #[test]
    fn new_uses_current_keybinds_for_view_toggles_and_variant() {
        let palette = CommandPalette::new();
        let keybind_of = |title: &str| {
            palette
                .commands
                .iter()
                .find(|c| c.title == title)
                .and_then(|c| c.keybind.as_deref())
        };

        // R5: keybind.rs の割当と表示を一致させる（alt+v / ctrl+t / alt+t）。
        assert_eq!(keybind_of("Cycle model variant"), Some("alt+v"));
        assert_eq!(keybind_of("Toggle thinking"), Some("ctrl+t"));
        assert_eq!(keybind_of("Toggle tool details"), Some("alt+t"));
    }

    // --- new / Default ---

    #[test]
    fn new_starts_with_full_filter_and_first_selected_and_closed() {
        let palette = CommandPalette::new();

        assert!(palette.query.is_empty());
        assert_eq!(palette.filtered.len(), palette.commands.len());
        assert_eq!(palette.filtered.first().copied(), Some(0));
        assert_eq!(palette.state.selected(), Some(0));
        assert!(!palette.is_open());
    }

    #[test]
    fn default_matches_new_state() {
        let palette = CommandPalette::default();

        assert!(palette.query.is_empty());
        assert_eq!(palette.filtered.len(), palette.commands.len());
        assert_eq!(palette.state.selected(), Some(0));
        assert!(!palette.is_open());
    }

    // --- open / close / is_open ---

    #[test]
    fn open_sets_open_state_and_resets_query_filter_and_selection() {
        let mut palette = CommandPalette::new();
        for c in "exit".chars() {
            palette.handle_input(c);
        }
        palette.move_down();
        assert!(palette.filtered.len() < palette.commands.len());
        assert!(!palette.is_open());

        palette.open();

        assert!(palette.is_open());
        assert!(palette.query.is_empty());
        assert_eq!(palette.filtered.len(), palette.commands.len());
        assert_eq!(palette.state.selected(), Some(0));
    }

    #[test]
    fn close_clears_open_state_and_preserves_data() {
        let mut palette = CommandPalette::new();
        palette.open();
        palette.move_down();
        assert!(palette.is_open());
        assert_eq!(palette.state.selected(), Some(1));

        palette.close();

        assert!(!palette.is_open());
        assert_eq!(palette.state.selected(), Some(1));
        assert_eq!(palette.filtered.len(), palette.commands.len());
    }

    #[test]
    fn open_close_cycles_toggle_state() {
        let mut palette = CommandPalette::new();

        for _ in 0..3 {
            palette.open();
            assert!(palette.is_open());
            palette.close();
            assert!(!palette.is_open());
        }
    }

    #[test]
    fn close_is_idempotent_from_new() {
        let mut palette = CommandPalette::new();

        palette.close();
        palette.close();

        assert!(!palette.is_open());
        assert!(palette.query.is_empty());
    }

    #[test]
    fn open_is_idempotent() {
        let mut palette = CommandPalette::new();

        palette.open();
        palette.open();

        assert!(palette.is_open());
        assert_eq!(palette.state.selected(), Some(0));
        assert!(palette.query.is_empty());
    }

    // --- クエリ入力とフィルタ (handle_input / handle_backspace) ---

    #[test]
    fn handle_input_appends_chars_to_query() {
        let mut palette = CommandPalette::new();

        for c in "exit".chars() {
            palette.handle_input(c);
        }

        assert_eq!(palette.query, "exit");
    }

    #[test]
    fn handle_input_filters_to_matching_command() {
        let mut palette = CommandPalette::new();
        let total = palette.commands.len();

        for c in "exit".chars() {
            palette.handle_input(c);
        }

        assert!(palette.filtered.len() < total);
        assert!(!palette.filtered.is_empty());
        let selected = palette.selected_command().expect("a command should match");
        assert_eq!(selected.title, "Exit");
        assert!(matches!(&selected.action, CommandAction::Exit));
    }

    #[test]
    fn handle_input_with_no_match_empties_filter_and_selection() {
        let mut palette = CommandPalette::new();

        type_no_match(&mut palette);

        assert_eq!(palette.query, "zzz_qqq_xxx");
        assert!(palette.filtered.is_empty());
        assert_eq!(palette.state.selected(), None);
        assert!(palette.selected_command().is_none());
        assert!(palette.selected_action().is_none());
    }

    #[test]
    fn handle_backspace_removes_last_char_and_refilters() {
        let mut palette = CommandPalette::new();

        for c in "exit".chars() {
            palette.handle_input(c);
        }
        assert_eq!(palette.query, "exit");
        let narrow_len = palette.filtered.len();

        palette.handle_backspace();

        assert_eq!(palette.query, "exi");
        assert!(palette.filtered.len() >= narrow_len);
        assert_eq!(palette.state.selected(), Some(0));
    }

    #[test]
    fn handle_backspace_until_empty_restores_full_list() {
        let mut palette = CommandPalette::new();
        let total = palette.commands.len();

        for c in "exit".chars() {
            palette.handle_input(c);
        }
        assert!(palette.filtered.len() < total);

        for _ in 0..4 {
            palette.handle_backspace();
        }

        assert!(palette.query.is_empty());
        assert_eq!(palette.filtered.len(), total);
        assert_eq!(palette.state.selected(), Some(0));
    }

    #[test]
    fn handle_backspace_on_empty_query_is_noop() {
        let mut palette = CommandPalette::new();
        let total = palette.commands.len();

        palette.handle_backspace();

        assert!(palette.query.is_empty());
        assert_eq!(palette.filtered.len(), total);
        assert_eq!(palette.state.selected(), Some(0));
    }

    // --- 選択移動の状態遷移 (move_up / move_down / selected_command / selected_action) ---

    #[test]
    fn move_down_advances_selection_and_selected_command() {
        let mut palette = CommandPalette::new();
        palette.open();
        assert_eq!(palette.state.selected(), Some(0));
        let first_title = palette
            .selected_command()
            .expect("first command should be selected")
            .title
            .clone();

        palette.move_down();

        assert_eq!(palette.state.selected(), Some(1));
        let second_title = palette
            .selected_command()
            .expect("second command should be selected")
            .title
            .clone();
        assert_ne!(second_title, first_title);
    }

    #[test]
    fn move_down_clamps_at_last_item() {
        let mut palette = CommandPalette::new();
        palette.open();
        let last = palette.commands.len().saturating_sub(1);

        for _ in 0..(palette.commands.len() + 5) {
            palette.move_down();
        }

        assert_eq!(palette.state.selected(), Some(last));
    }

    #[test]
    fn move_up_moves_back_and_clamps_at_zero() {
        let mut palette = CommandPalette::new();
        palette.open();
        palette.move_down();
        palette.move_down();
        assert_eq!(palette.state.selected(), Some(2));

        palette.move_up();
        assert_eq!(palette.state.selected(), Some(1));
        palette.move_up();
        assert_eq!(palette.state.selected(), Some(0));
        palette.move_up();
        assert_eq!(palette.state.selected(), Some(0));
    }

    #[test]
    fn move_up_and_move_down_are_noop_without_selection() {
        let mut palette = CommandPalette::new();
        type_no_match(&mut palette);
        assert!(palette.filtered.is_empty());
        assert_eq!(palette.state.selected(), None);

        palette.move_up();
        palette.move_down();

        assert_eq!(
            palette.state.selected(),
            None,
            "empty list must not gain a selection"
        );
        assert!(palette.selected_command().is_none());
    }

    #[test]
    fn move_down_is_noop_when_filtered_is_empty_even_with_selection() {
        let mut palette = CommandPalette::new();
        palette.state.select(Some(0));
        palette.filtered.clear();
        assert_eq!(palette.state.selected(), Some(0));
        assert!(palette.filtered.is_empty());

        palette.move_down();

        // filtered が空のとき saturating_sub(1) == 0 となり、選択はクランプされ据え置きになる。
        assert_eq!(palette.state.selected(), Some(0));
    }

    #[test]
    fn selected_action_matches_selected_command() {
        let palette = CommandPalette::new();

        let cmd = palette
            .selected_command()
            .expect("a command should be selected");
        assert_eq!(cmd.title, "Submit prompt");
        assert!(matches!(&cmd.action, CommandAction::SubmitPrompt));
        assert!(matches!(
            palette.selected_action(),
            Some(CommandAction::SubmitPrompt)
        ));
    }

    #[test]
    fn selected_action_changes_after_move() {
        let mut palette = CommandPalette::new();
        palette.open();
        assert!(matches!(
            palette.selected_action(),
            Some(CommandAction::SubmitPrompt)
        ));

        palette.move_down();

        assert!(matches!(
            palette.selected_action(),
            Some(CommandAction::ClearPrompt)
        ));
    }

    #[test]
    fn selected_command_and_action_follow_filter() {
        let mut palette = CommandPalette::new();
        for c in "exit".chars() {
            palette.handle_input(c);
        }

        let cmd = palette.selected_command().expect("filtered command");
        assert_eq!(cmd.title, "Exit");
        assert!(matches!(&cmd.action, CommandAction::Exit));
        assert!(matches!(
            palette.selected_action(),
            Some(CommandAction::Exit)
        ));
    }

    #[test]
    fn selected_command_and_action_none_without_selection() {
        let mut palette = CommandPalette::new();
        type_no_match(&mut palette);

        assert!(palette.selected_command().is_none());
        assert!(palette.selected_action().is_none());
    }

    // --- sync_visibility_labels ---

    #[test]
    fn sync_visibility_labels_updates_titles_and_refilters() {
        let mut palette = CommandPalette::new();

        let mode = MessageDensity::Cozy;
        palette.sync_visibility_labels(false, false, mode, false, false, true, true);

        let header = palette
            .commands
            .iter()
            .find(|c| matches!(&c.action, CommandAction::ToggleHeader))
            .expect("ToggleHeader command exists");
        assert_eq!(header.title, "Show header");

        let density = palette
            .commands
            .iter()
            .find(|c| matches!(&c.action, CommandAction::ToggleDensity))
            .expect("ToggleDensity command exists");
        assert_eq!(density.title, "Switch to compact density");

        let scrollbar = palette
            .commands
            .iter()
            .find(|c| matches!(&c.action, CommandAction::ToggleScrollbar))
            .expect("ToggleScrollbar command exists");
        assert_eq!(scrollbar.title, "Hide scrollbar");

        let tips = palette
            .commands
            .iter()
            .find(|c| matches!(&c.action, CommandAction::ToggleTips))
            .expect("ToggleTips command exists");
        assert_eq!(tips.title, "Show tips");

        assert_eq!(palette.state.selected(), Some(0));
        assert_eq!(palette.filtered.len(), palette.commands.len());
    }

    #[test]
    fn sync_visibility_labels_compact_density_and_thinking_titles() {
        let mut palette = CommandPalette::new();

        let mode = MessageDensity::Compact;
        palette.sync_visibility_labels(true, true, mode, true, true, false, false);

        let thinking = palette
            .commands
            .iter()
            .find(|c| matches!(&c.action, CommandAction::ToggleThinking))
            .expect("ToggleThinking command exists");
        assert_eq!(thinking.title, "Hide thinking");

        let density = palette
            .commands
            .iter()
            .find(|c| matches!(&c.action, CommandAction::ToggleDensity))
            .expect("ToggleDensity command exists");
        assert_eq!(density.title, "Switch to cozy density");

        let semantic = palette
            .commands
            .iter()
            .find(|c| matches!(&c.action, CommandAction::ToggleSemanticHighlight))
            .expect("ToggleSemanticHighlight command exists");
        assert_eq!(semantic.title, "Disable semantic highlight");
    }

    // --- render ---

    #[test]
    fn render_when_closed_draws_nothing() {
        let palette = CommandPalette::new();
        let theme = Theme::default();
        assert!(!palette.is_open());

        let text = render_to_string(80, 24, |frame| {
            palette.render(frame, Rect::new(0, 0, 80, 24), &theme);
        });

        assert!(
            text.trim().is_empty(),
            "closed command palette must not draw any content"
        );
    }

    #[test]
    fn render_when_open_shows_query_and_command_titles() {
        let mut palette = CommandPalette::new();
        palette.open();
        for c in "help".chars() {
            palette.handle_input(c);
        }
        let theme = Theme::default();

        let text = render_to_string(80, 24, |frame| {
            palette.render(frame, Rect::new(0, 0, 80, 24), &theme);
        });

        assert!(text.contains("Commands"), "dialog title");
        assert!(text.contains("help"), "query text");
        assert!(text.contains("Show help"), "filtered command title");
    }

    // --- 検索行の水平スクロール ---

    #[test]
    fn visible_query_tail_keeps_short_query_unchanged() {
        assert_eq!(visible_query_tail("help", 10), "help");
        // "> "(2) + "help"(4) + "▏"(1) == 7
        assert_eq!(visible_query_tail("help", 7), "help");
    }

    #[test]
    fn visible_query_tail_drops_head_and_keeps_cursor_side() {
        // inner_width 12 -> 入力に使える幅は 12 - 2 - 1 = 9
        assert_eq!(visible_query_tail("abcdefghijklmnop", 12), "hijklmnop");
    }

    #[test]
    fn visible_query_tail_does_not_split_wide_chars() {
        let tail = visible_query_tail("日本語テスト", 12);
        assert_eq!(tail, "語テスト");
        assert_eq!(tail.width(), 8);
    }

    #[test]
    fn render_with_long_query_shows_the_tail_and_cursor() {
        let mut palette = CommandPalette::new();
        palette.open();
        let long_query = format!("HEADMARK{}TAILMARK", "x".repeat(80));
        for c in long_query.chars() {
            palette.handle_input(c);
        }
        let theme = Theme::default();

        let text = render_to_string(80, 24, |frame| {
            palette.render(frame, Rect::new(0, 0, 80, 24), &theme);
        });

        assert!(
            text.contains("TAILMARK"),
            "the query tail next to the cursor must stay visible"
        );
        assert!(
            !text.contains("HEADMARK"),
            "the overflowing query head must be scrolled out"
        );
        assert!(text.contains("▏"), "cursor must remain visible");
    }
}

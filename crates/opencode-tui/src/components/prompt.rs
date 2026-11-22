use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Padding, Paragraph, Wrap},
    Frame,
};
use std::collections::hash_map::DefaultHasher;
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};
use unicode_width::UnicodeWidthChar;

use crate::context::{AppContext, SessionStatus};
use crate::file_index::FileIndex;
use crate::theme::Theme;

use super::spinner::{KnightRiderSpinner, SpinnerMode, TaskKind};

const MAX_HISTORY_ENTRIES: usize = 200;
const MAX_STASH_ENTRIES: usize = 50;
const MAX_FRECENCY_ENTRIES: usize = 1000;
const PROMPT_MIN_INPUT_LINES: u16 = 1;
const PROMPT_MAX_INPUT_LINES: u16 = 6;
const SHELL_PLACEHOLDER: &str = "Run a command... \"ls -la\"";
const INTERRUPT_CONFIRM_WINDOW_SECS: u64 = 5;
const FILE_INDEX_MAX_DEPTH: usize = 8;
const FILE_SUGGESTION_LIMIT: usize = 20;
const PROMPT_BLOCK_PAD_LEFT: u16 = 1;
const PROMPT_BLOCK_PAD_RIGHT: u16 = 1;
const PROMPT_BLOCK_PAD_TOP: u16 = 1;
const PROMPT_BLOCK_PAD_BOTTOM: u16 = 1;
const PROMPT_LINE_H_INSET: u16 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PromptMode {
    Normal,
    Shell,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct PromptStashEntry {
    pub input: String,
    pub created_at: i64,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, Default)]
struct FrecencyEntry {
    frequency: u64,
    last_used: i64,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, Default)]
struct HistoryStore {
    entries: Vec<String>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, Default)]
struct FrecencyStore {
    entries: HashMap<String, FrecencyEntry>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, Default)]
struct StashStore {
    entries: Vec<PromptStashEntry>,
}

pub struct Prompt {
    context: Arc<AppContext>,
    input: String,
    cursor_position: usize,
    focused: bool,
    placeholder: String,
    history: Vec<String>,
    history_index: Option<usize>,
    history_draft: Option<String>,
    frecency: HashMap<String, FrecencyEntry>,
    stash: Vec<PromptStashEntry>,
    suggestions: Vec<String>,
    suggestion_index: Option<usize>,
    known_commands: Vec<String>,
    known_agents: Vec<String>,
    known_skills: Vec<String>,
    history_path: PathBuf,
    frecency_path: PathBuf,
    stash_path: PathBuf,
    file_index: FileIndex,
    spinner: KnightRiderSpinner,
    mode: PromptMode,
    interrupt_press_count: u8,
    last_interrupt_time: Option<Instant>,
    /// Ctrl+X の leader キー入力待ち中か（app から同期される）。
    leader_active: bool,
}

impl Prompt {
    pub fn new(context: Arc<AppContext>) -> Self {
        let state_dir = prompt_state_dir();
        let history_path = state_dir.join("prompt-history.json");
        let frecency_path = state_dir.join("prompt-frecency.json");
        let stash_path = state_dir.join("prompt-stash.json");

        let history = load_history(&history_path);
        let frecency = load_frecency(&frecency_path);
        let stash = load_stash(&stash_path);

        let spinner_color = {
            let theme = context.theme.read();
            let agent = context.current_agent.read();
            prompt_agent_color(&theme, agent.as_str())
        };

        let mut spinner = KnightRiderSpinner::with_color(spinner_color);
        spinner.set_mode(spinner_mode_from_env());

        let mut prompt = Self {
            context,
            input: String::new(),
            cursor_position: 0,
            focused: true,
            placeholder: "Ask anything...".to_string(),
            history,
            history_index: None,
            history_draft: None,
            frecency,
            stash,
            suggestions: Vec::new(),
            suggestion_index: None,
            known_commands: vec![
                "/help".to_string(),
                "/model".to_string(),
                "/agent".to_string(),
                "/status".to_string(),
                "/session".to_string(),
                "/sessions".to_string(),
                "/mcp".to_string(),
                "/mcps".to_string(),
                "/skill".to_string(),
                "/export".to_string(),
                "/stash".to_string(),
                "/new".to_string(),
                "/clear".to_string(),
                "/share".to_string(),
                "/unshare".to_string(),
                "/rename".to_string(),
                "/fork".to_string(),
                "/compact".to_string(),
                "/timeline".to_string(),
                "/undo".to_string(),
                "/redo".to_string(),
                "/copy".to_string(),
                "/themes".to_string(),
                "/timestamps".to_string(),
                "/tips.toggle".to_string(),
                "/tips".to_string(),
                "/thinking".to_string(),
                "/density".to_string(),
                "/highlight".to_string(),
                "/sidebar".to_string(),
                "/command".to_string(),
                "/connect".to_string(),
                "/editor".to_string(),
                "/exit".to_string(),
                "/quit".to_string(),
            ],
            known_agents: vec![
                "build".to_string(),
                "plan".to_string(),
                "general".to_string(),
                "explore".to_string(),
                "compaction".to_string(),
                "title".to_string(),
            ],
            known_skills: Vec::new(),
            history_path,
            frecency_path,
            stash_path,
            file_index: FileIndex::default(),
            spinner,
            mode: PromptMode::Normal,
            interrupt_press_count: 0,
            last_interrupt_time: None,
            leader_active: false,
        };
        prompt.recompute_suggestions();
        prompt
    }

    pub fn with_placeholder(mut self, placeholder: &str) -> Self {
        self.placeholder = placeholder.to_string();
        self
    }

    pub fn set_agent_suggestions(&mut self, agents: Vec<String>) {
        if agents.is_empty() {
            return;
        }
        self.known_agents = dedup_sort(agents);
        self.recompute_suggestions();
    }

    pub fn set_skill_suggestions(&mut self, skills: Vec<String>) {
        self.known_skills = dedup_sort(skills);
        self.recompute_suggestions();
    }

    pub fn render(&self, frame: &mut Frame, area: Rect) {
        let theme = self.context.theme.read();
        let agent = self.context.current_agent.read();
        let model = self.context.current_model.read();
        let variant = self.context.current_model_variant();
        let animations_enabled = *self.context.animations_enabled.read();

        let highlight_color = prompt_agent_color(&theme, agent.as_str());
        let active_color = if matches!(self.mode, PromptMode::Shell) {
            theme.primary
        } else {
            highlight_color
        };
        let placeholder = if matches!(self.mode, PromptMode::Shell) {
            SHELL_PLACEHOLDER
        } else {
            self.placeholder.as_str()
        };

        let max_content_lines = area
            .height
            .saturating_sub(3)
            .saturating_sub(PROMPT_BLOCK_PAD_TOP)
            .saturating_sub(PROMPT_BLOCK_PAD_BOTTOM)
            .max(PROMPT_MIN_INPUT_LINES);
        let content_lines = self.input_display_lines(area.width).min(max_content_lines);
        let input_lines = content_lines
            .saturating_add(PROMPT_BLOCK_PAD_TOP)
            .saturating_add(PROMPT_BLOCK_PAD_BOTTOM);
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(input_lines),
                Constraint::Length(1),
                Constraint::Length(1),
                Constraint::Length(1),
            ])
            .split(area);

        let border_set = ratatui::symbols::border::Set {
            top_left: " ",
            top_right: " ",
            bottom_left: " ",
            bottom_right: " ",
            vertical_left: "┃",
            vertical_right: " ",
            horizontal_top: " ",
            horizontal_bottom: " ",
        };

        let paragraph = if self.input.is_empty() {
            Paragraph::new(Line::from(Span::styled(
                placeholder,
                Style::default().fg(theme.text_muted),
            )))
            .block(
                Block::default()
                    .borders(Borders::LEFT)
                    .border_set(border_set)
                    .border_style(Style::default().fg(active_color))
                    .padding(Padding::new(
                        PROMPT_BLOCK_PAD_LEFT,
                        PROMPT_BLOCK_PAD_RIGHT,
                        PROMPT_BLOCK_PAD_TOP,
                        PROMPT_BLOCK_PAD_BOTTOM,
                    ))
                    .style(Style::default().bg(theme.background_element)),
            )
            .style(Style::default().fg(if self.focused {
                theme.text
            } else {
                theme.text_muted
            }))
        } else {
            Paragraph::new(self.input.clone())
                .block(
                    Block::default()
                        .borders(Borders::LEFT)
                        .border_set(border_set)
                        .border_style(Style::default().fg(active_color))
                        .padding(Padding::new(
                            PROMPT_BLOCK_PAD_LEFT,
                            PROMPT_BLOCK_PAD_RIGHT,
                            PROMPT_BLOCK_PAD_TOP,
                            PROMPT_BLOCK_PAD_BOTTOM,
                        ))
                        .style(Style::default().bg(theme.background_element)),
                )
                .wrap(Wrap { trim: false })
                .style(Style::default().fg(if self.focused {
                    theme.text
                } else {
                    theme.text_muted
                }))
        };

        frame.render_widget(paragraph, chunks[0]);

        let mut info_parts = vec![
            Span::styled(
                if matches!(self.mode, PromptMode::Shell) {
                    "shell"
                } else {
                    agent.as_str()
                },
                Style::default().fg(active_color).bold(),
            ),
            Span::raw("  "),
        ];

        if let Some(m) = model.as_ref() {
            let provider = self.context.current_provider.read();
            if let Some(ref p) = *provider {
                info_parts.push(Span::styled(m.clone(), Style::default().fg(theme.text)));
                info_parts.push(Span::styled(
                    format!(" {p}"),
                    Style::default().fg(theme.text_muted),
                ));
            } else {
                info_parts.push(Span::styled(m.clone(), Style::default().fg(theme.text)));
            }
            if let Some(ref v) = variant {
                info_parts.push(Span::styled(" · ", Style::default().fg(theme.text_muted)));
                info_parts.push(Span::styled(
                    v.clone(),
                    Style::default().fg(theme.warning).bold(),
                ));
            }
        }

        render_prompt_continuation_row(frame, chunks[1], active_color, theme.background_element);
        let info_row = row_content_area(chunks[1], PROMPT_LINE_H_INSET);
        let info_line = Line::from(info_parts);
        let info_paragraph =
            Paragraph::new(info_line).style(Style::default().bg(theme.background_element));
        frame.render_widget(info_paragraph, info_row);

        let spinner_row = inset_horizontal(chunks[2], PROMPT_LINE_H_INSET);
        frame.render_widget(
            Paragraph::new("").style(Style::default().bg(theme.background)),
            spinner_row,
        );
        let spinner_chunks = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Length(9), Constraint::Min(0)])
            .split(spinner_row);

        self.spinner.render(
            frame,
            spinner_chunks[0],
            animations_enabled,
            theme.background,
        );

        let status_line = Paragraph::new(self.render_status_line(&theme))
            .style(Style::default().bg(theme.background));
        frame.render_widget(
            status_line,
            inset_horizontal(chunks[3], PROMPT_LINE_H_INSET),
        );
    }

    pub fn tick_spinner(&mut self, delta_ms: u64) -> bool {
        let spinner_changed = self.spinner.advance(delta_ms);
        let interrupt_changed = self.maybe_reset_interrupt_confirmation();
        spinner_changed || interrupt_changed
    }

    pub fn set_spinner_active(&mut self, active: bool) {
        self.spinner.set_active(active);
        if !active {
            self.reset_interrupt_confirmation();
        }
    }

    pub fn spinner_active(&self) -> bool {
        self.spinner.is_active()
    }

    pub fn set_spinner_task_kind(&mut self, task_kind: TaskKind) {
        self.spinner.set_task_kind(task_kind);
    }

    pub fn spinner_task_kind(&self) -> TaskKind {
        self.spinner.task_kind()
    }

    pub fn set_spinner_color(&mut self, color: Color) {
        self.spinner.set_color(color);
    }

    pub fn handle_key(&mut self, key: crossterm::event::KeyEvent) -> bool {
        use crossterm::event::{KeyCode, KeyModifiers};

        match key.code {
            KeyCode::Char(c) => {
                if c == '!'
                    && key.modifiers.is_empty()
                    && matches!(self.mode, PromptMode::Normal)
                    && self.cursor_position == 0
                    && self.input.is_empty()
                {
                    self.mode = PromptMode::Shell;
                    return false;
                }
                self.input.insert(self.cursor_position, c);
                self.cursor_position += c.len_utf8();
                self.reset_history_cursor();
                self.recompute_suggestions();
            }
            KeyCode::Backspace => {
                if matches!(self.mode, PromptMode::Shell) && self.cursor_position == 0 {
                    self.mode = PromptMode::Normal;
                    return false;
                }
                if let Some(prev) = prev_grapheme_boundary(&self.input, self.cursor_position) {
                    self.input.replace_range(prev..self.cursor_position, "");
                    self.cursor_position = prev;
                    self.reset_history_cursor();
                    self.recompute_suggestions();
                }
            }
            KeyCode::Delete => {
                if let Some(next) = next_grapheme_boundary(&self.input, self.cursor_position) {
                    self.input.replace_range(self.cursor_position..next, "");
                    self.reset_history_cursor();
                    self.recompute_suggestions();
                }
            }
            KeyCode::Left => {
                if let Some(prev) = prev_grapheme_boundary(&self.input, self.cursor_position) {
                    self.cursor_position = prev;
                }
            }
            KeyCode::Right => {
                if let Some(next) = next_grapheme_boundary(&self.input, self.cursor_position) {
                    self.cursor_position = next;
                }
            }
            KeyCode::Home => {
                self.cursor_position = 0;
            }
            KeyCode::End => {
                self.cursor_position = self.input.len();
            }
            KeyCode::Tab => {
                if key.modifiers.contains(KeyModifiers::SHIFT) {
                    self.apply_autocomplete_previous();
                } else {
                    self.apply_autocomplete_next();
                }
            }
            KeyCode::BackTab => {
                self.apply_autocomplete_previous();
            }
            KeyCode::Enter if !self.input.is_empty() => {
                return true;
            }
            KeyCode::Esc => {
                if matches!(self.mode, PromptMode::Shell) {
                    self.mode = PromptMode::Normal;
                }
            }
            KeyCode::Up => {
                self.history_previous();
            }
            KeyCode::Down => {
                self.history_next();
            }
            _ => {}
        }

        false
    }

    pub fn history_previous_entry(&mut self) {
        self.history_previous();
    }

    pub fn history_next_entry(&mut self) {
        self.history_next();
    }

    pub fn autocomplete_next(&mut self) {
        self.apply_autocomplete_next();
    }

    pub fn autocomplete_previous(&mut self) {
        self.apply_autocomplete_previous();
    }

    pub fn get_input(&self) -> &str {
        &self.input
    }

    /// 未送信の入力があるか。送信済みの履歴は含まない。
    pub fn has_input(&self) -> bool {
        !self.input.is_empty()
    }

    /// 現在の入力トークンに対して補完候補が存在するか。
    pub fn has_completion_candidates(&self) -> bool {
        !self.suggestions.is_empty()
    }

    pub fn cursor_position(&self) -> usize {
        self.cursor_position
    }

    pub fn take_input(&mut self) -> String {
        let input = std::mem::take(&mut self.input);
        let trimmed = input.trim();
        if !trimmed.is_empty() {
            self.push_history(input.clone());
            self.bump_frecency(trimmed);
            if let Some(first) = trimmed.split_whitespace().next() {
                if first.starts_with('/') || first.starts_with('@') {
                    self.bump_frecency(first);
                }
            }
            store_history(&self.history_path, &self.history);
            store_frecency(&self.frecency_path, &self.frecency);
        }
        self.cursor_position = 0;
        self.history_index = None;
        self.history_draft = None;
        self.suggestions.clear();
        self.suggestion_index = None;
        self.mode = PromptMode::Normal;
        self.reset_interrupt_confirmation();
        input
    }

    pub fn clear(&mut self) {
        self.input.clear();
        self.cursor_position = 0;
        self.history_index = None;
        self.history_draft = None;
        self.suggestions.clear();
        self.suggestion_index = None;
        self.mode = PromptMode::Normal;
        self.reset_interrupt_confirmation();
    }

    pub fn set_focused(&mut self, focused: bool) {
        self.focused = focused;
    }

    pub fn is_focused(&self) -> bool {
        self.focused
    }

    pub fn set_input(&mut self, input: String) {
        self.input = input;
        self.cursor_position = self.input.len();
        self.history_index = None;
        self.history_draft = None;
        self.reset_interrupt_confirmation();
        self.recompute_suggestions();
    }

    pub fn mode(&self) -> PromptMode {
        self.mode
    }

    pub fn is_shell_mode(&self) -> bool {
        matches!(self.mode, PromptMode::Shell)
    }

    pub fn exit_shell_mode(&mut self) {
        self.mode = PromptMode::Normal;
    }

    pub fn register_interrupt_keypress(&mut self) -> bool {
        if self.interrupt_confirmation_active() {
            self.reset_interrupt_confirmation();
            return true;
        }
        self.interrupt_press_count = 1;
        self.last_interrupt_time = Some(Instant::now());
        false
    }

    pub fn clear_interrupt_confirmation(&mut self) {
        self.reset_interrupt_confirmation();
    }

    pub fn insert_text(&mut self, text: &str) {
        self.input.insert_str(self.cursor_position, text);
        self.cursor_position = self.cursor_position.saturating_add(text.len());
        self.reset_history_cursor();
        self.recompute_suggestions();
    }

    pub fn desired_height(&self, width: u16) -> u16 {
        self.input_display_lines(width)
            .saturating_add(PROMPT_BLOCK_PAD_TOP)
            .saturating_add(PROMPT_BLOCK_PAD_BOTTOM)
            .saturating_add(3)
    }

    pub fn stash_current(&mut self) -> bool {
        if self.input.trim().is_empty() {
            return false;
        }

        self.stash.push(PromptStashEntry {
            input: self.input.clone(),
            created_at: chrono::Utc::now().timestamp_millis(),
        });
        if self.stash.len() > MAX_STASH_ENTRIES {
            let overflow = self.stash.len().saturating_sub(MAX_STASH_ENTRIES);
            self.stash.drain(0..overflow);
        }
        store_stash(&self.stash_path, &self.stash);
        self.clear();
        true
    }

    pub fn stash_entries(&self) -> &[PromptStashEntry] {
        &self.stash
    }

    pub fn pop_stash(&mut self) -> Option<PromptStashEntry> {
        let entry = self.stash.pop();
        if entry.is_some() {
            store_stash(&self.stash_path, &self.stash);
        }
        entry
    }

    pub fn remove_stash(&mut self, index: usize) -> bool {
        if index >= self.stash.len() {
            return false;
        }
        self.stash.remove(index);
        store_stash(&self.stash_path, &self.stash);
        true
    }

    pub fn load_stash(&mut self, index: usize) -> bool {
        let Some(entry) = self.stash.get(index) else {
            return false;
        };
        self.set_input(entry.input.clone());
        true
    }

    fn history_previous(&mut self) {
        if self.history.is_empty() {
            return;
        }
        match self.history_index {
            Some(idx) => {
                if idx > 0 {
                    self.history_index = Some(idx - 1);
                }
            }
            None => {
                self.history_draft = Some(self.input.clone());
                self.history_index = Some(self.history.len() - 1);
            }
        }

        if let Some(idx) = self.history_index {
            self.input = self.history[idx].clone();
            self.cursor_position = self.input.len();
            self.recompute_suggestions();
        }
    }

    fn history_next(&mut self) {
        let Some(idx) = self.history_index else {
            return;
        };

        if idx + 1 < self.history.len() {
            let next = idx + 1;
            self.history_index = Some(next);
            self.input = self.history[next].clone();
            self.cursor_position = self.input.len();
        } else {
            self.history_index = None;
            self.input = self.history_draft.take().unwrap_or_default();
            self.cursor_position = self.input.len();
        }
        self.recompute_suggestions();
    }

    fn reset_history_cursor(&mut self) {
        self.history_index = None;
        self.history_draft = None;
    }

    fn push_history(&mut self, entry: String) {
        if self
            .history
            .last()
            .is_some_and(|existing| existing == &entry)
        {
            return;
        }
        self.history.push(entry);
        if self.history.len() > MAX_HISTORY_ENTRIES {
            let overflow = self.history.len().saturating_sub(MAX_HISTORY_ENTRIES);
            self.history.drain(0..overflow);
        }
    }

    fn bump_frecency(&mut self, key: &str) {
        let key = key.trim();
        if key.is_empty() {
            return;
        }
        let now = chrono::Utc::now().timestamp_millis();
        let entry = self.frecency.entry(key.to_string()).or_default();
        entry.frequency = entry.frequency.saturating_add(1);
        entry.last_used = now;

        if self.frecency.len() > MAX_FRECENCY_ENTRIES {
            let mut items = self.frecency.iter().collect::<Vec<_>>();
            items.sort_by(|(_, a), (_, b)| b.last_used.cmp(&a.last_used));
            let keep = items
                .into_iter()
                .take(MAX_FRECENCY_ENTRIES)
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect::<HashMap<_, _>>();
            self.frecency = keep;
        }
    }

    fn frecency_score(&self, key: &str) -> f64 {
        let Some(entry) = self.frecency.get(key) else {
            return 0.0;
        };
        let now = chrono::Utc::now().timestamp_millis();
        let days_since = (now - entry.last_used).max(0) as f64 / 86_400_000.0;
        let weight = 1.0 / (1.0 + days_since);
        entry.frequency as f64 * weight
    }

    fn selected_suggestion(&self) -> Option<&str> {
        self.suggestion_index
            .and_then(|idx| self.suggestions.get(idx))
            .map(String::as_str)
    }

    fn refresh_file_index_if_needed(&mut self) {
        let directory = self.context.directory.read().clone();
        let root = if directory.trim().is_empty() {
            std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
        } else {
            PathBuf::from(directory)
        };
        self.file_index.refresh(&root, FILE_INDEX_MAX_DEPTH);
    }

    fn push_candidate(
        scored: &mut Vec<(String, i32)>,
        dedup: &mut HashMap<String, ()>,
        token: &str,
        token_lower: &str,
        item: String,
        pre_scored: Option<i32>,
    ) {
        if item.eq_ignore_ascii_case(token) {
            return;
        }
        if dedup.insert(item.to_lowercase(), ()).is_some() {
            return;
        }

        let score = pre_scored
            .or_else(|| crate::command::fuzzy_match(token, &item))
            .or_else(|| {
                if item.to_lowercase().starts_with(token_lower) {
                    Some(1)
                } else {
                    None
                }
            });
        if let Some(score) = score {
            scored.push((item, score));
        }
    }

    fn recompute_suggestions(&mut self) {
        let Some((_, _, token)) = self.current_token() else {
            self.suggestions.clear();
            self.suggestion_index = None;
            return;
        };

        if token.is_empty() {
            self.suggestions.clear();
            self.suggestion_index = None;
            return;
        }

        let token_lower = token.to_lowercase();
        let mut dedup = HashMap::<String, ()>::new();
        let mut scored: Vec<(String, i32)> = Vec::new();
        if token.starts_with('/') {
            for item in self.known_commands.iter().cloned() {
                Self::push_candidate(
                    &mut scored,
                    &mut dedup,
                    token.as_str(),
                    token_lower.as_str(),
                    item,
                    None,
                );
            }
            for item in self
                .known_skills
                .iter()
                .map(|skill| format!("/{}", skill.trim()))
            {
                Self::push_candidate(
                    &mut scored,
                    &mut dedup,
                    token.as_str(),
                    token_lower.as_str(),
                    item,
                    None,
                );
            }
        } else if let Some(after_at) = token.strip_prefix('@') {
            // Agent completions
            for item in self
                .known_agents
                .iter()
                .map(|agent| format!("@{}", agent.trim()))
            {
                Self::push_candidate(
                    &mut scored,
                    &mut dedup,
                    token.as_str(),
                    token_lower.as_str(),
                    item,
                    None,
                );
            }
            // File path completions: strip @ prefix and optional #line range
            let (file_part, _line_range) = extract_line_range(after_at);
            let range_suffix = after_at
                .strip_prefix(file_part)
                .filter(|suffix| suffix.starts_with('#'))
                .unwrap_or("");
            self.refresh_file_index_if_needed();
            let path_score_boost = if file_part.trim().is_empty() { 0 } else { 1024 };
            for (path, path_score) in self.file_index.search(file_part, FILE_SUGGESTION_LIMIT) {
                let candidate = format!("@{}{}", path, range_suffix);
                let path_score = i32::try_from(path_score).unwrap_or(i32::MAX);
                let score = path_score.saturating_add(path_score_boost);
                Self::push_candidate(
                    &mut scored,
                    &mut dedup,
                    token.as_str(),
                    token_lower.as_str(),
                    candidate,
                    Some(score),
                );
            }
        } else {
            for item in self.history.iter().rev() {
                let history_score = legacy_subsequence_score(token.as_str(), item.as_str());
                Self::push_candidate(
                    &mut scored,
                    &mut dedup,
                    token.as_str(),
                    token_lower.as_str(),
                    item.clone(),
                    history_score,
                );
            }
        }

        scored.sort_by(|a, b| {
            let frecency_cmp = self
                .frecency_score(&b.0)
                .partial_cmp(&self.frecency_score(&a.0))
                .unwrap_or(std::cmp::Ordering::Equal);
            b.1.cmp(&a.1)
                .then(frecency_cmp)
                .then_with(|| a.0.len().cmp(&b.0.len()))
                .then_with(|| {
                    let a_last = self.frecency.get(&a.0).map(|e| e.last_used).unwrap_or(0);
                    let b_last = self.frecency.get(&b.0).map(|e| e.last_used).unwrap_or(0);
                    b_last.cmp(&a_last)
                })
                .then_with(|| {
                    let a_history_pos = self
                        .history
                        .iter()
                        .rev()
                        .position(|h| h == &a.0)
                        .unwrap_or(usize::MAX);
                    let b_history_pos = self
                        .history
                        .iter()
                        .rev()
                        .position(|h| h == &b.0)
                        .unwrap_or(usize::MAX);
                    a_history_pos.cmp(&b_history_pos)
                })
                .then_with(|| a.0.to_lowercase().cmp(&b.0.to_lowercase()))
        });

        self.suggestions = scored.into_iter().map(|(item, _)| item).collect();
        self.suggestion_index = if self.suggestions.is_empty() {
            None
        } else {
            Some(0)
        };
    }

    fn apply_autocomplete_next(&mut self) {
        if self.suggestions.is_empty() {
            self.recompute_suggestions();
            if self.suggestions.is_empty() {
                return;
            }
        }

        self.suggestion_index = Some(self.suggestion_index.unwrap_or(0));
        self.apply_selected_suggestion();
    }

    fn apply_autocomplete_previous(&mut self) {
        if self.suggestions.is_empty() {
            self.recompute_suggestions();
            if self.suggestions.is_empty() {
                return;
            }
        }

        self.suggestion_index = Some(
            self.suggestion_index
                .map(|current| {
                    if current == 0 {
                        self.suggestions.len() - 1
                    } else {
                        current - 1
                    }
                })
                .unwrap_or(self.suggestions.len() - 1),
        );
        self.apply_selected_suggestion();
    }

    fn apply_selected_suggestion(&mut self) {
        let Some((start, end, _)) = self.current_token() else {
            return;
        };
        let Some(suggestion) = self.selected_suggestion().map(ToString::to_string) else {
            return;
        };

        self.input.replace_range(start..end, &suggestion);
        self.cursor_position = start + suggestion.len();
        if (suggestion.starts_with('/') || suggestion.starts_with('@'))
            && self.cursor_position < self.input.len()
            && !self
                .input
                .as_bytes()
                .get(self.cursor_position)
                .copied()
                .is_some_and(|b| b.is_ascii_whitespace())
        {
            self.input.insert(self.cursor_position, ' ');
            self.cursor_position += 1;
        }
    }

    fn current_token(&self) -> Option<(usize, usize, String)> {
        if self.input.is_empty() || self.cursor_position > self.input.len() {
            return None;
        }

        let bytes = self.input.as_bytes();
        let mut start = self.cursor_position;
        while start > 0 && !bytes[start - 1].is_ascii_whitespace() {
            start -= 1;
        }

        let mut end = self.cursor_position;
        while end < bytes.len() && !bytes[end].is_ascii_whitespace() {
            end += 1;
        }

        let token = self.input[start..self.cursor_position].to_string();
        Some((start, end, token))
    }

    fn input_display_lines(&self, width: u16) -> u16 {
        let reserved = 1u16
            .saturating_add(PROMPT_BLOCK_PAD_LEFT)
            .saturating_add(PROMPT_BLOCK_PAD_RIGHT);
        let input_width = usize::from(width.saturating_sub(reserved)).max(1);
        let raw_lines = visual_line_count(&self.input, input_width) as u16;
        raw_lines
            .max(PROMPT_MIN_INPUT_LINES)
            .min(PROMPT_MAX_INPUT_LINES)
    }

    /// App の leader キー状態をプロンプトのステータス行へ反映する。
    pub fn set_leader_active(&mut self, active: bool) {
        self.leader_active = active;
    }

    fn render_status_line(&self, theme: &Theme) -> Line<'static> {
        // leader 待機中はセッション状態より優先して案内を出す。
        if self.leader_active {
            return self.leader_hint_line(theme);
        }
        if let Some(status) = self.current_session_status() {
            return self.status_line_for_session(status, theme);
        }
        self.hint_line(theme)
    }

    /// `command_list` バインドを leader キーとして配線し、次のキー候補を表示する。
    fn leader_hint_line(&self, theme: &Theme) -> Line<'static> {
        let keybind = self.context.keybind.read();
        let leader = keybind.print("command_list");
        drop(keybind);

        Line::from(vec![
            Span::styled(
                format!("{} then n/l/m/a/t/b/s/q/u/r", leader),
                Style::default().fg(theme.warning),
            ),
            Span::styled("  Esc to cancel", Style::default().fg(theme.text_muted)),
        ])
    }

    fn current_session_status(&self) -> Option<SessionStatus> {
        let session_id = match self.context.current_route() {
            crate::router::Route::Session { session_id } => session_id,
            _ => return None,
        };
        let session_ctx = self.context.session.read();
        Some(session_ctx.status(&session_id).clone())
    }

    fn status_line_for_session(&self, status: SessionStatus, theme: &Theme) -> Line<'static> {
        if matches!(self.mode, PromptMode::Shell) {
            return Line::from(vec![
                Span::styled("esc", Style::default().fg(theme.text)),
                Span::styled(" exit shell mode", Style::default().fg(theme.text_muted)),
            ]);
        }
        let interrupt = self.context.keybind.read().print("session_interrupt");
        match status {
            SessionStatus::Retrying {
                message,
                attempt,
                next,
            } => {
                let now = chrono::Utc::now().timestamp_millis();
                let secs = ((next - now) / 1000).max(0);
                let truncated = truncate_for_status(&message, 72);
                Line::from(vec![
                    Span::styled(
                        format!("retrying in {}s (#{}) ", secs, attempt),
                        Style::default().fg(theme.warning),
                    ),
                    Span::styled(truncated, Style::default().fg(theme.text_muted)),
                    Span::raw("  "),
                    Span::styled(interrupt, Style::default().fg(theme.text)),
                    Span::styled(" interrupt", Style::default().fg(theme.text_muted)),
                ])
            }
            SessionStatus::Running => {
                let mut spans = vec![
                    Span::styled("thinking", Style::default().fg(theme.text_muted)),
                    Span::raw("  "),
                ];
                if self.interrupt_confirmation_active() {
                    spans.push(Span::styled(interrupt, Style::default().fg(theme.warning)));
                    spans.push(Span::styled(
                        " again to interrupt",
                        Style::default().fg(theme.warning),
                    ));
                } else {
                    spans.push(Span::styled(interrupt, Style::default().fg(theme.text)));
                    spans.push(Span::styled(
                        " interrupt",
                        Style::default().fg(theme.text_muted),
                    ));
                }
                Line::from(spans)
            }
            SessionStatus::Idle => self.hint_line(theme),
        }
    }

    fn interrupt_confirmation_active(&self) -> bool {
        if self.interrupt_press_count == 0 {
            return false;
        }
        self.last_interrupt_time
            .is_some_and(|t| t.elapsed() < Duration::from_secs(INTERRUPT_CONFIRM_WINDOW_SECS))
    }

    fn maybe_reset_interrupt_confirmation(&mut self) -> bool {
        if !self.interrupt_confirmation_active() {
            return self.reset_interrupt_confirmation();
        }
        false
    }

    fn reset_interrupt_confirmation(&mut self) -> bool {
        let changed = self.interrupt_press_count != 0 || self.last_interrupt_time.is_some();
        self.interrupt_press_count = 0;
        self.last_interrupt_time = None;
        changed
    }

    fn hint_line(&self, theme: &Theme) -> Line<'static> {
        let keybind = self.context.keybind.read();
        let variant_cycle = keybind.print("variant_cycle");
        let agent_cycle = keybind.print("agent_cycle");
        drop(keybind);

        let mut spans = Vec::new();
        if self.context.current_model_variant().is_some() {
            spans.push(Span::styled(variant_cycle, Style::default().fg(theme.text)));
            spans.push(Span::styled(
                " variants",
                Style::default().fg(theme.text_muted),
            ));
            spans.push(Span::raw("  "));
        }
        spans.push(Span::styled(agent_cycle, Style::default().fg(theme.text)));
        spans.push(Span::styled(
            " agents",
            Style::default().fg(theme.text_muted),
        ));
        Line::from(spans)
    }
}

fn truncate_for_status(input: &str, max_chars: usize) -> String {
    if input.chars().count() <= max_chars {
        return input.to_string();
    }
    let mut out = String::with_capacity(max_chars + 1);
    for ch in input.chars().take(max_chars.saturating_sub(1)) {
        out.push(ch);
    }
    out.push('…');
    out
}

fn visual_line_count(text: &str, width: usize) -> usize {
    if text.is_empty() {
        return 1;
    }

    let mut rows = 1usize;
    let mut col = 0usize;
    for ch in text.chars() {
        if ch == '\n' {
            rows += 1;
            col = 0;
            continue;
        }

        let ch_width = UnicodeWidthChar::width(ch).unwrap_or(0);
        if col > 0 && col + ch_width > width {
            rows += 1;
            col = 0;
        }
        col += ch_width;
    }

    rows.max(1)
}

fn dedup_sort(mut items: Vec<String>) -> Vec<String> {
    items.retain(|item| !item.trim().is_empty());
    items.sort_by_key(|a| a.to_ascii_lowercase());
    items.dedup_by(|a, b| a.eq_ignore_ascii_case(b));
    items
}

fn inset_horizontal(area: Rect, padding: u16) -> Rect {
    if area.width <= padding.saturating_mul(2) {
        return area;
    }
    Rect {
        x: area.x.saturating_add(padding),
        y: area.y,
        width: area.width.saturating_sub(padding.saturating_mul(2)),
        height: area.height,
    }
}

fn row_content_area(area: Rect, horizontal_padding: u16) -> Rect {
    if area.width <= 1 {
        return area;
    }
    let inner = Rect {
        x: area.x.saturating_add(1),
        y: area.y,
        width: area.width.saturating_sub(1),
        height: area.height,
    };
    inset_horizontal(inner, horizontal_padding)
}

fn render_prompt_continuation_row(
    frame: &mut Frame,
    area: Rect,
    border_color: Color,
    background: Color,
) {
    if area.width == 0 || area.height == 0 {
        return;
    }

    let mut spans = vec![Span::styled(
        "┃",
        Style::default().fg(border_color).bg(background),
    )];
    let trailing = usize::from(area.width).saturating_sub(1);
    if trailing > 0 {
        spans.push(Span::styled(
            " ".repeat(trailing),
            Style::default().bg(background),
        ));
    }

    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

fn legacy_subsequence_score(query: &str, target: &str) -> Option<i32> {
    let query_lower = query.trim().to_lowercase();
    if query_lower.is_empty() {
        return Some(0);
    }
    let target_lower = target.to_lowercase();
    let mut score = 0i32;
    let mut query_idx = 0usize;
    let query_chars: Vec<char> = query_lower.chars().collect();
    for (idx, ch) in target_lower.chars().enumerate() {
        if query_idx < query_chars.len() && ch == query_chars[query_idx] {
            score += if idx == 0 || query_idx == 0 { 10 } else { 5 };
            query_idx += 1;
        }
    }
    if query_idx == query_chars.len() {
        Some(score)
    } else {
        None
    }
}

fn prompt_agent_color(theme: &Theme, agent_name: &str) -> Color {
    if theme.agent_colors.is_empty() {
        return theme.primary;
    }

    let mut hasher = DefaultHasher::new();
    agent_name.hash(&mut hasher);
    let idx = (hasher.finish() as usize) % theme.agent_colors.len();
    theme.agent_colors[idx]
}

fn spinner_mode_from_env() -> SpinnerMode {
    match std::env::var("OPENCODE_TUI_SPINNER")
        .ok()
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase()
        .as_str()
    {
        "knight" | "knight-rider" | "scan" | "scanner" => SpinnerMode::KnightRider,
        _ => SpinnerMode::Braille,
    }
}

/// UAX #29 の Grapheme_Extend 相当を std だけで近似した判定。
///
/// `unicode-segmentation` は workspace の直接依存ではないため追加せず、
/// 一般的な結合記号・異体字セレクタ・スキントーン等を「直前の書記素に
/// 吸着する」文字として扱う。完全な_grapheme cluster_規則ではなく、
/// プロンプトのカーソル移動・削除が結合文字列を壊さないことを目的とする。
fn is_grapheme_extend(ch: char) -> bool {
    matches!(ch,
        '\u{0300}'..='\u{036F}'
        | '\u{0483}'..='\u{0489}'
        | '\u{0591}'..='\u{05BD}'
        | '\u{05BF}'
        | '\u{05C1}'..='\u{05C2}'
        | '\u{05C4}'..='\u{05C5}'
        | '\u{05C7}'
        | '\u{0610}'..='\u{061A}'
        | '\u{064B}'..='\u{065F}'
        | '\u{0670}'
        | '\u{06D6}'..='\u{06DC}'
        | '\u{06DF}'..='\u{06E4}'
        | '\u{06E7}'..='\u{06E8}'
        | '\u{06EA}'..='\u{06ED}'
        | '\u{0711}'
        | '\u{0730}'..='\u{074A}'
        | '\u{07A6}'..='\u{07B0}'
        | '\u{07EB}'..='\u{07F3}'
        | '\u{0816}'..='\u{0819}'
        | '\u{081B}'..='\u{0823}'
        | '\u{0825}'..='\u{0827}'
        | '\u{0829}'..='\u{082D}'
        | '\u{0859}'..='\u{085B}'
        | '\u{08D3}'..='\u{0903}'
        | '\u{093A}'..='\u{094F}'
        | '\u{0951}'..='\u{0957}'
        | '\u{0962}'..='\u{0963}'
        | '\u{0981}'
        | '\u{09BC}'
        | '\u{09C1}'..='\u{09CD}'
        | '\u{0BCD}'
        | '\u{0E31}'
        | '\u{0E34}'..='\u{0E3A}'
        | '\u{0E47}'..='\u{0E4E}'
        | '\u{0F71}'..='\u{0F84}'
        | '\u{0F86}'..='\u{0F87}'
        | '\u{0F8D}'..='\u{0FBC}'
        | '\u{1160}'..='\u{11FF}'
        | '\u{135D}'..='\u{135F}'
        | '\u{17B4}'..='\u{17B5}'
        | '\u{17B7}'..='\u{17BD}'
        | '\u{17C6}'
        | '\u{17C9}'..='\u{17D3}'
        | '\u{17DD}'
        | '\u{1AB0}'..='\u{1AFF}'
        | '\u{1DC0}'..='\u{1DFF}'
        | '\u{200C}'
        | '\u{20D0}'..='\u{20FF}'
        | '\u{302A}'..='\u{302F}'
        | '\u{3099}'..='\u{309A}'
        | '\u{FE00}'..='\u{FE0F}'
        | '\u{FE20}'..='\u{FE2F}'
        | '\u{1F3FB}'..='\u{1F3FF}'
        | '\u{E0020}'..='\u{E007F}'
        | '\u{E0100}'..='\u{E01EF}'
        | '\u{FF9E}'..='\u{FF9F}'
    )
}

fn is_regional_indicator(ch: char) -> bool {
    matches!(ch, '\u{1F1E6}'..='\u{1F1FF}')
}

/// 入力中の書記素クラスタ境界（バイトオフセット）を列挙する。
/// 0 と `input.len()` は常に境界に含まれる。
fn grapheme_boundaries(input: &str) -> Vec<usize> {
    let mut boundaries = Vec::new();
    let mut previous: Option<char> = None;
    // 直前の地域指標がペアの 1 文字目かどうか（国旗は 2 文字で 1 クラスタ）。
    let mut regional_pair_lead = false;

    for (index, ch) in input.char_indices() {
        // ZWJ は自身と直後の文字を同じクラスタに結合する（家族絵文字等）。
        let continues = previous.is_some_and(|prev| {
            ch == '\u{200D}'
                || prev == '\u{200D}'
                || is_grapheme_extend(ch)
                || (prev == '\r' && ch == '\n')
                || (is_regional_indicator(ch) && regional_pair_lead)
        });
        if !continues {
            boundaries.push(index);
        }

        regional_pair_lead = if is_regional_indicator(ch) {
            !regional_pair_lead
        } else {
            false
        };
        previous = Some(ch);
    }
    boundaries.push(input.len());
    boundaries
}

fn prev_grapheme_boundary(input: &str, cursor_position: usize) -> Option<usize> {
    if cursor_position == 0 || cursor_position > input.len() {
        return None;
    }
    grapheme_boundaries(input)
        .into_iter()
        .take_while(|&boundary| boundary < cursor_position)
        .last()
}

fn next_grapheme_boundary(input: &str, cursor_position: usize) -> Option<usize> {
    if cursor_position >= input.len() {
        return None;
    }
    grapheme_boundaries(input)
        .into_iter()
        .find(|&boundary| boundary > cursor_position)
}

fn prompt_state_dir() -> PathBuf {
    let base = std::env::var("OPENCODE_STATE_DIR")
        .ok()
        .filter(|v| !v.trim().is_empty())
        .map(PathBuf::from)
        .or_else(|| dirs::state_dir().map(|d| d.join("opencode")))
        .unwrap_or_else(|| std::env::temp_dir().join("opencode"));
    let path = base.join("tui");
    let _ = std::fs::create_dir_all(&path);
    path
}

/// Best-effort quarantine of a corrupted store file. Only a fixed suffix is
/// appended; paths and file contents are never included in log messages.
fn backup_corrupted_store(path: &Path) {
    let mut backup_name = path.as_os_str().to_os_string();
    backup_name.push(".bak");
    let _ = std::fs::rename(path, PathBuf::from(backup_name));
}

/// Reads and deserializes a store. A missing file is a normal first-run case
/// and stays silent; read/parse failures emit a fixed warning (kind only),
/// quarantine the corrupted file, and let the caller fall back to empty data.
fn read_store<T: serde::de::DeserializeOwned>(path: &Path, kind: &str) -> Option<T> {
    let raw = match std::fs::read_to_string(path) {
        Ok(raw) => raw,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return None,
        Err(_) => {
            tracing::warn!(kind = kind, "prompt store read failed; using empty data");
            backup_corrupted_store(path);
            return None;
        }
    };
    match serde_json::from_str::<T>(&raw) {
        Ok(store) => Some(store),
        Err(_) => {
            tracing::warn!(kind = kind, "prompt store parse failed; using empty data");
            backup_corrupted_store(path);
            None
        }
    }
}

/// Writes the bytes to a temp sibling, flushes, then renames into place. Any
/// failure before the rename leaves the existing file untouched.
fn write_store_to_temp(temp: &Path, path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write as _;

    // 既存ファイルの権限（例: 0600）を temp へ引き継ぐ。新規 inode の
    // デフォルトモード（umask 依存の 0644 等）で作ると機密情報が露出する（R4）。
    let permissions = std::fs::metadata(path).ok().map(|m| m.permissions());
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(temp)?;
    if let Some(permissions) = permissions {
        std::fs::set_permissions(temp, permissions)?;
    }
    file.write_all(bytes)?;
    file.sync_all()?;
    drop(file);
    std::fs::rename(temp, path)
}

/// Atomic store write: temp sibling + `write_all` + `sync_all` + `rename`.
fn write_store_atomic(path: &Path, json: &str) -> std::io::Result<()> {
    let mut temp_name = path.as_os_str().to_os_string();
    temp_name.push(".tmp");
    let temp = PathBuf::from(temp_name);
    let result = write_store_to_temp(&temp, path, json.as_bytes());
    if result.is_err() {
        let _ = std::fs::remove_file(&temp);
    }
    result
}

/// Serializes and stores a payload, warning (kind only) on any failure.
fn write_store<T: serde::Serialize>(path: &Path, payload: &T, kind: &str) {
    let json = match serde_json::to_string(payload) {
        Ok(json) => json,
        Err(_) => {
            tracing::warn!(
                kind = kind,
                "prompt store serialize failed; nothing written"
            );
            return;
        }
    };
    if write_store_atomic(path, &json).is_err() {
        tracing::warn!(kind = kind, "prompt store write failed; existing data kept");
    }
}

fn load_history(path: &Path) -> Vec<String> {
    let store = read_store::<HistoryStore>(path, "history").unwrap_or_default();
    let mut entries = store.entries;
    if entries.len() > MAX_HISTORY_ENTRIES {
        let overflow = entries.len() - MAX_HISTORY_ENTRIES;
        entries.drain(0..overflow);
    }
    entries
}

fn load_frecency(path: &Path) -> HashMap<String, FrecencyEntry> {
    read_store::<FrecencyStore>(path, "frecency")
        .map(|store| store.entries)
        .unwrap_or_default()
}

fn load_stash(path: &Path) -> Vec<PromptStashEntry> {
    let store = read_store::<StashStore>(path, "stash").unwrap_or_default();
    let mut entries = store.entries;
    if entries.len() > MAX_STASH_ENTRIES {
        let overflow = entries.len() - MAX_STASH_ENTRIES;
        entries.drain(0..overflow);
    }
    entries
}

fn store_history(path: &Path, entries: &[String]) {
    let payload = HistoryStore {
        entries: entries.to_vec(),
    };
    write_store(path, &payload, "history");
}

fn store_frecency(path: &Path, entries: &HashMap<String, FrecencyEntry>) {
    let payload = FrecencyStore {
        entries: entries.clone(),
    };
    write_store(path, &payload, "frecency");
}

fn store_stash(path: &Path, entries: &[PromptStashEntry]) {
    let payload = StashStore {
        entries: entries.to_vec(),
    };
    write_store(path, &payload, "stash");
}

/// Extract a `#line` or `#line-line` range suffix from a file path reference.
/// Returns the base path and an optional (start, optional_end) line range.
fn extract_line_range(input: &str) -> (&str, Option<(usize, Option<usize>)>) {
    if let Some(hash_idx) = input.rfind('#') {
        let base = &input[..hash_idx];
        let range_str = &input[hash_idx + 1..];
        if let Some(dash_idx) = range_str.find('-') {
            let start = range_str[..dash_idx].parse().ok();
            let end = range_str[dash_idx + 1..].parse().ok();
            if let Some(s) = start {
                return (base, Some((s, end)));
            }
        } else if let Ok(line) = range_str.parse::<usize>() {
            return (base, Some((line, None)));
        }
    }
    (input, None)
}

/// crate 共通のテスト専用ロック。
///
/// `OPENCODE_STATE_DIR` を書き換えるテストは home.rs / prompt.rs の別モジュールに
/// 分散しており、個別のロックでは同じテストバイナリ内の並行実行時に環境変数が
/// 汚染し合う。ここで 1 つのミューテックスを定義して両モジュールから共有する。
/// lib.rs は担当スコープ外のため、このモジュールを `pub(crate)` で公開する。
#[cfg(test)]
pub(crate) mod test_support {
    use std::sync::{LazyLock, Mutex};

    /// `OPENCODE_STATE_DIR` を守る crate 共通ロック。
    pub(crate) static STATE_DIR_LOCK: LazyLock<Mutex<()>> = LazyLock::new(|| Mutex::new(()));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::components::prompt::test_support::STATE_DIR_LOCK;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use std::fmt::Write as _;
    use std::sync::Mutex;
    use tracing::field::{Field, Visit};
    use tracing::span::{Attributes, Id, Record};
    use tracing::{Event, Metadata, Subscriber};

    /// Minimal `tracing` capture for warning assertions; avoids adding a
    /// `tracing-subscriber` dev-dependency to this crate.
    #[derive(Clone, Default)]
    struct WarnBuffer(Arc<Mutex<String>>);

    impl WarnBuffer {
        fn text(&self) -> String {
            self.0.lock().expect("warning buffer lock").clone()
        }
    }

    struct WarnVisitor(WarnBuffer);

    impl Visit for WarnVisitor {
        fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
            let mut guard = self.0 .0.lock().expect("warning buffer lock");
            let buffer: &mut String = &mut guard;
            let _ = write!(buffer, "{}={:?} ", field.name(), value);
        }

        fn record_str(&mut self, field: &Field, value: &str) {
            let mut guard = self.0 .0.lock().expect("warning buffer lock");
            let buffer: &mut String = &mut guard;
            let _ = write!(buffer, "{}={} ", field.name(), value);
        }
    }

    struct WarnSubscriber {
        buffer: WarnBuffer,
    }

    impl Subscriber for WarnSubscriber {
        fn enabled(&self, metadata: &Metadata<'_>) -> bool {
            *metadata.level() <= tracing::Level::WARN
        }
        fn new_span(&self, _: &Attributes<'_>) -> Id {
            Id::from_u64(1)
        }
        fn record(&self, _: &Id, _: &Record<'_>) {}
        fn record_follows_from(&self, _: &Id, _: &Id) {}
        fn event(&self, event: &Event<'_>) {
            event.record(&mut WarnVisitor(self.buffer.clone()));
        }
        fn enter(&self, _: &Id) {}
        fn exit(&self, _: &Id) {}
    }

    fn fresh_store_dir(tag: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("opencode-tui-store-{tag}-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).expect("create store test dir");
        dir
    }

    fn with_isolated_prompt<T>(f: impl FnOnce(Prompt) -> T) -> T {
        let _guard = STATE_DIR_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let state_dir =
            std::env::temp_dir().join(format!("opencode-tui-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&state_dir).expect("create state dir");
        let previous = std::env::var("OPENCODE_STATE_DIR").ok();
        std::env::set_var("OPENCODE_STATE_DIR", &state_dir);

        let context = Arc::new(AppContext::new());
        let prompt = Prompt::new(context);
        let result = f(prompt);

        if let Some(prev) = previous {
            std::env::set_var("OPENCODE_STATE_DIR", prev);
        } else {
            std::env::remove_var("OPENCODE_STATE_DIR");
        }
        let _ = std::fs::remove_dir_all(state_dir);
        result
    }

    #[test]
    fn tab_autocomplete_uses_first_candidate() {
        with_isolated_prompt(|mut prompt| {
            prompt.set_input("team".to_string());
            let _ = prompt.take_input();
            prompt.set_input("test".to_string());
            let _ = prompt.take_input();

            prompt.set_input("te".to_string());

            prompt.handle_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::empty()));
            assert_eq!(prompt.get_input(), "test");
        });
    }

    #[test]
    fn tab_autocomplete_keeps_line_range_for_file_candidates() {
        with_isolated_prompt(|mut prompt| {
            let root =
                std::env::temp_dir().join(format!("opencode-tui-files-{}", uuid::Uuid::new_v4()));
            let src_dir = root.join("src");
            std::fs::create_dir_all(&src_dir).expect("create src dir");
            std::fs::write(src_dir.join("main.rs"), "fn main() {}\n").expect("write file");
            *prompt.context.directory.write() = root.to_string_lossy().to_string();

            prompt.set_input("@src/main#12-20".to_string());
            prompt.handle_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::empty()));
            assert_eq!(prompt.get_input(), "@src/main.rs#12-20");

            let _ = std::fs::remove_dir_all(root);
        });
    }

    #[test]
    fn history_navigation_preserves_draft() {
        with_isolated_prompt(|mut prompt| {
            prompt.set_input("alpha".to_string());
            let _ = prompt.take_input();
            prompt.set_input("beta".to_string());
            let _ = prompt.take_input();
            prompt.set_input("draft".to_string());

            prompt.history_previous_entry();
            assert_eq!(prompt.get_input(), "beta");

            prompt.history_previous_entry();
            assert_eq!(prompt.get_input(), "alpha");

            prompt.history_next_entry();
            assert_eq!(prompt.get_input(), "beta");

            prompt.history_next_entry();
            assert_eq!(prompt.get_input(), "draft");
        });
    }

    #[test]
    fn utf8_backspace_delete_and_cursor_are_char_safe() {
        with_isolated_prompt(|mut prompt| {
            prompt.set_input("你好".to_string());

            prompt.handle_key(KeyEvent::new(KeyCode::Left, KeyModifiers::empty()));
            prompt.handle_key(KeyEvent::new(KeyCode::Backspace, KeyModifiers::empty()));
            assert_eq!(prompt.get_input(), "好");

            prompt.handle_key(KeyEvent::new(KeyCode::Home, KeyModifiers::empty()));
            prompt.handle_key(KeyEvent::new(KeyCode::Delete, KeyModifiers::empty()));
            assert_eq!(prompt.get_input(), "");
        });
    }

    #[test]
    fn grapheme_backspace_keeps_combining_sequence_intact() {
        with_isolated_prompt(|mut prompt| {
            prompt.set_input("e\u{301}x".to_string());

            prompt.handle_key(KeyEvent::new(KeyCode::Backspace, KeyModifiers::empty()));
            assert_eq!(prompt.get_input(), "e\u{301}");

            prompt.handle_key(KeyEvent::new(KeyCode::Backspace, KeyModifiers::empty()));
            assert_eq!(prompt.get_input(), "");
        });
    }

    #[test]
    fn grapheme_cursor_skips_zwj_emoji_as_one_unit() {
        with_isolated_prompt(|mut prompt| {
            let emoji = "👨‍👩‍👧‍👦";
            prompt.set_input(format!("A{emoji}B"));
            let after_a = 1;
            let after_emoji = 1 + emoji.len();

            prompt.handle_key(KeyEvent::new(KeyCode::Home, KeyModifiers::empty()));
            prompt.handle_key(KeyEvent::new(KeyCode::Right, KeyModifiers::empty()));
            assert_eq!(prompt.cursor_position(), after_a);

            prompt.handle_key(KeyEvent::new(KeyCode::Right, KeyModifiers::empty()));
            assert_eq!(prompt.cursor_position(), after_emoji);

            prompt.handle_key(KeyEvent::new(KeyCode::Left, KeyModifiers::empty()));
            assert_eq!(prompt.cursor_position(), after_a);

            prompt.handle_key(KeyEvent::new(KeyCode::Backspace, KeyModifiers::empty()));
            assert_eq!(prompt.get_input(), format!("{emoji}B"));
        });
    }

    #[test]
    fn grapheme_delete_removes_emoji_with_modifier_and_variation_selector() {
        with_isolated_prompt(|mut prompt| {
            prompt.set_input("👍🏽\u{FE0F}X".to_string());

            prompt.handle_key(KeyEvent::new(KeyCode::Home, KeyModifiers::empty()));
            prompt.handle_key(KeyEvent::new(KeyCode::Delete, KeyModifiers::empty()));
            assert_eq!(prompt.get_input(), "X");
        });
    }

    #[test]
    fn grapheme_boundaries_group_extend_zwj_and_regional_indicators() {
        assert_eq!(grapheme_boundaries("a\u{301}b"), vec![0, 3, 4]);

        let family = "👨‍👩‍👧‍👦";
        assert_eq!(grapheme_boundaries(family), vec![0, family.len()]);

        let flag = "🇯🇵X";
        assert_eq!(grapheme_boundaries(flag), vec![0, 8, 9]);
    }

    #[test]
    fn has_input_tracks_only_unsent_text() {
        with_isolated_prompt(|mut prompt| {
            assert!(!prompt.has_input());
            prompt.set_input("draft".to_string());
            assert!(prompt.has_input());
            let _ = prompt.take_input();
            assert!(!prompt.has_input());
        });
    }

    #[test]
    fn has_completion_candidates_tracks_suggestion_state() {
        with_isolated_prompt(|mut prompt| {
            assert!(!prompt.has_completion_candidates());
            prompt.set_input("@b".to_string());
            assert!(prompt.has_completion_candidates());
            prompt.set_input(String::new());
            assert!(!prompt.has_completion_candidates());
        });
    }

    fn line_text(line: &Line<'static>) -> String {
        line.spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect()
    }

    #[test]
    fn leader_wait_is_shown_in_prompt_status_line() {
        with_isolated_prompt(|mut prompt| {
            let theme = Theme::default();

            prompt.set_leader_active(true);
            let waiting = line_text(&prompt.render_status_line(&theme));
            assert!(
                waiting
                    .to_lowercase()
                    .contains("ctrl+x then n/l/m/a/t/b/s/q/u/r"),
                "leader keys must be spelled out: {waiting}"
            );
            assert!(waiting.contains("Esc to cancel"));

            prompt.set_leader_active(false);
            let idle = line_text(&prompt.render_status_line(&theme));
            assert!(!idle.contains("then n/l/m/a/t/b/s/q/u/r"));
        });
    }

    #[test]
    fn corrupted_store_warns_and_is_backed_up() {
        let dir = fresh_store_dir("corrupt");
        let path = dir.join("prompt-history.json");
        let secret = "SECRET_TOKEN_SENTINEL_NEVER_LOG";
        std::fs::write(
            &path,
            format!(r#"{{"entries":"not-an-array","token":"{secret}"}}"#),
        )
        .expect("write corrupted store");

        let buffer = WarnBuffer::default();
        let entries = tracing::subscriber::with_default(
            WarnSubscriber {
                buffer: buffer.clone(),
            },
            || load_history(&path),
        );

        assert!(entries.is_empty());
        let backup = dir.join("prompt-history.json.bak");
        assert!(backup.exists(), "corrupted store must be quarantined");
        assert!(!path.exists(), "corrupted file should be moved to .bak");
        assert!(
            std::fs::read_to_string(&backup)
                .expect("read backup")
                .contains(secret),
            "backup must keep the original corrupted bytes"
        );
        let text = buffer.text();
        let dir_text = dir.to_string_lossy().to_string();
        assert!(text.contains("history"), "kind must be logged: {text}");
        assert!(
            text.contains("parse failed"),
            "reason must be logged: {text}"
        );
        assert!(
            !text.contains(secret),
            "token value leaked into warning: {text}"
        );
        assert!(
            !text.contains(dir_text.as_str()),
            "path leaked into warning: {text}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn failed_store_write_keeps_existing_data() {
        let dir = fresh_store_dir("writefail");
        let path = dir.join("prompt-history.json");
        let original = r#"{"entries":["keep-me"]}"#;
        std::fs::write(&path, original).expect("write original store");
        // A directory at the temp sibling makes the atomic write fail before rename.
        std::fs::create_dir(dir.join("prompt-history.json.tmp")).expect("create blocking dir");

        let buffer = WarnBuffer::default();
        tracing::subscriber::with_default(
            WarnSubscriber {
                buffer: buffer.clone(),
            },
            || store_history(&path, &["replacement-value".to_string()]),
        );

        assert_eq!(
            std::fs::read_to_string(&path).expect("original store still readable"),
            original,
            "existing data must survive a failed write"
        );
        let text = buffer.text();
        assert!(text.contains("history"), "kind must be logged: {text}");
        assert!(
            text.contains("write failed"),
            "reason must be logged: {text}"
        );
        assert!(
            !text.contains("replacement-value"),
            "payload leaked into warning: {text}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn store_round_trip_keeps_json_layout_and_is_silent() {
        let dir = fresh_store_dir("roundtrip");
        let buffer = WarnBuffer::default();

        tracing::subscriber::with_default(
            WarnSubscriber {
                buffer: buffer.clone(),
            },
            || {
                let history_path = dir.join("prompt-history.json");
                let history = vec!["alpha".to_string(), "beta".to_string()];
                store_history(&history_path, &history);
                assert_eq!(load_history(&history_path), history);
                assert_eq!(
                    std::fs::read_to_string(&history_path).expect("read history"),
                    serde_json::to_string(&HistoryStore {
                        entries: history.clone(),
                    })
                    .expect("encode history")
                );

                let frecency_path = dir.join("prompt-frecency.json");
                let mut frecency = HashMap::new();
                frecency.insert(
                    "gamma".to_string(),
                    FrecencyEntry {
                        frequency: 3,
                        last_used: 42,
                    },
                );
                store_frecency(&frecency_path, &frecency);
                let loaded_frecency = load_frecency(&frecency_path);
                let loaded_entry = loaded_frecency.get("gamma").expect("frecency entry");
                assert_eq!(loaded_entry.frequency, 3);
                assert_eq!(loaded_entry.last_used, 42);
                assert_eq!(
                    std::fs::read_to_string(&frecency_path).expect("read frecency"),
                    serde_json::to_string(&FrecencyStore {
                        entries: frecency.clone(),
                    })
                    .expect("encode frecency")
                );

                let stash_path = dir.join("prompt-stash.json");
                let stash = vec![PromptStashEntry {
                    input: "draft".to_string(),
                    created_at: 7,
                }];
                store_stash(&stash_path, &stash);
                let loaded_stash = load_stash(&stash_path);
                assert_eq!(loaded_stash.len(), 1);
                assert_eq!(loaded_stash[0].input, "draft");
                assert_eq!(loaded_stash[0].created_at, 7);
                assert_eq!(
                    std::fs::read_to_string(&stash_path).expect("read stash"),
                    serde_json::to_string(&StashStore {
                        entries: stash.clone(),
                    })
                    .expect("encode stash")
                );

                for name in [
                    "prompt-history.json.tmp",
                    "prompt-frecency.json.tmp",
                    "prompt-stash.json.tmp",
                ] {
                    assert!(
                        !dir.join(name).exists(),
                        "temporary file must be removed: {name}"
                    );
                }
            },
        );

        let text = buffer.text();
        assert!(text.is_empty(), "normal round trip must not warn: {text}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn missing_store_files_load_empty_without_warnings() {
        let dir = fresh_store_dir("missing");
        let buffer = WarnBuffer::default();
        tracing::subscriber::with_default(
            WarnSubscriber {
                buffer: buffer.clone(),
            },
            || {
                assert!(load_history(&dir.join("prompt-history.json")).is_empty());
                assert!(load_frecency(&dir.join("prompt-frecency.json")).is_empty());
                assert!(load_stash(&dir.join("prompt-stash.json")).is_empty());
            },
        );

        let text = buffer.text();
        assert!(
            text.is_empty(),
            "missing stores are a normal first-run case: {text}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}

use std::collections::hash_map::DefaultHasher;
use std::collections::{HashMap, HashSet, VecDeque};
use std::hash::{Hash, Hasher};
use std::io::{self, BufRead, BufReader, IsTerminal, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use chrono::{TimeZone, Utc};
use crossterm::event::{Event as CrosstermEvent, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use uuid::Uuid;

use crate::api::{
    ApiClient, McpStatusInfo, MessageInfo, QuestionRequestInfo, SessionInfo, SessionRevertInfo,
    UnsupportedEndpointError,
};
use crate::app::state::AppState;
use crate::app::terminal;
use crate::command::CommandAction;
use crate::components::{
    Agent, AgentSelectDialog, AlertDialog, CommandPalette, ForkDialog, ForkEntry, HelpDialog,
    HomeView, McpDialog, McpItem, Model, ModelSelectDialog, PermissionAction, PermissionPrompt,
    PermissionRequest, PermissionType, Prompt, PromptStashDialog, ProviderDialog, QuestionOption,
    QuestionPrompt, QuestionRequest as QuestionPromptRequest, QuestionType, SessionDeleteState,
    SessionExportDialog, SessionItem, SessionListDialog, SessionRenameDialog, SessionView,
    SkillListDialog, SlashCommandPopup, StashItem, StatusDialog, StatusLine, SubagentDialog,
    TagDialog, TaskKind, ThemeListDialog, ThemeOption, TimelineDialog, TimelineEntry, Toast,
    ToastVariant,
};
use crate::context::keybind::LeaderKeyState;
use crate::context::{
    AppContext, McpConnectionStatus, McpServerStatus, Message, MessagePart as ContextMessagePart,
    MessageRole, RevertInfo, Session, SessionStatus, TodoItem, TokenUsage,
};
use crate::event::{CustomEvent, Event, StateChange};
use crate::router::Route;
use crate::ui::{Clipboard, Selection};

// TS parity: renderer targetFps is 60, ~16ms frame budget.
const TICK_RATE_MS: u64 = 16;
const MAX_EVENTS_PER_FRAME: usize = 256;

/// メッセージ時刻をローカルタイムゾーンの `HH:MM:SS` で表示する。
/// wire/DB 上の値は UTC のまま保持し、描画時のみ変換する。
fn format_message_timestamp(created_at: &chrono::DateTime<Utc>) -> String {
    created_at
        .with_timezone(&chrono::Local)
        .format("%H:%M:%S")
        .to_string()
}

/// 無修飾 `q` で終了してよいか。未送信入力がある、またはプロンプトが
/// テキスト入力を捕捉している場合は `q` を文字として扱う。
fn should_exit_on_unmodified_q(prompt_has_input: bool, prompt_captures_text: bool) -> bool {
    !(prompt_has_input || prompt_captures_text)
}

/// Tab / BackTab を補完へ委譲すべきか。候補があるか未送信入力があれば
/// エージェント切替より補完を優先する。
fn should_delegate_navigation_key_to_prompt(has_input: bool, has_candidates: bool) -> bool {
    has_input || has_candidates
}

/// Paste をプロンプトへ挿入してよいか。permission → question → dialog の
/// 順に入力を占有する層があれば挿入しない。
fn should_insert_paste_into_prompt(
    permission_open: bool,
    question_open: bool,
    dialog_open: bool,
) -> bool {
    !(permission_open || question_open || dialog_open)
}

/// モーダルの単文字ショートカットは Ctrl/Alt が付いていないときだけ発動する。
/// これにより Ctrl+A などのアプリ共通キーバインドをモーダルが横取りしない。
fn is_plain_shortcut(key: KeyEvent) -> bool {
    !key.modifiers.contains(KeyModifiers::CONTROL) && !key.modifiers.contains(KeyModifiers::ALT)
}

/// 権限プロンプトのキー入力をアクションへ変換する。
///
/// Enter / Esc は暗黙 approve を避け、既定で Deny（fail-safe）にする。
/// Allow は明示的な `y` / `a` でのみ発動する。
fn permission_action_for_key(key: KeyEvent) -> Option<PermissionAction> {
    match key.code {
        KeyCode::Enter | KeyCode::Esc => Some(PermissionAction::Deny),
        KeyCode::Char('y') if is_plain_shortcut(key) => Some(PermissionAction::Approve),
        KeyCode::Char('n') if is_plain_shortcut(key) => Some(PermissionAction::Deny),
        KeyCode::Char('a') if is_plain_shortcut(key) => Some(PermissionAction::ApproveAlways),
        _ => None,
    }
}

/// raw mode の解除を保証する RAII ガード（エラー時も端末状態を戻す）。
struct TerminalRawModeGuard;

impl Drop for TerminalRawModeGuard {
    fn drop(&mut self) {
        let _ = crossterm::terminal::disable_raw_mode();
    }
}

/// 秘密情報入力のキー処理結果。
enum SecretInputStep {
    Continue,
    Submit,
    Cancel,
}

/// エコーオフ入力の 1 キーを処理する。Enter で確定、Esc / Backspace を扱う。
fn apply_secret_key(secret: &mut String, code: KeyCode) -> SecretInputStep {
    match code {
        KeyCode::Char(c) => {
            secret.push(c);
            SecretInputStep::Continue
        }
        KeyCode::Backspace => {
            secret.pop();
            SecretInputStep::Continue
        }
        KeyCode::Enter => SecretInputStep::Submit,
        KeyCode::Esc => SecretInputStep::Cancel,
        _ => SecretInputStep::Continue,
    }
}

/// A question request received from the server.
///
/// Multi-question requests are asked sequentially; answers are accumulated
/// per question and posted back once every question has been answered.
struct PendingQuestionRequest {
    request_id: String,
    questions: Vec<crate::api::QuestionInfo>,
    index: usize,
    answers: Vec<Vec<String>>,
}

pub struct App {
    context: Arc<AppContext>,
    state: AppState,
    terminal: terminal::Tui,
    event_rx: Receiver<Event>,
    /// SSE リスナへ終了を通知するフラグ。Drop で set してから join する。
    sse_shutdown: Arc<AtomicBool>,
    /// SSE リスナの JoinHandle。App 終了後にポーリングが残らないようにする。
    sse_listener: Option<thread::JoinHandle<()>>,
    /// 入力ポーリングスレッドへ終了を通知するフラグ。Drop で set してから join する。
    input_shutdown: Arc<AtomicBool>,
    /// 入力ポーリングスレッドの JoinHandle。App 終了後にポーリングが残らないようにする。
    input_listener: Option<thread::JoinHandle<()>>,
    prompt: Prompt,
    selection: Selection,
    session_view: Option<SessionView>,
    active_session_id: Option<String>,
    command_palette: CommandPalette,
    slash_popup: SlashCommandPopup,
    leader_state: LeaderKeyState,
    model_select: ModelSelectDialog,
    agent_select: AgentSelectDialog,
    alert_dialog: AlertDialog,
    help_dialog: HelpDialog,
    session_list_dialog: SessionListDialog,
    session_rename_dialog: SessionRenameDialog,
    session_export_dialog: SessionExportDialog,
    prompt_stash_dialog: PromptStashDialog,
    skill_list_dialog: SkillListDialog,
    theme_list_dialog: ThemeListDialog,
    status_dialog: StatusDialog,
    mcp_dialog: McpDialog,
    timeline_dialog: TimelineDialog,
    fork_dialog: ForkDialog,
    provider_dialog: ProviderDialog,
    subagent_dialog: SubagentDialog,
    tag_dialog: TagDialog,
    permission_prompt: PermissionPrompt,
    question_prompt: QuestionPrompt,
    pending_question_requests: VecDeque<PendingQuestionRequest>,
    active_question_request: Option<PendingQuestionRequest>,
    toast: Toast,
    /// Snapshot of rendered screen lines for text selection copy.
    screen_lines: Vec<String>,
    available_models: HashSet<String>,
    model_variants: HashMap<String, Vec<String>>,
    model_variant_selection: HashMap<String, Option<String>>,
    pending_initial_submit: bool,
    pending_session_sync: Option<String>,
    last_session_sync: Instant,
    last_aux_sync: Instant,
    event_caused_change: bool,
}

impl App {
    pub fn new() -> anyhow::Result<Self> {
        let (event_tx, event_rx) = mpsc::channel();
        let event_tx_input = event_tx.clone();
        let context = Arc::new(AppContext::new());
        let terminal = terminal::init()?;
        let mut prompt = Prompt::new(context.clone())
            .with_placeholder("Ask anything... \"Fix a TODO in the codebase\"");
        let mut pending_initial_submit = false;
        let mut initial_session_id: Option<String> = None;

        if let Ok(dir) = std::env::current_dir() {
            *context.directory.write() = dir.display().to_string();
        }

        let base_url = resolve_tui_base_url();
        let auth_token = resolve_tui_auth_token();
        context.set_auth_token(auth_token.clone());
        let api_client = Arc::new(ApiClient::with_auth_token(
            base_url.clone(),
            auth_token.as_deref(),
        ));
        context.set_api_client(api_client);
        let sse_shutdown = Arc::new(AtomicBool::new(false));
        let sse_listener = spawn_server_event_listener(
            event_tx.clone(),
            base_url,
            context.clone(),
            sse_shutdown.clone(),
        );

        if let Ok(agent) = std::env::var("OPENCODE_TUI_AGENT") {
            let agent = agent.trim();
            if !agent.is_empty() {
                context.set_agent(agent.to_string());
            }
        }
        if let Ok(model) = std::env::var("OPENCODE_TUI_MODEL") {
            let model = model.trim();
            if !model.is_empty() {
                context.set_model_selection(model.to_string(), provider_from_model(model));
                context.set_model_variant(None);
            }
        }
        if let Ok(session_id) = std::env::var("OPENCODE_TUI_SESSION") {
            let session_id = session_id.trim();
            if !session_id.is_empty() {
                initial_session_id = Some(session_id.to_string());
                context.navigate(Route::Session {
                    session_id: session_id.to_string(),
                });
            }
        }
        if let Ok(initial_prompt) = std::env::var("OPENCODE_TUI_PROMPT") {
            let initial_prompt = initial_prompt.trim();
            if !initial_prompt.is_empty() {
                prompt.set_input(initial_prompt.to_string());
                pending_initial_submit = true;
            }
        }
        {
            let theme = context.theme.read().clone();
            let agent = context.current_agent.read().clone();
            prompt.set_spinner_color(agent_color_from_name(&theme, &agent));
        }

        let input_shutdown = Arc::new(AtomicBool::new(false));
        let input_listener = spawn_input_listener(event_tx_input, input_shutdown.clone());

        let mut app = Self {
            context,
            state: AppState::default(),
            terminal,
            event_rx,
            sse_shutdown,
            sse_listener: Some(sse_listener),
            input_shutdown,
            input_listener: Some(input_listener),
            prompt,
            selection: Selection::new(),
            session_view: None,
            active_session_id: None,
            command_palette: CommandPalette::new(),
            slash_popup: SlashCommandPopup::new(),
            leader_state: LeaderKeyState::new(),
            model_select: ModelSelectDialog::new(),
            agent_select: AgentSelectDialog::new(),
            alert_dialog: AlertDialog::info(""),
            help_dialog: HelpDialog::new(),
            session_list_dialog: SessionListDialog::new(),
            session_rename_dialog: SessionRenameDialog::new(),
            session_export_dialog: SessionExportDialog::new(),
            prompt_stash_dialog: PromptStashDialog::new(),
            skill_list_dialog: SkillListDialog::new(),
            theme_list_dialog: ThemeListDialog::new(),
            status_dialog: StatusDialog::new(),
            mcp_dialog: McpDialog::new(),
            timeline_dialog: TimelineDialog::new(),
            fork_dialog: ForkDialog::new(),
            provider_dialog: ProviderDialog::new(),
            subagent_dialog: SubagentDialog::new(),
            tag_dialog: TagDialog::new(),
            permission_prompt: PermissionPrompt::new(),
            question_prompt: QuestionPrompt::new(),
            pending_question_requests: VecDeque::new(),
            active_question_request: None,
            toast: Toast::new(),
            screen_lines: Vec::new(),
            available_models: HashSet::new(),
            model_variants: HashMap::new(),
            model_variant_selection: HashMap::new(),
            pending_initial_submit,
            pending_session_sync: None,
            last_session_sync: Instant::now(),
            last_aux_sync: Instant::now(),
            event_caused_change: true,
        };

        app.refresh_model_dialog();
        app.refresh_agent_dialog();
        let _ = app.refresh_skill_list_dialog();
        app.refresh_session_list_dialog();
        app.refresh_theme_list_dialog();
        let _ = app.refresh_lsp_status();
        let _ = app.refresh_mcp_dialog();

        if let Some(session_id) = initial_session_id {
            let _ = app.sync_session_from_server(&session_id);
            app.ensure_session_view(&session_id);
        }
        app.sync_prompt_spinner_style();
        app.sync_prompt_spinner_state();

        Ok(app)
    }

    pub fn run(&mut self) -> anyhow::Result<()> {
        self.draw()?;

        while self.state != AppState::Exiting {
            let mut should_draw = false;

            let first_event = match self
                .event_rx
                .recv_timeout(Duration::from_millis(TICK_RATE_MS))
            {
                Ok(event) => Some(event),
                Err(mpsc::RecvTimeoutError::Timeout) => None,
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            };

            if let Some(event) = first_event {
                self.handle_event(&event)?;
                should_draw |= self.event_caused_change;

                let mut deferred_mouse_move: Option<Event> = None;
                for _ in 0..MAX_EVENTS_PER_FRAME {
                    let next = match self.event_rx.try_recv() {
                        Ok(next) => next,
                        Err(mpsc::TryRecvError::Empty) => break,
                        Err(mpsc::TryRecvError::Disconnected) => {
                            self.state = AppState::Exiting;
                            break;
                        }
                    };

                    let is_mouse_move = matches!(
                        next,
                        Event::Mouse(crossterm::event::MouseEvent {
                            kind: crossterm::event::MouseEventKind::Moved,
                            ..
                        })
                    );

                    if is_mouse_move {
                        deferred_mouse_move = Some(next);
                        continue;
                    }

                    if let Some(moved) = deferred_mouse_move.take() {
                        self.handle_event(&moved)?;
                        should_draw |= self.event_caused_change;
                    }

                    self.handle_event(&next)?;
                    should_draw |= self.event_caused_change;
                }

                if let Some(moved) = deferred_mouse_move {
                    self.handle_event(&moved)?;
                    should_draw |= self.event_caused_change;
                }
            }

            if should_draw {
                self.draw()?;
            }
        }

        terminal::restore()?;
        Ok(())
    }

    /// 端末エコーを無効化して 1 行の秘密情報（トークン）を読み取る。
    ///
    /// 対話端末でない、または raw mode を開始できない場合は `Ok(None)` を返し、
    /// 呼び出し側は `--token` / 環境変数の案内へフォールバックする。
    /// 入力値はログ・エラーへ一切含めない。
    pub fn read_secret_from_terminal(prompt: &str) -> anyhow::Result<Option<String>> {
        if !io::stdin().is_terminal() {
            return Ok(None);
        }

        print!("{}", prompt);
        io::stdout().flush()?;

        if crossterm::terminal::enable_raw_mode().is_err() {
            println!();
            return Ok(None);
        }

        let outcome: anyhow::Result<Option<String>> = {
            let _guard = TerminalRawModeGuard;
            let mut secret = String::new();
            loop {
                match crossterm::event::read() {
                    Ok(CrosstermEvent::Key(key)) if key.kind != KeyEventKind::Release => {
                        // raw mode では Ctrl+C がシグナルにならないため明示的にキャンセル扱い。
                        if key.modifiers.contains(KeyModifiers::CONTROL)
                            && matches!(key.code, KeyCode::Char('c'))
                        {
                            break Ok(None);
                        }
                        match apply_secret_key(&mut secret, key.code) {
                            SecretInputStep::Continue => {}
                            SecretInputStep::Submit => {
                                break Ok(Some(secret.trim().to_string()));
                            }
                            SecretInputStep::Cancel => break Ok(None),
                        }
                    }
                    Ok(_) => {}
                    Err(error) => {
                        break Err(anyhow::anyhow!("Failed to read terminal input: {}", error));
                    }
                }
            }
        };

        // エコーオフでは Enter が改行を表示しないため、raw mode 解除後に改行を出す。
        println!();
        outcome
    }

    /// Reply to a permission request, surfacing delivery failures to the user.
    ///
    /// 送信失敗時は `Err` を返す。クライアント未設定もサイレントに捨てず、
    /// 呼び出し側がローカルの要求を残して再試行できるようにする。
    fn reply_permission(&mut self, id: &str, decision: &str) -> anyhow::Result<()> {
        let Some(client) = self.context.get_api_client() else {
            // id・トークン・パスを出さない固定文言（他失敗経路とログ方針を統一、R4）。
            tracing::warn!("permission reply skipped: API client unavailable");
            self.toast.show(
                ToastVariant::Error,
                "Failed to send permission reply: API client unavailable",
                3000,
            );
            return Err(anyhow::anyhow!("API client unavailable"));
        };
        if let Err(error) = client.reply_permission(id, decision) {
            self.toast.show(
                ToastVariant::Error,
                &format!("Failed to send permission reply: {}", error),
                3000,
            );
            return Err(error);
        }
        Ok(())
    }

    /// 権限アクションをプロンプトへ適用し、サーバへ reply をちょうど 1 回送る。
    ///
    /// キーボード入力とマウスクリックの両方からこれを呼ぶことで、
    /// 決定文字列（once / reject / always）と id の扱いを一致させる。
    /// Enter / Esc の既定 Deny は `permission_action_for_key` が
    /// `PermissionAction::Deny` を返すことで維持され、ここでは変更しない。
    ///
    /// fail-safe のため、要求を削除する前に reply を送り、成功時のみ削除する。
    /// 送信失敗時は要求を残し（トーストは `reply_permission` が表示）、
    /// ユーザーが同じ要求を再試行できるようにする。
    fn apply_permission_action(&mut self, action: PermissionAction) {
        // 削除せずに現在の要求を参照して id を取得する。
        let Some(request) = self.permission_prompt.current_request() else {
            return;
        };
        let id = request.id.clone();
        let decision = match action {
            PermissionAction::Approve => "once",
            PermissionAction::Deny => "reject",
            PermissionAction::ApproveAlways => "always",
        };

        // 先に送信し、失敗したら要求を残す（ローカルだけプロンプトが消えるのを防ぐ）。
        if self.reply_permission(&id, decision).is_err() {
            return;
        }

        // 送信成功時のみ ID を忘却する。失敗時は上の early return でキーが残り、
        // 同じ要求を再試行しても二重 enqueue としてスキップされない。
        forget_request_seen(&self.context, "permission", &id);

        // 送信成功時のみ要求を削除してプロンプトを閉じる。
        match action {
            PermissionAction::Approve | PermissionAction::ApproveAlways => {
                self.permission_prompt.approve();
            }
            PermissionAction::Deny => {
                self.permission_prompt.deny();
            }
        }
    }

    fn handle_event(&mut self, event: &Event) -> anyhow::Result<()> {
        self.event_caused_change = true;

        match event {
            Event::Key(key) => {
                // Handle inline permission prompt before dialogs
                if self.permission_prompt.is_open {
                    // Enter/Esc は暗黙 approve せず既定で Deny（fail-safe）。
                    if let Some(action) = permission_action_for_key(*key) {
                        self.apply_permission_action(action);
                    }
                    return Ok(());
                }

                // Handle inline question prompt before dialogs
                if self.question_prompt.is_open {
                    match key.code {
                        KeyCode::Up => self.question_prompt.move_up(),
                        KeyCode::Down => self.question_prompt.move_down(),
                        KeyCode::Char(' ') if is_plain_shortcut(*key) => {
                            self.question_prompt.toggle_selected();
                        }
                        KeyCode::Enter => {
                            // 未選択時は confirm_selected() が None を返し、送信しない。
                            if let Some(answer) = self.question_prompt.confirm_selected() {
                                self.submit_question_answer(answer);
                            }
                        }
                        KeyCode::Esc => {
                            self.reject_active_question();
                        }
                        KeyCode::Char(c) if is_plain_shortcut(*key) => {
                            self.question_prompt.type_char(c);
                        }
                        KeyCode::Backspace => self.question_prompt.backspace(),
                        _ => {}
                    }
                    return Ok(());
                }

                if self.handle_dialog_key(*key)? {
                    return Ok(());
                }

                // Leader key handling
                if self.leader_state.active {
                    if self.leader_state.check_timeout() {
                        // Leader timed out, fall through to normal handling
                        self.prompt.set_leader_active(false);
                    } else {
                        let action = match key.code {
                            KeyCode::Char('n') => Some(CommandAction::NewSession),
                            KeyCode::Char('l') => Some(CommandAction::SwitchSession),
                            KeyCode::Char('m') => Some(CommandAction::SwitchModel),
                            KeyCode::Char('a') => Some(CommandAction::SwitchAgent),
                            KeyCode::Char('t') => Some(CommandAction::SwitchTheme),
                            KeyCode::Char('b') => Some(CommandAction::ToggleSidebar),
                            KeyCode::Char('s') => Some(CommandAction::ViewStatus),
                            KeyCode::Char('q') => Some(CommandAction::Exit),
                            KeyCode::Char('u') => Some(CommandAction::Undo),
                            KeyCode::Char('r') => Some(CommandAction::Redo),
                            _ => None,
                        };
                        self.leader_state.reset();
                        self.prompt.set_leader_active(false);
                        if let Some(action) = action {
                            self.execute_command_action(action)?;
                        }
                        return Ok(());
                    }
                }

                // Ctrl+X starts leader key sequence
                if key.code == KeyCode::Char('x') && key.modifiers == KeyModifiers::CONTROL {
                    self.leader_state.start(KeyCode::Char('x'));
                    self.prompt.set_leader_active(true);
                    return Ok(());
                }

                // Ctrl+Shift+C (crossterm reports uppercase 'C' with SHIFT modifier)
                // 選択があるときだけ画面選択をコピーする。選択が無い場合は
                // 下流の input_copy (copy_prompt_to_clipboard) へ fall through させる。
                if (key.code == KeyCode::Char('C') || key.code == KeyCode::Char('c'))
                    && key.modifiers.contains(KeyModifiers::CONTROL)
                    && key.modifiers.contains(KeyModifiers::SHIFT)
                {
                    // copy_selection() が選択をクリアするため、判定前に退避する。
                    let had_selection = self.selection.is_active();
                    if had_selection {
                        self.copy_selection();
                    } else if key.code == KeyCode::Char('C') {
                        // 大文字 'C' は input_copy（小文字 'c'）にマッチしないため、
                        // 選択なしではここでプロンプトを直接コピーする（R4）。
                        self.copy_prompt_to_clipboard();
                    }
                    // 小文字 'c' かつ選択なしは input_copy へ fall through。
                    if had_selection || key.code == KeyCode::Char('C') {
                        return Ok(());
                    }
                }

                if key.code == KeyCode::Char('c') && key.modifiers == KeyModifiers::CONTROL {
                    // If there's an active selection, copy it instead of exiting (TS parity)
                    if self.selection.is_active() {
                        self.copy_selection();
                        return Ok(());
                    }
                    self.state = AppState::Exiting;
                    return Ok(());
                }

                if key.code == KeyCode::Esc && self.selection.is_active() {
                    self.selection.clear();
                    return Ok(());
                }

                // 未送信入力がある／シェルモード中は 'q' を終了キーとして扱わず、
                // 文字としてプロンプトへ渡す（入力の誤消去を防ぐ）。
                if key.code == KeyCode::Char('q')
                    && key.modifiers.is_empty()
                    && should_exit_on_unmodified_q(
                        self.prompt.has_input(),
                        self.prompt_captures_text(),
                    )
                {
                    self.state = AppState::Exiting;
                    return Ok(());
                }

                if self.matches_keybind("session_interrupt", *key) {
                    if self.prompt.is_shell_mode() {
                        self.prompt.exit_shell_mode();
                        self.prompt.clear_interrupt_confirmation();
                        return Ok(());
                    }
                    if let Route::Session { session_id } = self.context.current_route() {
                        let status = {
                            let session_ctx = self.context.session.read();
                            session_ctx.status(&session_id).clone()
                        };
                        if !matches!(status, SessionStatus::Idle) {
                            if !self.prompt.register_interrupt_keypress() {
                                return Ok(());
                            }
                            if let Some(client) = self.context.get_api_client() {
                                let _ = client.abort_session(&session_id);
                            }
                            self.prompt.clear_interrupt_confirmation();
                            self.set_session_status(&session_id, SessionStatus::Idle);
                            self.sync_prompt_spinner_state();
                            return Ok(());
                        }
                    }
                    self.prompt.clear_interrupt_confirmation();
                    return Ok(());
                }

                if self.matches_keybind("input_paste", *key) {
                    self.paste_clipboard_to_prompt();
                    return Ok(());
                }
                if self.matches_keybind("input_copy", *key) {
                    self.copy_prompt_to_clipboard();
                    return Ok(());
                }
                if self.matches_keybind("input_cut", *key) {
                    self.cut_prompt_to_clipboard();
                    return Ok(());
                }
                if self.matches_keybind("history_previous", *key) {
                    self.prompt.history_previous_entry();
                    return Ok(());
                }
                if self.matches_keybind("history_next", *key) {
                    self.prompt.history_next_entry();
                    return Ok(());
                }
                if self.matches_keybind("page_up", *key) {
                    if let Route::Session { .. } = self.context.current_route() {
                        if let Some(ref mut sv) = self.session_view {
                            sv.scroll_page_up();
                            return Ok(());
                        }
                    }
                }
                if self.matches_keybind("page_down", *key) {
                    if let Route::Session { .. } = self.context.current_route() {
                        if let Some(ref mut sv) = self.session_view {
                            sv.scroll_page_down();
                            return Ok(());
                        }
                    }
                }

                if self.matches_keybind("command_palette", *key) {
                    self.sync_command_palette_labels();
                    self.command_palette.open();
                    return Ok(());
                }
                if self.matches_keybind("model_cycle", *key) {
                    self.refresh_model_dialog();
                    self.model_select.open();
                    return Ok(());
                }
                if self.matches_keybind("agent_cycle", *key) {
                    if should_delegate_navigation_key_to_prompt(
                        self.prompt.has_input(),
                        self.prompt.has_completion_candidates(),
                    ) {
                        // 入力中／補完候補あり: Tab は補完として扱う（agent 切替は行わない）。
                        self.prompt.handle_key(*key);
                    } else {
                        self.cycle_agent(1);
                    }
                    return Ok(());
                }
                if self.matches_keybind("agent_cycle_reverse", *key) {
                    if should_delegate_navigation_key_to_prompt(
                        self.prompt.has_input(),
                        self.prompt.has_completion_candidates(),
                    ) {
                        // 入力中／補完候補あり: BackTab は前の補完候補へ戻す。
                        self.prompt.handle_key(*key);
                    } else {
                        self.cycle_agent(-1);
                    }
                    return Ok(());
                }
                if self.matches_keybind("variant_cycle", *key) {
                    self.cycle_model_variant();
                    return Ok(());
                }
                if self.matches_keybind("sidebar_toggle", *key) {
                    self.context.toggle_sidebar();
                    return Ok(());
                }
                if self.matches_keybind("display_thinking", *key) {
                    self.context.toggle_thinking();
                    return Ok(());
                }
                if self.matches_keybind("tool_details", *key) {
                    self.context.toggle_tool_details();
                    return Ok(());
                }
                if self.matches_keybind("input_clear", *key) {
                    self.prompt.clear();
                    return Ok(());
                }
                if self.matches_keybind("input_newline", *key) {
                    let route = self.context.current_route();
                    if matches!(route, Route::Home | Route::Session { .. }) {
                        self.prompt.insert_text("\n");
                        return Ok(());
                    }
                }
                if self.matches_keybind("help_toggle", *key) {
                    self.help_dialog.open();
                    return Ok(());
                }

                // Slash command popup: open when '/' is typed at position 0
                if key.code == KeyCode::Char('/')
                    && key.modifiers.is_empty()
                    && self.prompt.cursor_position() == 0
                    && self.prompt.get_input().is_empty()
                {
                    self.slash_popup.open();
                    return Ok(());
                }

                let route = self.context.current_route();
                match route {
                    Route::Home | Route::Session { .. } => {
                        if key.code == KeyCode::Enter && key.modifiers.is_empty() {
                            self.submit_prompt()?;
                        } else if !key.modifiers.contains(KeyModifiers::CONTROL)
                            && !key.modifiers.contains(KeyModifiers::ALT)
                        {
                            self.prompt.handle_key(*key);
                        }
                    }
                    _ => {
                        if !key.modifiers.contains(KeyModifiers::CONTROL)
                            && !key.modifiers.contains(KeyModifiers::ALT)
                        {
                            self.prompt.handle_key(*key);
                        }
                    }
                }
            }
            Event::Resize(_, _) => {
                self.terminal.autoresize()?;
            }
            Event::Mouse(mouse_event) => {
                use crossterm::event::{MouseButton, MouseEventKind};
                match mouse_event.kind {
                    MouseEventKind::Down(button) => {
                        let col = mouse_event.column;
                        let row = mouse_event.row;

                        if button == MouseButton::Right {
                            // Right-click copies selection (if any) then clears it
                            if self.selection.is_active() {
                                self.copy_selection();
                            }
                            return Ok(());
                        }

                        if self.permission_prompt.is_open {
                            self.permission_prompt.handle_click(col, row);
                            if let Some(action) = self.permission_prompt.take_pending_action() {
                                // キーボード経路と同じくサーバへ reply を送る。
                                // 戻り値を捨てるとローカル表示は消えるがサーバ側の
                                // pending が残りツールがハングするため必ず送る。
                                self.apply_permission_action(action);
                            }
                            return Ok(());
                        }

                        // Question prompt click
                        if self.question_prompt.is_open {
                            self.question_prompt.handle_click(col, row);
                            return Ok(());
                        }

                        if self.handle_dialog_mouse(mouse_event)? {
                            return Ok(());
                        }

                        if button == MouseButton::Left {
                            if let Route::Session { .. } = self.context.current_route() {
                                if let Some(ref mut sv) = self.session_view {
                                    if sv.handle_sidebar_click(col, row) {
                                        return Ok(());
                                    }
                                    if sv.is_point_in_sidebar(col, row) {
                                        return Ok(());
                                    }
                                    if sv.handle_click(col, row) {
                                        return Ok(());
                                    }
                                }
                            }
                            // Clear previous selection and start a new one
                            self.selection.start(row, col);
                        }
                    }
                    MouseEventKind::ScrollUp => {
                        if self.handle_dialog_mouse(mouse_event)? {
                            return Ok(());
                        }
                        if let Some(ref mut sv) = self.session_view {
                            if !sv.scroll_sidebar_up_at(mouse_event.column, mouse_event.row) {
                                sv.scroll_up_mouse();
                            }
                        }
                    }
                    MouseEventKind::ScrollDown => {
                        if self.handle_dialog_mouse(mouse_event)? {
                            return Ok(());
                        }
                        if let Some(ref mut sv) = self.session_view {
                            if !sv.scroll_sidebar_down_at(mouse_event.column, mouse_event.row) {
                                sv.scroll_down_mouse();
                            }
                        }
                    }
                    MouseEventKind::Drag(_) => {
                        let col = mouse_event.column;
                        let row = mouse_event.row;
                        self.selection.update(row, col);
                    }
                    MouseEventKind::Moved => {
                        if self.handle_dialog_mouse(mouse_event)? {
                            return Ok(());
                        }
                        self.event_caused_change = false;
                    }
                    MouseEventKind::Up(_) => {
                        self.selection.finalize();
                    }
                    _ => {}
                }
            }
            Event::Paste(text) if !text.is_empty() => {
                // Key と同じく permission → question → dialog の順で入力の
                // 所有者を確認し、モーダル表示中はプロンプトへ挿入しない。
                if should_insert_paste_into_prompt(
                    self.permission_prompt.is_open,
                    self.question_prompt.is_open,
                    self.has_open_dialog_layer(),
                ) {
                    self.prompt.insert_text(text);
                }
            }
            Event::Custom(event) => match event {
                CustomEvent::StateChanged(StateChange::SessionUpdated(session_id)) => {
                    if let Route::Session { session_id: active } = self.context.current_route() {
                        if active == *session_id {
                            if self.last_session_sync.elapsed() >= Duration::from_millis(50) {
                                let _ = self.sync_session_from_server(session_id);
                                self.pending_session_sync = None;
                            } else {
                                self.pending_session_sync = Some(session_id.to_string());
                            }
                        }
                    }
                    self.sync_prompt_spinner_state();
                }
                CustomEvent::StateChanged(StateChange::SessionStatusBusy(session_id)) => {
                    self.set_session_status(session_id, SessionStatus::Running);
                    self.sync_prompt_spinner_state();
                }
                CustomEvent::StateChanged(StateChange::SessionStatusIdle(session_id)) => {
                    self.set_session_status(session_id, SessionStatus::Idle);
                    self.sync_prompt_spinner_state();
                }
                CustomEvent::StateChanged(StateChange::SessionStatusRetrying {
                    session_id,
                    attempt,
                    message,
                    next,
                }) => {
                    self.set_session_status(
                        session_id,
                        SessionStatus::Retrying {
                            message: message.clone(),
                            attempt: *attempt,
                            next: *next,
                        },
                    );
                    self.sync_prompt_spinner_state();
                }
                CustomEvent::QuestionAsked(request) => {
                    self.enqueue_question_request(request.clone());
                }
                CustomEvent::PermissionRequested {
                    id,
                    permission_type,
                    resource,
                    tool_name,
                } => {
                    self.permission_prompt.add_request(PermissionRequest {
                        id: id.clone(),
                        permission_type: PermissionType::from_permission_str(permission_type),
                        resource: resource.clone(),
                        tool_name: tool_name.clone(),
                    });
                    self.context
                        .set_pending_permissions(self.permission_prompt.pending_count());
                }
                _ => {}
            },
            Event::Tick => {
                let mut tick_changed = false;
                tick_changed |= self.toast.tick(TICK_RATE_MS);
                tick_changed |= self.prompt.tick_spinner(TICK_RATE_MS);
                tick_changed |= self.sync_prompt_spinner_state();

                if self.pending_initial_submit && !self.prompt.get_input().trim().is_empty() {
                    self.pending_initial_submit = false;
                    self.submit_prompt()?;
                    tick_changed = true;
                }

                let route = self.context.current_route();
                if let Route::Session { session_id } = route {
                    if self.pending_session_sync.as_deref() == Some(session_id.as_str())
                        && self.last_session_sync.elapsed() >= Duration::from_millis(50)
                        && self.sync_session_from_server(&session_id).is_ok()
                    {
                        tick_changed = true;
                        self.pending_session_sync = None;
                    }
                    if self.last_session_sync.elapsed() >= Duration::from_secs(2)
                        && self.sync_session_from_server(&session_id).is_ok()
                    {
                        tick_changed = true;
                    }
                }
                if self.last_aux_sync.elapsed() >= Duration::from_secs(5) {
                    self.refresh_session_list_dialog();
                    let _ = self.refresh_mcp_dialog();
                    self.last_aux_sync = Instant::now();
                    tick_changed = true;
                }
                self.event_caused_change = tick_changed;
            }
            _ => {}
        }

        Ok(())
    }

    /// プロンプトが現在テキスト入力を捕捉しているか（未送信入力またはシェルモード）。
    fn prompt_captures_text(&self) -> bool {
        self.prompt.has_input() || self.prompt.is_shell_mode()
    }

    fn has_open_dialog_layer(&self) -> bool {
        self.alert_dialog.is_open()
            || self.help_dialog.is_open()
            || self.status_dialog.is_open()
            || self.session_rename_dialog.is_open()
            || self.session_export_dialog.is_open()
            || self.prompt_stash_dialog.is_open()
            || self.skill_list_dialog.is_open()
            || self.slash_popup.is_open()
            || self.command_palette.is_open()
            || self.model_select.is_open()
            || self.agent_select.is_open()
            || self.session_list_dialog.is_open()
            || self.theme_list_dialog.is_open()
            || self.mcp_dialog.is_open()
            || self.timeline_dialog.is_open()
            || self.fork_dialog.is_open()
            || self.provider_dialog.is_open()
            || self.subagent_dialog.is_open()
            || self.tag_dialog.is_open()
    }

    fn close_top_dialog(&mut self) -> bool {
        if self.alert_dialog.is_open() {
            self.alert_dialog.close();
            return true;
        }
        if self.help_dialog.is_open() {
            self.help_dialog.close();
            return true;
        }
        if self.status_dialog.is_open() {
            self.status_dialog.close();
            return true;
        }
        if self.session_rename_dialog.is_open() {
            self.session_rename_dialog.close();
            return true;
        }
        if self.session_export_dialog.is_open() {
            self.session_export_dialog.close();
            return true;
        }
        if self.prompt_stash_dialog.is_open() {
            self.prompt_stash_dialog.close();
            return true;
        }
        if self.skill_list_dialog.is_open() {
            self.skill_list_dialog.close();
            return true;
        }
        if self.slash_popup.is_open() {
            self.slash_popup.close();
            return true;
        }
        if self.command_palette.is_open() {
            self.command_palette.close();
            return true;
        }
        if self.model_select.is_open() {
            self.model_select.close();
            return true;
        }
        if self.agent_select.is_open() {
            self.agent_select.close();
            return true;
        }
        if self.session_list_dialog.is_open() {
            self.session_list_dialog.close();
            return true;
        }
        if self.theme_list_dialog.is_open() {
            let initial = self.theme_list_dialog.initial_theme_id().to_string();
            let _ = self.context.set_theme_by_name(&initial);
            self.theme_list_dialog.close();
            return true;
        }
        if self.mcp_dialog.is_open() {
            self.mcp_dialog.close();
            return true;
        }
        if self.timeline_dialog.is_open() {
            self.timeline_dialog.close();
            return true;
        }
        if self.fork_dialog.is_open() {
            self.fork_dialog.close();
            return true;
        }
        if self.provider_dialog.is_open() {
            self.provider_dialog.close();
            return true;
        }
        if self.subagent_dialog.is_open() {
            self.subagent_dialog.close();
            return true;
        }
        if self.tag_dialog.is_open() {
            self.tag_dialog.close();
            return true;
        }
        false
    }

    fn scroll_active_dialog(&mut self, up: bool) {
        if self.prompt_stash_dialog.is_open() {
            if up {
                self.prompt_stash_dialog.move_up();
            } else {
                self.prompt_stash_dialog.move_down();
            }
            return;
        }
        if self.skill_list_dialog.is_open() {
            if up {
                self.skill_list_dialog.move_up();
            } else {
                self.skill_list_dialog.move_down();
            }
            return;
        }
        if self.slash_popup.is_open() {
            if up {
                self.slash_popup.move_up();
            } else {
                self.slash_popup.move_down();
            }
            return;
        }
        if self.command_palette.is_open() {
            if up {
                self.command_palette.move_up();
            } else {
                self.command_palette.move_down();
            }
            return;
        }
        if self.model_select.is_open() {
            if up {
                self.model_select.move_up();
            } else {
                self.model_select.move_down();
            }
            return;
        }
        if self.agent_select.is_open() {
            if up {
                self.agent_select.move_up();
            } else {
                self.agent_select.move_down();
            }
            return;
        }
        if self.session_list_dialog.is_open() {
            if self.session_list_dialog.is_renaming() {
                return;
            }
            if up {
                self.session_list_dialog.move_up();
            } else {
                self.session_list_dialog.move_down();
            }
            return;
        }
        if self.theme_list_dialog.is_open() {
            if up {
                self.theme_list_dialog.move_up();
            } else {
                self.theme_list_dialog.move_down();
            }
            if let Some(theme_id) = self.theme_list_dialog.selected_theme_id() {
                let _ = self.context.set_theme_by_name(&theme_id);
            }
            return;
        }
        if self.mcp_dialog.is_open() {
            if up {
                self.mcp_dialog.move_up();
            } else {
                self.mcp_dialog.move_down();
            }
            return;
        }
        if self.timeline_dialog.is_open() {
            if up {
                self.timeline_dialog.move_up();
            } else {
                self.timeline_dialog.move_down();
            }
            return;
        }
        if self.fork_dialog.is_open() {
            if up {
                self.fork_dialog.move_up();
            } else {
                self.fork_dialog.move_down();
            }
            return;
        }
        if self.provider_dialog.is_open() {
            if up {
                self.provider_dialog.move_up();
            } else {
                self.provider_dialog.move_down();
            }
            return;
        }
        if self.subagent_dialog.is_open() {
            if up {
                self.subagent_dialog.scroll_up();
            } else {
                self.subagent_dialog.scroll_down(50);
            }
            return;
        }
        if self.tag_dialog.is_open() {
            if up {
                self.tag_dialog.move_up();
            } else {
                self.tag_dialog.move_down();
            }
        }
    }

    /// Left-click hit testing for general dialogs.
    ///
    /// Returns `true` when the click was inside the topmost dialog frame (and
    /// updates its selection when an item was hit). Returns `false` only for
    /// clicks on the backdrop, which the caller closes via `close_top_dialog`.
    /// Permission/question prompts keep their own `handle_click` path and are
    /// handled before this function in `handle_event`.
    fn handle_dialog_click(&mut self, col: u16, row: u16) -> bool {
        if self.alert_dialog.is_open() {
            return self.alert_dialog.handle_click(col, row);
        }
        if self.help_dialog.is_open() {
            return self.help_dialog.handle_click(col, row);
        }
        if self.status_dialog.is_open() {
            return self.status_dialog.handle_click(col, row);
        }
        if self.session_rename_dialog.is_open() {
            return self.session_rename_dialog.handle_click(col, row);
        }
        if self.session_export_dialog.is_open() {
            return self.session_export_dialog.handle_click(col, row);
        }
        if self.prompt_stash_dialog.is_open() {
            return self.prompt_stash_dialog.handle_click(col, row);
        }
        if self.skill_list_dialog.is_open() {
            return self.skill_list_dialog.handle_click(col, row);
        }
        if self.slash_popup.is_open() {
            return self.slash_popup.handle_click(col, row);
        }
        if self.command_palette.is_open() {
            return self.command_palette.handle_click(col, row);
        }
        if self.model_select.is_open() {
            return self.model_select.handle_click(col, row);
        }
        if self.agent_select.is_open() {
            return self.agent_select.handle_click(col, row);
        }
        if self.session_list_dialog.is_open() {
            return self.session_list_dialog.handle_click(col, row);
        }
        if self.theme_list_dialog.is_open() {
            return self.theme_list_dialog.handle_click(col, row);
        }
        if self.mcp_dialog.is_open() {
            return self.mcp_dialog.handle_click(col, row);
        }
        if self.timeline_dialog.is_open() {
            return self.timeline_dialog.handle_click(col, row);
        }
        if self.fork_dialog.is_open() {
            return self.fork_dialog.handle_click(col, row);
        }
        if self.provider_dialog.is_open() {
            return self.provider_dialog.handle_click(col, row);
        }
        if self.subagent_dialog.is_open() {
            return self.subagent_dialog.handle_click(col, row);
        }
        if self.tag_dialog.is_open() {
            return self.tag_dialog.handle_click(col, row);
        }
        false
    }

    fn handle_dialog_mouse(
        &mut self,
        mouse_event: &crossterm::event::MouseEvent,
    ) -> anyhow::Result<bool> {
        use crossterm::event::{MouseButton, MouseEventKind};

        if !self.has_open_dialog_layer() {
            return Ok(false);
        }

        match mouse_event.kind {
            MouseEventKind::ScrollUp => {
                self.scroll_active_dialog(true);
                self.event_caused_change = true;
                Ok(true)
            }
            MouseEventKind::ScrollDown => {
                self.scroll_active_dialog(false);
                self.event_caused_change = true;
                Ok(true)
            }
            MouseEventKind::Down(MouseButton::Left) => {
                let col = mouse_event.column;
                let row = mouse_event.row;
                if self.handle_dialog_click(col, row) {
                    // ダイアログ枠内（項目命中時は選択済み）。閉じない。
                    self.event_caused_change = true;
                } else {
                    // 背景クリックのみ最上位ダイアログを閉じる。
                    self.event_caused_change = self.close_top_dialog();
                }
                Ok(true)
            }
            MouseEventKind::Moved
            | MouseEventKind::Down(_)
            | MouseEventKind::Drag(_)
            | MouseEventKind::Up(_) => {
                self.event_caused_change = false;
                Ok(true)
            }
            _ => {
                self.event_caused_change = false;
                Ok(true)
            }
        }
    }

    fn handle_dialog_key(&mut self, key: KeyEvent) -> anyhow::Result<bool> {
        // 修飾付きの非文字キー（Alt+Enter / Alt+Up 等）はアプリ共通キーバインドへ委譲する。
        // Ctrl+C はダイアログ側で意図的に消費するため例外とする。
        if !is_plain_shortcut(key) && !matches!(key.code, KeyCode::Char(_)) {
            return Ok(false);
        }
        if self.alert_dialog.is_open() {
            match key.code {
                KeyCode::Esc | KeyCode::Enter => {
                    self.alert_dialog.close();
                }
                KeyCode::Char('c')
                    if key.modifiers.contains(KeyModifiers::CONTROL)
                        && !self.alert_dialog.message().is_empty() =>
                {
                    let _ = Clipboard::write_text(self.alert_dialog.message());
                }
                _ => {}
            }
            return Ok(true);
        }
        if self.help_dialog.is_open() {
            if matches!(key.code, KeyCode::Esc | KeyCode::Enter) {
                self.help_dialog.close();
            }
            return Ok(true);
        }
        if self.status_dialog.is_open() {
            if matches!(key.code, KeyCode::Esc | KeyCode::Enter) {
                self.status_dialog.close();
            }
            return Ok(true);
        }
        if self.session_rename_dialog.is_open() {
            match key.code {
                KeyCode::Esc => self.session_rename_dialog.close(),
                KeyCode::Backspace => self.session_rename_dialog.handle_backspace(),
                KeyCode::Enter => {
                    if let Some((session_id, title)) = self.session_rename_dialog.confirm() {
                        if let Some(client) = self.context.get_api_client() {
                            if let Err(err) = client.update_session_title(&session_id, &title) {
                                self.alert_dialog.set_message(&format!(
                                    "Failed to rename session `{}`:\n{}",
                                    session_id, err
                                ));
                                self.alert_dialog.open();
                            } else {
                                self.refresh_session_list_dialog();
                                let _ = self.sync_session_from_server(&session_id);
                            }
                        }
                    }
                }
                KeyCode::Char(c)
                    if !key.modifiers.contains(KeyModifiers::CONTROL)
                        && !key.modifiers.contains(KeyModifiers::ALT) =>
                {
                    self.session_rename_dialog.handle_input(c);
                }
                _ => {}
            }
            return Ok(true);
        }
        if self.session_export_dialog.is_open() {
            match key.code {
                KeyCode::Esc => self.session_export_dialog.close(),
                KeyCode::Backspace => self.session_export_dialog.handle_backspace(),
                KeyCode::Enter => {
                    if let Some(session_id) = self.session_export_dialog.session_id() {
                        let filename = self.session_export_dialog.filename().trim();
                        if filename.is_empty() {
                            self.alert_dialog
                                .set_message("Filename cannot be empty for export.");
                            self.alert_dialog.open();
                        } else {
                            match self.export_session_to_file(session_id, filename) {
                                Ok(path) => {
                                    self.alert_dialog.set_message(&format!(
                                        "Session exported to `{}`.",
                                        path.display()
                                    ));
                                    self.alert_dialog.open();
                                    self.session_export_dialog.close();
                                }
                                Err(err) => {
                                    self.alert_dialog.set_message(&format!(
                                        "Failed to export session:\n{}",
                                        err
                                    ));
                                    self.alert_dialog.open();
                                }
                            }
                        }
                    }
                }
                KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    if let Some(session_id) = self.session_export_dialog.session_id() {
                        match self.build_session_transcript(session_id) {
                            Some(text) => {
                                if let Err(err) = Clipboard::write_text(&text) {
                                    self.alert_dialog.set_message(&format!(
                                        "Failed to copy transcript to clipboard:\n{}",
                                        err
                                    ));
                                    self.alert_dialog.open();
                                } else {
                                    self.alert_dialog
                                        .set_message("Session transcript copied to clipboard.");
                                    self.alert_dialog.open();
                                }
                            }
                            None => {
                                self.alert_dialog
                                    .set_message("No transcript available for current session.");
                                self.alert_dialog.open();
                            }
                        }
                    }
                }
                KeyCode::Char(c)
                    if !key.modifiers.contains(KeyModifiers::CONTROL)
                        && !key.modifiers.contains(KeyModifiers::ALT) =>
                {
                    self.session_export_dialog.handle_input(c);
                }
                _ => {}
            }
            return Ok(true);
        }
        if self.prompt_stash_dialog.is_open() {
            match key.code {
                KeyCode::Esc => self.prompt_stash_dialog.close(),
                KeyCode::Up => self.prompt_stash_dialog.move_up(),
                KeyCode::Down => self.prompt_stash_dialog.move_down(),
                KeyCode::Backspace => self.prompt_stash_dialog.handle_backspace(),
                KeyCode::Enter => {
                    if let Some(index) = self.prompt_stash_dialog.selected_index() {
                        if self.prompt.load_stash(index) {
                            self.prompt_stash_dialog.close();
                        }
                    }
                }
                KeyCode::Char('d') if is_plain_shortcut(key) => {
                    if let Some(index) = self.prompt_stash_dialog.selected_index() {
                        if self.prompt.remove_stash(index) {
                            let entries = self
                                .prompt
                                .stash_entries()
                                .iter()
                                .cloned()
                                .map(|entry| StashItem {
                                    input: entry.input,
                                    created_at: entry.created_at,
                                })
                                .collect::<Vec<_>>();
                            self.prompt_stash_dialog.set_entries(entries);
                        }
                    }
                }
                KeyCode::Char(c)
                    if !key.modifiers.contains(KeyModifiers::CONTROL)
                        && !key.modifiers.contains(KeyModifiers::ALT) =>
                {
                    self.prompt_stash_dialog.handle_input(c);
                }
                _ => {}
            }
            return Ok(true);
        }
        if self.skill_list_dialog.is_open() {
            match key.code {
                KeyCode::Esc => self.skill_list_dialog.close(),
                KeyCode::Up => self.skill_list_dialog.move_up(),
                KeyCode::Down => self.skill_list_dialog.move_down(),
                KeyCode::Backspace => self.skill_list_dialog.handle_backspace(),
                KeyCode::Enter => {
                    if let Some(skill) = self.skill_list_dialog.selected_skill() {
                        self.prompt.set_input(format!("/{} ", skill));
                        self.skill_list_dialog.close();
                    }
                }
                KeyCode::Char(c)
                    if !key.modifiers.contains(KeyModifiers::CONTROL)
                        && !key.modifiers.contains(KeyModifiers::ALT) =>
                {
                    self.skill_list_dialog.handle_input(c);
                }
                _ => {}
            }
            return Ok(true);
        }

        if self.slash_popup.is_open() {
            match key.code {
                KeyCode::Esc => self.slash_popup.close(),
                KeyCode::Up => self.slash_popup.move_up(),
                KeyCode::Down => self.slash_popup.move_down(),
                KeyCode::Backspace if !self.slash_popup.handle_backspace() => {
                    self.slash_popup.close();
                }
                KeyCode::Enter => {
                    self.slash_popup.select_current();
                    if let Some(action) = self.slash_popup.take_action() {
                        self.execute_command_action(action)?;
                    }
                }
                KeyCode::Char(c)
                    if !key.modifiers.contains(KeyModifiers::CONTROL)
                        && !key.modifiers.contains(KeyModifiers::ALT) =>
                {
                    self.slash_popup.handle_input(c);
                }
                _ => {}
            }
            return Ok(true);
        }

        if self.command_palette.is_open() {
            match key.code {
                KeyCode::Esc => self.command_palette.close(),
                KeyCode::Up => self.command_palette.move_up(),
                KeyCode::Down => self.command_palette.move_down(),
                KeyCode::Backspace => self.command_palette.handle_backspace(),
                KeyCode::Enter => {
                    let action = self.command_palette.selected_action();
                    self.command_palette.close();
                    if let Some(action) = action {
                        self.execute_command_action(action)?;
                    }
                }
                KeyCode::Char(c)
                    if !key.modifiers.contains(KeyModifiers::CONTROL)
                        && !key.modifiers.contains(KeyModifiers::ALT) =>
                {
                    self.command_palette.handle_input(c);
                }
                _ => {}
            }
            return Ok(true);
        }

        if self.model_select.is_open() {
            match key.code {
                KeyCode::Esc => self.model_select.close(),
                KeyCode::Up => self.model_select.move_up(),
                KeyCode::Down => self.model_select.move_down(),
                KeyCode::Backspace => self.model_select.handle_backspace(),
                KeyCode::Enter => {
                    if let Some(model) = self.model_select.selected_model() {
                        let model_ref = format!("{}/{}", model.provider, model.id);
                        self.set_active_model_selection(model_ref, Some(model.provider.clone()));
                    }
                    self.model_select.close();
                }
                KeyCode::Char(c)
                    if !key.modifiers.contains(KeyModifiers::CONTROL)
                        && !key.modifiers.contains(KeyModifiers::ALT) =>
                {
                    self.model_select.handle_input(c);
                }
                _ => {}
            }
            return Ok(true);
        }

        if self.agent_select.is_open() {
            match key.code {
                KeyCode::Esc => self.agent_select.close(),
                KeyCode::Up => self.agent_select.move_up(),
                KeyCode::Down => self.agent_select.move_down(),
                KeyCode::Enter => {
                    if let Some(agent) = self.agent_select.selected_agent() {
                        self.context.set_agent(agent.name.clone());
                    }
                    self.agent_select.close();
                }
                _ => {}
            }
            return Ok(true);
        }

        if self.session_list_dialog.is_open() {
            if self.session_list_dialog.is_renaming() {
                match key.code {
                    KeyCode::Esc => self.session_list_dialog.cancel_rename(),
                    KeyCode::Backspace => self.session_list_dialog.handle_rename_backspace(),
                    KeyCode::Enter => {
                        if let Some((session_id, title)) = self.session_list_dialog.confirm_rename()
                        {
                            if let Some(client) = self.context.get_api_client() {
                                if let Err(err) = client.update_session_title(&session_id, &title) {
                                    self.alert_dialog.set_message(&format!(
                                        "Failed to rename session `{}`:\n{}",
                                        session_id, err
                                    ));
                                    self.alert_dialog.open();
                                } else {
                                    self.refresh_session_list_dialog();
                                    if self.active_session_id.as_deref()
                                        == Some(session_id.as_str())
                                    {
                                        let _ = self.sync_session_from_server(&session_id);
                                    }
                                }
                            }
                        }
                    }
                    KeyCode::Char(c)
                        if !key.modifiers.contains(KeyModifiers::CONTROL)
                            && !key.modifiers.contains(KeyModifiers::ALT) =>
                    {
                        self.session_list_dialog.handle_rename_input(c);
                    }
                    _ => {}
                }
                return Ok(true);
            }

            match key.code {
                KeyCode::Esc => self.session_list_dialog.close(),
                KeyCode::Up => self.session_list_dialog.move_up(),
                KeyCode::Down => self.session_list_dialog.move_down(),
                KeyCode::Backspace => {
                    self.session_list_dialog.handle_backspace();
                    self.refresh_session_list_dialog();
                }
                KeyCode::Enter => {
                    let target = self.session_list_dialog.selected_session_id();
                    self.session_list_dialog.close();
                    if let Some(session_id) = target {
                        self.context.navigate(Route::Session {
                            session_id: session_id.clone(),
                        });
                        self.ensure_session_view(&session_id);
                        let _ = self.sync_session_from_server(&session_id);
                    }
                }
                KeyCode::Char('r') if self.matches_keybind("session_rename", key) => {
                    let _ = self.session_list_dialog.start_rename_selected();
                }
                KeyCode::Char('d') if self.matches_keybind("session_delete", key) => {
                    if let Some(state) = self.session_list_dialog.trigger_delete_selected() {
                        match state {
                            SessionDeleteState::Armed(_) => {}
                            SessionDeleteState::Confirmed(session_id) => {
                                if let Some(client) = self.context.get_api_client() {
                                    if let Err(err) = client.delete_session(&session_id) {
                                        self.alert_dialog.set_message(&format!(
                                            "Failed to delete session `{}`:\n{}",
                                            session_id, err
                                        ));
                                        self.alert_dialog.open();
                                    } else {
                                        if self.active_session_id.as_deref()
                                            == Some(session_id.as_str())
                                        {
                                            self.context.navigate(Route::Home);
                                            self.active_session_id = None;
                                            self.session_view = None;
                                        }
                                        self.refresh_session_list_dialog();
                                    }
                                }
                            }
                        }
                    }
                }
                KeyCode::Char(c)
                    if !key.modifiers.contains(KeyModifiers::CONTROL)
                        && !key.modifiers.contains(KeyModifiers::ALT) =>
                {
                    self.session_list_dialog.handle_input(c);
                    self.refresh_session_list_dialog();
                }
                _ => {}
            }
            return Ok(true);
        }

        if self.theme_list_dialog.is_open() {
            match key.code {
                KeyCode::Esc => {
                    let initial = self.theme_list_dialog.initial_theme_id().to_string();
                    let _ = self.context.set_theme_by_name(&initial);
                    self.theme_list_dialog.close();
                }
                KeyCode::Up => {
                    self.theme_list_dialog.move_up();
                    if let Some(theme_id) = self.theme_list_dialog.selected_theme_id() {
                        let _ = self.context.set_theme_by_name(&theme_id);
                    }
                }
                KeyCode::Down => {
                    self.theme_list_dialog.move_down();
                    if let Some(theme_id) = self.theme_list_dialog.selected_theme_id() {
                        let _ = self.context.set_theme_by_name(&theme_id);
                    }
                }
                KeyCode::Backspace => {
                    self.theme_list_dialog.handle_backspace();
                    if let Some(theme_id) = self.theme_list_dialog.selected_theme_id() {
                        let _ = self.context.set_theme_by_name(&theme_id);
                    }
                }
                KeyCode::Enter => {
                    if let Some(theme_id) = self.theme_list_dialog.selected_theme_id() {
                        let _ = self.context.set_theme_by_name(&theme_id);
                    }
                    self.theme_list_dialog.close();
                }
                KeyCode::Char(c)
                    if !key.modifiers.contains(KeyModifiers::CONTROL)
                        && !key.modifiers.contains(KeyModifiers::ALT) =>
                {
                    self.theme_list_dialog.handle_input(c);
                    if let Some(theme_id) = self.theme_list_dialog.selected_theme_id() {
                        let _ = self.context.set_theme_by_name(&theme_id);
                    }
                }
                _ => {}
            }
            return Ok(true);
        }

        if self.mcp_dialog.is_open() {
            match key.code {
                KeyCode::Esc => self.mcp_dialog.close(),
                KeyCode::Up => self.mcp_dialog.move_up(),
                KeyCode::Down => self.mcp_dialog.move_down(),
                KeyCode::Char('r') if is_plain_shortcut(key) => {
                    let _ = self.refresh_mcp_dialog();
                }
                KeyCode::Char('a') if is_plain_shortcut(key) => {
                    if let Some(item) = self.mcp_dialog.selected_item() {
                        if let Some(client) = self.context.get_api_client() {
                            match client.start_mcp_auth(&item.name) {
                                Ok(auth) => {
                                    self.alert_dialog.set_message(&format!(
                                        "MCP `{}` auth started:\n{}\n\nComplete OAuth, then reconnect.",
                                        item.name, auth.authorization_url
                                    ));
                                    self.alert_dialog.open();
                                }
                                Err(err) => {
                                    self.alert_dialog.set_message(&format!(
                                        "Failed to start MCP auth `{}`:\n{}",
                                        item.name, err
                                    ));
                                    self.alert_dialog.open();
                                }
                            }
                            let _ = client.authenticate_mcp(&item.name);
                            let _ = self.refresh_mcp_dialog();
                        }
                    }
                }
                KeyCode::Char('x') if is_plain_shortcut(key) => {
                    if let Some(item) = self.mcp_dialog.selected_item() {
                        if let Some(client) = self.context.get_api_client() {
                            if let Err(err) = client.remove_mcp_auth(&item.name) {
                                self.alert_dialog.set_message(&format!(
                                    "Failed to clear MCP auth `{}`:\n{}",
                                    item.name, err
                                ));
                                self.alert_dialog.open();
                            }
                            let _ = self.refresh_mcp_dialog();
                        }
                    }
                }
                KeyCode::Enter => {
                    if let Some(item) = self.mcp_dialog.selected_item() {
                        if let Some(client) = self.context.get_api_client() {
                            let result = if item.status == "connected" {
                                client.disconnect_mcp(&item.name)
                            } else {
                                client.connect_mcp(&item.name)
                            };
                            if let Err(err) = result {
                                self.alert_dialog.set_message(&format!(
                                    "Failed to toggle MCP `{}`:\n{}",
                                    item.name, err
                                ));
                                self.alert_dialog.open();
                            }
                            let _ = self.refresh_mcp_dialog();
                        }
                    }
                }
                _ => {}
            }
            return Ok(true);
        }

        if self.timeline_dialog.is_open() {
            match key.code {
                KeyCode::Esc => self.timeline_dialog.close(),
                KeyCode::Up => self.timeline_dialog.move_up(),
                KeyCode::Down => self.timeline_dialog.move_down(),
                KeyCode::Enter => {
                    if let Some(msg_id) = self.timeline_dialog.selected_message_id() {
                        let msg_id = msg_id.to_string();
                        self.timeline_dialog.close();
                        if let Some(ref mut sv) = self.session_view {
                            sv.scroll_to_message(&msg_id);
                        }
                    }
                }
                _ => {}
            }
            return Ok(true);
        }

        if self.fork_dialog.is_open() {
            match key.code {
                KeyCode::Esc => self.fork_dialog.close(),
                KeyCode::Up => self.fork_dialog.move_up(),
                KeyCode::Down => self.fork_dialog.move_down(),
                KeyCode::Enter => {
                    let session_id = self.fork_dialog.session_id().map(|s| s.to_string());
                    let msg_id = self
                        .fork_dialog
                        .selected_message_id()
                        .map(|s| s.to_string());
                    self.fork_dialog.close();
                    if let Some(sid) = session_id {
                        if let Some(client) = self.context.get_api_client() {
                            match client.fork_session(&sid, msg_id.as_deref()) {
                                Ok(new_session) => {
                                    self.cache_session_from_api(&new_session);
                                    self.context.navigate(Route::Session {
                                        session_id: new_session.id.clone(),
                                    });
                                    self.ensure_session_view(&new_session.id);
                                    let _ = self.sync_session_from_server(&new_session.id);
                                    self.alert_dialog.set_message(&format!(
                                        "Forked session created: {}",
                                        new_session.title
                                    ));
                                    self.alert_dialog.open();
                                }
                                Err(err) => {
                                    self.alert_dialog
                                        .set_message(&format!("Failed to fork session:\n{}", err));
                                    self.alert_dialog.open();
                                }
                            }
                        }
                    }
                }
                _ => {}
            }
            return Ok(true);
        }

        if self.provider_dialog.is_open() {
            match key.code {
                KeyCode::Esc => self.provider_dialog.close(),
                KeyCode::Up => self.provider_dialog.move_up(),
                KeyCode::Down => self.provider_dialog.move_down(),
                KeyCode::Enter => {
                    if let Some(provider) = self.provider_dialog.selected_provider() {
                        self.provider_dialog.selected_provider = Some(provider.clone());
                        self.provider_dialog.input_mode = true;
                    }
                }
                _ => {}
            }
            return Ok(true);
        }

        if self.subagent_dialog.is_open() {
            match key.code {
                KeyCode::Esc => self.subagent_dialog.close(),
                KeyCode::Up => self.subagent_dialog.scroll_up(),
                KeyCode::Down => self.subagent_dialog.scroll_down(50),
                _ => {}
            }
            return Ok(true);
        }

        if self.tag_dialog.is_open() {
            match key.code {
                KeyCode::Esc => self.tag_dialog.close(),
                KeyCode::Up => self.tag_dialog.move_up(),
                KeyCode::Down => self.tag_dialog.move_down(),
                KeyCode::Char(' ') if is_plain_shortcut(key) => {
                    self.tag_dialog.toggle_selection();
                }
                KeyCode::Enter => self.tag_dialog.close(),
                _ => {}
            }
            return Ok(true);
        }

        Ok(false)
    }

    fn execute_command_action(&mut self, action: CommandAction) -> anyhow::Result<()> {
        match action {
            CommandAction::SubmitPrompt => self.submit_prompt()?,
            CommandAction::ClearPrompt => self.prompt.clear(),
            CommandAction::PasteClipboard => self.paste_clipboard_to_prompt(),
            CommandAction::CopyPrompt => self.copy_prompt_to_clipboard(),
            CommandAction::CutPrompt => self.cut_prompt_to_clipboard(),
            CommandAction::HistoryPrevious => self.prompt.history_previous_entry(),
            CommandAction::HistoryNext => self.prompt.history_next_entry(),
            CommandAction::ToggleSidebar => self.context.toggle_sidebar(),
            CommandAction::ToggleHeader => self.context.toggle_header(),
            CommandAction::ToggleScrollbar => self.context.toggle_scrollbar(),
            CommandAction::SwitchSession => {
                self.refresh_session_list_dialog();
                self.session_list_dialog
                    .open(self.active_session_id.as_deref());
            }
            CommandAction::RenameSession => {
                self.open_session_rename_dialog();
            }
            CommandAction::ExportSession => {
                self.open_session_export_dialog();
            }
            CommandAction::PromptStashPush => {
                if self.prompt.stash_current() {
                    self.alert_dialog.set_message("Prompt stashed.");
                    self.alert_dialog.open();
                } else {
                    self.alert_dialog
                        .set_message("Prompt is empty, nothing to stash.");
                    self.alert_dialog.open();
                }
            }
            CommandAction::PromptStashList => {
                self.open_prompt_stash_dialog();
            }
            CommandAction::PromptSkillList => {
                self.open_skill_list_dialog();
            }
            CommandAction::SwitchTheme => {
                self.refresh_theme_list_dialog();
                let current_theme = self.context.current_theme_name();
                self.theme_list_dialog.open(&current_theme);
            }
            CommandAction::CycleVariant => {
                self.cycle_model_variant();
            }
            CommandAction::ToggleAppearance => {
                let _ = self.context.toggle_theme_mode();
            }
            CommandAction::ViewStatus => {
                self.refresh_status_dialog();
                self.status_dialog.open();
            }
            CommandAction::ToggleMcp => {
                let _ = self.refresh_mcp_dialog();
                self.mcp_dialog.open();
            }
            CommandAction::ToggleTips => {
                self.context.toggle_tips_hidden();
            }
            CommandAction::SwitchModel => {
                self.refresh_model_dialog();
                self.model_select.open();
            }
            CommandAction::SwitchAgent => {
                self.refresh_agent_dialog();
                self.agent_select.open();
            }
            CommandAction::NewSession => {
                self.context.navigate(Route::Home);
                self.active_session_id = None;
                self.session_view = None;
            }
            CommandAction::ShowHelp => {
                self.help_dialog.open();
            }
            CommandAction::ToggleCommandPalette => {
                self.sync_command_palette_labels();
                self.command_palette.open();
            }
            CommandAction::ToggleTimestamps => {
                self.context.toggle_timestamps();
            }
            CommandAction::ToggleThinking => {
                self.context.toggle_thinking();
            }
            CommandAction::ToggleToolDetails => {
                self.context.toggle_tool_details();
            }
            CommandAction::ToggleDensity => {
                self.context.toggle_message_density();
            }
            CommandAction::ToggleSemanticHighlight => {
                self.context.toggle_semantic_highlight();
            }
            CommandAction::ExternalEditor => {}
            CommandAction::ConnectProvider => {
                self.provider_dialog.open();
            }
            CommandAction::ShareSession => {
                self.handle_share_session();
            }
            CommandAction::UnshareSession => {
                self.handle_unshare_session();
            }
            CommandAction::ForkSession => {
                self.handle_fork_session();
            }
            CommandAction::CompactSession => {
                self.handle_compact_session();
            }
            CommandAction::Timeline => {
                self.handle_open_timeline();
            }
            CommandAction::Undo => {
                self.handle_undo();
            }
            CommandAction::Redo => {
                self.handle_redo();
            }
            CommandAction::ListSessions | CommandAction::OpenSessionList => {
                self.refresh_session_list_dialog();
                self.session_list_dialog
                    .open(self.active_session_id.as_deref());
            }
            CommandAction::CopySession => {
                self.handle_copy_session();
            }
            CommandAction::OpenStash => {
                self.open_prompt_stash_dialog();
            }
            CommandAction::OpenSkills => {
                self.open_skill_list_dialog();
            }
            CommandAction::OpenThemeList => {
                self.refresh_theme_list_dialog();
                let current_theme = self.context.current_theme_name();
                self.theme_list_dialog.open(&current_theme);
            }
            CommandAction::ShowStatus => {
                self.refresh_status_dialog();
                self.status_dialog.open();
            }
            CommandAction::ManageMcp | CommandAction::OpenMcpList => {
                let _ = self.refresh_mcp_dialog();
                self.mcp_dialog.open();
            }
            CommandAction::OpenModelList => {
                self.refresh_model_dialog();
                self.model_select.open();
            }
            CommandAction::OpenAgentList => {
                self.refresh_agent_dialog();
                self.agent_select.open();
            }
            CommandAction::Exit => self.state = AppState::Exiting,
        }

        Ok(())
    }

    fn paste_clipboard_to_prompt(&mut self) {
        match Clipboard::read_text() {
            Ok(text) => {
                if !text.is_empty() {
                    self.prompt.insert_text(&text);
                }
            }
            Err(err) => {
                self.alert_dialog
                    .set_message(&format!("Failed to read clipboard:\n{}", err));
                self.alert_dialog.open();
            }
        }
    }

    fn copy_prompt_to_clipboard(&mut self) {
        let content = self.prompt.get_input().trim();
        if content.is_empty() {
            return;
        }
        #[cfg(test)]
        clipboard_probe::record("prompt", content);
        if let Err(err) = Clipboard::write_text(content) {
            self.alert_dialog
                .set_message(&format!("Failed to write clipboard:\n{}", err));
            self.alert_dialog.open();
        }
    }

    fn cut_prompt_to_clipboard(&mut self) {
        let content = self.prompt.get_input().trim();
        if content.is_empty() {
            return;
        }
        if let Err(err) = Clipboard::write_text(content) {
            self.alert_dialog
                .set_message(&format!("Failed to write clipboard:\n{}", err));
            self.alert_dialog.open();
            return;
        }
        self.prompt.clear();
    }

    /// Copy the current screen selection to clipboard and show a toast.
    fn copy_selection(&mut self) {
        if !self.selection.is_active() {
            return;
        }
        let lines = self.screen_lines.clone();
        let text = self
            .selection
            .get_selected_text(|row| lines.get(row as usize).cloned());
        if !text.is_empty() {
            #[cfg(test)]
            clipboard_probe::record("selection", &text);
            match Clipboard::write_text(&text) {
                Ok(()) => {
                    self.toast
                        .show(ToastVariant::Info, "Copied to clipboard", 2000);
                }
                Err(err) => {
                    self.toast
                        .show(ToastVariant::Error, &format!("Copy failed: {}", err), 3000);
                }
            }
        }
        self.selection.clear();
    }

    fn current_session_id(&self) -> Option<String> {
        match self.context.current_route() {
            Route::Session { session_id } => Some(session_id),
            _ => self.active_session_id.clone(),
        }
    }

    fn open_session_rename_dialog(&mut self) {
        let Some(session_id) = self.current_session_id() else {
            self.alert_dialog
                .set_message("No active session to rename.");
            self.alert_dialog.open();
            return;
        };

        let title = self
            .context
            .session
            .read()
            .sessions
            .get(&session_id)
            .map(|s| s.title.clone())
            .unwrap_or_else(|| "New Session".to_string());
        self.session_rename_dialog.open(session_id, title);
    }

    fn open_session_export_dialog(&mut self) {
        let Some(session_id) = self.current_session_id() else {
            self.alert_dialog
                .set_message("No active session to export.");
            self.alert_dialog.open();
            return;
        };

        let title = self
            .context
            .session
            .read()
            .sessions
            .get(&session_id)
            .map(|s| s.title.clone())
            .unwrap_or_else(|| "New Session".to_string());
        let default_filename = default_export_filename(&title, &session_id);
        self.session_export_dialog
            .open(session_id, default_filename);
    }

    fn open_prompt_stash_dialog(&mut self) {
        let entries = self
            .prompt
            .stash_entries()
            .iter()
            .cloned()
            .map(|entry| StashItem {
                input: entry.input,
                created_at: entry.created_at,
            })
            .collect::<Vec<_>>();
        self.prompt_stash_dialog.set_entries(entries);
        self.prompt_stash_dialog.open();
    }

    fn open_skill_list_dialog(&mut self) {
        match self.refresh_skill_list_dialog() {
            Ok(()) => {}
            Err(err) if err.downcast_ref::<UnsupportedEndpointError>().is_some() => {
                // 501 (unsupported) は通信エラーではないためアラートを出さない。
                // SkillListDialog は空リストを "No skills available" として描画するため、
                // 候補を空にしてダイアログを開くことで「利用不可」を表現する。
                self.skill_list_dialog.set_skills(Vec::new());
                self.prompt.set_skill_suggestions(Vec::new());
            }
            Err(err) => {
                self.alert_dialog
                    .set_message(&format!("Failed to refresh skills:\n{}", err));
                self.alert_dialog.open();
            }
        }
        self.skill_list_dialog.open();
    }

    fn handle_share_session(&mut self) {
        let Some(session_id) = self.current_session_id() else {
            self.alert_dialog.set_message("No active session to share.");
            self.alert_dialog.open();
            return;
        };
        let Some(client) = self.context.get_api_client() else {
            return;
        };
        match client.share_session(&session_id) {
            Ok(response) => {
                let _ = Clipboard::write_text(&response.url);
                self.alert_dialog.set_message(&format!(
                    "Session shared. Link copied to clipboard:\n{}",
                    response.url
                ));
                self.alert_dialog.open();
            }
            Err(err) => {
                self.alert_dialog
                    .set_message(&format!("Failed to share session:\n{}", err));
                self.alert_dialog.open();
            }
        }
    }

    fn handle_unshare_session(&mut self) {
        let Some(session_id) = self.current_session_id() else {
            self.alert_dialog
                .set_message("No active session to unshare.");
            self.alert_dialog.open();
            return;
        };
        let Some(client) = self.context.get_api_client() else {
            return;
        };
        match client.unshare_session(&session_id) {
            Ok(_) => {
                self.alert_dialog
                    .set_message("Session sharing link revoked.");
                self.alert_dialog.open();
            }
            Err(err) => {
                self.alert_dialog
                    .set_message(&format!("Failed to unshare session:\n{}", err));
                self.alert_dialog.open();
            }
        }
    }

    fn handle_compact_session(&mut self) {
        let Some(session_id) = self.current_session_id() else {
            self.alert_dialog
                .set_message("No active session to compact.");
            self.alert_dialog.open();
            return;
        };
        let Some(client) = self.context.get_api_client() else {
            return;
        };
        match client.compact_session(&session_id) {
            Ok(_) => {
                let _ = self.sync_session_from_server(&session_id);
                self.alert_dialog
                    .set_message("Session compacted successfully.");
                self.alert_dialog.open();
            }
            Err(err) => {
                self.alert_dialog
                    .set_message(&format!("Failed to compact session:\n{}", err));
                self.alert_dialog.open();
            }
        }
    }

    fn handle_undo(&mut self) {
        let Some(session_id) = self.current_session_id() else {
            self.alert_dialog.set_message("No active session for undo.");
            self.alert_dialog.open();
            return;
        };
        let session_ctx = self.context.session.read();
        let messages = session_ctx.messages.get(&session_id);
        let last_user_msg = messages
            .and_then(|msgs| msgs.iter().rev().find(|m| m.role == MessageRole::User))
            .map(|m| (m.id.clone(), m.content.clone()));
        drop(session_ctx);

        let Some((msg_id, msg_content)) = last_user_msg else {
            self.alert_dialog.set_message("No user message to revert.");
            self.alert_dialog.open();
            return;
        };
        let Some(client) = self.context.get_api_client() else {
            return;
        };
        match client.revert_session(&session_id, &msg_id) {
            Ok(_) => {
                self.prompt.set_input(msg_content);
                let _ = self.sync_session_from_server(&session_id);
                self.alert_dialog
                    .set_message("Message reverted. Prompt restored.");
                self.alert_dialog.open();
            }
            Err(err) => {
                self.alert_dialog
                    .set_message(&format!("Failed to revert message:\n{}", err));
                self.alert_dialog.open();
            }
        }
    }

    fn handle_redo(&mut self) {
        let Some(_session_id) = self.current_session_id() else {
            self.alert_dialog.set_message("No active session for redo.");
            self.alert_dialog.open();
            return;
        };
        // Redo re-submits the current prompt content (which was restored by undo)
        let input = self.prompt.get_input().trim().to_string();
        if input.is_empty() {
            self.alert_dialog
                .set_message("Nothing to redo. Prompt is empty.");
            self.alert_dialog.open();
            return;
        }
        // Re-submit the prompt to effectively redo
        if let Err(err) = self.submit_prompt() {
            self.alert_dialog
                .set_message(&format!("Failed to redo:\n{}", err));
            self.alert_dialog.open();
        }
    }

    fn handle_copy_session(&mut self) {
        let Some(session_id) = self.current_session_id() else {
            self.alert_dialog.set_message("No active session to copy.");
            self.alert_dialog.open();
            return;
        };
        match self.build_session_transcript(&session_id) {
            Some(text) => {
                if let Err(err) = Clipboard::write_text(&text) {
                    self.alert_dialog
                        .set_message(&format!("Failed to copy transcript to clipboard:\n{}", err));
                    self.alert_dialog.open();
                } else {
                    self.alert_dialog
                        .set_message("Session transcript copied to clipboard.");
                    self.alert_dialog.open();
                }
            }
            None => {
                self.alert_dialog
                    .set_message("No transcript available for current session.");
                self.alert_dialog.open();
            }
        }
    }

    fn handle_open_timeline(&mut self) {
        let Some(session_id) = self.current_session_id() else {
            self.alert_dialog
                .set_message("No active session for timeline.");
            self.alert_dialog.open();
            return;
        };
        let session_ctx = self.context.session.read();
        let entries = session_ctx
            .messages
            .get(&session_id)
            .map(|msgs| {
                msgs.iter()
                    .map(|m| {
                        let role = match m.role {
                            MessageRole::User => "user",
                            MessageRole::Assistant => "assistant",
                            MessageRole::System => "system",
                        };
                        let preview = m
                            .content
                            .chars()
                            .take(60)
                            .collect::<String>()
                            .replace('\n', " ");
                        TimelineEntry {
                            message_id: m.id.clone(),
                            role: role.to_string(),
                            preview,
                            timestamp: format_message_timestamp(&m.created_at),
                        }
                    })
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        drop(session_ctx);
        self.timeline_dialog.open(entries);
    }

    fn handle_fork_session(&mut self) {
        let Some(session_id) = self.current_session_id() else {
            self.alert_dialog.set_message("No active session to fork.");
            self.alert_dialog.open();
            return;
        };
        let session_ctx = self.context.session.read();
        let entries = session_ctx
            .messages
            .get(&session_id)
            .map(|msgs| {
                msgs.iter()
                    .map(|m| {
                        let role = match m.role {
                            MessageRole::User => "user",
                            MessageRole::Assistant => "assistant",
                            MessageRole::System => "system",
                        };
                        let preview = m
                            .content
                            .chars()
                            .take(60)
                            .collect::<String>()
                            .replace('\n', " ");
                        ForkEntry {
                            message_id: m.id.clone(),
                            role: role.to_string(),
                            preview,
                            timestamp: format_message_timestamp(&m.created_at),
                        }
                    })
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        drop(session_ctx);
        self.fork_dialog.open(session_id, entries);
    }

    fn build_session_transcript(&self, session_id: &str) -> Option<String> {
        let session_ctx = self.context.session.read();
        let session = session_ctx.sessions.get(session_id)?;
        let messages = session_ctx.messages.get(session_id)?;

        let mut output = String::new();
        output.push_str(&format!("# {}\n\n", session.title));
        output.push_str(&format!("Session ID: `{}`\n", session.id));
        output.push_str(&format!("Created: {}\n", session.created_at.to_rfc3339()));
        output.push_str(&format!("Updated: {}\n\n", session.updated_at.to_rfc3339()));

        if messages.is_empty() {
            output.push_str("_No messages_\n");
            return Some(output);
        }

        for message in messages {
            let role = match message.role {
                MessageRole::User => "User",
                MessageRole::Assistant => "Assistant",
                MessageRole::System => "System",
            };
            output.push_str(&format!("## {}\n\n", role));
            if message.content.trim().is_empty() {
                output.push_str("_Empty message_\n\n");
            } else {
                output.push_str(&message.content);
                output.push_str("\n\n");
            }
        }

        Some(output)
    }

    fn export_session_to_file(&self, session_id: &str, filename: &str) -> anyhow::Result<PathBuf> {
        let transcript = self.build_session_transcript(session_id).ok_or_else(|| {
            anyhow::anyhow!("No transcript available for session `{}`", session_id)
        })?;

        let mut path = PathBuf::from(filename.trim());
        if path.as_os_str().is_empty() {
            anyhow::bail!("filename cannot be empty");
        }
        if path.is_relative() {
            path = std::env::current_dir()?.join(path);
        }
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)?;
            }
        }
        std::fs::write(&path, transcript)?;
        Ok(path)
    }

    /// Queue a question request from the server and start asking it.
    fn enqueue_question_request(&mut self, request: QuestionRequestInfo) {
        if request.questions.is_empty() {
            // Nothing to ask: reject the request so the server-side broker does
            // not keep the tool waiting for an answer that can never arrive.
            tracing::warn!("received question request without questions; rejecting");
            if let Some(client) = self.context.get_api_client() {
                let _ = client.reject_question(&request.id);
            }
            return;
        }

        self.pending_question_requests
            .push_back(PendingQuestionRequest {
                request_id: request.id,
                questions: request.questions,
                index: 0,
                answers: Vec::new(),
            });

        self.ask_next_question();
    }

    /// Show the next unanswered question, if any.
    fn ask_next_question(&mut self) {
        if self.active_question_request.is_none() {
            self.active_question_request = self.pending_question_requests.pop_front();
        }

        let Some(active) = self.active_question_request.as_ref() else {
            return;
        };
        let Some(question) = active.questions.get(active.index) else {
            return;
        };

        let question_type = if question.options.is_empty() {
            QuestionType::Text
        } else if question.multiple {
            QuestionType::MultipleChoice
        } else {
            QuestionType::SingleChoice
        };
        let options = question
            .options
            .iter()
            .map(|option| QuestionOption {
                id: option.label.clone(),
                label: option.label.clone(),
            })
            .collect();
        let request_id = active.request_id.clone();
        let question_text = question.question.clone();

        self.question_prompt.ask(QuestionPromptRequest {
            id: request_id,
            question: question_text,
            question_type,
            options,
        });
    }

    /// Record an answer for the active question and advance the request.
    fn submit_question_answer(&mut self, answer: Vec<String>) {
        let Some(active) = self.active_question_request.as_mut() else {
            return;
        };
        active.answers.push(answer);
        active.index += 1;

        if active.index < active.questions.len() {
            self.ask_next_question();
            return;
        }

        // 要求を削除せずに参照し、id / answers を取得する。
        // 送信失敗時に同じ要求を再送できるよう、take() は成功時まで遅らせる。
        let (request_id, answers) = {
            let Some(active) = self.active_question_request.as_ref() else {
                return;
            };
            (active.request_id.clone(), active.answers.clone())
        };

        let Some(client) = self.context.get_api_client() else {
            // id・トークン・パスを出さない固定文言（権限側と同じ方針）。
            tracing::warn!("question reply skipped: API client unavailable");
            self.toast.show(
                ToastVariant::Error,
                "Failed to send answer: API client unavailable",
                3000,
            );
            self.restore_question_for_retry();
            return;
        };

        if let Err(error) = client.reply_question(&request_id, &answers) {
            // 404 の「既に回答済み」扱いは今回スコープ外（RETRY-01）。
            // Err のまま要求を残し、再送できるようにする。
            self.toast.show(
                ToastVariant::Error,
                &format!("Failed to send answer: {}", error),
                3000,
            );
            self.restore_question_for_retry();
            return;
        }

        // 送信成功時のみ ID を忘却する。失敗時は restore_question_for_retry が
        // 呼ばれ、キーが残ることで同じ要求の再送が抑止されない。
        forget_request_seen(&self.context, "question", &request_id);

        // 送信成功時のみ要求を削除してダイアログを閉じ、次の要求へ進む。
        self.active_question_request.take();
        self.question_prompt.close();
        self.ask_next_question();
    }

    /// 送信失敗時に最後の質問を再表示し、同じ要求の再送／拒否を可能にする。
    /// 直前に追加した回答は取り消し、再確認時に重複しないようにする。
    fn restore_question_for_retry(&mut self) {
        if let Some(active) = self.active_question_request.as_mut() {
            active.index = active.index.saturating_sub(1);
            active.answers.pop();
        }
        self.ask_next_question();
    }

    /// Reject the active question request and continue with the next one.
    fn reject_active_question(&mut self) {
        let Some(active) = self.active_question_request.as_ref() else {
            // 要求が無い場合は従前どおりダイアログだけ閉じる。
            self.question_prompt.close();
            return;
        };
        let request_id = active.request_id.clone();

        let Some(client) = self.context.get_api_client() else {
            // id・トークン・パスを出さない固定文言。
            tracing::warn!("question reject skipped: API client unavailable");
            self.toast.show(
                ToastVariant::Error,
                "Failed to reject question: API client unavailable",
                3000,
            );
            // 要求もダイアログも残して再試行可能にする。
            return;
        };

        if let Err(error) = client.reject_question(&request_id) {
            self.toast.show(
                ToastVariant::Error,
                &format!("Failed to reject question: {}", error),
                3000,
            );
            // 要求もダイアログも残して再試行可能にする。
            return;
        }

        // 送信成功時のみ ID を忘却する。失敗時は要求とダイアログを残して再試行する。
        forget_request_seen(&self.context, "question", &request_id);

        // 送信成功時のみ要求を削除してダイアログを閉じ、次の要求へ進む。
        self.active_question_request.take();
        self.question_prompt.close();
        self.ask_next_question();
    }

    fn submit_prompt(&mut self) -> anyhow::Result<()> {
        let shell_mode = self.prompt.is_shell_mode();
        let input = self.prompt.take_input();
        if input.trim().is_empty() {
            return Ok(());
        }

        if shell_mode {
            return self.submit_shell_command(input);
        }

        let Some(client) = self.context.get_api_client() else {
            eprintln!("API client not initialized");
            return Ok(());
        };

        let agent = selected_agent(&self.context);
        let model = self.selected_model_for_prompt();
        let variant = self.context.current_model_variant();

        match self.context.current_route() {
            Route::Home => {
                let optimistic_session_id = self.create_optimistic_session();
                let opt_id = self.append_optimistic_user_message(
                    &optimistic_session_id,
                    &input,
                    agent.clone(),
                    model.clone(),
                    variant.clone(),
                );
                self.context.navigate(Route::Session {
                    session_id: optimistic_session_id.clone(),
                });
                self.ensure_session_view(&optimistic_session_id);
                self.set_session_status(&optimistic_session_id, SessionStatus::Running);
                self.prompt.set_spinner_task_kind(TaskKind::LlmRequest);
                self.prompt.set_spinner_active(true);
                // Render immediately so the user sees their message before network I/O.
                let _ = self.draw();

                let session = match client.create_session(None) {
                    Ok(session) => session,
                    Err(err) => {
                        self.remove_optimistic_session(&optimistic_session_id);
                        self.context.navigate(Route::Home);
                        self.active_session_id = None;
                        self.session_view = None;
                        self.prompt.set_spinner_active(false);
                        self.alert_dialog
                            .set_message(&format!("Failed to create session:\n{}", err));
                        self.alert_dialog.open();
                        return Ok(());
                    }
                };
                self.promote_optimistic_session(&optimistic_session_id, &session);
                self.context.navigate(Route::Session {
                    session_id: session.id.clone(),
                });
                self.ensure_session_view(&session.id);
                if let Err(err) = client.send_prompt(
                    &session.id,
                    input.clone(),
                    agent.clone(),
                    model.clone(),
                    variant.clone(),
                ) {
                    self.remove_optimistic_message(&session.id, &opt_id);
                    self.set_session_status(&session.id, SessionStatus::Idle);
                    self.sync_prompt_spinner_state();
                    self.alert_dialog
                        .set_message(&format!("Failed to send prompt:\n{}", err));
                    self.alert_dialog.open();
                    return Ok(());
                }
                self.set_session_status(&session.id, SessionStatus::Running);
                self.prompt.set_spinner_task_kind(TaskKind::LlmRequest);
                self.prompt.set_spinner_active(true);
            }
            Route::Session { session_id } => {
                // Optimistic: show user message immediately before network call
                let opt_id = self.append_optimistic_user_message(
                    &session_id,
                    &input,
                    agent.clone(),
                    model.clone(),
                    variant.clone(),
                );
                self.set_session_status(&session_id, SessionStatus::Running);
                self.prompt.set_spinner_task_kind(TaskKind::LlmRequest);
                self.prompt.set_spinner_active(true);
                self.ensure_session_view(&session_id);
                // Render immediately so the user sees their message before network I/O.
                let _ = self.draw();
                if let Err(err) = client.send_prompt(
                    &session_id,
                    input.clone(),
                    agent.clone(),
                    model.clone(),
                    variant.clone(),
                ) {
                    self.remove_optimistic_message(&session_id, &opt_id);
                    self.set_session_status(&session_id, SessionStatus::Idle);
                    self.sync_prompt_spinner_state();
                    self.alert_dialog
                        .set_message(&format!("Failed to send prompt:\n{}", err));
                    self.alert_dialog.open();
                    return Ok(());
                }
            }
            _ => {}
        }

        Ok(())
    }

    fn submit_shell_command(&mut self, command: String) -> anyhow::Result<()> {
        let command = command.trim().to_string();
        if command.is_empty() {
            return Ok(());
        }

        let Some(client) = self.context.get_api_client() else {
            eprintln!("API client not initialized");
            return Ok(());
        };

        let user_line = format!("$ {}", command);
        let agent = selected_agent(&self.context);
        let model = self.selected_model_for_prompt();
        let variant = self.context.current_model_variant();

        match self.context.current_route() {
            Route::Home => {
                let optimistic_session_id = self.create_optimistic_session();
                let opt_id = self.append_optimistic_user_message(
                    &optimistic_session_id,
                    &user_line,
                    agent.clone(),
                    model.clone(),
                    variant.clone(),
                );
                self.context.navigate(Route::Session {
                    session_id: optimistic_session_id.clone(),
                });
                self.ensure_session_view(&optimistic_session_id);
                let _ = self.draw();

                let session = match client.create_session(None) {
                    Ok(session) => session,
                    Err(err) => {
                        self.remove_optimistic_session(&optimistic_session_id);
                        self.context.navigate(Route::Home);
                        self.active_session_id = None;
                        self.session_view = None;
                        self.alert_dialog
                            .set_message(&format!("Failed to create session:\n{}", err));
                        self.alert_dialog.open();
                        return Ok(());
                    }
                };
                self.promote_optimistic_session(&optimistic_session_id, &session);
                self.context.navigate(Route::Session {
                    session_id: session.id.clone(),
                });
                self.ensure_session_view(&session.id);

                if let Err(err) = client.execute_shell(&session.id, command.clone(), None) {
                    self.remove_optimistic_message(&session.id, &opt_id);
                    self.alert_dialog
                        .set_message(&format!("Failed to execute shell command:\n{}", err));
                    self.alert_dialog.open();
                    return Ok(());
                }

                self.set_session_status(&session.id, SessionStatus::Idle);
                // 同期に失敗すると楽観メッセージがサーバー応答へ置換されず残るため、
                // execute_shell 失敗経路と同じく明示的に削除する。
                if self.sync_session_from_server(&session.id).is_err() {
                    self.remove_optimistic_message(&session.id, &opt_id);
                }
            }
            Route::Session { session_id } => {
                let opt_id = self.append_optimistic_user_message(
                    &session_id,
                    &user_line,
                    agent.clone(),
                    model.clone(),
                    variant.clone(),
                );
                self.ensure_session_view(&session_id);
                let _ = self.draw();
                if let Err(err) = client.execute_shell(&session_id, command.clone(), None) {
                    self.remove_optimistic_message(&session_id, &opt_id);
                    self.alert_dialog
                        .set_message(&format!("Failed to execute shell command:\n{}", err));
                    self.alert_dialog.open();
                    return Ok(());
                }
                self.set_session_status(&session_id, SessionStatus::Idle);
                // 同期に失敗すると楽観メッセージがサーバー応答へ置換されず残るため、
                // execute_shell 失敗経路と同じく明示的に削除する。
                if self.sync_session_from_server(&session_id).is_err() {
                    self.remove_optimistic_message(&session_id, &opt_id);
                }
            }
            _ => {}
        }

        self.sync_prompt_spinner_state();
        Ok(())
    }

    fn refresh_model_dialog(&mut self) {
        let Some(client) = self.context.get_api_client() else {
            self.context.set_has_connected_provider(false);
            return;
        };

        let Ok(providers) = client.get_config_providers() else {
            self.context.set_has_connected_provider(false);
            return;
        };
        let has_connected_provider = providers.providers.iter().any(|p| !p.models.is_empty());
        self.context
            .set_has_connected_provider(has_connected_provider);

        let mut available_models = HashSet::new();
        let mut variant_map: HashMap<String, Vec<String>> = HashMap::new();
        let mut models = Vec::new();
        let mut context_providers = Vec::new();
        for provider in providers.providers {
            let provider_id = provider.id.clone();
            let provider_name = provider.name.clone();
            let mut provider_models = Vec::new();
            for model in provider.models {
                let model_id = model.id;
                let model_name = model.name;
                let model_context_window = model.context_window.unwrap_or(0);
                let model_ref = format!("{}/{}", provider_id, model_id);
                available_models.insert(model_ref.clone());
                let entry = variant_map.entry(model_ref).or_default();
                for variant in model.variants {
                    if !entry.iter().any(|value| value == &variant) {
                        entry.push(variant);
                    }
                }
                models.push(Model {
                    id: model_id.clone(),
                    name: model_name.clone(),
                    provider: provider_id.clone(),
                    context_window: model_context_window,
                });
                provider_models.push(crate::context::ModelInfo {
                    id: format!("{}/{}", provider_id, model_id),
                    name: model_name,
                    context_window: model_context_window,
                    max_output_tokens: 0,
                    supports_vision: false,
                    supports_tools: true,
                });
            }
            context_providers.push(crate::context::ProviderInfo {
                id: provider_id,
                name: provider_name,
                models: provider_models,
            });
        }
        *self.context.providers.write() = context_providers;
        models.sort_by(|a, b| {
            a.provider
                .cmp(&b.provider)
                .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
        });
        for variants in variant_map.values_mut() {
            variants.sort();
        }
        self.model_select.set_models(models);
        self.available_models = available_models;
        self.model_variants = variant_map;
        self.model_variant_selection.retain(|model_key, variant| {
            let Some(available) = self.model_variants.get(model_key) else {
                return false;
            };
            match variant {
                Some(value) => available.iter().any(|item| item == value),
                None => true,
            }
        });
        self.sync_current_model_variant();

        let model_missing = self.context.current_model.read().is_none();
        if model_missing {
            if let Some((provider, model_id)) = providers.default_model.iter().next() {
                self.set_active_model_selection(
                    format!("{}/{}", provider, model_id),
                    Some(provider.clone()),
                );
            }
        }
    }

    fn refresh_agent_dialog(&mut self) {
        let Some(client) = self.context.get_api_client() else {
            return;
        };

        let Ok(agents) = client.list_agents() else {
            return;
        };

        if agents.is_empty() {
            return;
        }

        let theme = self.context.theme.read().clone();
        let mut agent_names = Vec::new();
        let mapped = agents
            .into_iter()
            .enumerate()
            .map(|(idx, agent)| {
                agent_names.push(agent.id.clone());
                Agent {
                    name: agent.id,
                    description: agent
                        .description
                        .unwrap_or_else(|| "No description".to_string()),
                    color: theme.agent_color(idx),
                }
            })
            .collect::<Vec<_>>();
        self.agent_select.set_agents(mapped);
        let current = self.context.current_agent.read().clone();
        if !agent_names.iter().any(|name| name == &current) {
            if let Some(first) = agent_names.first() {
                self.context.set_agent(first.clone());
                self.sync_prompt_spinner_style();
            }
        }
        self.prompt.set_agent_suggestions(agent_names);
    }

    fn cycle_agent(&mut self, direction: i8) {
        self.refresh_agent_dialog();

        let agents = self.agent_select.agents();
        if agents.is_empty() {
            return;
        }

        let current = self.context.current_agent.read().clone();
        let current_index = agents
            .iter()
            .position(|agent| agent.name == current)
            .unwrap_or(0);

        let len = agents.len();
        let next_index = if direction >= 0 {
            (current_index + 1) % len
        } else if current_index == 0 {
            len - 1
        } else {
            current_index - 1
        };
        let next_agent = agents[next_index].name.clone();

        self.context.set_agent(next_agent);
        self.sync_prompt_spinner_style();
    }

    fn refresh_session_list_dialog(&mut self) {
        let Some(client) = self.context.get_api_client() else {
            return;
        };

        let query = self.session_list_dialog.query().trim().to_string();
        let sessions_result = if query.is_empty() {
            client.list_sessions()
        } else {
            client.list_sessions_filtered(Some(&query), Some(30))
        };
        let Ok(sessions) = sessions_result else {
            return;
        };
        let Ok(status_map) = client.get_session_status() else {
            return;
        };
        {
            let mut session_ctx = self.context.session.write();
            for session in &sessions {
                if let Some(status) = status_map.get(&session.id) {
                    session_ctx.set_status(&session.id, map_api_run_status(status));
                }
            }
        }

        let items = sessions
            .into_iter()
            .map(|session| SessionItem {
                is_busy: status_map.get(&session.id).map(|s| s.busy).unwrap_or(false),
                id: session.id,
                title: session.title,
                directory: session.directory,
                parent_id: session.parent_id,
                updated_at: session.time.updated,
            })
            .collect::<Vec<_>>();
        self.session_list_dialog.set_sessions(items);
    }

    fn refresh_theme_list_dialog(&mut self) {
        let options = self
            .context
            .available_theme_names()
            .into_iter()
            .map(|name| ThemeOption {
                id: name.clone(),
                name: format_theme_option_label(&name),
            })
            .collect::<Vec<_>>();
        self.theme_list_dialog.set_options(options);
    }

    fn refresh_skill_list_dialog(&mut self) -> anyhow::Result<()> {
        let Some(client) = self.context.get_api_client() else {
            return Ok(());
        };
        let skills = client.list_skills()?;
        self.skill_list_dialog.set_skills(skills.clone());
        self.prompt.set_skill_suggestions(skills);
        Ok(())
    }

    fn refresh_lsp_status(&mut self) -> anyhow::Result<()> {
        let Some(client) = self.context.get_api_client() else {
            return Ok(());
        };
        let servers = client.get_lsp_servers()?;
        let statuses = servers
            .into_iter()
            .map(|id| crate::context::LspStatus {
                id,
                root: "-".to_string(),
                status: crate::context::LspConnectionStatus::Connected,
            })
            .collect::<Vec<_>>();
        *self.context.lsp_status.write() = statuses;
        Ok(())
    }

    fn refresh_status_dialog(&mut self) {
        // LSP 一覧は 501 終点（常に空配列）のため、ステータス表示を開く時だけ同期する。
        // これにより 5 秒ごとの Tick 同期を廃止しても表示内容は変わらない。
        let _ = self.refresh_lsp_status();
        let formatters = self
            .context
            .get_api_client()
            .and_then(|client| client.get_formatters().ok())
            .unwrap_or_default();
        let route_label = match self.context.current_route() {
            Route::Home => "home".to_string(),
            Route::Session { session_id } => format!("session ({})", session_id),
            Route::Settings => "settings".to_string(),
            Route::Help => "help".to_string(),
        };
        let session_ctx = self.context.session.read();
        let mcp_servers = self.context.mcp_servers.read();
        let lsp_status = self.context.lsp_status.read();
        let connected_mcp = mcp_servers
            .iter()
            .filter(|s| matches!(s.status, McpConnectionStatus::Connected))
            .count();
        let mut lines = vec![
            StatusLine::title("Runtime"),
            StatusLine::normal(format!("Route: {}", route_label)),
            StatusLine::normal(format!(
                "Directory: {}",
                self.context.directory.read().as_str()
            )),
            StatusLine::normal(format!(
                "Agent: {}",
                self.context.current_agent.read().as_str()
            )),
            StatusLine::normal(format!("Model: {}", self.current_model_label())),
            StatusLine::normal(format!(
                "Theme: {}",
                format_theme_option_label(&self.context.current_theme_name())
            )),
            StatusLine::normal(format!("Loaded sessions: {}", session_ctx.sessions.len())),
            StatusLine::muted(""),
            StatusLine::title(format!(
                "MCP Servers ({}, connected: {})",
                mcp_servers.len(),
                connected_mcp
            )),
        ];
        if mcp_servers.is_empty() {
            lines.push(StatusLine::muted("- No MCP servers"));
        } else {
            for server in mcp_servers.iter() {
                let status_text = match server.status {
                    McpConnectionStatus::Connected => "connected",
                    McpConnectionStatus::Disconnected => "disconnected",
                    McpConnectionStatus::Failed => "failed",
                    McpConnectionStatus::NeedsAuth => "needs auth",
                    McpConnectionStatus::NeedsClientRegistration => "needs client ID",
                    McpConnectionStatus::Disabled => "disabled",
                };
                let base = format!("- {}: {}", server.name, status_text);
                match server.status {
                    McpConnectionStatus::Connected => lines.push(StatusLine::success(base)),
                    McpConnectionStatus::NeedsAuth
                    | McpConnectionStatus::NeedsClientRegistration => {
                        lines.push(StatusLine::warning(base))
                    }
                    McpConnectionStatus::Failed => {
                        let text = if let Some(error) = &server.error {
                            format!("{} ({})", base, error)
                        } else {
                            base
                        };
                        lines.push(StatusLine::error(text));
                    }
                    _ => lines.push(StatusLine::muted(base)),
                }
            }
        }

        lines.push(StatusLine::muted(""));
        lines.push(StatusLine::title(format!(
            "LSP Servers ({})",
            lsp_status.len()
        )));
        if lsp_status.is_empty() {
            lines.push(StatusLine::muted("- No LSP servers"));
        } else {
            for server in lsp_status.iter() {
                lines.push(StatusLine::success(format!("- {}", server.id)));
            }
        }

        lines.push(StatusLine::muted(""));
        lines.push(StatusLine::title(format!(
            "Formatters ({})",
            formatters.len()
        )));
        if formatters.is_empty() {
            lines.push(StatusLine::muted("- No formatters"));
        } else {
            for formatter in formatters {
                lines.push(StatusLine::success(format!("- {}", formatter)));
            }
        }
        self.status_dialog.set_status_lines(lines);
    }

    fn refresh_mcp_dialog(&mut self) -> anyhow::Result<()> {
        let Some(client) = self.context.get_api_client() else {
            return Ok(());
        };
        let servers = client.get_mcp_status()?;

        let mcp_items = servers
            .iter()
            .map(|server| McpItem {
                name: server.name.clone(),
                status: server.status.clone(),
                tools: server.tools,
                resources: server.resources,
                error: server.error.clone(),
            })
            .collect::<Vec<_>>();
        self.mcp_dialog.set_items(mcp_items);

        let statuses = servers
            .into_iter()
            .map(|server| {
                let status = map_mcp_status(&server);
                McpServerStatus {
                    name: server.name,
                    status,
                    error: server.error,
                }
            })
            .collect::<Vec<_>>();
        *self.context.mcp_servers.write() = statuses;
        Ok(())
    }

    fn set_active_model_selection(&mut self, model_ref: String, provider: Option<String>) {
        let (model_key, explicit_variant) =
            parse_model_ref_selection(&model_ref, &self.available_models, &self.model_variants);
        let resolved_provider = provider.or_else(|| provider_from_model(&model_key));
        self.context
            .set_model_selection(model_key.clone(), resolved_provider);
        if let Some(variant) = explicit_variant {
            self.model_variant_selection
                .insert(model_key.clone(), Some(variant.clone()));
            self.context.set_model_variant(Some(variant));
            return;
        }
        let variant = self
            .model_variant_selection
            .get(&model_key)
            .cloned()
            .flatten();
        self.context.set_model_variant(variant);
    }

    fn sync_current_model_variant(&mut self) {
        let Some(model_ref) = self.context.current_model.read().clone() else {
            self.context.set_model_variant(None);
            return;
        };
        let (model_key, explicit_variant) =
            parse_model_ref_selection(&model_ref, &self.available_models, &self.model_variants);
        if let Some(explicit) = explicit_variant {
            self.model_variant_selection
                .insert(model_key.clone(), Some(explicit.clone()));
            self.context.set_model_variant(Some(explicit));
            return;
        }
        let selected = self
            .model_variant_selection
            .get(&model_key)
            .cloned()
            .flatten();
        let available = self.model_variants.get(&model_key);
        let valid = selected.filter(|value| {
            available
                .map(|items| items.iter().any(|item| item == value))
                .unwrap_or(false)
        });
        if valid.is_none() {
            self.model_variant_selection.insert(model_key, None);
        }
        self.context.set_model_variant(valid);
    }

    fn cycle_model_variant(&mut self) {
        let Some(model_ref) = self.context.current_model.read().clone() else {
            return;
        };
        let (model_key, explicit_variant) =
            parse_model_ref_selection(&model_ref, &self.available_models, &self.model_variants);
        let Some(variants) = self.model_variants.get(&model_key).cloned() else {
            self.model_variant_selection.remove(&model_key);
            self.context.set_model_variant(None);
            return;
        };
        if variants.is_empty() {
            self.model_variant_selection.insert(model_key, None);
            self.context.set_model_variant(None);
            return;
        }

        let current = self
            .model_variant_selection
            .get(&model_key)
            .cloned()
            .flatten()
            .or(explicit_variant);
        let next = match current {
            None => Some(variants[0].clone()),
            Some(current_value) => {
                let index = variants.iter().position(|item| item == &current_value);
                match index {
                    Some(idx) if idx + 1 < variants.len() => Some(variants[idx + 1].clone()),
                    _ => None,
                }
            }
        };
        self.model_variant_selection.insert(model_key, next.clone());
        self.context.set_model_variant(next);
    }

    fn current_model_label(&self) -> String {
        let Some(model) = self.context.current_model.read().clone() else {
            return "(not selected)".to_string();
        };
        let (base_model, _) =
            parse_model_ref_selection(&model, &self.available_models, &self.model_variants);
        if let Some(variant) = self.context.current_model_variant() {
            return format!("{base_model} ({variant})");
        }
        base_model
    }

    fn selected_model_for_prompt(&self) -> Option<String> {
        let model = self.context.current_model.read().clone()?;
        let (base, inline_variant) =
            parse_model_ref_selection(&model, &self.available_models, &self.model_variants);
        let variant = self.context.current_model_variant();

        let resolved = if let Some(variant) = variant {
            let candidate = format!("{base}/{variant}");
            if self.available_models.contains(&candidate) {
                candidate
            } else {
                model.clone()
            }
        } else if inline_variant.is_some() && self.available_models.contains(&base) {
            base
        } else {
            model.clone()
        };

        Some(resolved)
    }

    fn ensure_session_view(&mut self, session_id: &str) {
        if self.active_session_id.as_deref() == Some(session_id) {
            return;
        }

        self.active_session_id = Some(session_id.to_string());
        self.session_view = Some(SessionView::new(
            self.context.clone(),
            session_id.to_string(),
        ));
    }

    fn cache_session_from_api(&self, session: &SessionInfo) {
        let mut session_ctx = self.context.session.write();
        session_ctx.upsert_session(map_api_session(session));
    }

    fn create_optimistic_session(&mut self) -> String {
        let now = Utc::now();
        let session_id = format!(
            "local_session_{}_{}",
            now.timestamp_millis(),
            Uuid::new_v4().simple()
        );
        let session = Session {
            id: session_id.clone(),
            title: "New Session".to_string(),
            created_at: now,
            updated_at: now,
            parent_id: None,
            share: None,
        };

        let mut session_ctx = self.context.session.write();
        session_ctx.upsert_session(session);
        session_ctx.messages.entry(session_id.clone()).or_default();
        session_ctx.set_status(&session_id, SessionStatus::Idle);
        session_id
    }

    fn remove_optimistic_session(&mut self, session_id: &str) {
        let mut session_ctx = self.context.session.write();
        session_ctx.sessions.remove(session_id);
        session_ctx.messages.remove(session_id);
        session_ctx.session_status.remove(session_id);
        session_ctx.session_diff.remove(session_id);
        session_ctx.todos.remove(session_id);
        session_ctx.revert.remove(session_id);
        if session_ctx.current_session_id.as_deref() == Some(session_id) {
            session_ctx.current_session_id = None;
        }
    }

    fn promote_optimistic_session(&mut self, optimistic_session_id: &str, session: &SessionInfo) {
        let mut session_ctx = self.context.session.write();
        let optimistic_messages = session_ctx
            .messages
            .remove(optimistic_session_id)
            .unwrap_or_default();
        let optimistic_status = session_ctx.session_status.remove(optimistic_session_id);
        let optimistic_diff = session_ctx.session_diff.remove(optimistic_session_id);
        let optimistic_todos = session_ctx.todos.remove(optimistic_session_id);
        let optimistic_revert = session_ctx.revert.remove(optimistic_session_id);
        session_ctx.sessions.remove(optimistic_session_id);

        let real_session_id = session.id.clone();
        session_ctx.upsert_session(map_api_session(session));
        if !optimistic_messages.is_empty() {
            session_ctx
                .messages
                .insert(real_session_id.clone(), optimistic_messages);
        }
        if let Some(status) = optimistic_status {
            session_ctx
                .session_status
                .insert(real_session_id.clone(), status);
        }
        if let Some(diff) = optimistic_diff {
            session_ctx
                .session_diff
                .insert(real_session_id.clone(), diff);
        }
        if let Some(todos) = optimistic_todos {
            session_ctx.todos.insert(real_session_id.clone(), todos);
        }
        if let Some(revert) = optimistic_revert {
            session_ctx.revert.insert(real_session_id, revert);
        }
    }

    fn append_optimistic_user_message(
        &mut self,
        session_id: &str,
        content: &str,
        agent: Option<String>,
        model: Option<String>,
        variant: Option<String>,
    ) -> String {
        let now = Utc::now();
        let id = format!(
            "local_user_{}_{}",
            now.timestamp_millis(),
            Uuid::new_v4().simple()
        );
        let message = Message {
            id: id.clone(),
            role: MessageRole::User,
            content: content.to_string(),
            created_at: now,
            agent,
            model,
            mode: variant,
            finish: None,
            error: None,
            completed_at: None,
            cost: 0.0,
            tokens: TokenUsage::default(),
            parts: vec![ContextMessagePart::Text {
                text: content.to_string(),
            }],
        };

        let mut session_ctx = self.context.session.write();
        session_ctx
            .messages
            .entry(session_id.to_string())
            .or_default();
        session_ctx.add_message(session_id, message);
        if let Some(session) = session_ctx.sessions.get_mut(session_id) {
            session.updated_at = now;
        }
        id
    }

    fn remove_optimistic_message(&mut self, session_id: &str, msg_id: &str) {
        let mut session_ctx = self.context.session.write();
        if let Some(msgs) = session_ctx.messages.get_mut(session_id) {
            msgs.retain(|m| m.id != msg_id);
        }
    }

    fn sync_session_from_server(&mut self, session_id: &str) -> anyhow::Result<()> {
        let Some(client) = self.context.get_api_client() else {
            return Ok(());
        };

        let session = client.get_session(session_id)?;
        let messages = client.get_messages(session_id)?;
        let revert = session.revert.as_ref().map(map_api_revert);

        let mut session_ctx = self.context.session.write();
        session_ctx.upsert_session(map_api_session(&session));
        session_ctx.set_messages(
            session_id,
            messages
                .iter()
                .map(map_api_message)
                .collect::<Vec<Message>>(),
        );
        if let Some(revert_info) = revert {
            session_ctx
                .revert
                .insert(session_id.to_string(), revert_info);
        } else {
            session_ctx.revert.remove(session_id);
        }
        drop(session_ctx);

        self.last_session_sync = Instant::now();
        Ok(())
    }

    fn set_session_status(&mut self, session_id: &str, status: SessionStatus) {
        let mut session_ctx = self.context.session.write();
        session_ctx.set_status(session_id, status);
    }

    fn sync_prompt_spinner_style(&mut self) {
        let theme = self.context.theme.read().clone();
        let agent = self.context.current_agent.read().clone();
        self.prompt
            .set_spinner_color(agent_color_from_name(&theme, &agent));
    }

    fn sync_prompt_spinner_state(&mut self) -> bool {
        let before_active = self.prompt.spinner_active();
        let before_kind = self.prompt.spinner_task_kind();

        let Route::Session { session_id } = self.context.current_route() else {
            self.prompt.set_spinner_active(false);
            self.prompt.clear_interrupt_confirmation();
            return before_active != self.prompt.spinner_active()
                || before_kind != self.prompt.spinner_task_kind();
        };

        let status = {
            let session_ctx = self.context.session.read();
            session_ctx.status(&session_id).clone()
        };
        let is_active = !matches!(status, SessionStatus::Idle);
        self.prompt.set_spinner_active(is_active);
        if !is_active {
            self.prompt.clear_interrupt_confirmation();
            return before_active != self.prompt.spinner_active()
                || before_kind != self.prompt.spinner_task_kind();
        }

        let task_kind = self.infer_spinner_task_kind(&session_id, &status);
        if self.prompt.spinner_task_kind() != task_kind {
            self.prompt.set_spinner_task_kind(task_kind);
        }

        before_active != self.prompt.spinner_active()
            || before_kind != self.prompt.spinner_task_kind()
    }

    fn infer_spinner_task_kind(&self, session_id: &str, status: &SessionStatus) -> TaskKind {
        if matches!(status, SessionStatus::Retrying { .. }) {
            return TaskKind::LlmResponse;
        }

        let session_ctx = self.context.session.read();
        let Some(messages) = session_ctx.messages.get(session_id) else {
            return TaskKind::LlmRequest;
        };
        let Some(last_message) = messages.last() else {
            return TaskKind::LlmRequest;
        };

        match last_message.role {
            MessageRole::User => TaskKind::LlmRequest,
            MessageRole::Assistant => infer_task_kind_from_message(last_message),
            MessageRole::System => TaskKind::LlmResponse,
        }
    }

    fn matches_keybind(&self, keybind_name: &str, key: KeyEvent) -> bool {
        self.context
            .keybind
            .read()
            .match_key(keybind_name, key.code, key.modifiers)
    }

    fn sync_command_palette_labels(&mut self) {
        let show_thinking = *self.context.show_thinking.read();
        let show_tool_details = *self.context.show_tool_details.read();
        let density = *self.context.message_density.read();
        let semantic_hl = *self.context.semantic_highlight.read();
        let show_header = *self.context.show_header.read();
        let show_scrollbar = *self.context.show_scrollbar.read();
        let tips_hidden = *self.context.tips_hidden.read();
        self.command_palette.sync_visibility_labels(
            show_thinking,
            show_tool_details,
            density,
            semantic_hl,
            show_header,
            show_scrollbar,
            tips_hidden,
        );
    }

    fn draw(&mut self) -> anyhow::Result<()> {
        self.context
            .set_pending_permissions(self.permission_prompt.pending_count());

        let route = self.context.current_route();
        if let Route::Session { session_id } = &route {
            self.ensure_session_view(session_id);
        } else {
            self.active_session_id = None;
            self.session_view = None;
        }

        let context = self.context.clone();
        let prompt = &self.prompt;
        let route_for_draw = route.clone();
        let show_modal_overlay = self.has_open_dialog_layer()
            || self.permission_prompt.is_open
            || self.question_prompt.is_open;
        let session_view = self.session_view.as_mut();
        let theme = self.context.theme.read().clone();
        let command_palette = &self.command_palette;
        let model_select = &self.model_select;
        let agent_select = &self.agent_select;
        let session_list_dialog = &self.session_list_dialog;
        let theme_list_dialog = &self.theme_list_dialog;
        let status_dialog = &self.status_dialog;
        let mcp_dialog = &self.mcp_dialog;
        let help_dialog = &self.help_dialog;
        let alert_dialog = &self.alert_dialog;
        let session_rename_dialog = &self.session_rename_dialog;
        let session_export_dialog = &self.session_export_dialog;
        let prompt_stash_dialog = &self.prompt_stash_dialog;
        let skill_list_dialog = &self.skill_list_dialog;
        let timeline_dialog = &self.timeline_dialog;
        let fork_dialog = &self.fork_dialog;
        let provider_dialog = &self.provider_dialog;
        let subagent_dialog = &self.subagent_dialog;
        let tag_dialog = &self.tag_dialog;
        let permission_prompt = &self.permission_prompt;
        let question_prompt = &self.question_prompt;
        let slash_popup = &self.slash_popup;
        let toast = &self.toast;
        let selection = &self.selection;

        let mut captured_lines: Vec<String> = Vec::new();
        let mut snapshot_skipped = false;

        self.terminal.draw(|frame| {
            let area = frame.area();
            if area.width < 10 || area.height < 10 {
                // 描画もスナップショットもしない。古い screen_lines を残すと
                // 選択コピーが現在の座標で古い行を参照してしまうため、
                // 描画後にスナップショットと選択を破棄する。
                snapshot_skipped = true;
                return;
            }

            match route_for_draw {
                Route::Home => {
                    let home = HomeView::new(context.clone());
                    home.render_with_prompt(frame, area, prompt);
                }
                Route::Session { .. } => {
                    if let Some(view) = session_view {
                        view.render(frame, area, prompt);
                    } else {
                        let home = HomeView::new(context.clone());
                        home.render_with_prompt(frame, area, prompt);
                    }
                }
                _ => {
                    let home = HomeView::new(context.clone());
                    home.render_with_prompt(frame, area, prompt);
                }
            }

            if show_modal_overlay {
                let modal_backdrop = ratatui::widgets::Block::default()
                    .style(ratatui::style::Style::default().bg(theme.background_menu));
                frame.render_widget(modal_backdrop, area);
            }

            slash_popup.render(frame, area, &theme);
            command_palette.render(frame, area, &theme);
            model_select.render(frame, area, &theme);
            agent_select.render(frame, area, &theme);
            session_list_dialog.render(frame, area, &theme);
            theme_list_dialog.render(frame, area, &theme);
            status_dialog.render(frame, area, &theme);
            mcp_dialog.render(frame, area, &theme);
            help_dialog.render(frame, area, &theme);
            alert_dialog.render(frame, area, &theme);
            session_rename_dialog.render(frame, area, &theme);
            session_export_dialog.render(frame, area, &theme);
            prompt_stash_dialog.render(frame, area, &theme);
            skill_list_dialog.render(frame, area, &theme);
            timeline_dialog.render(frame, area, &theme);
            fork_dialog.render(frame, area, &theme);
            provider_dialog.render(frame, area, &theme);
            subagent_dialog.render(frame, area, &theme);
            tag_dialog.render(frame, area, &theme);
            permission_prompt.render(frame, area, &theme);
            question_prompt.render(frame, area, &theme);

            // Render toast notification (top-right corner)
            if toast.is_visible() {
                let toast_width = 60u16.min(area.width.saturating_sub(4));
                let toast_height = toast.desired_height(toast_width);
                let base_x = area.x + area.width.saturating_sub(toast_width.saturating_add(2));
                let max_x = area.x + area.width.saturating_sub(toast_width);
                let toast_x = base_x.saturating_add(toast.slide_offset()).min(max_x);
                let toast_area = ratatui::layout::Rect {
                    x: toast_x,
                    y: 2.min(area.height.saturating_sub(1)),
                    width: toast_width,
                    height: toast_height.min(area.height.saturating_sub(2)),
                };
                toast.render(frame, toast_area, &theme);
            }

            // Snapshot the rendered buffer for text selection (before highlight overlay)
            let buf = frame.buffer_mut();
            captured_lines.clear();
            for y in area.y..area.y + area.height {
                let mut line = String::with_capacity(area.width as usize);
                for x in area.x..area.x + area.width {
                    let cell = &buf[(x, y)];
                    line.push_str(cell.symbol());
                }
                let trimmed = line.trim_end().to_string();
                captured_lines.push(trimmed);
            }

            // Render selection highlight — invert colors on non-empty cells,
            // matching standard terminal selection behavior (like opentui).
            if selection.is_active() {
                use ratatui::style::Color;
                for y in area.y..area.y + area.height {
                    for x in area.x..area.x + area.width {
                        if !selection.is_selected(y, x) {
                            continue;
                        }
                        let cell = &buf[(x, y)];
                        let sym = cell.symbol();
                        // Only highlight cells with visible text content
                        if sym.is_empty() || sym.chars().all(|c| c == ' ') {
                            continue;
                        }
                        let cell = &mut buf[(x, y)];
                        // Resolve Reset to concrete terminal defaults before swapping.
                        // Reset fg = terminal default foreground (typically white/light).
                        // Reset bg = terminal default background (typically black/dark).
                        let fg = if cell.fg == Color::Reset {
                            Color::White
                        } else {
                            cell.fg
                        };
                        let bg = if cell.bg == Color::Reset {
                            Color::Black
                        } else {
                            cell.bg
                        };
                        cell.fg = bg;
                        cell.bg = fg;
                    }
                }
            }
        })?;

        if finish_screen_snapshot(
            &mut self.screen_lines,
            &mut self.selection,
            snapshot_skipped,
            captured_lines,
        ) {
            // 極小ターミナルではスナップショットを破棄したので、次のイベントで
            // 再描画し、必ず新しいバッファから取り直す。
            self.event_caused_change = true;
        }
        Ok(())
    }
}

impl Drop for App {
    fn drop(&mut self) {
        // event_rx は Drop::drop の後にフィールドが破棄されるため、SSE / 入力の
        // 両スレッドとも send 失敗だけでは停止を検知できない。
        // 先に両方のフラグを立て、それから各ハンドルを join して残留を防ぐ。
        self.sse_shutdown.store(true, Ordering::Relaxed);
        self.input_shutdown.store(true, Ordering::Relaxed);
        if let Some(handle) = self.sse_listener.take() {
            let _ = handle.join();
        }
        if let Some(handle) = self.input_listener.take() {
            let _ = handle.join();
        }
        let _ = terminal::restore();
    }
}

/// 描画結果を選択用スナップショットへ反映する。
///
/// 極小ターミナルで描画をスキップした場合は、古い行を現在の座標で参照しないよう
/// スナップショットと選択を破棄し、再スナップショットが必要であることを返す。
fn finish_screen_snapshot(
    screen_lines: &mut Vec<String>,
    selection: &mut Selection,
    snapshot_skipped: bool,
    captured_lines: Vec<String>,
) -> bool {
    if snapshot_skipped {
        screen_lines.clear();
        selection.clear();
        true
    } else {
        *screen_lines = captured_lines;
        false
    }
}

fn map_api_session(session: &SessionInfo) -> Session {
    Session {
        id: session.id.clone(),
        title: session.title.clone(),
        created_at: Utc
            .timestamp_millis_opt(session.time.created)
            .single()
            .unwrap_or_else(Utc::now),
        updated_at: Utc
            .timestamp_millis_opt(session.time.updated)
            .single()
            .unwrap_or_else(Utc::now),
        parent_id: session.parent_id.clone(),
        share: None,
    }
}

fn map_api_message(message: &MessageInfo) -> Message {
    let parts: Vec<ContextMessagePart> = message
        .parts
        .iter()
        .filter_map(map_api_message_part)
        .collect();

    Message {
        id: message.id.clone(),
        role: match message.role.as_str() {
            "assistant" => MessageRole::Assistant,
            "system" => MessageRole::System,
            _ => MessageRole::User,
        },
        content: parts
            .iter()
            .map(message_part_text)
            .filter(|part| !part.is_empty())
            .collect::<Vec<_>>()
            .join("\n"),
        created_at: Utc
            .timestamp_millis_opt(message.created_at)
            .single()
            .unwrap_or_else(Utc::now),
        completed_at: message
            .completed_at
            .and_then(|ts| Utc.timestamp_millis_opt(ts).single()),
        agent: message.agent.clone(),
        model: message.model.clone(),
        mode: message.mode.clone(),
        finish: message.finish.clone(),
        error: message.error.clone(),
        cost: message.cost,
        tokens: TokenUsage {
            input: message.tokens.input,
            output: message.tokens.output,
            reasoning: message.tokens.reasoning,
            cache_read: message.tokens.cache_read,
            cache_write: message.tokens.cache_write,
        },
        parts,
    }
}

fn map_api_revert(revert: &SessionRevertInfo) -> RevertInfo {
    RevertInfo {
        message_id: revert.message_id.clone(),
        part_id: revert.part_id.clone(),
        snapshot: revert.snapshot.clone(),
        diff: revert.diff.clone(),
    }
}

fn map_api_message_part(part: &crate::api::MessagePart) -> Option<ContextMessagePart> {
    if let Some(text) = &part.text {
        if part.ignored == Some(true) {
            return None;
        }
        if part.part_type == "reasoning" {
            return Some(ContextMessagePart::Reasoning { text: text.clone() });
        }
        // Skip synthetic text parts (auto-continue prompts, etc.)
        if part.synthetic == Some(true) {
            return None;
        }
        return Some(ContextMessagePart::Text { text: text.clone() });
    }

    if let Some(file) = &part.file {
        return Some(ContextMessagePart::File {
            path: file.filename.clone(),
            mime: file.mime.clone(),
        });
    }

    if let Some(tool_call) = &part.tool_call {
        let arguments = if let Some(value) = tool_call.input.as_str() {
            value.to_string()
        } else {
            tool_call.input.to_string()
        };
        return Some(ContextMessagePart::ToolCall {
            id: tool_call.id.clone(),
            name: tool_call.name.clone(),
            arguments,
        });
    }

    if let Some(tool_result) = &part.tool_result {
        return Some(ContextMessagePart::ToolResult {
            id: tool_result.tool_call_id.clone(),
            result: tool_result.content.clone(),
            is_error: tool_result.is_error,
        });
    }

    None
}

fn message_part_text(part: &ContextMessagePart) -> String {
    match part {
        ContextMessagePart::Text { text } => text.clone(),
        ContextMessagePart::Reasoning { text } => format!("[reasoning] {}", text),
        ContextMessagePart::ToolCall {
            name, arguments, ..
        } => format!("[tool:{}] {}", name, arguments),
        ContextMessagePart::ToolResult {
            result, is_error, ..
        } => {
            if *is_error {
                return format!("[tool-error] {}", result);
            }
            format!("[tool-result] {}", result)
        }
        ContextMessagePart::File { path, .. } => format!("[file] {}", path),
        ContextMessagePart::Image { url } => format!("[image] {}", url),
    }
}

fn infer_task_kind_from_message(message: &Message) -> TaskKind {
    let Some(last_part) = message.parts.last() else {
        return TaskKind::LlmResponse;
    };

    match last_part {
        ContextMessagePart::Text { .. } | ContextMessagePart::Reasoning { .. } => {
            TaskKind::LlmResponse
        }
        ContextMessagePart::ToolCall { name, .. } => task_kind_from_tool_name(name),
        ContextMessagePart::ToolResult { id, .. } => message
            .parts
            .iter()
            .rev()
            .find_map(|part| match part {
                ContextMessagePart::ToolCall {
                    id: call_id, name, ..
                } if call_id == id => Some(task_kind_from_tool_name(name)),
                _ => None,
            })
            .unwrap_or(TaskKind::ToolCall),
        ContextMessagePart::File { .. } => TaskKind::FileRead,
        ContextMessagePart::Image { .. } => TaskKind::LlmResponse,
    }
}

fn task_kind_from_tool_name(name: &str) -> TaskKind {
    let normalized = name.trim().to_ascii_lowercase();
    if normalized.is_empty() {
        return TaskKind::ToolCall;
    }

    // 区切りなしのビルトイン連結名（todowrite / todoread / multiedit）は
    // トークン分割では既知キーワードと一致しないため、完全一致で先に判定する
    // （旧 contains 実装の分類を維持、R4）。
    match normalized.as_str() {
        "todowrite" | "multiedit" => return TaskKind::FileWrite,
        "todoread" => return TaskKind::FileRead,
        _ => {}
    }

    // ビルトイン名は trimmed 全体、MCP 名は '_' / '-' 区切りの各トークンを
    // キーワードと完全一致で比較する。部分一致は "thread" / "readme" 等を
    // 誤分類するため行わない。名前・トークン値はログやエラーに出さない。
    let tokens = normalized.split(['_', '-']);
    let mut is_read = false;
    let mut is_write = false;
    let mut is_exec = false;
    for token in tokens {
        match token {
            "read" | "grep" | "glob" | "list" => is_read = true,
            "write" | "edit" | "patch" | "todo" => is_write = true,
            "bash" | "shell" | "exec" | "command" => is_exec = true,
            _ => {}
        }
    }

    if is_read {
        TaskKind::FileRead
    } else if is_write {
        TaskKind::FileWrite
    } else if is_exec {
        TaskKind::CommandExec
    } else {
        TaskKind::ToolCall
    }
}

fn map_mcp_status(server: &McpStatusInfo) -> McpConnectionStatus {
    match server.status.as_str() {
        "connected" => McpConnectionStatus::Connected,
        "failed" => McpConnectionStatus::Failed,
        "needs_auth" => McpConnectionStatus::NeedsAuth,
        "needs_client_registration" => McpConnectionStatus::NeedsClientRegistration,
        "disabled" => McpConnectionStatus::Disabled,
        _ => McpConnectionStatus::Disconnected,
    }
}

fn map_api_run_status(status: &crate::api::SessionStatusInfo) -> SessionStatus {
    if status.busy {
        if status.status.eq_ignore_ascii_case("retry") {
            return SessionStatus::Retrying {
                message: status.message.clone().unwrap_or_default(),
                attempt: status.attempt.unwrap_or(0),
                next: status.next.unwrap_or_default(),
            };
        }
        return SessionStatus::Running;
    }
    SessionStatus::Idle
}

fn agent_color_from_name(theme: &crate::theme::Theme, agent_name: &str) -> ratatui::style::Color {
    if theme.agent_colors.is_empty() {
        return theme.primary;
    }
    let mut hasher = DefaultHasher::new();
    agent_name.hash(&mut hasher);
    let idx = (hasher.finish() as usize) % theme.agent_colors.len();
    theme.agent_colors[idx]
}

fn provider_from_model(model: &str) -> Option<String> {
    let model = model.trim();
    let (provider, _) = model
        .split_once('/')
        .or_else(|| model.split_once(':'))
        .unwrap_or((model, ""));
    if provider.is_empty() || provider == model {
        return None;
    }
    Some(provider.to_string())
}

/// CLI の attach -p/--password が設定した認証トークンを読み取る。
///
/// 値そのものは変更せず保持し、空文字・空白のみは未指定として扱う。
/// 読み取った直後に env から削除し、メモリ上の `AppContext::auth_token` と
/// `ApiClient` のみで保持する。子プロセス（シェル／プラグイン）が Bearer トークンを
/// 継承しないようにするため。SSE リスナーは起動後に env ではなく
/// `context.auth_token()` を参照するため、削除後も動作する
/// （`spawn_server_event_listener` を参照）。
fn resolve_tui_auth_token() -> Option<String> {
    let token = auth_token_from_env(std::env::var("OPENCODE_TUI_AUTH_TOKEN").ok());
    // 値の有無にかかわらず env に残さない（空文字・空白のみの場合も含む）。
    std::env::remove_var("OPENCODE_TUI_AUTH_TOKEN");
    token
}

fn auth_token_from_env(value: Option<String>) -> Option<String> {
    value.filter(|token| !token.trim().is_empty())
}

fn resolve_tui_base_url() -> String {
    if let Ok(value) = std::env::var("OPENCODE_TUI_BASE_URL") {
        let trimmed = value.trim();
        if !trimmed.is_empty() {
            return trimmed.to_string();
        }
    }

    // Prefer a live backend endpoint over a hardcoded default. This avoids
    // accidental 404s when localhost:3000 is occupied by a non-opencode service.
    let candidates = [
        "http://127.0.0.1:3000",
        "http://localhost:3000",
        "http://127.0.0.1:4096",
        "http://localhost:4096",
    ];
    let client = match reqwest::blocking::Client::builder()
        .timeout(Duration::from_millis(300))
        .build()
    {
        Ok(client) => client,
        Err(_) => return "http://localhost:3000".to_string(),
    };

    for base in candidates {
        let health_url = format!("{}/health", base);
        if let Ok(response) = client.get(&health_url).send() {
            if response.status().is_success() {
                return base.to_string();
            }
        }
    }

    "http://localhost:3000".to_string()
}

/// サーバの TODO 状態を取得して TUI の `SessionContext` へ反映する。
///
/// `/event` の `todo.updated` は fire-and-forget のため、再接続中に届いた更新を
/// 取りこぼすことがある。接続確立時に GET で上書きして欠落を回復する。
fn resync_todos_from_server(context: &Arc<AppContext>) {
    let Some(client) = context.get_api_client() else {
        return;
    };
    let session_ids: Vec<String> = {
        let session_ctx = context.session.read();
        let mut ids: Vec<String> = session_ctx.sessions.keys().cloned().collect();
        if let Some(current) = session_ctx.current_session_id.as_ref() {
            if !ids.contains(current) {
                ids.push(current.clone());
            }
        }
        ids
    };
    for session_id in session_ids {
        match client.get_todos(&session_id) {
            Ok(todos) => {
                let items: Vec<TodoItem> = todos
                    .into_iter()
                    .map(|todo| {
                        let status = todo.status;
                        TodoItem::from_wire(todo.content, &status)
                    })
                    .collect();
                let mut session_ctx = context.session.write();
                if items.is_empty() {
                    session_ctx.todos.remove(&session_id);
                } else {
                    session_ctx.todos.insert(session_id.clone(), items);
                }
            }
            Err(err) => {
                tracing::debug!(
                    session_id = %session_id,
                    %err,
                    "failed to resync todos from server"
                );
            }
        }
    }
}

/// SSE と resync の両経路で共有する要求 ID の登録ヘルパ。
///
/// `context.seen_request_ids` に `{kind}:{id}` を挿入し、新規登録（= enqueue してよい）
/// の場合のみ `true` を返す。SSE リスナスレッドと resync が同じ `AppContext` を
/// 共有するため、どちらが先に要求を受け取っても二重 enqueue を抑止できる。
/// キーと要求 ID はログ・エラーへ出力しない。
///
/// 回答／拒否が送信成功した時点で `forget_request_seen` が対になるキーを削除するため、
/// 集合は要求のライフサイクルに追従する（送信失敗時は再送のため残す）。
fn mark_request_seen(context: &Arc<AppContext>, kind: &str, id: &str) -> bool {
    context
        .seen_request_ids
        .write()
        .insert(format!("{kind}:{id}"))
}

/// `mark_request_seen` の対。回答／拒否がサーバへ届いた場合にだけ `kind:{id}` を
/// 集合から削除し、要求 ID が再利用されたときに新規要求が誤スキップされるのを防ぐ。
/// 送信失敗時に呼ぶと再送が抑止されるため、成功経路からのみ呼び出す。
/// キーと要求 ID はログ・エラーへ出力しない。
fn forget_request_seen(context: &Arc<AppContext>, kind: &str, id: &str) {
    context
        .seen_request_ids
        .write()
        .remove(&format!("{kind}:{id}"));
}

/// サーバの保留中 permission / question を取得し、`CustomEvent` として enqueue する。
///
/// SSE の `permission.requested` / `question.asked` は切断中に発生した要求を
/// 取りこぼすため、接続確立時に GET で再同期する。失敗時は固定文言の debug
/// ログだけを残し、要求 ID / トークン / パスを一切出力しない。
fn resync_pending_requests_from_server(context: &Arc<AppContext>, event_tx: &Sender<CustomEvent>) {
    let Some(client) = context.get_api_client() else {
        return;
    };

    match client.get_pending_permissions() {
        Ok(permissions) => {
            for value in permissions {
                // SSE の `permission.requested` は `{"request": ...}` で届くため、
                // 既存のパースヘルパへ同じ形に包んで渡し、変換ロジックを共有する。
                let wrapper = serde_json::json!({ "request": value });
                let Some(event) = parse_permission_requested(&wrapper) else {
                    continue;
                };
                let CustomEvent::PermissionRequested { id, .. } = &event else {
                    continue;
                };
                if !mark_request_seen(context, "permission", id) {
                    continue;
                }
                if event_tx.send(event).is_err() {
                    return;
                }
            }
        }
        Err(_) => tracing::debug!("failed to resync pending permissions from server"),
    }

    match client.get_pending_questions() {
        Ok(questions) => {
            for request in questions {
                if !mark_request_seen(context, "question", &request.id) {
                    continue;
                }
                if event_tx.send(CustomEvent::QuestionAsked(request)).is_err() {
                    return;
                }
            }
        }
        Err(_) => tracing::debug!("failed to resync pending questions from server"),
    }
}

/// SSE リスナが保持する `Sender<Event>` 用の薄いアダプタ。
///
/// `resync_pending_requests_from_server` は `CustomEvent` のみを扱い、ここで
/// 既存の `Event::Custom` チャネルへ橋渡しする（SSE 側の送信経路は不変）。
fn resync_pending_requests_for_listener(context: &Arc<AppContext>, event_tx: &Sender<Event>) {
    let (custom_tx, custom_rx) = mpsc::channel();
    resync_pending_requests_from_server(context, &custom_tx);
    drop(custom_tx);
    for custom in custom_rx {
        if event_tx.send(Event::Custom(custom)).is_err() {
            break;
        }
    }
}

/// SSE 初回再接続待機（従来の 400ms を維持）。
const SSE_INITIAL_RETRY_MS: u64 = 400;
/// 指数バックオフの上限。
const SSE_MAX_RETRY_MS: u64 = 30_000;
/// 上限までのシフト数（400ms * 2^8 = 102.4s、30s 上限へ丸められる）。
/// シフト 7（51.2s）の時点で既に上限を超えるため、実効待機は 7 以降すべて 30s。
const SSE_MAX_BACKOFF_SHIFT: u32 = 8;
/// 同時再接続を分散するためのジッタ上限。
const SSE_JITTER_MAX_MS: u64 = 250;
/// ストリーム読み出しのタイムアウト。reqwest は本文読込を含むリクエスト全体へ適用する。
const SSE_READ_TIMEOUT: Duration = Duration::from_secs(120);
/// これ以上生存したストリームは健全とみなし、バックオフをリセットする。
const SSE_HEALTHY_STREAM_MIN: Duration = Duration::from_secs(10);

fn sse_backoff_base_ms(attempt: u32) -> u64 {
    let shift = attempt.min(SSE_MAX_BACKOFF_SHIFT);
    let factor = 1u64.checked_shl(shift).unwrap_or(u64::MAX);
    SSE_INITIAL_RETRY_MS
        .saturating_mul(factor)
        .min(SSE_MAX_RETRY_MS)
}

fn sse_retry_delay(attempt: u32, jitter_ms: u64) -> Duration {
    let jitter = jitter_ms % (SSE_JITTER_MAX_MS + 1);
    Duration::from_millis(sse_backoff_base_ms(attempt).saturating_add(jitter))
}

/// 依存追加なしの軽量ジッタ源。同時起動したクライアントの再接続を分散する。
fn sse_jitter_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.subsec_nanos() as u64)
        .unwrap_or(0)
        % (SSE_JITTER_MAX_MS + 1)
}

/// HTTP エラー／接続エラー後の試行回数と待機時間。
fn sse_delay_after_failure(previous_attempt: u32, jitter_ms: u64) -> (u32, Duration) {
    let attempt = previous_attempt.saturating_add(1);
    (attempt, sse_retry_delay(attempt, jitter_ms))
}

/// EOF / エラー / タイムアウトを問わず、ストリーム終了後は必ず待機する。
/// 十分長く生存したストリームのみ試行回数をリセットする。
fn sse_delay_after_stream_end(
    stream_lifetime: Duration,
    previous_attempt: u32,
    jitter_ms: u64,
) -> (u32, Duration) {
    if stream_lifetime >= SSE_HEALTHY_STREAM_MIN {
        (0, sse_retry_delay(0, jitter_ms))
    } else {
        sse_delay_after_failure(previous_attempt, jitter_ms)
    }
}

/// 指数バックオフの待機時間を維持しつつ、終了通知だけは即座に反映する。
/// 戻り値は待機を最後まで終えた場合 (再接続を続けてよい場合) に true。
fn sse_sleep_until(delay: Duration, shutdown: &AtomicBool) -> bool {
    const SHUTDOWN_POLL: Duration = Duration::from_millis(50);
    let deadline = Instant::now() + delay;
    loop {
        if shutdown.load(Ordering::Relaxed) {
            return false;
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return true;
        }
        thread::sleep(SHUTDOWN_POLL.min(remaining));
    }
}

/// 入力ポーリングスレッドを起動し、JoinHandle を呼び出し側へ返す。
/// 終了は `shutdown` フラグで通知し、ハンドルを join してスレッドの残留を防ぐ。
/// 入力内容やトークン値はログ・エラーへ含めない。
fn spawn_input_listener(
    event_tx: Sender<Event>,
    shutdown: Arc<AtomicBool>,
) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        let tick_rate = Duration::from_millis(TICK_RATE_MS);
        let mut last_tick = Instant::now();

        loop {
            // SSE リスナと同じく、受信側の消失に頼らず明示的なフラグで停止する。
            // poll のタイムアウトは最大 TICK_RATE_MS なので、join は遅くてもその程度で戻る。
            if shutdown.load(Ordering::Relaxed) {
                break;
            }

            let timeout = tick_rate
                .checked_sub(last_tick.elapsed())
                .unwrap_or(tick_rate);

            if crossterm::event::poll(timeout).unwrap_or(false) {
                let event = match crossterm::event::read() {
                    Ok(crossterm::event::Event::Key(key)) => Some(Event::Key(key)),
                    Ok(crossterm::event::Event::Mouse(mouse))
                        if !matches!(mouse.kind, crossterm::event::MouseEventKind::Moved) =>
                    {
                        Some(Event::Mouse(mouse))
                    }
                    Ok(crossterm::event::Event::Resize(w, h)) => Some(Event::Resize(w, h)),
                    Ok(crossterm::event::Event::FocusGained) => Some(Event::FocusGained),
                    Ok(crossterm::event::Event::FocusLost) => Some(Event::FocusLost),
                    Ok(crossterm::event::Event::Paste(s)) => Some(Event::Paste(s)),
                    _ => None,
                };

                if let Some(e) = event {
                    if event_tx.send(e).is_err() {
                        break;
                    }
                }
            }

            if last_tick.elapsed() >= tick_rate {
                if event_tx.send(Event::Tick).is_err() {
                    break;
                }
                last_tick = Instant::now();
            }
        }
    })
}

/// サーバイベントリスナを起動し、JoinHandle を呼び出し側へ返す。
/// 終了は `shutdown` フラグで通知し、ハンドルを join してスレッドの残留を防ぐ。
fn spawn_server_event_listener(
    event_tx: Sender<Event>,
    base_url: String,
    context: Arc<AppContext>,
    shutdown: Arc<AtomicBool>,
) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        // attach 先サーバが認証を有効にしている場合は SSE にも Bearer を付与する。
        // トークン値はログへ出力しない。
        let mut builder =
            reqwest::blocking::Client::builder().connect_timeout(Duration::from_secs(2));
        if let Some(token) = context.auth_token() {
            // blocking::ClientBuilder には bearer_auth が無いため default_headers を使う。
            if let Ok(value) = reqwest::header::HeaderValue::from_str(&format!("Bearer {token}")) {
                let mut headers = reqwest::header::HeaderMap::new();
                headers.insert(reqwest::header::AUTHORIZATION, value);
                builder = builder.default_headers(headers);
            }
        }
        let client = match builder.build() {
            Ok(client) => client,
            Err(err) => {
                tracing::warn!(%err, "failed to initialize server event stream client");
                return;
            }
        };

        if shutdown.load(Ordering::Relaxed) {
            return;
        }
        let event_url = format!("{}/event", base_url.trim_end_matches('/'));
        // 起動時にもサーバ側の状態で上書きし、過去の更新の取りこぼしを回復する。
        resync_todos_from_server(&context);
        // 切断中に発生した permission / question も GET で取り戻す。
        resync_pending_requests_for_listener(&context, &event_tx);
        let mut retry_attempt: u32 = 0;
        loop {
            if shutdown.load(Ordering::Relaxed) {
                break;
            }
            match client
                .get(&event_url)
                .header(reqwest::header::ACCEPT, "text/event-stream")
                .timeout(SSE_READ_TIMEOUT)
                .send()
            {
                Ok(response) if response.status().is_success() => {
                    if shutdown.load(Ordering::Relaxed) {
                        break;
                    }
                    // 再接続中に届いた todo.updated を取りこぼす場合があるため、
                    // ストリームの消費前に GET で再同期する。
                    resync_todos_from_server(&context);
                    // 切断中にサーバへ積まれた permission / question も同様に再同期する。
                    resync_pending_requests_for_listener(&context, &event_tx);
                    let stream_started = Instant::now();
                    // 受信側が消えた (send が Err) 場合は再接続せず終了する。
                    if !consume_server_event_stream(response, &event_tx, &context, &shutdown) {
                        break;
                    }
                    // EOF / ストリーム終了も失敗として扱い、即再接続しない。
                    let (attempt, delay) = sse_delay_after_stream_end(
                        stream_started.elapsed(),
                        retry_attempt,
                        sse_jitter_ms(),
                    );
                    retry_attempt = attempt;
                    tracing::debug!(
                        url = %event_url,
                        attempt,
                        ?delay,
                        "server event stream ended; backing off"
                    );
                    if !sse_sleep_until(delay, &shutdown) {
                        break;
                    }
                }
                Ok(response) => {
                    tracing::debug!(
                        url = %event_url,
                        status = %response.status(),
                        "server event stream subscription rejected"
                    );
                    let (attempt, delay) = sse_delay_after_failure(retry_attempt, sse_jitter_ms());
                    retry_attempt = attempt;
                    if !sse_sleep_until(delay, &shutdown) {
                        break;
                    }
                }
                Err(err) => {
                    tracing::debug!(
                        url = %event_url,
                        %err,
                        "server event stream disconnected"
                    );
                    let (attempt, delay) = sse_delay_after_failure(retry_attempt, sse_jitter_ms());
                    retry_attempt = attempt;
                    if !sse_sleep_until(delay, &shutdown) {
                        break;
                    }
                }
            }
        }
    })
}

/// SSE ストリームを消費する。この関数が戻ることはすなわち EOF / 読込エラー /
/// タイムアウトによるストリーム終了を意味し、呼び出し側はバックオフ待機を行う。
/// 受信側の消失 (send 失敗) または終了要求では false を返し、再接続しない。
fn consume_server_event_stream(
    response: reqwest::blocking::Response,
    event_tx: &Sender<Event>,
    context: &Arc<AppContext>,
    shutdown: &AtomicBool,
) -> bool {
    let mut reader = BufReader::new(response);
    let mut line = String::new();
    let mut data_lines: Vec<String> = Vec::new();

    loop {
        if shutdown.load(Ordering::Relaxed) {
            return false;
        }
        line.clear();
        match reader.read_line(&mut line) {
            Ok(0) => {
                if !forward_server_event(&data_lines, event_tx, context, shutdown) {
                    return false;
                }
                break;
            }
            Ok(_) => {
                let trimmed = line.trim_end_matches(['\r', '\n']);
                if trimmed.is_empty() {
                    if !forward_server_event(&data_lines, event_tx, context, shutdown) {
                        return false;
                    }
                    data_lines.clear();
                    continue;
                }
                if trimmed.starts_with(':') {
                    continue;
                }
                if let Some(payload) = trimmed.strip_prefix("data:") {
                    data_lines.push(payload.trim_start().to_string());
                }
            }
            Err(err) => {
                tracing::debug!(%err, "error while reading server event stream");
                break;
            }
        }
    }
    true
}

/// サーバイベントを TUI のイベントチャネルへ転送する。
/// 戻り値はリスナを継続してよい場合のみ true。受信側の消失・終了要求で false。
fn forward_server_event(
    data_lines: &[String],
    event_tx: &Sender<Event>,
    context: &Arc<AppContext>,
    shutdown: &AtomicBool,
) -> bool {
    if shutdown.load(Ordering::Relaxed) {
        return false;
    }
    if data_lines.is_empty() {
        return true;
    }
    let payload = data_lines.join("\n");
    let Ok(value) = serde_json::from_str::<serde_json::Value>(&payload) else {
        return true;
    };
    // send が Err になったら握り潰さず、呼び出し側へ停止を伝える。
    let send = |event: Event| event_tx.send(event).is_ok();
    let event_type = value.get("type").and_then(|item| item.as_str());
    let session_id = value
        .get("sessionID")
        .and_then(|item| item.as_str())
        .or_else(|| value.get("sessionId").and_then(|item| item.as_str()));
    match event_type {
        Some("todo.updated") | Some("todo.changed") => {
            let Some(session_id) = session_id else {
                return true;
            };
            if let Some(items) = value.get("todos").and_then(|todos| todos.as_array()) {
                let todos: Vec<TodoItem> = items
                    .iter()
                    .filter_map(|item| {
                        let content = item.get("content")?.as_str()?;
                        let status = item
                            .get("status")
                            .and_then(|status| status.as_str())
                            .unwrap_or("pending");
                        Some(TodoItem::from_wire(content.to_string(), status))
                    })
                    .collect();
                let mut session_ctx = context.session.write();
                session_ctx.todos.insert(session_id.to_string(), todos);
            }
            if !send(Event::Custom(CustomEvent::StateChanged(
                StateChange::TodoUpdated,
            ))) {
                return false;
            }
        }
        Some("session.updated") => {
            let Some(session_id) = session_id else {
                return true;
            };
            if !send(Event::Custom(CustomEvent::StateChanged(
                StateChange::SessionUpdated(session_id.to_string()),
            ))) {
                return false;
            }
        }
        Some("session.status") => {
            let Some(session_id) = session_id else {
                return true;
            };
            let status_type = value
                .get("status")
                .and_then(|status| status.get("type"))
                .and_then(|item| item.as_str())
                .or_else(|| value.get("status").and_then(|item| item.as_str()));
            match status_type {
                Some("busy") => {
                    if !send(Event::Custom(CustomEvent::StateChanged(
                        StateChange::SessionStatusBusy(session_id.to_string()),
                    ))) {
                        return false;
                    }
                }
                Some("idle") => {
                    if !send(Event::Custom(CustomEvent::StateChanged(
                        StateChange::SessionStatusIdle(session_id.to_string()),
                    ))) {
                        return false;
                    }
                }
                Some("retry") => {
                    let attempt = value
                        .get("status")
                        .and_then(|status| status.get("attempt"))
                        .and_then(|item| item.as_u64())
                        .and_then(|v| u32::try_from(v).ok())
                        .unwrap_or(0);
                    let message = value
                        .get("status")
                        .and_then(|status| status.get("message"))
                        .and_then(|item| item.as_str())
                        .unwrap_or_default()
                        .to_string();
                    let next = value
                        .get("status")
                        .and_then(|status| status.get("next"))
                        .and_then(|item| item.as_i64())
                        .unwrap_or_default();
                    if !send(Event::Custom(CustomEvent::StateChanged(
                        StateChange::SessionStatusRetrying {
                            session_id: session_id.to_string(),
                            attempt,
                            message,
                            next,
                        },
                    ))) {
                        return false;
                    }
                }
                _ => {}
            }
        }
        Some("question.asked") => {
            if let Some(event) = parse_question_asked(&value) {
                let CustomEvent::QuestionAsked(request) = &event else {
                    return true;
                };
                // resync と同じ ID 集合を参照し、二重 enqueue を防ぐ。
                if !mark_request_seen(context, "question", &request.id) {
                    return true;
                }
                if !send(Event::Custom(event)) {
                    return false;
                }
            }
        }
        Some("permission.requested") => {
            if let Some(event) = parse_permission_requested(&value) {
                let CustomEvent::PermissionRequested { id, .. } = &event else {
                    return true;
                };
                // resync と同じ ID 集合を参照し、二重 enqueue を防ぐ。
                if !mark_request_seen(context, "permission", id) {
                    return true;
                }
                if !send(Event::Custom(event)) {
                    return false;
                }
            }
        }
        _ => {}
    }
    true
}

/// Parse a `question.asked` server event into a TUI custom event.
fn parse_question_asked(value: &serde_json::Value) -> Option<CustomEvent> {
    let request = value.get("request")?;
    // 空 ID は共有集合の "question:" キーを汚染し、後続の要求を誤スキップ
    // するため parse 段階で破棄する（permission 側と同基準、R4）。
    if request.get("id").and_then(|item| item.as_str())?.is_empty() {
        return None;
    }
    let request = serde_json::from_value::<QuestionRequestInfo>(request.clone()).ok()?;
    Some(CustomEvent::QuestionAsked(request))
}

/// Parse a `permission.requested` server event into a TUI custom event.
fn parse_permission_requested(value: &serde_json::Value) -> Option<CustomEvent> {
    let request = value.get("request")?;
    let id = request.get("id").and_then(|item| item.as_str())?;
    if id.is_empty() {
        return None;
    }
    let permission_type = request
        .get("permission_type")
        .and_then(|item| item.as_str())
        .unwrap_or_default()
        .to_string();
    let tool_name = request
        .get("tool")
        .and_then(|item| item.as_str())
        .unwrap_or_default()
        .to_string();
    let resource = request
        .get("patterns")
        .and_then(|item| item.as_array())
        .map(|patterns| {
            patterns
                .iter()
                .filter_map(|item| item.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        })
        .unwrap_or_default();
    Some(CustomEvent::PermissionRequested {
        id: id.to_string(),
        permission_type,
        resource,
        tool_name,
    })
}

fn format_theme_option_label(theme_id: &str) -> String {
    if let Some((base, variant)) = split_theme_variant(theme_id) {
        return format!("{base} ({variant})");
    }
    theme_id.to_string()
}

fn split_theme_variant(theme_id: &str) -> Option<(&str, &str)> {
    let (base, variant) = theme_id
        .rsplit_once('@')
        .or_else(|| theme_id.rsplit_once(':'))?;
    if base.is_empty() || !matches!(variant, "dark" | "light") {
        return None;
    }
    Some((base, variant))
}

fn parse_model_ref_selection(
    model_ref: &str,
    available_models: &HashSet<String>,
    model_variants: &HashMap<String, Vec<String>>,
) -> (String, Option<String>) {
    let trimmed = model_ref.trim();
    if trimmed.is_empty() {
        return (String::new(), None);
    }
    if available_models.contains(trimmed) {
        return (trimmed.to_string(), None);
    }

    let Some((candidate_base, candidate_variant)) = trimmed.rsplit_once('/') else {
        return (trimmed.to_string(), None);
    };
    if candidate_variant.is_empty() || !available_models.contains(candidate_base) {
        return (trimmed.to_string(), None);
    }
    let Some(known_variants) = model_variants.get(candidate_base) else {
        return (trimmed.to_string(), None);
    };
    if !known_variants
        .iter()
        .any(|value| value == candidate_variant)
    {
        return (trimmed.to_string(), None);
    }
    (
        candidate_base.to_string(),
        Some(candidate_variant.to_string()),
    )
}

fn selected_agent(context: &Arc<AppContext>) -> Option<String> {
    let agent = context.current_agent.read().clone();
    if agent.trim().is_empty() {
        return None;
    }
    Some(agent)
}

fn default_export_filename(title: &str, session_id: &str) -> String {
    let mut slug = String::new();
    let mut prev_dash = false;
    for ch in title.trim().chars() {
        let normalized = if ch.is_ascii_alphanumeric() {
            ch.to_ascii_lowercase()
        } else if ch.is_ascii_whitespace() || ch == '-' || ch == '_' {
            '-'
        } else {
            continue;
        };
        if normalized == '-' {
            if prev_dash || slug.is_empty() {
                continue;
            }
            prev_dash = true;
            slug.push('-');
        } else {
            prev_dash = false;
            slug.push(normalized);
        }
    }
    if slug.ends_with('-') {
        slug.pop();
    }
    if slug.is_empty() {
        let short_id = session_id.chars().take(8).collect::<String>();
        slug = format!("session-{}", short_id);
    }
    format!("{slug}.md")
}

/// テスト専用: クリップボード書き込みの経路と内容を記録する。
/// 実クリップボードや外部コマンドへ依存せず、キー入力のルーティングを検証する。
#[cfg(test)]
mod clipboard_probe {
    use std::cell::RefCell;

    thread_local! {
        static WRITES: RefCell<Vec<(&'static str, String)>> = const { RefCell::new(Vec::new()) };
    }

    pub(super) fn record(kind: &'static str, text: &str) {
        WRITES.with(|writes| writes.borrow_mut().push((kind, text.to_string())));
    }

    pub(super) fn drain() -> Vec<(&'static str, String)> {
        WRITES.with(|writes| std::mem::take(&mut *writes.borrow_mut()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read as _;
    use std::net::TcpListener;

    #[test]
    fn message_timestamp_uses_local_timezone() {
        use chrono::TimeZone;

        let created_at = Utc
            .with_ymd_and_hms(2026, 1, 2, 3, 4, 5)
            .single()
            .expect("valid UTC datetime");
        let rendered = format_message_timestamp(&created_at);
        let expected = created_at
            .with_timezone(&chrono::Local)
            .format("%H:%M:%S")
            .to_string();
        assert_eq!(rendered, expected, "timestamp must use local timezone");

        // オフセットが 0 のホストでは UTC と一致するため、差分がある場合のみ検証する。
        let raw_utc = created_at.format("%H:%M:%S").to_string();
        if expected != raw_utc {
            assert_ne!(rendered, raw_utc, "timestamp must not be raw UTC");
        }
    }

    #[test]
    fn optimistic_ids_include_unique_uuid_suffix() {
        fn assert_uuid_suffix(id: &str) {
            let suffix = id.rsplit('_').next().expect("id must contain '_'");
            assert_eq!(suffix.len(), 32, "id must end with UUID simple: {id}");
            assert!(
                suffix.bytes().all(|b| b.is_ascii_hexdigit()),
                "UUID suffix must be hex: {id}"
            );
        }

        let context = Arc::new(AppContext::new());
        let mut app = headless_app(context);
        let session_ids: Vec<String> = (0..64).map(|_| app.create_optimistic_session()).collect();
        let unique_sessions: HashSet<_> = session_ids.iter().collect();
        assert_eq!(unique_sessions.len(), session_ids.len());
        for id in &session_ids {
            assert_uuid_suffix(id);
        }

        let session_id = &session_ids[0];
        let message_ids: Vec<String> = (0..64)
            .map(|i| {
                app.append_optimistic_user_message(
                    session_id,
                    &format!("probe-{i}"),
                    None,
                    None,
                    None,
                )
            })
            .collect();
        let unique_messages: HashSet<_> = message_ids.iter().collect();
        assert_eq!(unique_messages.len(), message_ids.len());
        for id in &message_ids {
            assert_uuid_suffix(id);
        }
    }

    #[test]
    fn modal_single_char_shortcuts_ignore_ctrl_and_alt() {
        assert!(is_plain_shortcut(KeyEvent::new(
            KeyCode::Char('a'),
            KeyModifiers::empty()
        )));
        // SHIFT は許容（crossterm は大文字を SHIFT 付きで報告する）。
        assert!(is_plain_shortcut(KeyEvent::new(
            KeyCode::Char('A'),
            KeyModifiers::SHIFT
        )));
        assert!(!is_plain_shortcut(KeyEvent::new(
            KeyCode::Char('a'),
            KeyModifiers::CONTROL
        )));
        assert!(!is_plain_shortcut(KeyEvent::new(
            KeyCode::Char('a'),
            KeyModifiers::ALT
        )));
        assert!(!is_plain_shortcut(KeyEvent::new(
            KeyCode::Char('a'),
            KeyModifiers::CONTROL | KeyModifiers::ALT
        )));
    }

    #[test]
    fn permission_enter_and_esc_default_to_deny() {
        let plain = KeyModifiers::empty();
        assert_eq!(
            permission_action_for_key(KeyEvent::new(KeyCode::Enter, plain)),
            Some(PermissionAction::Deny)
        );
        assert_eq!(
            permission_action_for_key(KeyEvent::new(KeyCode::Esc, plain)),
            Some(PermissionAction::Deny)
        );
        // Allow / Always allow は明示キーでのみ発動する。
        assert_eq!(
            permission_action_for_key(KeyEvent::new(KeyCode::Char('y'), plain)),
            Some(PermissionAction::Approve)
        );
        assert_eq!(
            permission_action_for_key(KeyEvent::new(KeyCode::Char('n'), plain)),
            Some(PermissionAction::Deny)
        );
        assert_eq!(
            permission_action_for_key(KeyEvent::new(KeyCode::Char('a'), plain)),
            Some(PermissionAction::ApproveAlways)
        );
    }

    #[test]
    fn permission_shortcuts_ignore_ctrl_modifier() {
        let ctrl = KeyModifiers::CONTROL;
        assert_eq!(
            permission_action_for_key(KeyEvent::new(KeyCode::Char('y'), ctrl)),
            None
        );
        assert_eq!(
            permission_action_for_key(KeyEvent::new(KeyCode::Char('a'), ctrl)),
            None
        );
        assert_eq!(
            permission_action_for_key(KeyEvent::new(KeyCode::Char('z'), KeyModifiers::empty())),
            None
        );
    }

    #[test]
    fn secret_input_processing_handles_backspace_submit_and_cancel() {
        let mut secret = String::new();
        assert!(matches!(
            apply_secret_key(&mut secret, KeyCode::Char('t')),
            SecretInputStep::Continue
        ));
        assert!(matches!(
            apply_secret_key(&mut secret, KeyCode::Char('x')),
            SecretInputStep::Continue
        ));
        assert_eq!(secret, "tx");
        assert!(matches!(
            apply_secret_key(&mut secret, KeyCode::Backspace),
            SecretInputStep::Continue
        ));
        assert_eq!(secret, "t");
        assert!(matches!(
            apply_secret_key(&mut secret, KeyCode::Enter),
            SecretInputStep::Submit
        ));
        assert!(matches!(
            apply_secret_key(&mut secret, KeyCode::Esc),
            SecretInputStep::Cancel
        ));
    }

    #[test]
    fn parse_question_asked_reads_server_payload() {
        let value = serde_json::json!({
            "type": "question.asked",
            "request": {
                "id": "qst_1",
                "session_id": "ses_1",
                "questions": [{
                    "question": "Continue?",
                    "header": "Confirm",
                    "options": [{"label": "Yes", "description": "Go on"}],
                    "multiple": false
                }]
            }
        });

        match parse_question_asked(&value) {
            Some(CustomEvent::QuestionAsked(request)) => {
                assert_eq!(request.id, "qst_1");
                assert_eq!(request.session_id, "ses_1");
                assert_eq!(request.questions.len(), 1);
                assert_eq!(request.questions[0].question, "Continue?");
                assert_eq!(request.questions[0].options[0].label, "Yes");
                assert!(!request.questions[0].multiple);
            }
            other => panic!("expected question.asked event, got {:?}", other),
        }
    }

    #[test]
    fn parse_question_asked_ignores_malformed_payloads() {
        assert!(parse_question_asked(&serde_json::json!({"type": "question.asked"})).is_none());
        assert!(parse_question_asked(&serde_json::json!({"request": {"id": 1}})).is_none());
    }

    #[test]
    fn parse_permission_requested_maps_server_fields() {
        let value = serde_json::json!({
            "type": "permission.requested",
            "request": {
                "id": "per_1",
                "session_id": "ses_1",
                "tool": "bash",
                "permission_type": "bash",
                "patterns": ["ls", "pwd"],
                "message": "Tool 'bash' is requesting permission"
            }
        });

        match parse_permission_requested(&value) {
            Some(CustomEvent::PermissionRequested {
                id,
                permission_type,
                resource,
                tool_name,
            }) => {
                assert_eq!(id, "per_1");
                assert_eq!(permission_type, "bash");
                assert_eq!(tool_name, "bash");
                assert_eq!(resource, "ls, pwd");
            }
            other => panic!("expected permission.requested event, got {:?}", other),
        }
    }

    #[test]
    fn parse_permission_requested_requires_an_id() {
        assert!(parse_permission_requested(&serde_json::json!({"request": {}})).is_none());
        assert!(parse_permission_requested(&serde_json::json!({"request": {"id": ""}})).is_none());
    }

    #[test]
    fn task_kind_from_tool_name_categories() {
        assert_eq!(task_kind_from_tool_name("read_file"), TaskKind::FileRead);
        assert_eq!(task_kind_from_tool_name("Grep"), TaskKind::FileRead);
        assert_eq!(task_kind_from_tool_name("write"), TaskKind::FileWrite);
        assert_eq!(task_kind_from_tool_name("apply_patch"), TaskKind::FileWrite);
        assert_eq!(task_kind_from_tool_name("Bash"), TaskKind::CommandExec);
        // 区切りなしのビルトイン連結名（旧 contains 実装の分類を維持、R4）。
        assert_eq!(task_kind_from_tool_name("todowrite"), TaskKind::FileWrite);
        assert_eq!(task_kind_from_tool_name("todoread"), TaskKind::FileRead);
        assert_eq!(task_kind_from_tool_name("multiedit"), TaskKind::FileWrite);
        assert_eq!(task_kind_from_tool_name("custom"), TaskKind::ToolCall);
        assert_eq!(task_kind_from_tool_name(""), TaskKind::ToolCall);
    }

    #[test]
    fn task_kind_from_tool_name_rejects_substring_matches() {
        // キーワードが部分一致するだけの任意 / MCP ツール名は誤分類しない。
        assert_eq!(task_kind_from_tool_name("thread"), TaskKind::ToolCall);
        assert_eq!(task_kind_from_tool_name("readme"), TaskKind::ToolCall);
        assert_eq!(task_kind_from_tool_name("listening"), TaskKind::ToolCall);
        assert_eq!(task_kind_from_tool_name("editor"), TaskKind::ToolCall);
        assert_eq!(task_kind_from_tool_name("execution"), TaskKind::ToolCall);
        assert_eq!(
            task_kind_from_tool_name("mcp__thread_pool"),
            TaskKind::ToolCall
        );
    }

    #[test]
    fn task_kind_from_tool_name_splits_mcp_delimiters() {
        // '_' / '-' で分割した各トークンの完全一致で分類する。
        assert_eq!(
            task_kind_from_tool_name("mcp__read_file"),
            TaskKind::FileRead
        );
        assert_eq!(
            task_kind_from_tool_name("mcp__write-file"),
            TaskKind::FileWrite
        );
        assert_eq!(
            task_kind_from_tool_name("mcp-server__bash"),
            TaskKind::CommandExec
        );
        assert_eq!(task_kind_from_tool_name("custom-tool"), TaskKind::ToolCall);
    }

    #[test]
    fn provider_from_model_splits_slash_or_colon() {
        assert_eq!(
            provider_from_model("anthropic/claude-3"),
            Some("anthropic".to_string())
        );
        assert_eq!(
            provider_from_model("openai:gpt-5"),
            Some("openai".to_string())
        );
        assert_eq!(provider_from_model("bare-model"), None);
        assert_eq!(provider_from_model("  "), None);
    }

    #[test]
    fn mcp_status_mapping() {
        let status = |s: &str| McpStatusInfo {
            name: "server".to_string(),
            status: s.to_string(),
            tools: 0,
            resources: 0,
            error: None,
        };
        assert!(matches!(
            map_mcp_status(&status("connected")),
            McpConnectionStatus::Connected
        ));
        assert!(matches!(
            map_mcp_status(&status("failed")),
            McpConnectionStatus::Failed
        ));
        assert!(matches!(
            map_mcp_status(&status("needs_auth")),
            McpConnectionStatus::NeedsAuth
        ));
        assert!(matches!(
            map_mcp_status(&status("unknown")),
            McpConnectionStatus::Disconnected
        ));
    }

    #[test]
    fn agent_color_is_deterministic() {
        let theme = crate::theme::Theme::default();
        let first = agent_color_from_name(&theme, "build");
        let second = agent_color_from_name(&theme, "build");
        assert_eq!(first, second);
        if theme.agent_colors.is_empty() {
            assert_eq!(first, theme.primary);
        } else {
            assert!(theme.agent_colors.contains(&first));
        }
    }

    #[test]
    fn run_status_mapping() {
        let status = |busy: bool| crate::api::SessionStatusInfo {
            status: "idle".to_string(),
            idle: !busy,
            busy,
            attempt: None,
            message: None,
            next: None,
        };
        assert!(matches!(
            map_api_run_status(&status(false)),
            SessionStatus::Idle
        ));
        assert!(matches!(
            map_api_run_status(&status(true)),
            SessionStatus::Running
        ));
    }

    #[test]
    fn auth_token_from_env_treats_blank_values_as_unset() {
        assert_eq!(auth_token_from_env(None), None);
        assert_eq!(auth_token_from_env(Some(String::new())), None);
        assert_eq!(auth_token_from_env(Some("   ".to_string())), None);
    }

    #[test]
    fn auth_token_from_env_preserves_the_original_value() {
        assert_eq!(
            auth_token_from_env(Some(" token with spaces ".to_string())).as_deref(),
            Some(" token with spaces ")
        );
    }

    /// env を書き換えるテストを直列化する。
    static TUI_AUTH_TOKEN_ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// テスト後に `OPENCODE_TUI_AUTH_TOKEN` を元へ戻すガード。
    struct RestoreTuiAuthToken(Option<String>);

    impl Drop for RestoreTuiAuthToken {
        fn drop(&mut self) {
            match &self.0 {
                Some(value) => std::env::set_var("OPENCODE_TUI_AUTH_TOKEN", value),
                None => std::env::remove_var("OPENCODE_TUI_AUTH_TOKEN"),
            }
        }
    }

    #[test]
    fn resolve_tui_auth_token_clears_env_after_reading_it() {
        let _guard = TUI_AUTH_TOKEN_ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let _restore = RestoreTuiAuthToken(std::env::var("OPENCODE_TUI_AUTH_TOKEN").ok());

        std::env::set_var("OPENCODE_TUI_AUTH_TOKEN", "dummy-token-for-test");
        let token = resolve_tui_auth_token();
        assert_eq!(token.as_deref(), Some("dummy-token-for-test"));
        assert!(
            std::env::var("OPENCODE_TUI_AUTH_TOKEN").is_err(),
            "auth token must not remain in the TUI process env"
        );
    }

    #[test]
    fn resolve_tui_auth_token_clears_blank_env_value_too() {
        let _guard = TUI_AUTH_TOKEN_ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let _restore = RestoreTuiAuthToken(std::env::var("OPENCODE_TUI_AUTH_TOKEN").ok());

        std::env::set_var("OPENCODE_TUI_AUTH_TOKEN", "   ");
        assert_eq!(resolve_tui_auth_token(), None);
        assert!(
            std::env::var("OPENCODE_TUI_AUTH_TOKEN").is_err(),
            "even a blank auth token env value must be removed"
        );
    }

    #[test]
    fn unmodified_q_does_not_exit_while_typing() {
        // 未送信入力がある／入力を捕捉中は 'q' を文字として扱う。
        assert!(!should_exit_on_unmodified_q(true, true));
        assert!(!should_exit_on_unmodified_q(true, false));
        assert!(!should_exit_on_unmodified_q(false, true));
        // 入力が空でフォーカスもプロンプト外のときだけ終了する。
        assert!(should_exit_on_unmodified_q(false, false));
    }

    #[test]
    fn tab_navigation_is_delegated_to_prompt_while_typing() {
        // 入力非空 or 補完候補あり → エージェント切替ではなく補完へ委譲。
        assert!(should_delegate_navigation_key_to_prompt(true, false));
        assert!(should_delegate_navigation_key_to_prompt(false, true));
        assert!(should_delegate_navigation_key_to_prompt(true, true));
        // 入力がなく候補もない場合のみ agent 切替を許可する。
        assert!(!should_delegate_navigation_key_to_prompt(false, false));
    }

    #[test]
    fn paste_respects_permission_question_and_dialog_layers() {
        // permission → question → dialog の順で、1 つでも開いていれば挿入しない。
        assert!(!should_insert_paste_into_prompt(true, false, false));
        assert!(!should_insert_paste_into_prompt(false, true, false));
        assert!(!should_insert_paste_into_prompt(false, false, true));
        assert!(!should_insert_paste_into_prompt(true, true, true));
        assert!(should_insert_paste_into_prompt(false, false, false));
    }

    #[test]
    fn eof_stream_end_backs_off_before_reconnecting() {
        // EOF で直ちに再接続せず、初回待機（400ms）以上を必ず挟む。
        let (attempt, delay) = sse_delay_after_stream_end(Duration::from_millis(1), 0, 0);
        assert_eq!(attempt, 1);
        assert!(delay >= Duration::from_millis(SSE_INITIAL_RETRY_MS));

        // 短いストリームが続くと指数バックオフで待機が伸びる。
        let (attempt2, delay2) = sse_delay_after_stream_end(Duration::from_millis(1), attempt, 0);
        assert_eq!(attempt2, 2);
        assert!(delay2 >= delay);

        // 十分長く生存したストリームは試行回数をリセットするが、待機は残す。
        let (reset, healthy_delay) =
            sse_delay_after_stream_end(SSE_HEALTHY_STREAM_MIN, attempt2, 0);
        assert_eq!(reset, 0);
        assert_eq!(healthy_delay, Duration::from_millis(SSE_INITIAL_RETRY_MS));
    }

    #[test]
    fn sse_backoff_grows_exponentially_and_is_capped() {
        assert_eq!(sse_backoff_base_ms(0), SSE_INITIAL_RETRY_MS);
        assert_eq!(sse_backoff_base_ms(1), SSE_INITIAL_RETRY_MS * 2);
        assert_eq!(sse_backoff_base_ms(2), SSE_INITIAL_RETRY_MS * 4);
        assert!(sse_backoff_base_ms(3) > sse_backoff_base_ms(2));
        assert_eq!(sse_backoff_base_ms(u32::MAX), SSE_MAX_RETRY_MS);
    }

    #[test]
    fn sse_backoff_is_capped_at_thirty_seconds() {
        // 実装の上限シフトは 2^8（=102.4s）。コメントと定数の不一致を固定する。
        assert_eq!(SSE_MAX_BACKOFF_SHIFT, 8);
        assert_eq!(SSE_MAX_RETRY_MS, 30_000);
        // シフト 6（25.6s）は未達、シフト 7（51.2s→30s 丸め）以降は頭打ち。
        assert!(sse_backoff_base_ms(SSE_MAX_BACKOFF_SHIFT - 2) < SSE_MAX_RETRY_MS);
        for attempt in (SSE_MAX_BACKOFF_SHIFT - 1)..=SSE_MAX_BACKOFF_SHIFT + 2 {
            assert_eq!(sse_backoff_base_ms(attempt), SSE_MAX_RETRY_MS);
        }
        assert_eq!(sse_backoff_base_ms(u32::MAX), SSE_MAX_RETRY_MS);
    }

    #[test]
    fn sse_retry_delay_adds_bounded_jitter() {
        let base = sse_backoff_base_ms(3);
        let no_jitter = sse_retry_delay(3, 0);
        let with_jitter = sse_retry_delay(3, u64::MAX);
        assert_eq!(no_jitter, Duration::from_millis(base));
        assert!(with_jitter >= no_jitter);
        assert!(
            with_jitter <= Duration::from_millis(base + SSE_JITTER_MAX_MS),
            "jitter must stay within the bounded range"
        );
    }

    #[test]
    fn input_listener_stops_on_shutdown_and_can_be_joined() {
        let (event_tx, _event_rx) = mpsc::channel();
        let shutdown = Arc::new(AtomicBool::new(false));
        let handle = spawn_input_listener(event_tx, shutdown.clone());
        // App の Drop と同じ手順でフラグを立てて join できれば、入力スレッドが残留しない。
        shutdown.store(true, Ordering::Relaxed);
        assert!(handle.join().is_ok());
    }

    #[test]
    fn sse_listener_stops_on_shutdown_and_can_be_joined() {
        let (event_tx, _event_rx) = mpsc::channel();
        let shutdown = Arc::new(AtomicBool::new(false));
        let handle = spawn_server_event_listener(
            event_tx,
            "http://127.0.0.1:1".to_string(),
            Arc::new(AppContext::new()),
            shutdown.clone(),
        );
        // App の Drop と同じ手順で通知して join できれば、リスナが残留しない。
        shutdown.store(true, Ordering::Relaxed);
        assert!(handle.join().is_ok());
    }

    #[test]
    fn sse_backoff_sleep_is_interruptible() {
        let shutdown = AtomicBool::new(true);
        let started = Instant::now();
        assert!(!sse_sleep_until(Duration::from_secs(60), &shutdown));
        assert!(started.elapsed() < Duration::from_secs(1));

        // 終了要求が無い場合は指定したバックオフ時間をそのまま待つ。
        let shutdown = AtomicBool::new(false);
        let started = Instant::now();
        assert!(sse_sleep_until(Duration::from_millis(60), &shutdown));
        assert!(started.elapsed() >= Duration::from_millis(55));
    }

    #[test]
    fn sse_forwarding_stops_when_receiver_is_gone() {
        let context = Arc::new(AppContext::new());
        let shutdown = AtomicBool::new(false);
        let payload =
            vec![r#"{"type":"session.status","sessionID":"ses_1","status":"idle"}"#.to_string()];

        let (tx, rx) = mpsc::channel();
        assert!(forward_server_event(&payload, &tx, &context, &shutdown));
        assert!(rx.try_recv().is_ok());

        drop(rx);
        assert!(!forward_server_event(&payload, &tx, &context, &shutdown));

        // 終了要求時は send せず停止を返す。
        let (tx, rx) = mpsc::channel();
        shutdown.store(true, Ordering::Relaxed);
        assert!(!forward_server_event(&payload, &tx, &context, &shutdown));
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn tiny_terminal_discards_stale_screen_snapshot_and_selection() {
        let mut screen_lines = vec!["stale".to_string()];
        let mut selection = Selection::new();
        selection.start(2, 3);
        assert!(selection.is_active());

        assert!(finish_screen_snapshot(
            &mut screen_lines,
            &mut selection,
            true,
            Vec::new()
        ));
        assert!(screen_lines.is_empty());
        assert!(!selection.is_active());

        // 通常フレームでは新しいスナップショットへ差し替え、選択は維持する。
        let mut screen_lines = vec!["old".to_string()];
        let mut selection = Selection::new();
        selection.start(1, 1);
        let captured = vec!["fresh".to_string()];
        assert!(!finish_screen_snapshot(
            &mut screen_lines,
            &mut selection,
            false,
            captured.clone()
        ));
        assert_eq!(screen_lines, captured);
        assert!(selection.is_active());
    }

    /// 選択が無い Ctrl+Shift+C は input_copy へ fall through し、
    /// プロンプト入力をコピーする（TUIEV-05 回帰）。
    #[test]
    fn ctrl_shift_c_without_selection_copies_prompt() {
        super::clipboard_probe::drain();
        let context = Arc::new(AppContext::new());
        let mut app = headless_app(context);
        let dummy_prompt = "dummy prompt for ctrl shift c regression";
        app.prompt.set_input(dummy_prompt.to_string());

        let key = KeyEvent::new(
            KeyCode::Char('c'),
            KeyModifiers::CONTROL | KeyModifiers::SHIFT,
        );
        assert!(!app.selection.is_active());
        assert!(
            app.matches_keybind("input_copy", key),
            "default input_copy must be Ctrl+Shift+C"
        );

        app.handle_event(&Event::Key(key))
            .expect("Ctrl+Shift+C must be handled without error");

        let writes = super::clipboard_probe::drain();
        assert_eq!(writes.len(), 1, "exactly one clipboard write expected");
        assert_eq!(
            writes[0].0, "prompt",
            "prompt must be copied when no selection is active"
        );
        assert_eq!(writes[0].1, dummy_prompt);
        assert_eq!(
            app.prompt.get_input(),
            dummy_prompt,
            "copy must not alter the prompt input"
        );
    }

    /// 選択がある Ctrl+Shift+C は従来どおり画面選択をコピーし、
    /// プロンプト入力には触れない（後方互換）。
    #[test]
    fn ctrl_shift_c_with_selection_copies_selection_only() {
        super::clipboard_probe::drain();
        let context = Arc::new(AppContext::new());
        let mut app = headless_app(context);
        let dummy_prompt = "dummy prompt must stay untouched";
        app.prompt.set_input(dummy_prompt.to_string());
        app.screen_lines = vec!["dummy selected line".to_string()];
        app.selection.start(0, 0);
        app.selection.update(0, 4);
        app.selection.finalize();
        assert!(app.selection.is_active());

        let key = KeyEvent::new(
            KeyCode::Char('c'),
            KeyModifiers::CONTROL | KeyModifiers::SHIFT,
        );
        app.handle_event(&Event::Key(key))
            .expect("Ctrl+Shift+C must be handled without error");

        let writes = super::clipboard_probe::drain();
        assert_eq!(writes.len(), 1, "exactly one clipboard write expected");
        assert_eq!(
            writes[0].0, "selection",
            "active selection must take priority over prompt copy"
        );
        assert_eq!(writes[0].1, "dummy", "columns 0..=4 must be copied");
        assert!(
            !app.selection.is_active(),
            "selection must be cleared after copy"
        );
        assert_eq!(
            app.prompt.get_input(),
            dummy_prompt,
            "prompt must not be copied while a selection is active"
        );
    }

    /// crossterm が SHIFT 付きで大文字 'C' を報告する場合も、
    /// 選択アクティブなら画面選択をコピーする従来動作を維持する。
    #[test]
    fn ctrl_shift_uppercase_c_with_selection_still_copies_selection() {
        super::clipboard_probe::drain();
        let context = Arc::new(AppContext::new());
        let mut app = headless_app(context);
        app.screen_lines = vec!["dummy selected line".to_string()];
        app.selection.start(0, 0);
        app.selection.update(0, 4);
        app.selection.finalize();

        let key = KeyEvent::new(
            KeyCode::Char('C'),
            KeyModifiers::CONTROL | KeyModifiers::SHIFT,
        );
        app.handle_event(&Event::Key(key))
            .expect("uppercase Ctrl+Shift+C must be handled without error");

        let writes = super::clipboard_probe::drain();
        assert_eq!(writes.len(), 1, "exactly one clipboard write expected");
        assert_eq!(writes[0].0, "selection");
        assert_eq!(writes[0].1, "dummy");
    }

    /// 選択なし・大文字 'C' でもプロンプトがコピーされる（R4 回帰）。
    #[test]
    fn ctrl_shift_uppercase_c_without_selection_copies_prompt() {
        super::clipboard_probe::drain();
        let context = Arc::new(AppContext::new());
        let mut app = headless_app(context);
        let dummy_prompt = "dummy prompt uppercase";
        app.prompt.set_input(dummy_prompt.to_string());

        let key = KeyEvent::new(
            KeyCode::Char('C'),
            KeyModifiers::CONTROL | KeyModifiers::SHIFT,
        );
        app.handle_event(&Event::Key(key))
            .expect("uppercase Ctrl+Shift+C must be handled without error");

        let writes = super::clipboard_probe::drain();
        assert_eq!(writes.len(), 1, "exactly one clipboard write expected");
        assert_eq!(writes[0].0, "prompt");
        assert_eq!(writes[0].1, dummy_prompt);
        assert_eq!(
            app.prompt.get_input(),
            dummy_prompt,
            "prompt must stay intact"
        );
    }

    /// TTY を必要としない `App` を組み立てる（テスト専用）。
    ///
    /// 固定ビューポートの `Terminal` を使うため raw mode / alternate screen へ
    /// 入らず、`submit_shell_command` の実コードをそのまま呼び出せる。
    fn headless_app(context: Arc<AppContext>) -> App {
        let prompt = Prompt::new(context.clone());
        let (_event_tx, event_rx) = mpsc::channel();
        let terminal = ratatui::Terminal::with_options(
            ratatui::backend::CrosstermBackend::new(io::stdout()),
            ratatui::TerminalOptions {
                viewport: ratatui::Viewport::Fixed(ratatui::layout::Rect::new(0, 0, 8, 8)),
            },
        )
        .expect("fixed-viewport headless terminal");
        App {
            context,
            state: AppState::default(),
            terminal,
            event_rx,
            sse_shutdown: Arc::new(AtomicBool::new(false)),
            sse_listener: None,
            input_shutdown: Arc::new(AtomicBool::new(false)),
            input_listener: None,
            prompt,
            selection: Selection::new(),
            session_view: None,
            active_session_id: None,
            command_palette: CommandPalette::new(),
            slash_popup: SlashCommandPopup::new(),
            leader_state: LeaderKeyState::new(),
            model_select: ModelSelectDialog::new(),
            agent_select: AgentSelectDialog::new(),
            alert_dialog: AlertDialog::info(""),
            help_dialog: HelpDialog::new(),
            session_list_dialog: SessionListDialog::new(),
            session_rename_dialog: SessionRenameDialog::new(),
            session_export_dialog: SessionExportDialog::new(),
            prompt_stash_dialog: PromptStashDialog::new(),
            skill_list_dialog: SkillListDialog::new(),
            theme_list_dialog: ThemeListDialog::new(),
            status_dialog: StatusDialog::new(),
            mcp_dialog: McpDialog::new(),
            timeline_dialog: TimelineDialog::new(),
            fork_dialog: ForkDialog::new(),
            provider_dialog: ProviderDialog::new(),
            subagent_dialog: SubagentDialog::new(),
            tag_dialog: TagDialog::new(),
            permission_prompt: PermissionPrompt::new(),
            question_prompt: QuestionPrompt::new(),
            pending_question_requests: VecDeque::new(),
            active_question_request: None,
            toast: Toast::new(),
            screen_lines: Vec::new(),
            available_models: HashSet::new(),
            model_variants: HashMap::new(),
            model_variant_selection: HashMap::new(),
            pending_initial_submit: false,
            pending_session_sync: None,
            last_session_sync: Instant::now(),
            last_aux_sync: Instant::now(),
            event_caused_change: true,
        }
    }

    /// `create_session` は成功させ、`/shell` は失敗させるローカル HTTP スタブ。
    ///
    /// トークンも実ファイルパスもレスポンスへ含めない。
    fn spawn_create_ok_shell_fail_api() -> (String, Arc<std::sync::atomic::AtomicUsize>) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind stub api");
        let addr = listener.local_addr().expect("stub api addr");
        let shell_hits = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let stub_hits = Arc::clone(&shell_hits);
        listener
            .set_nonblocking(true)
            .expect("nonblocking stub api");
        let _stub = thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(10);
            while Instant::now() < deadline {
                let (mut stream, _) = match listener.accept() {
                    Ok(pair) => pair,
                    Err(err) if err.kind() == io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(10));
                        continue;
                    }
                    Err(_) => break,
                };
                let mut request = Vec::new();
                let mut buf = [0u8; 1024];
                while !request.windows(4).any(|w| w == b"\r\n\r\n") && request.len() < 64 * 1024 {
                    match stream.read(&mut buf) {
                        Ok(0) | Err(_) => break,
                        Ok(n) => request.extend_from_slice(&buf[..n]),
                    }
                }
                let first_line = String::from_utf8_lossy(&request)
                    .lines()
                    .next()
                    .unwrap_or("")
                    .to_string();
                let (status, body) = if first_line.contains("/shell") {
                    stub_hits.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    (
                        "500 Internal Server Error",
                        r#"{"type":"error","message":"shell failed"}"#,
                    )
                } else if first_line.contains("/session") {
                    (
                        "200 OK",
                        r#"{"id":"ses_stub_1","slug":"s","project_id":"p","directory":"","parent_id":null,"title":"t","version":"1","time":{"created":0,"updated":0,"compacting":null,"archived":null},"revert":null}"#,
                    )
                } else {
                    ("404 Not Found", r#"{}"#)
                };
                let response = format!(
                    "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = stream.write_all(response.as_bytes());
                let _ = stream.flush();
            }
        });
        (format!("http://{addr}"), shell_hits)
    }

    #[test]
    fn home_shell_failure_removes_optimistic_message() {
        let (base_url, shell_hits) = spawn_create_ok_shell_fail_api();
        let context = Arc::new(AppContext::new());
        context.set_api_client(Arc::new(ApiClient::new(base_url)));
        context.navigate(Route::Home);

        let mut app = headless_app(context.clone());
        app.submit_shell_command("echo probe".to_string())
            .expect("submit_shell_command must not fail hard");

        let session_ctx = context.session.read();
        let messages = session_ctx
            .messages
            .get("ses_stub_1")
            .expect("promoted session messages");
        assert!(
            messages.iter().all(|m| !m.id.starts_with("local_user_")),
            "optimistic user message must be removed when execute_shell fails"
        );
        assert!(
            messages.iter().all(|m| m.content != "$ echo probe"),
            "optimistic shell line must not remain after execute_shell failure"
        );
        assert!(
            shell_hits.load(std::sync::atomic::Ordering::SeqCst) >= 1,
            "the stub /shell endpoint must actually be reached (500 path)"
        );
    }

    /// `create_session` と `/shell` は成功させるが、その後の同期
    /// (`GET /session/{id}`) を失敗させるローカル HTTP スタブ。
    /// トークンも実ファイルパスもレスポンスへ含めない。
    fn spawn_shell_ok_sync_fail_api() -> (String, Arc<std::sync::atomic::AtomicUsize>) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind stub api");
        let addr = listener.local_addr().expect("stub api addr");
        let shell_hits = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let stub_hits = Arc::clone(&shell_hits);
        listener
            .set_nonblocking(true)
            .expect("nonblocking stub api");
        let _stub = thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(10);
            while Instant::now() < deadline {
                let (mut stream, _) = match listener.accept() {
                    Ok(pair) => pair,
                    Err(err) if err.kind() == io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(10));
                        continue;
                    }
                    Err(_) => break,
                };
                let mut request = Vec::new();
                let mut buf = [0u8; 1024];
                while !request.windows(4).any(|w| w == b"\r\n\r\n") && request.len() < 64 * 1024 {
                    match stream.read(&mut buf) {
                        Ok(0) | Err(_) => break,
                        Ok(n) => request.extend_from_slice(&buf[..n]),
                    }
                }
                let first_line = String::from_utf8_lossy(&request)
                    .lines()
                    .next()
                    .unwrap_or("")
                    .to_string();
                let (status, body) = if first_line.contains("POST /session ") {
                    (
                        "200 OK",
                        r#"{"id":"ses_stub_1","slug":"s","project_id":"p","directory":"","parent_id":null,"title":"t","version":"1","time":{"created":0,"updated":0,"compacting":null,"archived":null},"revert":null}"#,
                    )
                } else if first_line.contains("/shell") {
                    stub_hits.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    ("200 OK", r#"{}"#)
                } else {
                    (
                        "500 Internal Server Error",
                        r#"{"type":"error","message":"sync failed"}"#,
                    )
                };
                let response = format!(
                    "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = stream.write_all(response.as_bytes());
                let _ = stream.flush();
            }
        });
        (format!("http://{addr}"), shell_hits)
    }

    #[test]
    fn home_shell_sync_failure_removes_optimistic_message() {
        let (base_url, shell_hits) = spawn_shell_ok_sync_fail_api();
        let context = Arc::new(AppContext::new());
        context.set_api_client(Arc::new(ApiClient::new(base_url)));
        context.navigate(Route::Home);

        let mut app = headless_app(context.clone());
        app.submit_shell_command("echo probe".to_string())
            .expect("submit_shell_command must not fail hard");

        let session_ctx = context.session.read();
        let messages = session_ctx
            .messages
            .get("ses_stub_1")
            .expect("promoted session messages");
        assert!(
            messages.iter().all(|m| !m.id.starts_with("local_user_")),
            "optimistic user message must be removed when session sync fails"
        );
        assert!(
            messages.iter().all(|m| m.content != "$ echo probe"),
            "optimistic shell line must not remain after session sync failure"
        );
        assert!(
            shell_hits.load(std::sync::atomic::Ordering::SeqCst) >= 1,
            "the stub /shell endpoint must actually be reached (success path)"
        );
    }

    #[test]
    fn session_shell_sync_failure_removes_optimistic_message() {
        let (base_url, shell_hits) = spawn_shell_ok_sync_fail_api();
        let context = Arc::new(AppContext::new());
        context.set_api_client(Arc::new(ApiClient::new(base_url)));
        let session_id = context
            .session
            .write()
            .create_session(Some("shell sync".to_string()));
        context.navigate(Route::Session {
            session_id: session_id.clone(),
        });

        let mut app = headless_app(context.clone());
        app.submit_shell_command("echo probe".to_string())
            .expect("submit_shell_command must not fail hard");

        let session_ctx = context.session.read();
        let messages = session_ctx
            .messages
            .get(&session_id)
            .expect("existing session messages");
        assert!(
            messages.iter().all(|m| !m.id.starts_with("local_user_")),
            "optimistic user message must be removed when session sync fails"
        );
        assert!(
            messages.iter().all(|m| m.content != "$ echo probe"),
            "optimistic shell line must not remain after session sync failure"
        );
        assert!(
            shell_hits.load(std::sync::atomic::Ordering::SeqCst) >= 1,
            "the stub /shell endpoint must actually be reached (success path)"
        );
    }

    /// 権限 reply スタブが記録する (パス, 本文)。
    type RecordedReply = (String, String);
    /// 権限 reply スタブの (base_url, 呼び出し回数, 記録済みリクエスト)。
    type PermissionReplyStub = (
        String,
        Arc<std::sync::atomic::AtomicUsize>,
        Arc<std::sync::Mutex<Vec<RecordedReply>>>,
    );

    /// 権限 reply エンドポイント専用のローカル HTTP スタブ（成功 200）。
    /// reply の回数とリクエスト（パス＋本文）を記録する。
    /// トークンも実ファイルパスもレスポンスへ含めない。
    fn spawn_permission_reply_api() -> PermissionReplyStub {
        spawn_permission_reply_api_with_statuses(vec!["200 OK"])
    }

    /// `status` を返す権限 reply スタブ。送信失敗の再試行テストで使う。
    /// トークンも実ファイルパスもレスポンスへ含めない。
    fn spawn_permission_reply_api_with_status(status: &'static str) -> PermissionReplyStub {
        spawn_permission_reply_api_with_statuses(vec![status])
    }

    /// リクエストごとに `statuses` を順に返す権限 reply スタブ。
    /// 最後のステータスは以降のリクエストにも使う（再試行の成否を切り替える）。
    /// トークンも実ファイルパスもレスポンスへ含めない。
    fn spawn_permission_reply_api_with_statuses(
        statuses: Vec<&'static str>,
    ) -> PermissionReplyStub {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind stub api");
        let addr = listener.local_addr().expect("stub api addr");
        let reply_hits = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let recorded = Arc::new(std::sync::Mutex::new(Vec::new()));
        let hits = Arc::clone(&reply_hits);
        let log = Arc::clone(&recorded);
        let status_queue = Arc::new(std::sync::Mutex::new(statuses));
        listener
            .set_nonblocking(true)
            .expect("nonblocking stub api");
        let _stub = thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(10);
            while Instant::now() < deadline {
                let (mut stream, _) = match listener.accept() {
                    Ok(pair) => pair,
                    Err(err) if err.kind() == io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(10));
                        continue;
                    }
                    Err(_) => break,
                };
                let mut request = Vec::new();
                let mut buf = [0u8; 1024];
                while !request.windows(4).any(|w| w == b"\r\n\r\n") && request.len() < 64 * 1024 {
                    match stream.read(&mut buf) {
                        Ok(0) | Err(_) => break,
                        Ok(n) => request.extend_from_slice(&buf[..n]),
                    }
                }
                // Content-Length 分だけ追加で読み、reply 本文も記録する。
                if let Some(header_end) = request.windows(4).position(|w| w == b"\r\n\r\n") {
                    let headers = String::from_utf8_lossy(&request[..header_end + 4]).to_string();
                    let content_length: usize = headers
                        .lines()
                        .find_map(|line| {
                            let (name, value) = line.split_once(':')?;
                            if name.eq_ignore_ascii_case("content-length") {
                                value.trim().parse().ok()
                            } else {
                                None
                            }
                        })
                        .unwrap_or(0);
                    let body_start = header_end + 4;
                    while request.len() < body_start + content_length {
                        match stream.read(&mut buf) {
                            Ok(0) | Err(_) => break,
                            Ok(n) => request.extend_from_slice(&buf[..n]),
                        }
                    }
                }
                let text = String::from_utf8_lossy(&request).to_string();
                let first_line = text.lines().next().unwrap_or("").to_string();
                if first_line.contains("/permission/") && first_line.contains("/reply") {
                    hits.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    log.lock().expect("record lock").push((first_line, text));
                }
                let status = {
                    let mut queue = status_queue.lock().expect("status lock");
                    if queue.len() > 1 {
                        queue.remove(0)
                    } else {
                        queue.first().copied().unwrap_or("200 OK")
                    }
                };
                let body = "{}";
                let response = format!(
                    "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = stream.write_all(response.as_bytes());
                let _ = stream.flush();
            }
        });
        (format!("http://{addr}"), reply_hits, recorded)
    }

    #[test]
    fn mouse_click_sends_exactly_one_permission_reply_with_original_id() {
        use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};

        // (column, 期待する decision) — permission.rs のボタン列割当てに合わせる。
        let cases = [(11, "once"), (25, "reject"), (40, "always")];
        for (column, decision) in cases {
            let (base_url, reply_hits, recorded) = spawn_permission_reply_api();
            let context = Arc::new(AppContext::new());
            context.set_api_client(Arc::new(ApiClient::new(base_url)));
            let mut app = headless_app(context);

            app.permission_prompt.add_request(PermissionRequest {
                id: "perm_mouse_1".to_string(),
                permission_type: PermissionType::Bash,
                resource: "ls -la".to_string(),
                tool_name: "bash".to_string(),
            });
            // x=10, y=20, width=40, height=8 -> ボタン行は 26。
            app.permission_prompt
                .set_last_rendered_area_for_test(ratatui::layout::Rect::new(10, 20, 40, 8));

            app.handle_event(&Event::Mouse(MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column,
                row: 26,
                modifiers: KeyModifiers::NONE,
            }))
            .expect("mouse event must be handled without error");

            assert_eq!(
                reply_hits.load(Ordering::SeqCst),
                1,
                "decision={decision}: mouse click must send exactly one reply"
            );
            let recorded = recorded.lock().expect("record lock");
            assert_eq!(recorded.len(), 1, "decision={decision}");
            let (path, full) = &recorded[0];
            assert!(
                path.contains("/permission/perm_mouse_1/reply"),
                "reply must use the original request id: {path}"
            );
            assert!(
                !path.contains("_always"),
                "request id must not be suffixed: {path}"
            );
            assert!(
                full.contains(&format!("\"reply\":\"{decision}\"")),
                "reply decision must be {decision}: {full}"
            );
            assert!(
                app.permission_prompt.is_empty(),
                "prompt must be closed after click"
            );
        }
    }

    #[test]
    fn permission_reply_http_failure_keeps_request_for_retry() {
        // 送信失敗（404 以外の非成功ステータス）時は要求を残し、トーストを出して
        // 再試行可能にする。404（既に回答済み）は成功扱いになるため含めない。
        for status in ["500 Internal Server Error", "503 Service Unavailable"] {
            let (base_url, reply_hits, recorded) = spawn_permission_reply_api_with_status(status);
            let context = Arc::new(AppContext::new());
            context.set_api_client(Arc::new(ApiClient::new(base_url)));
            let mut app = headless_app(context);

            app.permission_prompt.add_request(PermissionRequest {
                id: "perm_retry_1".to_string(),
                permission_type: PermissionType::Bash,
                resource: "ls -la".to_string(),
                tool_name: "bash".to_string(),
            });

            // y = Approve ("once")。キーボード経路で送信。
            app.handle_event(&Event::Key(KeyEvent::new(
                KeyCode::Char('y'),
                KeyModifiers::NONE,
            )))
            .expect("key event must be handled without error");

            assert_eq!(
                reply_hits.load(Ordering::SeqCst),
                1,
                "status={status}: reply must be attempted exactly once"
            );
            assert!(
                app.permission_prompt.is_open,
                "status={status}: prompt must stay open after send failure"
            );
            assert_eq!(
                app.permission_prompt.pending_count(),
                1,
                "status={status}: request must be kept for retry"
            );
            assert_eq!(
                app.permission_prompt
                    .current_request()
                    .map(|r| r.id.as_str()),
                Some("perm_retry_1"),
                "status={status}: same request must remain for retry"
            );
            {
                let recorded = recorded.lock().expect("record lock");
                assert_eq!(recorded.len(), 1, "status={status}");
                assert!(
                    recorded[0].0.contains("/permission/perm_retry_1/reply"),
                    "status={status}: original id must be used: {}",
                    recorded[0].0
                );
            }
            assert!(
                app.toast.is_visible(),
                "status={status}: failure must surface a toast"
            );
        }
    }

    #[test]
    fn permission_reply_without_client_keeps_request_for_retry() {
        // API クライアント未設定でもサイレントに捨てず、要求を残す。
        let context = Arc::new(AppContext::new());
        let mut app = headless_app(context);
        app.permission_prompt.add_request(PermissionRequest {
            id: "perm_no_client".to_string(),
            permission_type: PermissionType::Bash,
            resource: "ls -la".to_string(),
            tool_name: "bash".to_string(),
        });

        app.apply_permission_action(PermissionAction::Approve);

        assert!(
            app.permission_prompt.is_open,
            "prompt must stay open when no API client is available"
        );
        assert_eq!(
            app.permission_prompt.pending_count(),
            1,
            "request must be kept when no API client is available"
        );
        assert_eq!(
            app.permission_prompt
                .current_request()
                .map(|r| r.id.as_str()),
            Some("perm_no_client")
        );
        assert!(
            app.toast.is_visible(),
            "missing client must surface a toast instead of silently dropping"
        );
    }

    #[test]
    fn permission_reply_retry_after_failure_closes_prompt_on_success() {
        // 1 回目は 500 で失敗、2 回目は 200 で成功 → プロンプトが閉じる。
        let (base_url, reply_hits, recorded) =
            spawn_permission_reply_api_with_statuses(vec!["500 Internal Server Error", "200 OK"]);
        let context = Arc::new(AppContext::new());
        context.set_api_client(Arc::new(ApiClient::new(base_url)));
        let mut app = headless_app(context);

        app.permission_prompt.add_request(PermissionRequest {
            id: "perm_retry_flow".to_string(),
            permission_type: PermissionType::Bash,
            resource: "ls -la".to_string(),
            tool_name: "bash".to_string(),
        });

        // 失敗: 要求が残る。
        app.apply_permission_action(PermissionAction::Approve);
        assert!(
            app.permission_prompt.is_open,
            "failed first reply must keep the request"
        );

        // 再試行成功: 要求が削除されプロンプトが閉じる。
        app.apply_permission_action(PermissionAction::Approve);
        assert!(
            app.permission_prompt.is_empty(),
            "successful retry must remove the request and close the prompt"
        );
        assert_eq!(
            reply_hits.load(Ordering::SeqCst),
            2,
            "both attempts must reach the server"
        );
        let recorded = recorded.lock().expect("record lock");
        assert_eq!(recorded.len(), 2, "both attempts must be recorded");
        assert!(
            recorded
                .iter()
                .all(|(path, _)| path.contains("/permission/perm_retry_flow/reply")),
            "both attempts must reuse the original request id"
        );
    }

    #[test]
    fn permission_reply_already_answered_404_closes_prompt_without_error() {
        // 1 回目 200 でサーバ側の要求は除去済み。同名 id の再送には 404 が返る。
        // 404 は「既に回答済み」として成功扱いになり、要求は残らずエラーも出ない。
        let (base_url, reply_hits, recorded) =
            spawn_permission_reply_api_with_statuses(vec!["200 OK", "404 Not Found"]);
        let context = Arc::new(AppContext::new());
        context.set_api_client(Arc::new(ApiClient::new(base_url)));
        let mut app = headless_app(context);

        app.permission_prompt.add_request(PermissionRequest {
            id: "perm_already_answered".to_string(),
            permission_type: PermissionType::Bash,
            resource: "ls -la".to_string(),
            tool_name: "bash".to_string(),
        });

        // 1 回目: 200 → 要求が削除されプロンプトが閉じる。
        app.apply_permission_action(PermissionAction::Approve);
        assert!(
            app.permission_prompt.is_empty(),
            "first 200 reply must close the prompt"
        );

        // 同じ id の要求が再キューされた状況を再現（重複イベント等）。
        app.permission_prompt.add_request(PermissionRequest {
            id: "perm_already_answered".to_string(),
            permission_type: PermissionType::Bash,
            resource: "ls -la".to_string(),
            tool_name: "bash".to_string(),
        });

        // 2 回目: 404（既に回答済み）→ Err にならず要求が残らない。
        app.apply_permission_action(PermissionAction::Approve);
        assert!(
            app.permission_prompt.is_empty(),
            "already-answered 404 must remove the request and close the prompt"
        );
        assert_eq!(
            app.permission_prompt.pending_count(),
            0,
            "already-answered 404 must not leave a pending request"
        );
        assert_eq!(
            reply_hits.load(Ordering::SeqCst),
            2,
            "both replies must reach the server"
        );
        {
            let recorded = recorded.lock().expect("record lock");
            assert_eq!(recorded.len(), 2, "both replies must be recorded");
            assert!(
                recorded
                    .iter()
                    .all(|(path, _)| path.contains("/permission/perm_already_answered/reply")),
                "both replies must reuse the original request id"
            );
        }
        assert!(
            !app.toast.is_visible(),
            "already-answered 404 must not surface an error toast"
        );
    }

    /// 質問 reply/reject スタブの (base_url, 呼び出し回数, 記録済みリクエスト)。
    type QuestionReplyStub = (
        String,
        Arc<std::sync::atomic::AtomicUsize>,
        Arc<std::sync::Mutex<Vec<RecordedReply>>>,
    );

    /// リクエストごとに `statuses` を順に返す質問 reply/reject スタブ
    /// （loopback のみ、外部送信なし）。最後のステータスは以降にも使う。
    /// トークンも実ファイルパスもレスポンスへ含めない。
    fn spawn_question_reply_api_with_statuses(statuses: Vec<&'static str>) -> QuestionReplyStub {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind stub api");
        let addr = listener.local_addr().expect("stub api addr");
        let reply_hits = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let recorded = Arc::new(std::sync::Mutex::new(Vec::new()));
        let hits = Arc::clone(&reply_hits);
        let log = Arc::clone(&recorded);
        let status_queue = Arc::new(std::sync::Mutex::new(statuses));
        listener
            .set_nonblocking(true)
            .expect("nonblocking stub api");
        let _stub = thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(10);
            while Instant::now() < deadline {
                let (mut stream, _) = match listener.accept() {
                    Ok(pair) => pair,
                    Err(err) if err.kind() == io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(10));
                        continue;
                    }
                    Err(_) => break,
                };
                let mut request = Vec::new();
                let mut buf = [0u8; 1024];
                while !request.windows(4).any(|w| w == b"\r\n\r\n") && request.len() < 64 * 1024 {
                    match stream.read(&mut buf) {
                        Ok(0) | Err(_) => break,
                        Ok(n) => request.extend_from_slice(&buf[..n]),
                    }
                }
                if let Some(header_end) = request.windows(4).position(|w| w == b"\r\n\r\n") {
                    let headers = String::from_utf8_lossy(&request[..header_end + 4]).to_string();
                    let content_length: usize = headers
                        .lines()
                        .find_map(|line| {
                            let (name, value) = line.split_once(':')?;
                            if name.eq_ignore_ascii_case("content-length") {
                                value.trim().parse().ok()
                            } else {
                                None
                            }
                        })
                        .unwrap_or(0);
                    let body_start = header_end + 4;
                    while request.len() < body_start + content_length {
                        match stream.read(&mut buf) {
                            Ok(0) | Err(_) => break,
                            Ok(n) => request.extend_from_slice(&buf[..n]),
                        }
                    }
                }
                let text = String::from_utf8_lossy(&request).to_string();
                let first_line = text.lines().next().unwrap_or("").to_string();
                if first_line.contains("/question/")
                    && (first_line.contains("/reply") || first_line.contains("/reject"))
                {
                    hits.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    log.lock().expect("record lock").push((first_line, text));
                }
                let status = {
                    let mut queue = status_queue.lock().expect("status lock");
                    if queue.len() > 1 {
                        queue.remove(0)
                    } else {
                        queue.first().copied().unwrap_or("200 OK")
                    }
                };
                let body = "{}";
                let response = format!(
                    "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = stream.write_all(response.as_bytes());
                let _ = stream.flush();
            }
        });
        (format!("http://{addr}"), reply_hits, recorded)
    }

    /// テキスト質問を 1 件キューし、アクティブ要求とダイアログを開く。
    fn enqueue_text_question(app: &mut App, id: &str) {
        app.enqueue_question_request(QuestionRequestInfo {
            id: id.to_string(),
            session_id: "session_probe".to_string(),
            questions: vec![crate::api::QuestionInfo {
                question: "Proceed?".to_string(),
                header: None,
                options: vec![],
                multiple: false,
            }],
        });
    }

    /// 2 問の質問リクエストを投入する（再送時の回答重複検証用）。
    fn enqueue_two_text_questions(app: &mut App, id: &str) {
        app.enqueue_question_request(QuestionRequestInfo {
            id: id.to_string(),
            session_id: "session_probe".to_string(),
            questions: vec![
                crate::api::QuestionInfo {
                    question: "First?".to_string(),
                    header: None,
                    options: vec![],
                    multiple: false,
                },
                crate::api::QuestionInfo {
                    question: "Second?".to_string(),
                    header: None,
                    options: vec![],
                    multiple: false,
                },
            ],
        });
    }

    #[test]
    fn question_reply_retry_with_multiple_questions_does_not_duplicate_answers() {
        // 1 問目回答 → 2 問目で 500 → 再送 200。answers が 1 セットだけ送られる。
        let (base_url, reply_hits, recorded) =
            spawn_question_reply_api_with_statuses(vec!["500 Internal Server Error", "200 OK"]);
        let context = Arc::new(AppContext::new());
        context.set_api_client(Arc::new(ApiClient::new(base_url)));
        let mut app = headless_app(context);
        enqueue_two_text_questions(&mut app, "q_multi_retry");

        // 1 問目に回答（まだ送信されない）。
        app.submit_question_answer(vec!["first".to_string()]);
        assert_eq!(reply_hits.load(Ordering::SeqCst), 0);

        // 2 問目に回答 → 500 で失敗、要求が残る。
        app.submit_question_answer(vec!["second".to_string()]);
        assert_eq!(reply_hits.load(Ordering::SeqCst), 1);
        assert!(app.active_question_request.is_some());

        // 再送 → 200 で要求が消える。
        app.submit_question_answer(vec!["second".to_string()]);
        assert_eq!(reply_hits.load(Ordering::SeqCst), 2);
        assert!(app.active_question_request.is_none());

        let recorded = recorded.lock().expect("record lock");
        assert_eq!(recorded.len(), 2, "both attempts must be recorded");
        for (_, raw) in recorded.iter() {
            // 記録は HTTP リクエスト全体（ヘッダ + ボディ）なのでボディだけを取り出す。
            let body = raw.split_once("\r\n\r\n").map(|(_, b)| b).unwrap_or("");
            let body = body.trim_end_matches(|c: char| c.is_control() || c.is_whitespace());
            let parsed: serde_json::Value = serde_json::from_str(body).expect("valid JSON body");
            let answers = parsed
                .get("answers")
                .and_then(|v| v.as_array())
                .expect("answers");
            assert_eq!(answers.len(), 2, "exactly one answer set must be sent");
        }
    }

    #[test]
    fn question_reply_retry_after_failure_removes_request_on_success() {
        // 1 回目 500 で失敗 → 要求・ダイアログが残り、2 回目 200 で消える。
        let (base_url, reply_hits, recorded) =
            spawn_question_reply_api_with_statuses(vec!["500 Internal Server Error", "200 OK"]);
        let context = Arc::new(AppContext::new());
        context.set_api_client(Arc::new(ApiClient::new(base_url)));
        let mut app = headless_app(context);
        enqueue_text_question(&mut app, "q_reply_retry");

        // 失敗: 要求が残り、ダイアログも残る（トーストを表示）。
        app.submit_question_answer(vec!["yes".to_string()]);
        assert_eq!(
            reply_hits.load(Ordering::SeqCst),
            1,
            "first reply must reach the stub"
        );
        assert!(
            app.active_question_request.is_some(),
            "failed reply must keep the request for retry"
        );
        assert_eq!(
            app.active_question_request
                .as_ref()
                .map(|r| r.request_id.as_str()),
            Some("q_reply_retry"),
            "the same request must remain for retry"
        );
        assert!(
            app.question_prompt.is_open,
            "failed reply must keep the question dialog open"
        );
        assert!(app.toast.is_visible(), "failed reply must surface a toast");

        // 2 回目成功: 要求が削除されダイアログが閉じる。
        app.submit_question_answer(vec!["yes".to_string()]);
        assert_eq!(
            reply_hits.load(Ordering::SeqCst),
            2,
            "retry must reach the stub"
        );
        assert!(
            app.active_question_request.is_none(),
            "successful retry must remove the request"
        );
        assert!(
            !app.question_prompt.is_open,
            "successful retry must close the question dialog"
        );
        let recorded = recorded.lock().expect("record lock");
        assert_eq!(recorded.len(), 2, "both attempts must be recorded");
        assert!(
            recorded
                .iter()
                .all(|(path, _)| path.contains("/question/q_reply_retry/reply")),
            "both attempts must reuse the original request id"
        );
        assert!(
            recorded
                .iter()
                .all(|(_, body)| body.contains("\"answers\":[[\"yes\"]]")),
            "retry must send the same answer exactly once"
        );
    }

    #[test]
    fn question_reject_retry_after_failure_removes_request_on_success() {
        // reject も送信成功時のみ要求を削除する。
        let (base_url, reject_hits, recorded) =
            spawn_question_reply_api_with_statuses(vec!["500 Internal Server Error", "200 OK"]);
        let context = Arc::new(AppContext::new());
        context.set_api_client(Arc::new(ApiClient::new(base_url)));
        let mut app = headless_app(context);
        enqueue_text_question(&mut app, "q_reject_retry");

        app.reject_active_question();
        assert_eq!(reject_hits.load(Ordering::SeqCst), 1);
        assert!(
            app.active_question_request.is_some(),
            "failed reject must keep the request"
        );
        assert!(
            app.question_prompt.is_open,
            "failed reject must keep the dialog open"
        );

        app.reject_active_question();
        assert_eq!(reject_hits.load(Ordering::SeqCst), 2);
        assert!(
            app.active_question_request.is_none(),
            "successful reject must remove the request"
        );
        assert!(
            !app.question_prompt.is_open,
            "successful reject must close the dialog"
        );
        let recorded = recorded.lock().expect("record lock");
        assert_eq!(recorded.len(), 2);
        assert!(
            recorded
                .iter()
                .all(|(path, _)| path.contains("/question/q_reject_retry/reject")),
            "both attempts must reuse the original request id"
        );
    }

    #[test]
    fn question_reply_and_reject_without_client_keep_request() {
        // API クライアント未設定でも要求を捨てず、再試行可能にする。
        let mut submit_app = headless_app(Arc::new(AppContext::new()));
        enqueue_text_question(&mut submit_app, "q_no_client_submit");
        submit_app.submit_question_answer(vec!["yes".to_string()]);
        assert!(
            submit_app.active_question_request.is_some() && submit_app.question_prompt.is_open,
            "submit without client must keep request and dialog"
        );
        assert!(submit_app.toast.is_visible());

        let mut reject_app = headless_app(Arc::new(AppContext::new()));
        enqueue_text_question(&mut reject_app, "q_no_client_reject");
        reject_app.reject_active_question();
        assert!(
            reject_app.active_question_request.is_some() && reject_app.question_prompt.is_open,
            "reject without client must keep request and dialog"
        );
        assert!(reject_app.toast.is_visible());
    }

    /// セッション一覧は成功させ、`/session/status` の成否を `status_fails`
    /// フラグで切り替えられるローカル HTTP スタブ（loopback のみ、外部送信なし）。
    /// トークンも実ファイルパスもレスポンスへ含めない。
    fn spawn_session_status_api() -> (
        String,
        Arc<std::sync::atomic::AtomicBool>,
        Arc<std::sync::atomic::AtomicUsize>,
    ) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind stub api");
        let addr = listener.local_addr().expect("stub api addr");
        let status_fails = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let status_hits = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let stub_fails = Arc::clone(&status_fails);
        let stub_hits = Arc::clone(&status_hits);
        listener
            .set_nonblocking(true)
            .expect("nonblocking stub api");
        let _stub = thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(10);
            while Instant::now() < deadline {
                let (mut stream, _) = match listener.accept() {
                    Ok(pair) => pair,
                    Err(err) if err.kind() == io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(10));
                        continue;
                    }
                    Err(_) => break,
                };
                let mut request = Vec::new();
                let mut buf = [0u8; 1024];
                while !request.windows(4).any(|w| w == b"\r\n\r\n") && request.len() < 64 * 1024 {
                    match stream.read(&mut buf) {
                        Ok(0) | Err(_) => break,
                        Ok(n) => request.extend_from_slice(&buf[..n]),
                    }
                }
                let first_line = String::from_utf8_lossy(&request)
                    .lines()
                    .next()
                    .unwrap_or("")
                    .to_string();
                let (status, body) = if first_line.contains("/session/status") {
                    stub_hits.fetch_add(1, Ordering::SeqCst);
                    if stub_fails.load(Ordering::SeqCst) {
                        (
                            "500 Internal Server Error",
                            r#"{"type":"error","message":"status failed"}"#,
                        )
                    } else {
                        (
                            "200 OK",
                            r#"{"ses_busy_1":{"status":"running","idle":false,"busy":true}}"#,
                        )
                    }
                } else if first_line.contains("/session") {
                    (
                        "200 OK",
                        r#"[{"id":"ses_busy_1","slug":"s","project_id":"p","directory":"","parent_id":null,"title":"Busy Session","version":"1","time":{"created":0,"updated":1,"compacting":null,"archived":null},"revert":null}]"#,
                    )
                } else {
                    ("404 Not Found", r#"{}"#)
                };
                let response = format!(
                    "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = stream.write_all(response.as_bytes());
                let _ = stream.flush();
            }
        });
        (format!("http://{addr}"), status_fails, status_hits)
    }

    /// セッション一覧ダイアログを TestBackend に描画し、文本内容を返す。
    fn render_session_list_dialog(dialog: &SessionListDialog) -> String {
        use ratatui::{backend::TestBackend, Terminal};

        let backend = TestBackend::new(100, 30);
        let mut terminal = Terminal::new(backend).expect("TestBackend should initialize");
        let theme = crate::theme::Theme::default();
        terminal
            .draw(|frame| {
                dialog.render(frame, ratatui::layout::Rect::new(0, 0, 100, 30), &theme);
            })
            .expect("draw should succeed");
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
    fn session_status_error_preserves_existing_busy_display() {
        let (base_url, status_fails, status_hits) = spawn_session_status_api();
        let context = Arc::new(AppContext::new());
        context.set_api_client(Arc::new(ApiClient::new(base_url)));
        let mut app = headless_app(context);
        app.session_list_dialog.open(None);

        // 1 回目: status 取得成功（busy=true）→ ビジー表示が描画される。
        status_fails.store(false, Ordering::SeqCst);
        app.refresh_session_list_dialog();
        let before = render_session_list_dialog(&app.session_list_dialog);
        assert!(
            before.contains('◌') && before.contains("ses_busy_1"),
            "successful status refresh must render the busy marker: {before}"
        );

        // 2 回目: status 取得失敗 → 早期 return し、既存のビジー表示が維持される。
        status_fails.store(true, Ordering::SeqCst);
        app.refresh_session_list_dialog();
        let after = render_session_list_dialog(&app.session_list_dialog);
        assert_eq!(
            before, after,
            "status failure must not overwrite the existing session display"
        );
        assert!(
            after.contains('◌'),
            "busy session must stay busy after a status fetch error: {after}"
        );
        assert_eq!(
            status_hits.load(Ordering::SeqCst),
            2,
            "each refresh must reach the /session/status stub exactly once"
        );
    }

    /// GET /permission と GET /question に保留要求を返す loopback スタブ。
    /// 外部送信は行わず、トークンや実ファイルパスはレスポンスへ含めない。
    fn spawn_pending_requests_api() -> String {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind stub api");
        let addr = listener.local_addr().expect("stub api addr");
        listener
            .set_nonblocking(true)
            .expect("nonblocking stub api");
        let _stub = thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(10);
            while Instant::now() < deadline {
                let (mut stream, _) = match listener.accept() {
                    Ok(pair) => pair,
                    Err(err) if err.kind() == io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(10));
                        continue;
                    }
                    Err(_) => break,
                };
                let mut request = Vec::new();
                let mut buf = [0u8; 1024];
                while !request.windows(4).any(|w| w == b"\r\n\r\n") && request.len() < 64 * 1024 {
                    match stream.read(&mut buf) {
                        Ok(0) | Err(_) => break,
                        Ok(n) => request.extend_from_slice(&buf[..n]),
                    }
                }
                let first_line = String::from_utf8_lossy(&request)
                    .lines()
                    .next()
                    .unwrap_or("")
                    .to_string();
                let body = if first_line.contains("/permission") {
                    r#"[{"id":"perm_resync_1","session_id":"ses_1","tool":"bash","input":{},"message":"allow","permission_type":"edit","patterns":["tool/read","tool/write"],"metadata":{},"always":[]}]"#
                } else if first_line.contains("/question") {
                    r#"[{"id":"q_resync_1","session_id":"ses_1","questions":[{"question":"Proceed?","header":null,"options":[{"label":"Yes","description":null}],"multiple":false}]}]"#
                } else {
                    "[]"
                };
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = stream.write_all(response.as_bytes());
                let _ = stream.flush();
            }
        });
        format!("http://{addr}")
    }

    #[test]
    fn resync_pending_requests_delivers_permission_and_question_events() {
        let base_url = spawn_pending_requests_api();
        let context = Arc::new(AppContext::new());
        context.set_api_client(Arc::new(ApiClient::new(base_url)));
        let (event_tx, event_rx) = mpsc::channel();

        resync_pending_requests_from_server(&context, &event_tx);

        let mut permissions = Vec::new();
        let mut questions = Vec::new();
        for event in event_rx.try_iter() {
            match event {
                CustomEvent::PermissionRequested {
                    id,
                    permission_type,
                    resource,
                    tool_name,
                } => permissions.push((id, permission_type, resource, tool_name)),
                CustomEvent::QuestionAsked(request) => questions.push(request),
                other => panic!("unexpected custom event: {other:?}"),
            }
        }

        assert_eq!(permissions.len(), 1, "one permission must be re-enqueued");
        assert_eq!(permissions[0].0, "perm_resync_1");
        assert_eq!(permissions[0].1, "edit");
        assert_eq!(permissions[0].2, "tool/read, tool/write");
        assert_eq!(permissions[0].3, "bash");
        assert_eq!(questions.len(), 1, "one question must be re-enqueued");
        assert_eq!(questions[0].id, "q_resync_1");
        assert_eq!(questions[0].questions[0].question, "Proceed?");

        // 同一 ID の再送は抑止される（再接続時の二重 enqueue 防止）。
        resync_pending_requests_from_server(&context, &event_tx);
        assert!(
            event_rx.try_recv().is_err(),
            "duplicate pending requests must not be re-enqueued"
        );
    }

    /// SSE と resync で共通の要求 ID を使うためのペイロード。
    /// ID は loopback スタブ (`spawn_pending_requests_api`) と同じ値を使い、
    /// トークンや実パスは一切含めない。
    fn pending_request_sse_payloads() -> [serde_json::Value; 2] {
        [
            serde_json::json!({
                "type": "permission.requested",
                "request": {
                    "id": "perm_resync_1",
                    "session_id": "ses_1",
                    "tool": "bash",
                    "input": {},
                    "message": "allow",
                    "permission_type": "edit",
                    "patterns": ["tool/read", "tool/write"],
                    "metadata": {},
                    "always": []
                }
            }),
            serde_json::json!({
                "type": "question.asked",
                "request": {
                    "id": "q_resync_1",
                    "session_id": "ses_1",
                    "questions": [{
                        "question": "Proceed?",
                        "header": null,
                        "options": [{"label": "Yes", "description": null}],
                        "multiple": false
                    }]
                }
            }),
        ]
    }

    fn forward_sse_payloads(
        payloads: &[serde_json::Value],
        event_tx: &Sender<Event>,
        context: &Arc<AppContext>,
    ) {
        let shutdown = AtomicBool::new(false);
        for payload in payloads {
            let lines = vec![payload.to_string()];
            assert!(
                forward_server_event(&lines, event_tx, context, &shutdown),
                "SSE forwarding must keep the listener alive"
            );
        }
    }

    #[test]
    fn sse_events_then_resync_do_not_enqueue_twice() {
        let base_url = spawn_pending_requests_api();
        let context = Arc::new(AppContext::new());
        context.set_api_client(Arc::new(ApiClient::new(base_url)));

        // 1) SSE 経由で先に enqueue する。
        let (event_tx, event_rx) = mpsc::channel::<Event>();
        forward_sse_payloads(&pending_request_sse_payloads(), &event_tx, &context);
        let sse_events: Vec<_> = event_rx.try_iter().collect();
        assert_eq!(sse_events.len(), 2, "SSE must deliver both requests");
        assert!(
            sse_events.iter().any(|event| matches!(
                event,
                Event::Custom(CustomEvent::PermissionRequested { id, .. })
                    if id == "perm_resync_1"
            )),
            "SSE permission event must be delivered"
        );
        assert!(
            sse_events.iter().any(|event| matches!(
                event,
                Event::Custom(CustomEvent::QuestionAsked(request))
                    if request.id == "q_resync_1"
            )),
            "SSE question event must be delivered"
        );

        // 2) 同じ保留要求を resync が取得しても再 enqueue されない。
        let (custom_tx, custom_rx) = mpsc::channel::<CustomEvent>();
        resync_pending_requests_from_server(&context, &custom_tx);
        assert!(
            custom_rx.try_recv().is_err(),
            "resync must skip requests already enqueued via SSE"
        );

        // 共有集合に両方のキーが登録されていることを確認する。
        let seen = context.seen_request_ids.read();
        assert!(seen.contains("permission:perm_resync_1"));
        assert!(seen.contains("question:q_resync_1"));
    }

    #[test]
    fn resync_then_sse_events_do_not_enqueue_twice() {
        let base_url = spawn_pending_requests_api();
        let context = Arc::new(AppContext::new());
        context.set_api_client(Arc::new(ApiClient::new(base_url)));

        // 1) resync で先に enqueue する。
        let (custom_tx, custom_rx) = mpsc::channel::<CustomEvent>();
        resync_pending_requests_from_server(&context, &custom_tx);
        assert_eq!(
            custom_rx.try_iter().count(),
            2,
            "resync must deliver both requests"
        );

        // 2) 同じ ID の SSE イベントは二重 enqueue されない。
        let (event_tx, event_rx) = mpsc::channel::<Event>();
        forward_sse_payloads(&pending_request_sse_payloads(), &event_tx, &context);
        assert!(
            event_rx.try_recv().is_err(),
            "SSE must skip requests already enqueued via resync"
        );

        let seen = context.seen_request_ids.read();
        assert!(seen.contains("permission:perm_resync_1"));
        assert!(seen.contains("question:q_resync_1"));
    }

    /// 回答成功後に要求 ID が忘却され、同じ ID の新規イベントが再 enqueue されること。
    #[test]
    fn answered_permission_id_can_be_enqueued_again() {
        let (base_url, reply_hits, recorded) = spawn_permission_reply_api();
        let context = Arc::new(AppContext::new());
        context.set_api_client(Arc::new(ApiClient::new(base_url)));
        let mut app = headless_app(Arc::clone(&context));
        let permissions = [pending_request_sse_payloads()[0].clone()];

        // 1) SSE 経由で enqueue し、アプリへ届ける。
        let (event_tx, event_rx) = mpsc::channel::<Event>();
        forward_sse_payloads(&permissions, &event_tx, &context);
        for event in event_rx.try_iter() {
            app.handle_event(&event)
                .expect("permission event must be handled");
        }
        assert!(app.permission_prompt.current_request().is_some());
        assert!(
            context
                .seen_request_ids
                .read()
                .contains("permission:perm_resync_1"),
            "SSE enqueue must register the id"
        );

        // 2) 回答成功 → 要求が消え、ID も忘却される。
        app.apply_permission_action(PermissionAction::Approve);
        assert!(app.permission_prompt.is_empty());
        assert_eq!(reply_hits.load(Ordering::SeqCst), 1);
        assert!(recorded.lock().expect("record lock")[0]
            .0
            .contains("/permission/perm_resync_1/reply"));
        assert!(
            !context
                .seen_request_ids
                .read()
                .contains("permission:perm_resync_1"),
            "successful reply must forget the id"
        );

        // 3) 同じ ID の新規イベントが再 enqueue される。
        let (retry_tx, retry_rx) = mpsc::channel::<Event>();
        forward_sse_payloads(&permissions, &retry_tx, &context);
        assert!(matches!(
            retry_rx.try_recv(),
            Ok(Event::Custom(CustomEvent::PermissionRequested { id, .. }))
                if id == "perm_resync_1"
        ));
    }

    #[test]
    fn answered_question_id_can_be_enqueued_again() {
        let base_url = spawn_pending_requests_api();
        let context = Arc::new(AppContext::new());
        context.set_api_client(Arc::new(ApiClient::new(base_url)));
        let mut app = headless_app(Arc::clone(&context));
        let questions = [pending_request_sse_payloads()[1].clone()];

        // 1) SSE 経由で enqueue し、アプリへ届ける。
        let (event_tx, event_rx) = mpsc::channel::<Event>();
        forward_sse_payloads(&questions, &event_tx, &context);
        for event in event_rx.try_iter() {
            app.handle_event(&event)
                .expect("question event must be handled");
        }
        assert!(app.active_question_request.is_some());
        assert!(
            context
                .seen_request_ids
                .read()
                .contains("question:q_resync_1"),
            "SSE enqueue must register the id"
        );

        // 2) 回答成功 → 要求が消え、ID も忘却される。
        app.submit_question_answer(vec!["Yes".to_string()]);
        assert!(
            app.active_question_request.is_none(),
            "successful reply must take the request"
        );
        assert!(
            !context
                .seen_request_ids
                .read()
                .contains("question:q_resync_1"),
            "successful reply must forget the id"
        );

        // 3) 同じ ID の新規イベントが再 enqueue される。
        let (retry_tx, retry_rx) = mpsc::channel::<Event>();
        forward_sse_payloads(&questions, &retry_tx, &context);
        assert!(matches!(
            retry_rx.try_recv(),
            Ok(Event::Custom(CustomEvent::QuestionAsked(request)))
                if request.id == "q_resync_1"
        ));
    }

    #[test]
    fn failed_replies_keep_request_id_seen() {
        // API クライアント未設定 = 送信失敗。キーを残して再送可能にする。
        let context = Arc::new(AppContext::new());
        let mut app = headless_app(Arc::clone(&context));

        mark_request_seen(&app.context, "permission", "perm_failed_1");
        app.permission_prompt.add_request(PermissionRequest {
            id: "perm_failed_1".to_string(),
            permission_type: PermissionType::Bash,
            resource: "ls".to_string(),
            tool_name: "bash".to_string(),
        });
        app.apply_permission_action(PermissionAction::Approve);
        assert!(
            context
                .seen_request_ids
                .read()
                .contains("permission:perm_failed_1"),
            "failed permission reply must keep the id for retry"
        );

        let payloads = pending_request_sse_payloads();
        let request: QuestionRequestInfo =
            serde_json::from_value(payloads[1]["request"].clone()).expect("valid question payload");
        mark_request_seen(&app.context, "question", &request.id);
        app.enqueue_question_request(request);
        app.submit_question_answer(vec!["Yes".to_string()]);
        assert!(
            context
                .seen_request_ids
                .read()
                .contains("question:q_resync_1"),
            "failed question reply must keep the id for retry"
        );
    }

    #[test]
    fn failed_reject_keeps_question_id_seen() {
        // API クライアント未設定 = reject 送信失敗。キーを残して再試行可能にする（R4）。
        let context = Arc::new(AppContext::new());
        let mut app = headless_app(Arc::clone(&context));

        let payloads = pending_request_sse_payloads();
        let request: QuestionRequestInfo =
            serde_json::from_value(payloads[1]["request"].clone()).expect("valid question payload");
        let id = request.id.clone();
        mark_request_seen(&app.context, "question", &id);
        app.enqueue_question_request(request);
        app.reject_active_question();
        assert!(
            context
                .seen_request_ids
                .read()
                .contains(&format!("question:{id}")),
            "failed question reject must keep the id for retry"
        );
        assert!(
            app.active_question_request.is_some(),
            "failed reject must keep the request for retry"
        );
    }
}

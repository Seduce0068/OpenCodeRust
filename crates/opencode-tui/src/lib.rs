#![allow(
    clippy::empty_line_after_doc_comments,
    clippy::module_inception,
    clippy::too_many_arguments,
    clippy::unnecessary_sort_by,
    clippy::manual_clamp,
    clippy::nonminimal_bool,
    clippy::if_same_then_else
)]

//! TUI フロントエンド: アプリケーション本体、ルーティング、入力イベント、
//! 描画コンポーネント、テーマ、端末制御を提供するクレート。
//!
//! 起動エントリポイントは [`run_tui`]。各画面は [`router::Route`] で表され、
//! [`event::Event`] を [`app::App`] が消費して [`components`] を描画する。

/// HTTP API クライアントとセッション／メッセージの wire 型。
pub mod api;
/// TUI アプリケーション本体とその状態。
pub mod app;
/// アプリ名・バージョンなどのブランド定数。
pub mod branding;
/// スラッシュコマンド定義とファジー一致ロジック。
pub mod command;
/// ratatui の描画コンポーネント群。
pub mod components;
/// アプリ／セッション／キーバインドの共有コンテキスト。
pub mod context;
/// キー・マウス・リサイズ・ティックなどの入力イベント。
pub mod event;
/// ファイル名のインデックスとファジー検索。
pub mod file_index;
/// 状態更新と副作用を扱うフック（`use_state` / `use_effect`）。
pub mod hooks;
/// 画面ルート定義と遷移。
pub mod router;
/// 端末タイトルや raw mode 周りの制御。
pub mod terminal;
/// カラーテーマのプリセットと解決。
pub mod theme;
/// 枠線・レイアウト・テキスト・クリップボードなどの描画ユーティリティ。
pub mod ui;

/// HTTP API クライアント。
pub use api::ApiClient;
/// TUI アプリケーション本体。
pub use app::App;
/// 入力イベント。
pub use event::Event;
/// 画面ルートとルータ。
pub use router::{Route, Router};
/// 端末タイトルの設定・リセットユーティリティ。
pub use terminal::{reset_title, set_session_title, set_title};
/// カラーテーマ。
pub use theme::Theme;

/// パンック時に端末を通常モードへ戻すフックを登録する。
fn setup_panic_hook() {
    std::panic::set_hook(Box::new(|panic_info| {
        let _ = crossterm::terminal::disable_raw_mode();
        let _ = crossterm::execute!(
            std::io::stdout(),
            crossterm::terminal::LeaveAlternateScreen,
            crossterm::event::DisableMouseCapture,
        );
        eprintln!("\n\nPANIC: {}", panic_info);
    }));
}

/// TUI を起動し、ユーザーが終了するまでイベントループを実行する。
///
/// UI 関連の Config（`theme` / `tui.sidebar`）は起動経路ここでのみ cwd から
/// 読み込み、[`App::new_with_config`] へ渡して反映する。
/// 既定コンストラクタ（`App::new` / `AppContext::new`）は設定ファイルを
/// 読み込まないため、ライブラリ利用側に副作用を及ぼさない。
pub fn run_tui() -> anyhow::Result<()> {
    setup_panic_hook();

    let config = std::env::current_dir()
        .ok()
        .and_then(|cwd| opencode_config::load_config(&cwd).ok())
        .unwrap_or_else(|| {
            // パス・設定値・エラー詳細は出さない固定文言（R4）。
            tracing::warn!("failed to load config; using TUI defaults");
            opencode_config::Config::default()
        });
    let mut app = App::new_with_config(&config)?;
    app.run()
}

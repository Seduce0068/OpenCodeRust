#![allow(
    clippy::needless_return,
    clippy::needless_range_loop,
    clippy::unnecessary_sort_by,
    clippy::ptr_arg,
    clippy::redundant_closure,
    clippy::to_string_in_format_args,
    clippy::type_complexity,
    clippy::wildcard_in_or_patterns
)]

//! エージェントが利用するツール群を提供するクレート。
//!
//! 各ツールは `Tool` トレイトを実装し、`ToolRegistry` に登録される。
//! 外部ディレクトリ判定やファイルロックなど、権限・並行制御の共通ヘルパーも
//! 本クレートが提供する。

/// `apply_patch` 形式のパッチでファイルを変更するツール。
pub mod apply_patch;
/// シェルコマンドを実行する `bash` ツール。
pub mod bash;
/// 複数のツール呼び出しをまとめて実行する `batch` ツール。
pub mod batch;
/// Exa MCP 経由でコードを検索するツール。
pub mod codesearch;
/// 文字列置換によるファイル編集ツールと置換ストラテジー。
pub mod edit;
/// 外部ディレクトリへのアクセスを許可／拒否する判定ロジック。
pub mod external_directory;
/// glob パターンによるファイル検索ツール。
pub mod glob_tool;
/// 正規表現によるファイル内容検索ツール。
pub mod grep_tool;
/// 無効なツール呼び出しを表現する `invalid` ツール。
pub mod invalid;
/// ディレクトリ一覧を取得する `ls` ツール。
pub mod ls;
/// LSP 診断を取得するツール。
pub mod lsp_tool;
/// 同一ファイルへの複数編集をまとめる `multiedit` ツール。
pub mod multiedit;
/// プランモードの開始・終了を扱うツール。
pub mod plan;
/// ユーザーへ質問を投げかける `question` ツール。
pub mod question;
/// ファイルを読み取る `read` ツール。
pub mod read;
/// ツールの登録・検索を行うレジストリ（crate 内部）。
pub(crate) mod registry;
/// スキル定義の読み込みと一覧取得を扱うツール。
pub mod skill;
/// サブエージェント（タスク）の起動と再帰深さ制限を扱うツール。
pub mod task;
/// Todo の読み取り・書き込みツール。
pub mod todo;
/// `Tool` トレイトと共通の実行コンテキスト（crate 内部）。
pub(crate) mod tool;
/// 長大な出力の切り詰めと一時ファイルへの退避。
pub mod truncation;
/// URL をフェッチしてテキスト化する `webfetch` ツール。
pub mod webfetch;
/// Exa 経由で Web 検索を行う `websearch` ツール。
pub mod websearch;
/// ファイルへ書き込む `write` ツール。
pub mod write;

/// 外部ディレクトリ判定のオプションと種別。
pub use external_directory::{
    assert_external_directory, ExternalDirectoryKind, ExternalDirectoryOptions,
};
/// ツールレジストリの公開 API を再エクスポート。
pub use registry::*;
/// `Tool` トレイトと実行コンテキストを再エクスポート。
pub use tool::*;

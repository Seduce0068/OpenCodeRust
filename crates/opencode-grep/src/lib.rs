//! 正規表現とディレクトリ走査によるファイル検索を提供するクレート。
//!
//! `Ripgrep` をエントリポイントに、`regex` でパターン一致を行い `walkdir` で
//! ファイルを列挙する。`FileSearchOptions` で glob・隠しファイル・階層深さを指定し、
//! 結果は `MatchResult` / `SubMatch`（ハイライト範囲）として返す。
//! `Stats` は検索の統計情報（検索回数・バイト数・一致行数など）を表す。
//!
//! # 使い方
//!
//! `use opencode_grep::Ripgrep;` で import し、`Ripgrep::search(path, pattern)` や
//! `Ripgrep::files(path, options)` を呼び出して検索する。

pub mod search;

pub use search::{FileSearchOptions, MatchResult, Ripgrep, Stats, SubMatch};

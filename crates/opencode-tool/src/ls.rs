use async_trait::async_trait;
use glob::Pattern;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use walkdir::WalkDir;

use crate::{Metadata, PermissionRequest, Tool, ToolContext, ToolError, ToolResult};

const IGNORE_PATTERNS: &[&str] = &[
    "node_modules/",
    "__pycache__/",
    ".git/",
    "dist/",
    "build/",
    "target/",
    "vendor/",
    "bin/",
    "obj/",
    ".idea/",
    ".vscode/",
    ".zig-cache/",
    "zig-out",
    ".coverage",
    "coverage/",
    "vendor/",
    "tmp/",
    "temp/",
    ".cache/",
    "cache/",
    "logs/",
    ".venv/",
    "venv/",
    "env/",
];

const LIMIT: usize = 100;

pub struct LsTool {}

impl LsTool {
    pub fn new() -> Self {
        Self {}
    }
}

impl Default for LsTool {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, serde::Deserialize)]
struct LsInput {
    path: Option<String>,
    ignore: Option<Vec<String>>,
}

fn has_glob_meta(pattern: &str) -> bool {
    pattern
        .chars()
        .any(|ch| matches!(ch, '*' | '?' | '[' | ']' | '{' | '}'))
}

/// WalkDir のエントリ列を走査して相対ファイルパスを集める。
///
/// `Err` は内容を一切参照せず件数だけを数え、走査完了後に 1 回だけ固定イベントを
/// 警告する。エラー型を汎化しているのは、呼び出し側の公開 API を変えずにテストから
/// `Err` を注入できるようにするため（権限変更は環境依存のため）。
/// 警告には失敗件数以外（パス・ファイル名・エラー本文）を含めない。
fn collect_files_from_entries<E>(
    entries: impl Iterator<Item = Result<walkdir::DirEntry, E>>,
    base_dir: &Path,
    ignore_set: &HashSet<String>,
    ignore_globs: &[Pattern],
) -> Vec<String> {
    let mut files: Vec<String> = Vec::new();
    let mut walk_errors: usize = 0;

    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(_) => {
                // エラー本文・パスは見ずに件数だけ数える（ログにも結果にも出さない）。
                walk_errors += 1;
                continue;
            }
        };

        // follow_links(false) でも WalkDir はリンク自身をエントリとして返す。
        // path.is_file() / File::open はリンク先を辿るため、外部のファイル名を
        // 返さないようリンクを明示的に除外する。
        if entry.file_type().is_symlink() {
            continue;
        }

        let rel_path = entry.path().strip_prefix(base_dir).unwrap_or(entry.path());
        let rel_str = rel_path.to_string_lossy().replace('\\', "/");

        if rel_str.is_empty() {
            continue;
        }

        let should_skip = rel_str.split('/').any(|part| ignore_set.contains(part))
            || ignore_globs.iter().any(|glob| glob.matches(&rel_str));

        if should_skip {
            continue;
        }

        if entry.file_type().is_file() {
            files.push(rel_str);
            if files.len() >= LIMIT {
                break;
            }
        }
    }

    if walk_errors > 0 {
        tracing::warn!(
            event = "ls_walk_skipped_unreadable_entries",
            failed = walk_errors,
            "ls walk skipped unreadable entries"
        );
    }

    files
}

#[async_trait]
impl Tool for LsTool {
    fn id(&self) -> &str {
        "ls"
    }

    fn description(&self) -> &str {
        "Lists files and directories in a given path."
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "The absolute path to the directory to list (must be absolute, not relative)"
                },
                "ignore": {
                    "type": "array",
                    "items": {"type": "string"},
                    "description": "List of glob patterns to ignore"
                }
            },
            "required": []
        })
    }

    async fn execute(
        &self,
        args: serde_json::Value,
        ctx: ToolContext,
    ) -> Result<ToolResult, ToolError> {
        // Do not fall back to `LsInput { path: None, .. }` on a type error:
        // that silently rewrites e.g. `"path": 123` into `"."` and triggers an
        // unintended permission prompt. Reject malformed arguments instead.
        let input: LsInput = serde_json::from_value(args)
            .map_err(|e| ToolError::InvalidArguments(format!("Invalid ls arguments: {}", e)))?;

        let requested_path = input.path.unwrap_or_else(|| ".".to_string());
        let mut base_dir = if Path::new(&requested_path).is_absolute() {
            PathBuf::from(&requested_path)
        } else {
            PathBuf::from(&ctx.directory).join(&requested_path)
        };
        if let Ok(canonical) = base_dir.canonicalize() {
            base_dir = canonical;
        }
        let base_dir_str = base_dir.to_string_lossy().to_string();

        ctx.ask_permission(
            PermissionRequest::new("list")
                .with_pattern(&base_dir_str)
                .with_metadata("path", serde_json::json!(&base_dir_str))
                .always_allow(),
        )
        .await?;

        if !base_dir.exists() {
            return Err(ToolError::FileNotFound(base_dir.display().to_string()));
        }

        if !base_dir.is_dir() {
            return Err(ToolError::ExecutionError(format!(
                "{} is not a directory",
                base_dir.display()
            )));
        }

        let mut ignore_set: HashSet<String> = IGNORE_PATTERNS
            .iter()
            .map(|s| s.trim_end_matches('/').to_string())
            .collect();
        let mut ignore_globs: Vec<Pattern> = Vec::new();

        if let Some(custom_ignore) = input.ignore {
            for pattern in custom_ignore {
                // 走査側の rel_str と同じ区切り正規化を先に適用する。Windows 形式の
                // バックスラッシュ区切り指定（例: "dir\\"）をスラッシュ区切りへ
                // 変換することで、ignore_set / ignore_globs の両分岐が同じ比較
                // 基準を持つ。 POSIX パス（'\\' 不含）では文字列は不変。
                let replaced = pattern.replace('\\', "/");
                let normalized = replaced.trim_start_matches('!').trim();
                if normalized.is_empty() {
                    continue;
                }

                if has_glob_meta(normalized) {
                    if let Ok(glob) = Pattern::new(normalized) {
                        ignore_globs.push(glob);
                    }
                } else {
                    ignore_set.insert(normalized.trim_end_matches('/').to_string());
                }
            }
        }

        // 外部ディレクトリの権限チェックは走査ルート base_dir に対して走査前に
        // 一度だけ行われる。シンボリックリンクを辿ると、ディレクトリ内のリンクが
        // 外部を指す場合にそのチェックをすり抜けて外部のファイル名を返してしまう
        // ため、辿らない。follow_links(false) は symlink cycle も構造的に防ぐ。
        let files = collect_files_from_entries(
            WalkDir::new(&base_dir).follow_links(false).into_iter(),
            &base_dir,
            &ignore_set,
            &ignore_globs,
        );

        let mut dirs: HashSet<String> = HashSet::new();
        let mut files_by_dir: HashMap<String, Vec<String>> = HashMap::new();

        for file in &files {
            let dir = Path::new(file)
                .parent()
                .map(|p| p.to_string_lossy().to_string())
                .unwrap_or_else(|| ".".to_string());

            let parts: Vec<&str> = if dir == "." {
                vec![]
            } else {
                dir.split('/').collect()
            };

            for i in 0..=parts.len() {
                let dir_path = if i == 0 {
                    ".".to_string()
                } else {
                    parts[..i].join("/")
                };
                dirs.insert(dir_path);
            }

            let file_name = Path::new(file)
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_else(|| file.clone());

            files_by_dir.entry(dir).or_default().push(file_name);
        }

        fn render_dir(
            dir_path: &str,
            depth: usize,
            dirs: &HashSet<String>,
            files_by_dir: &HashMap<String, Vec<String>>,
        ) -> String {
            let indent = "  ".repeat(depth);
            let mut output = String::new();

            if depth > 0 {
                let name = Path::new(dir_path)
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_else(|| dir_path.to_string());
                output.push_str(&format!("{}{}/\n", indent, name));
            }

            let child_indent = "  ".repeat(depth + 1);

            let parent_prefix = if dir_path == "." {
                String::new()
            } else {
                format!("{}/", dir_path)
            };

            let mut children: Vec<String> = dirs
                .iter()
                .filter(|d| {
                    let d_str = d.as_str();
                    if d_str == dir_path {
                        return false;
                    }
                    if dir_path == "." {
                        !d_str.contains('/')
                    } else {
                        d_str.starts_with(&parent_prefix)
                            && d_str[parent_prefix.len()..].matches('/').count() == 0
                    }
                })
                .cloned()
                .collect();
            children.sort();

            for child in &children {
                output.push_str(&render_dir(child, depth + 1, dirs, files_by_dir));
            }

            let mut files = files_by_dir.get(dir_path).cloned().unwrap_or_default();
            files.sort();
            for file in files {
                output.push_str(&format!("{}{}\n", child_indent, file));
            }

            output
        }

        let output = format!(
            "{}/\n{}",
            base_dir.display(),
            render_dir(".", 0, &dirs, &files_by_dir)
        );

        let title = match base_dir.strip_prefix(Path::new(&ctx.worktree)) {
            Ok(rel) if rel.as_os_str().is_empty() => ".".to_string(),
            Ok(rel) => rel.to_string_lossy().to_string(),
            Err(_) => base_dir.display().to_string(),
        };

        Ok(ToolResult {
            title,
            output,
            metadata: {
                let mut m = Metadata::new();
                m.insert("count".into(), serde_json::json!(files.len()));
                m.insert("truncated".into(), serde_json::json!(files.len() >= LIMIT));
                m
            },
            truncated: files.len() >= LIMIT,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fmt::Write as _;

    fn temp_dir() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("opencode-ls-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn context(directory: &Path) -> ToolContext {
        ToolContext::new(
            "session".to_owned(),
            "message".to_owned(),
            directory.to_string_lossy().to_string(),
        )
    }

    #[test]
    fn has_glob_meta_detects_pattern_chars() {
        assert!(has_glob_meta("*.rs"));
        assert!(has_glob_meta("src/?.ts"));
        assert!(has_glob_meta("[ab].txt"));
        assert!(!has_glob_meta("node_modules"));
    }

    #[test]
    fn identity_and_schema() {
        let tool = LsTool::new();
        assert_eq!(tool.id(), "ls");
        assert_eq!(tool.parameters()["required"][0], serde_json::Value::Null);
    }

    #[tokio::test]
    async fn execute_lists_files_and_dirs() {
        let dir = temp_dir();
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::write(dir.join("README.md"), "readme").unwrap();
        std::fs::write(dir.join("src").join("main.rs"), "fn main() {}").unwrap();

        let result = LsTool::new()
            .execute(serde_json::json!({}), context(&dir))
            .await
            .unwrap();

        assert_eq!(result.metadata["count"], 2);
        assert!(result.output.contains("README.md"));
        assert!(result.output.contains("main.rs"));
        assert!(result.output.contains("src/"));
        assert!(!result.truncated);

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[tokio::test]
    async fn execute_respects_custom_ignore() {
        let dir = temp_dir();
        std::fs::write(dir.join("keep.rs"), "keep").unwrap();
        std::fs::write(dir.join("skip.log"), "skip").unwrap();

        let result = LsTool::new()
            .execute(serde_json::json!({ "ignore": ["*.log"] }), context(&dir))
            .await
            .unwrap();

        assert_eq!(result.metadata["count"], 1);
        assert!(result.output.contains("keep.rs"));
        assert!(!result.output.contains("skip.log"));

        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// バックスラッシュ区切りの ignore 指定（例: "dir\\"）がスラッシュ区切りと
    /// 同じくディレクトリを除外できることを検証する（NORM-05 回帰テスト）。
    #[tokio::test]
    async fn execute_ignore_backslash_separator_excludes_dir() {
        let dir = temp_dir();
        std::fs::create_dir_all(dir.join("dir")).unwrap();
        std::fs::write(dir.join("dir").join("a.txt"), "nested").unwrap();
        std::fs::write(dir.join("keep.txt"), "keep").unwrap();

        let result = LsTool::new()
            .execute(serde_json::json!({ "ignore": ["dir\\"] }), context(&dir))
            .await
            .unwrap();

        assert_eq!(result.metadata["count"], 1);
        assert!(result.output.contains("keep.txt"));
        assert!(!result.output.contains("a.txt"));

        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// 既存のスラッシュ区切り指定（例: "dir/"）の挙動が不変であることを検証する。
    #[tokio::test]
    async fn execute_ignore_forward_slash_separator_unchanged() {
        let dir = temp_dir();
        std::fs::create_dir_all(dir.join("dir")).unwrap();
        std::fs::write(dir.join("dir").join("a.txt"), "nested").unwrap();
        std::fs::write(dir.join("keep.txt"), "keep").unwrap();

        let result = LsTool::new()
            .execute(serde_json::json!({ "ignore": ["dir/"] }), context(&dir))
            .await
            .unwrap();

        assert_eq!(result.metadata["count"], 1);
        assert!(result.output.contains("keep.txt"));
        assert!(!result.output.contains("a.txt"));

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[tokio::test]
    async fn execute_rejects_missing_directory() {
        let dir = temp_dir();
        let missing = dir.join("missing");
        let error = LsTool::new()
            .execute(
                serde_json::json!({ "path": missing.to_string_lossy() }),
                context(&dir),
            )
            .await
            .unwrap_err();

        assert!(matches!(error, ToolError::FileNotFound(_)));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[tokio::test]
    async fn execute_rejects_file_path() {
        let dir = temp_dir();
        let file = dir.join("a.txt");
        std::fs::write(&file, "x").unwrap();

        let error = LsTool::new()
            .execute(
                serde_json::json!({ "path": file.to_string_lossy() }),
                context(&dir),
            )
            .await
            .unwrap_err();

        assert!(matches!(error, ToolError::ExecutionError(_)));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[tokio::test]
    async fn execute_rejects_non_string_path() {
        let dir = temp_dir();
        let error = LsTool::new()
            .execute(serde_json::json!({ "path": 123 }), context(&dir))
            .await
            .unwrap_err();

        assert!(matches!(error, ToolError::InvalidArguments(_)));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[tokio::test]
    async fn execute_rejects_non_array_ignore() {
        let dir = temp_dir();
        let error = LsTool::new()
            .execute(serde_json::json!({ "ignore": "*.log" }), context(&dir))
            .await
            .unwrap_err();

        assert!(matches!(error, ToolError::InvalidArguments(_)));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// 一覧対象ディレクトリ内の symlink が外部のファイル／ディレクトリを指して
    /// も、その外部のファイル名を返してはならない（権限すり抜け回帰テスト）。
    #[cfg(unix)]
    #[tokio::test]
    async fn execute_does_not_follow_external_symlinks() {
        use std::os::unix::fs::symlink;

        let dir = temp_dir();
        let outside =
            std::env::temp_dir().join(format!("opencode-ls-outside-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("secret.txt"), "TOP_SECRET").unwrap();
        std::fs::write(dir.join("normal.txt"), "normal").unwrap();

        symlink(&outside, dir.join("outside_link")).unwrap();
        symlink(outside.join("secret.txt"), dir.join("secret_link.txt")).unwrap();

        let result = LsTool::new()
            .execute(serde_json::json!({}), context(&dir))
            .await
            .unwrap();

        // 通常ファイルだけが返り、外部を指す symlink の名前は漏れない。
        assert_eq!(result.metadata["count"], 1);
        assert!(result.output.contains("normal.txt"));
        assert!(!result.output.contains("outside_link"));
        assert!(!result.output.contains("secret_link.txt"));

        std::fs::remove_dir_all(&dir).unwrap();
        std::fs::remove_dir_all(&outside).unwrap();
    }

    // 警告だけを捕捉する最小サブスクライバ（tracing-subscriber を依存に追加しない）。
    #[derive(Clone, Default)]
    struct WarnBuffer(std::sync::Arc<std::sync::Mutex<Vec<String>>>);

    impl WarnBuffer {
        fn text(&self) -> String {
            self.0
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .join("\n")
        }

        fn len(&self) -> usize {
            self.0
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .len()
        }
    }

    struct WarnVisitor(String);

    impl tracing::field::Visit for WarnVisitor {
        fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
            let _ = writeln!(self.0, "{}={:?}", field.name(), value);
        }

        fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
            let _ = writeln!(self.0, "{}={}", field.name(), value);
        }
    }

    struct WarnSubscriber(WarnBuffer);

    impl tracing::Subscriber for WarnSubscriber {
        fn enabled(&self, metadata: &tracing::Metadata<'_>) -> bool {
            *metadata.level() <= tracing::Level::WARN
        }

        fn new_span(&self, _: &tracing::span::Attributes<'_>) -> tracing::span::Id {
            tracing::span::Id::from_u64(1)
        }

        fn record(&self, _: &tracing::span::Id, _: &tracing::span::Record<'_>) {}

        fn record_follows_from(&self, _: &tracing::span::Id, _: &tracing::span::Id) {}

        fn event(&self, event: &tracing::Event<'_>) {
            let mut visitor = WarnVisitor(String::new());
            event.record(&mut visitor);
            self.0
                 .0
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .push(visitor.0);
        }

        fn enter(&self, _: &tracing::span::Id) {}

        fn exit(&self, _: &tracing::span::Id) {}
    }

    /// 走査対象のエントリ列に、本文を持つ Err を注入して返す（権限変更を使わない）。
    /// 順序は「Err を先頭に 1 件 + 実エントリ + Err を末尾に 1 件」で、途中失敗でも
    /// 正常系の収集が続くことを確認できるようにしている。
    /// 本文は意図的に参照しない（参照しないことをテストで検証する）。
    #[allow(dead_code)]
    struct SentinelError(&'static str);

    fn entries_with_injected_errors(dir: &Path) -> Vec<Result<walkdir::DirEntry, SentinelError>> {
        let mut entries: Vec<Result<walkdir::DirEntry, SentinelError>> = Vec::new();
        entries.push(Err(SentinelError("SENTINEL_ERROR_BODY_NEVER_LOG")));
        entries.extend(
            WalkDir::new(dir)
                .follow_links(false)
                .into_iter()
                .map(|entry| entry.map_err(|_| SentinelError("SENTINEL_WALK_ERR"))),
        );
        entries.push(Err(SentinelError("SENTINEL_ERROR_BODY_NEVER_LOG")));
        entries
    }

    /// WalkDir の Err が 1 件以上ある場合、走査完了後に固定イベントが 1 回だけ出て
    /// 失敗件数が伝わること。パス・ファイル名・エラー本文は一切出ないこと。
    #[test]
    fn walk_errors_emit_single_fixed_warn_with_count_only() {
        let dir = temp_dir();
        std::fs::write(dir.join("visible.txt"), "ok").unwrap();
        let ignore_set: HashSet<String> = HashSet::new();
        let ignore_globs: Vec<Pattern> = Vec::new();

        let logs = WarnBuffer::default();
        let files = tracing::subscriber::with_default(WarnSubscriber(logs.clone()), || {
            collect_files_from_entries(
                entries_with_injected_errors(&dir).into_iter(),
                &dir,
                &ignore_set,
                &ignore_globs,
            )
        });

        // 正常系ロジックは不変：Err を除いたファイルだけが結果に残る。
        assert_eq!(files, vec!["visible.txt".to_string()]);
        let text = logs.text();
        assert_eq!(logs.len(), 1, "warn は 1 回だけ: {text}");
        assert!(
            text.contains("event=ls_walk_skipped_unreadable_entries"),
            "{text}"
        );
        assert!(text.contains("failed=2"), "{text}");
        // パス・ファイル名・エラー本文は warn に漏れない。
        assert!(!text.contains(&dir.to_string_lossy().to_string()), "{text}");
        assert!(!text.contains("visible.txt"), "{text}");
        assert!(!text.contains("SENTINEL_ERROR_BODY_NEVER_LOG"), "{text}");
        assert!(!text.contains("SENTINEL_WALK_ERR"), "{text}");
        // 結果一覧にもエラー本文は含まれない。
        assert!(!files.iter().any(|f| f.contains("SENTINEL")), "{files:?}");

        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// エントリ失敗が無い通常ツリーでは warn が出ないこと。
    #[test]
    fn clean_walk_emits_no_warn() {
        let dir = temp_dir();
        std::fs::write(dir.join("a.txt"), "a").unwrap();
        let ignore_set: HashSet<String> = HashSet::new();
        let ignore_globs: Vec<Pattern> = Vec::new();

        let logs = WarnBuffer::default();
        let files = tracing::subscriber::with_default(WarnSubscriber(logs.clone()), || {
            collect_files_from_entries(
                WalkDir::new(&dir).follow_links(false).into_iter(),
                &dir,
                &ignore_set,
                &ignore_globs,
            )
        });

        assert_eq!(files, vec!["a.txt".to_string()]);
        assert_eq!(logs.len(), 0, "正常系では warn が出ない: {}", logs.text());

        std::fs::remove_dir_all(&dir).unwrap();
    }
}

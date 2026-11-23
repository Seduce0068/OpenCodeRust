use async_trait::async_trait;
use std::path::{Path, PathBuf};
use tokio::fs;

use super::replacers::{normalize_line_endings, CompositeReplacer};
use crate::tool::parse_bool_arg;
use crate::{with_file_lock, Metadata, Tool, ToolContext, ToolError, ToolResult};

#[cfg(feature = "lsp")]
use crate::MAX_DIAGNOSTICS_PER_FILE;

/// 第1フェーズのロック内で計算し、権限確認を跨いで第2フェーズへ渡す値。
/// パスやトークン値は含まない（エラー・ログは固定文言のみ）。
struct PreparedEdit {
    existed: bool,
    diff: String,
    kind: PreparedEditKind,
}

enum PreparedEditKind {
    /// `old_string` が空: 新規内容で上書きする経路。
    Create { new_content: String },
    /// 通常の置換経路。
    Replace {
        new_content: String,
        replacements: usize,
    },
}

pub struct EditTool {
    directory: PathBuf,
}

impl EditTool {
    pub fn new() -> Self {
        Self {
            directory: std::env::current_dir().unwrap_or_default(),
        }
    }
}

impl Default for EditTool {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Tool for EditTool {
    fn id(&self) -> &str {
        "edit"
    }

    fn description(&self) -> &str {
        "Performs string replacements in a file with multiple matching strategies. Use this to make precise edits."
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "file_path": {
                    "type": "string",
                    "description": "The absolute path to the file to edit"
                },
                "old_string": {
                    "type": "string",
                    "description": "The text to replace"
                },
                "new_string": {
                    "type": "string",
                    "description": "The text to replace it with (must be different from old_string)"
                },
                "replace_all": {
                    "type": "boolean",
                    "description": "Replace all occurrences of old_string (default false)"
                }
            },
            "required": ["file_path", "old_string", "new_string"]
        })
    }

    async fn execute(
        &self,
        args: serde_json::Value,
        ctx: ToolContext,
    ) -> Result<ToolResult, ToolError> {
        let file_path: String = args["file_path"]
            .as_str()
            .ok_or_else(|| ToolError::InvalidArguments("file_path is required".into()))?
            .to_string();

        let old_string: String = args["old_string"]
            .as_str()
            .ok_or_else(|| ToolError::InvalidArguments("old_string is required".into()))?
            .to_string();

        let new_string: String = args["new_string"]
            .as_str()
            .ok_or_else(|| ToolError::InvalidArguments("new_string is required".into()))?
            .to_string();

        let replace_all = parse_bool_arg(&args, "replace_all", false)?;

        let base_dir = if ctx.directory.is_empty() {
            &self.directory
        } else {
            Path::new(&ctx.directory)
        };

        let path = if Path::new(&file_path).is_absolute() {
            PathBuf::from(&file_path)
        } else {
            base_dir.join(&file_path)
        };

        let path_str = path.to_string_lossy().to_string();

        if ctx.is_external_path(&path_str) {
            let parent = path
                .parent()
                .map(|p| p.to_string_lossy().to_string())
                .unwrap_or_else(|| path_str.clone());

            ctx.ask_permission(
                crate::PermissionRequest::new("external_directory")
                    .with_pattern(format!("{}/*", parent))
                    .with_metadata("filepath", serde_json::json!(&path_str))
                    .with_metadata("parentDir", serde_json::json!(parent)),
            )
            .await?;
        }

        let title = path
            .strip_prefix(&ctx.worktree)
            .unwrap_or(&path)
            .to_string_lossy()
            .to_string();

        // ---- 第1フェーズ: ロック内で存在確認〜diff 計算まで行い値を取り出す ----
        // IOORDER-01-R4: ask_permission はロック外で行うため、ここでは権限確認に
        // 必要な値（existed / diff）と書込内容だけを PreparedEdit として返す。
        let ctx_clone = ctx.clone();
        let path_clone = path.clone();
        let path_str_clone = path_str.clone();
        let old_string_clone = old_string.clone();
        let new_string_clone = new_string.clone();

        let prepared = with_file_lock(&path_str, || async move {
            // 既存判定は内容の空・非空ではなく、実際にパスが存在するかで行う。
            // 既存の空ファイルを新規作成と誤判定すると file_time_assert（stale 検出）
            // がスキップされ、並行編集による上書きを防げなくなる（write.rs の M-58 と同種）。
            // メタデータ取得が失敗した場合は「新規」へ丸めずエラーを伝播させる。
            // 権限不足等で既存ファイルを新規扱いすると stale 検出をすり抜けるため。
            let existed = fs::try_exists(&path_clone).await.map_err(|e| {
                ToolError::execution_with_source(
                    format!("Failed to check file existence: {}", e),
                    e,
                )
            })?;

            // 新規ファイル（パス未存在）は読取りに失敗するため空文字列として扱い、
            // 既存の「old_string が空なら新規作成」経路を従来どおり通す。
            // 既存ファイルの読取りエラーは従来どおり伝播させる。
            let content = if existed {
                fs::read_to_string(&path_clone).await.map_err(|e| {
                    ToolError::execution_with_source(format!("Failed to read file: {}", e), e)
                })?
            } else {
                String::new()
            };

            let content = normalize_line_endings(&content);

            if existed {
                ctx_clone
                    .do_file_time_assert(path_str_clone.clone())
                    .await?;
            }

            let new_string_normalized = normalize_line_endings(&new_string_clone);

            if old_string_clone.is_empty() {
                let diff = create_diff(&path_str_clone, "", &new_string_normalized);
                Ok::<_, ToolError>(PreparedEdit {
                    existed,
                    diff,
                    kind: PreparedEditKind::Create {
                        new_content: new_string_normalized,
                    },
                })
            } else {
                let replacer = CompositeReplacer::new();
                let old_string_normalized = normalize_line_endings(&old_string_clone);
                let new_content = replacer
                    .replace(
                        &content,
                        &old_string_normalized,
                        &new_string_normalized,
                        replace_all,
                    )
                    .map_err(ToolError::ExecutionError)?;

                let diff = create_diff(&path_str_clone, &content, &new_content);
                let replacements = if replace_all {
                    content.matches(&old_string_normalized).count()
                } else {
                    1
                };

                Ok::<_, ToolError>(PreparedEdit {
                    existed,
                    diff,
                    kind: PreparedEditKind::Replace {
                        new_content,
                        replacements,
                    },
                })
            }
        })
        .await?;

        // ---- 権限確認: ロック外で行う ----
        // 2 つの old_string.is_empty() 分岐は、どちらも同じくこの地点で権限確認し、
        // 第2フェーズで同じ構造（stale 再検証 → 書込 → 通知）へ合流する。
        ctx.ask_permission(
            crate::PermissionRequest::new("edit")
                .with_pattern(&path_str)
                .with_metadata("diff", serde_json::json!(prepared.diff))
                .always_allow(),
        )
        .await?;

        // ---- 第2フェーズ: 再度ロックを取り、stale 再検証後に書込む ----
        let ctx_clone = ctx.clone();
        let path_clone = path.clone();
        let path_str_clone = path_str.clone();
        let title_clone = title.clone();
        let existed = prepared.existed;
        let diff = prepared.diff;
        let kind = prepared.kind;

        with_file_lock(&path_str, || async move {
            // 権限確認中に他操作がファイルを変更していた場合は、ここで固定文言の
            // エラー（本番の file_time_assert: "File changed since read"）を返す。
            if existed {
                ctx_clone
                    .do_file_time_assert(path_str_clone.clone())
                    .await?;
            } else {
                // 新規作成だったのに権限待ち中に他操作が作成した場合は上書きしない（R4）。
                let now_exists = fs::try_exists(&path_clone).await.map_err(|e| {
                    ToolError::execution_with_source(
                        format!("Failed to check file existence: {}", e),
                        e,
                    )
                })?;
                if now_exists {
                    return Err(ToolError::ExecutionError(
                        "File changed since read".to_string(),
                    ));
                }
            }

            match kind {
                PreparedEditKind::Create { new_content } => {
                    fs::write(&path_clone, &new_content).await.map_err(|e| {
                        ToolError::execution_with_source(format!("Failed to write file: {}", e), e)
                    })?;

                    ctx_clone
                        .do_publish_bus(
                            "file.edited",
                            serde_json::json!({
                                "file": path_str_clone.clone()
                            }),
                        )
                        .await;

                    ctx_clone
                        .do_publish_bus(
                            "file_watcher.updated",
                            serde_json::json!({
                                "file": path_str_clone.clone(),
                                "event": if existed { "change" } else { "add" }
                            }),
                        )
                        .await;

                    ctx_clone
                        .do_lsp_touch_file(path_str_clone.clone(), true)
                        .await?;
                    ctx_clone.do_file_time_read(path_str_clone.clone()).await?;

                    let output = format!("Created new file content at {}", path_clone.display());
                    let (lsp_output, lsp_diagnostics) =
                        get_lsp_diagnostics_with_meta(&path_clone, &ctx_clone).await;
                    let final_output = if !lsp_output.is_empty() {
                        format!("{}\n\n{}", output, lsp_output)
                    } else {
                        output
                    };

                    Ok(ToolResult {
                        title: title_clone,
                        output: final_output,
                        metadata: {
                            let mut m = Metadata::new();
                            m.insert("filepath".into(), serde_json::json!(path_str_clone));
                            if !lsp_diagnostics.is_empty() {
                                m.insert("diagnostics".into(), serde_json::json!(lsp_diagnostics));
                            }
                            m
                        },
                        truncated: false,
                    })
                }
                PreparedEditKind::Replace {
                    new_content,
                    replacements,
                } => {
                    fs::write(&path_clone, &new_content).await.map_err(|e| {
                        ToolError::execution_with_source(format!("Failed to write file: {}", e), e)
                    })?;

                    ctx_clone
                        .do_publish_bus(
                            "file.edited",
                            serde_json::json!({
                                "file": path_str_clone.clone()
                            }),
                        )
                        .await;

                    ctx_clone
                        .do_publish_bus(
                            "file_watcher.updated",
                            serde_json::json!({
                                "file": path_str_clone.clone(),
                                "event": if existed { "change" } else { "add" }
                            }),
                        )
                        .await;

                    ctx_clone
                        .do_lsp_touch_file(path_str_clone.clone(), true)
                        .await?;
                    ctx_clone.do_file_time_read(path_str_clone.clone()).await?;

                    let base_output = format!(
                        "Successfully edited {} ({} replacement{})",
                        path_clone.display(),
                        replacements,
                        if replacements != 1 { "s" } else { "" }
                    );

                    let (lsp_output, lsp_diagnostics) =
                        get_lsp_diagnostics_with_meta(&path_clone, &ctx_clone).await;
                    let final_output = if lsp_output.is_empty() {
                        base_output
                    } else {
                        format!("{}\n\n{}", base_output, lsp_output)
                    };

                    Ok(ToolResult {
                        title: title_clone,
                        output: final_output,
                        metadata: {
                            let mut m = Metadata::new();
                            m.insert("replacements".into(), serde_json::json!(replacements));
                            m.insert("filepath".into(), serde_json::json!(path_str_clone));
                            m.insert("diff".into(), serde_json::json!(diff));
                            if !lsp_diagnostics.is_empty() {
                                m.insert("diagnostics".into(), serde_json::json!(lsp_diagnostics));
                            }
                            m
                        },
                        truncated: false,
                    })
                }
            }
        })
        .await
    }
}

async fn get_lsp_diagnostics_with_meta(
    path: &Path,
    ctx: &ToolContext,
) -> (String, Vec<serde_json::Value>) {
    #[cfg(feature = "lsp")]
    {
        if let Some(lsp_registry) = &ctx.lsp_registry {
            return get_lsp_diagnostics_impl_with_meta(path, lsp_registry.clone()).await;
        }
    }

    #[cfg(not(feature = "lsp"))]
    {
        let _ = (path, ctx);
    }

    (String::new(), Vec::new())
}

#[cfg(feature = "lsp")]
async fn get_lsp_diagnostics_impl_with_meta(
    path: &Path,
    lsp_registry: std::sync::Arc<opencode_lsp::LspClientRegistry>,
) -> (String, Vec<serde_json::Value>) {
    use opencode_lsp::detect_language;

    let language = detect_language(path);
    let clients = lsp_registry.list().await;

    let client = clients
        .iter()
        .find(|(id, _)| id.contains(language))
        .map(|(_, c)| c.clone());

    match client {
        Some(client) => {
            if let Ok(content) = tokio::fs::read_to_string(path).await {
                let _ = client.open_document(path, &content, language).await;

                tokio::time::sleep(std::time::Duration::from_millis(100)).await;

                let diagnostics = client.get_diagnostics(path).await;
                let errors: Vec<_> = diagnostics
                    .iter()
                    .filter(|d| d.severity == Some(lsp_types::DiagnosticSeverity::ERROR))
                    .collect();

                if errors.is_empty() {
                    return (String::new(), Vec::new());
                }

                let total_errors = errors.len();
                let limited: Vec<_> = errors.into_iter().take(MAX_DIAGNOSTICS_PER_FILE).collect();
                let suffix = if limited.len() < total_errors {
                    format!("\n... and {} more", total_errors - limited.len())
                } else {
                    String::new()
                };

                let error_lines: Vec<String> = limited
                    .iter()
                    .map(|d| {
                        let line = d.range.start.line + 1;
                        let msg = &d.message;
                        format!("  Line {}: {}", line, msg)
                    })
                    .collect();

                let diagnostics_meta: Vec<serde_json::Value> = limited.iter()
                    .map(|d| {
                        serde_json::json!({
                            "line": d.range.start.line + 1,
                            "message": d.message,
                            "severity": d.severity.as_ref().map(|s| format!("{:?}", s)).unwrap_or_else(|| "Unknown".to_string())
                        })
                    })
                    .collect();

                let output = format!(
                    "LSP errors detected in this file, please fix:\n<diagnostics file=\"{}\">\n{}{}\n</diagnostics>",
                    path.display(),
                    error_lines.join("\n"),
                    suffix
                );

                (output, diagnostics_meta)
            } else {
                (String::new(), Vec::new())
            }
        }
        None => (String::new(), Vec::new()),
    }
}

fn create_diff(filepath: &str, old_content: &str, new_content: &str) -> String {
    let old_lines: Vec<&str> = old_content.lines().collect();
    let new_lines: Vec<&str> = new_content.lines().collect();

    let mut diff = format!("--- {}\n+++ {}\n", filepath, filepath);

    let mut old_idx = 0;
    let mut new_idx = 0;

    while old_idx < old_lines.len() || new_idx < new_lines.len() {
        if old_idx >= old_lines.len() {
            diff.push_str(&format!("+{}\n", new_lines[new_idx]));
            new_idx += 1;
        } else if new_idx >= new_lines.len() {
            diff.push_str(&format!("-{}\n", old_lines[old_idx]));
            old_idx += 1;
        } else if old_lines[old_idx] == new_lines[new_idx] {
            old_idx += 1;
            new_idx += 1;
        } else {
            diff.push_str(&format!("-{}\n", old_lines[old_idx]));
            diff.push_str(&format!("+{}\n", new_lines[new_idx]));
            old_idx += 1;
            new_idx += 1;
        }
    }

    diff
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};

    #[test]
    fn identity_and_schema() {
        let tool = EditTool::default();
        assert_eq!(tool.id(), "edit");
        assert_eq!(tool.parameters()["required"][0], "file_path");
    }

    #[test]
    fn normalize_line_endings_replaces_crlf() {
        assert_eq!(normalize_line_endings("a\r\nb\r\nc"), "a\nb\nc");
        assert_eq!(normalize_line_endings("a\nb"), "a\nb");
    }

    #[test]
    fn normalize_line_endings_replaces_lone_cr() {
        // 回帰: CR 単独の改行も LF へ正規化しないと、行分割が 1 行扱いになり
        // LF で渡した old_string が一致しなくなる (NORM-07)。
        assert_eq!(normalize_line_endings("a\rb"), "a\nb");
        // CRLF / LF の既存結果は不変。
        assert_eq!(normalize_line_endings("a\r\nb"), "a\nb");
        assert_eq!(normalize_line_endings("a\nb"), "a\nb");
    }

    #[test]
    fn create_diff_identical_content() {
        let diff = create_diff("a.rs", "a\nb", "a\nb");
        assert_eq!(diff, "--- a.rs\n+++ a.rs\n");
    }

    #[test]
    fn create_diff_replacement() {
        let diff = create_diff("a.rs", "old\nkeep", "new\nkeep");
        assert!(diff.contains("-old\n"));
        assert!(diff.contains("+new\n"));
        assert!(!diff.contains("-keep"));
    }

    #[test]
    fn create_diff_added_lines() {
        let diff = create_diff("a.rs", "a", "a\nb");
        assert_eq!(diff, "--- a.rs\n+++ a.rs\n+b\n");
    }

    #[test]
    fn create_diff_removed_lines() {
        let diff = create_diff("a.rs", "a\nb", "a");
        assert!(!diff.contains("-a\n"));
        assert!(diff.contains("-b\n"));
    }

    fn temp_dir() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("opencode-edit-test-{}", uuid::Uuid::new_v4()));
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

    /// file_time_assert の呼び出し回数と publish_bus のイベントを記録するコンテキスト。
    fn observing_context(
        directory: &Path,
        assert_calls: Arc<AtomicUsize>,
        events: Arc<Mutex<Vec<(String, serde_json::Value)>>>,
    ) -> ToolContext {
        context(directory)
            .with_file_time_assert(move |_session_id, _file_path| {
                let assert_calls = assert_calls.clone();
                async move {
                    assert_calls.fetch_add(1, Ordering::SeqCst);
                    Ok::<(), ToolError>(())
                }
            })
            .with_publish_bus(move |event_type, properties| {
                let events = events.clone();
                async move {
                    events.lock().unwrap().push((event_type, properties));
                }
            })
    }

    fn watcher_event(events: &[(String, serde_json::Value)]) -> &serde_json::Value {
        &events
            .iter()
            .find(|(event_type, _)| event_type == "file_watcher.updated")
            .expect("file_watcher.updated が発行されること")
            .1
    }

    // 既存判定を内容の空・非空ではなくパスの存在で行うことの回帰テスト（M-67）。

    #[tokio::test]
    async fn execute_existing_empty_file_asserts_file_time_and_reports_change() {
        let dir = temp_dir();
        let file = dir.join("empty.txt");
        // 既存だが内容が空のファイル（旧実装では新規作成と誤判定されていた）
        std::fs::write(&file, "").unwrap();

        let assert_calls = Arc::new(AtomicUsize::new(0));
        let events = Arc::new(Mutex::new(Vec::new()));
        let ctx = observing_context(&dir, assert_calls.clone(), events.clone());

        EditTool::new()
            .execute(
                serde_json::json!({
                    "file_path": file.to_string_lossy(),
                    "old_string": "",
                    "new_string": "new content"
                }),
                ctx,
            )
            .await
            .unwrap();

        assert_eq!(
            assert_calls.load(Ordering::SeqCst),
            2,
            "既存の空ファイルでは第1フェーズと第2フェーズの両方で file_time_assert が呼ばれること"
        );
        {
            let events = events.lock().unwrap();
            assert_eq!(watcher_event(&events)["event"], "change");
        }
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "new content");

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[tokio::test]
    async fn execute_new_file_skips_file_time_assert_and_reports_add() {
        let dir = temp_dir();
        let file = dir.join("new.txt"); // まだ存在しない

        let assert_calls = Arc::new(AtomicUsize::new(0));
        let events = Arc::new(Mutex::new(Vec::new()));
        let ctx = observing_context(&dir, assert_calls.clone(), events.clone());

        EditTool::new()
            .execute(
                serde_json::json!({
                    "file_path": file.to_string_lossy(),
                    "old_string": "",
                    "new_string": "created"
                }),
                ctx,
            )
            .await
            .unwrap();

        assert_eq!(
            assert_calls.load(Ordering::SeqCst),
            0,
            "新規ファイルでは file_time_assert をスキップすること"
        );
        {
            let events = events.lock().unwrap();
            assert_eq!(watcher_event(&events)["event"], "add");
        }
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "created");

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[tokio::test]
    async fn execute_existing_nonempty_file_asserts_file_time_and_reports_change() {
        let dir = temp_dir();
        let file = dir.join("nonempty.txt");
        std::fs::write(&file, "old").unwrap();

        let assert_calls = Arc::new(AtomicUsize::new(0));
        let events = Arc::new(Mutex::new(Vec::new()));
        let ctx = observing_context(&dir, assert_calls.clone(), events.clone());

        EditTool::new()
            .execute(
                serde_json::json!({
                    "file_path": file.to_string_lossy(),
                    "old_string": "old",
                    "new_string": "new"
                }),
                ctx,
            )
            .await
            .unwrap();

        assert_eq!(
            assert_calls.load(Ordering::SeqCst),
            2,
            "既存の非空ファイルでは第1フェーズと第2フェーズの両方で file_time_assert が呼ばれること"
        );
        {
            let events = events.lock().unwrap();
            assert_eq!(watcher_event(&events)["event"], "change");
        }
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "new");

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[tokio::test]
    async fn execute_propagates_existence_check_error() {
        let dir = temp_dir();
        // NUL を含むパスはメタデータ取得が InvalidInput で失敗するため、
        // 存在確認エラーが「新規ファイル」へ丸められないことを固定できる。
        let invalid_path = dir.join("bad\0name.txt");

        let assert_calls = Arc::new(AtomicUsize::new(0));
        let events = Arc::new(Mutex::new(Vec::new()));
        let ctx = observing_context(&dir, assert_calls.clone(), events.clone());

        let error = EditTool::new()
            .execute(
                serde_json::json!({
                    "file_path": invalid_path.to_string_lossy(),
                    "old_string": "",
                    "new_string": "created"
                }),
                ctx,
            )
            .await
            .unwrap_err();

        assert!(
            matches!(&error, ToolError::ExecutionWithSource { .. }),
            "存在確認のエラーは新規扱いへ丸めず伝播すること（{}）",
            error
        );
        assert_eq!(assert_calls.load(Ordering::SeqCst), 0);
        assert!(events.lock().unwrap().is_empty());

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn parse_bool_arg_keeps_omitted_and_null_defaults() {
        let omitted = serde_json::json!({});
        assert!(!parse_bool_arg(&omitted, "replace_all", false).unwrap());

        let nulls = serde_json::json!({ "replace_all": null });
        assert!(!parse_bool_arg(&nulls, "replace_all", false).unwrap());

        let explicit = serde_json::json!({ "replace_all": true });
        assert!(parse_bool_arg(&explicit, "replace_all", false).unwrap());
    }

    #[test]
    fn parse_bool_arg_rejects_invalid_types() {
        for bad in [
            serde_json::json!("yes"),
            serde_json::json!(1),
            serde_json::json!([]),
            serde_json::json!({}),
        ] {
            let args = serde_json::json!({ "replace_all": bad });
            assert!(
                matches!(
                    parse_bool_arg(&args, "replace_all", false),
                    Err(ToolError::InvalidArguments(_))
                ),
                "unexpectedly accepted {bad}"
            );
        }
    }

    /// 不正型の `replace_all` は丸められず InvalidArguments になること。
    /// 旧実装では `"yes"` / `1` が `false` に丸められ、全置換の意図が
    /// 先頭 1 件だけの置換に退化していた（回帰テスト）。
    #[tokio::test]
    async fn execute_rejects_non_boolean_replace_all() {
        let dir = temp_dir();
        let file = dir.join("target.txt");
        std::fs::write(&file, "foo foo").unwrap();

        for bad in [serde_json::json!("yes"), serde_json::json!(1)] {
            let error = EditTool::new()
                .execute(
                    serde_json::json!({
                        "file_path": file.to_string_lossy(),
                        "old_string": "foo",
                        "new_string": "bar",
                        "replace_all": bad,
                    }),
                    context(&dir),
                )
                .await
                .unwrap_err();

            assert!(
                matches!(&error, ToolError::InvalidArguments(_)),
                "unexpectedly accepted {bad}: {error}"
            );
        }

        // 引数検証はファイル操作より前に行われるため、内容は変更されない。
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "foo foo");

        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// 正当な boolean / null が過剰に拒否されず従来どおり動くことの確認。
    #[tokio::test]
    async fn execute_replace_all_bool_and_null_defaults() {
        let dir = temp_dir();
        let file = dir.join("target.txt");

        // replace_all: true は全件置換。
        std::fs::write(&file, "foo foo").unwrap();
        EditTool::new()
            .execute(
                serde_json::json!({
                    "file_path": file.to_string_lossy(),
                    "old_string": "foo",
                    "new_string": "bar",
                    "replace_all": true,
                }),
                context(&dir),
            )
            .await
            .unwrap();
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "bar bar");

        // null は既定値 false として受け入れられる。複数一致は一意化エラーに
        // なるため、単一一致のケースで「拒否されないこと」を確認する。
        // （false の既定値そのものは parse_bool_arg の単体テストで固定。）
        std::fs::write(&file, "foo only").unwrap();
        EditTool::new()
            .execute(
                serde_json::json!({
                    "file_path": file.to_string_lossy(),
                    "old_string": "foo",
                    "new_string": "bar",
                    "replace_all": null,
                }),
                context(&dir),
            )
            .await
            .unwrap();
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "bar only");

        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// IOORDER-01-R4 回帰: `old_string` が空の分岐も権限待ちの後で
    /// 第2フェーズの stale 再検証を行い、固定文言のエラーで失敗すること。
    #[tokio::test]
    async fn empty_old_string_branch_revalidates_after_permission() {
        use std::time::Duration;

        let dir = temp_dir();
        let file = dir.join("create.txt");
        std::fs::write(&file, "old").unwrap();

        let permission_entered = Arc::new(AtomicUsize::new(0));
        let permission_gate = Arc::new(AtomicUsize::new(0));
        let entered = permission_entered.clone();
        let gate = permission_gate.clone();
        let assert_calls = Arc::new(AtomicUsize::new(0));
        let calls = assert_calls.clone();

        let ctx = context(&dir)
            .with_ask(move |_req| {
                let entered = entered.clone();
                let gate = gate.clone();
                async move {
                    entered.store(1, Ordering::SeqCst);
                    while gate.load(Ordering::SeqCst) == 0 {
                        tokio::time::sleep(Duration::from_millis(1)).await;
                    }
                    Ok::<(), ToolError>(())
                }
            })
            .with_file_time_assert(move |_s, _p| {
                let calls = calls.clone();
                async move {
                    let n = calls.fetch_add(1, Ordering::SeqCst) + 1;
                    if n == 2 {
                        Err(ToolError::ExecutionError(
                            "File changed since read".to_string(),
                        ))
                    } else {
                        Ok::<(), ToolError>(())
                    }
                }
            });

        let file_path = file.to_string_lossy().to_string();
        let edit = tokio::spawn(async move {
            EditTool::new()
                .execute(
                    serde_json::json!({
                        "file_path": file_path,
                        "old_string": "",
                        "new_string": "created",
                    }),
                    ctx,
                )
                .await
        });

        for _ in 0..500 {
            if permission_entered.load(Ordering::SeqCst) > 0 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
        assert!(permission_entered.load(Ordering::SeqCst) > 0);

        // 権限待ちの間に外部変更を模擬。
        std::fs::write(&file, "changed by other op").unwrap();
        permission_gate.store(1, Ordering::SeqCst);

        let error = tokio::time::timeout(Duration::from_secs(5), edit)
            .await
            .expect("編集が完了すること")
            .expect("スレッドが正常終了すること")
            .expect_err("stale エラーが返ること");

        assert!(
            matches!(&error, ToolError::ExecutionError(msg) if msg.as_str() == "File changed since read"),
            "固定文言の stale エラーであること (actual: {error})"
        );
        assert_eq!(assert_calls.load(Ordering::SeqCst), 2);
        assert_eq!(
            std::fs::read_to_string(&file).unwrap(),
            "changed by other op"
        );

        std::fs::remove_dir_all(&dir).unwrap();
    }
}

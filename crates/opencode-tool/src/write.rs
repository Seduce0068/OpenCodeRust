use async_trait::async_trait;
use std::path::{Path, PathBuf};
use tokio::fs;

use crate::{with_file_lock, Metadata, Tool, ToolContext, ToolError, ToolResult};

#[cfg(feature = "lsp")]
use crate::MAX_DIAGNOSTICS_PER_FILE;

#[cfg(feature = "lsp")]
const MAX_PROJECT_DIAGNOSTICS_FILES: usize = 5;

/// 第1フェーズのロック内で計算し、権限確認を跨いで第2フェーズへ渡す値。
/// パスやトークン値は含まない（エラー・ログは固定文言のみ）。
struct PreparedWrite {
    existed: bool,
    diff: String,
}

pub struct WriteTool {
    directory: PathBuf,
}

impl WriteTool {
    pub fn new() -> Self {
        Self {
            directory: std::env::current_dir().unwrap_or_default(),
        }
    }
}

impl Default for WriteTool {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Tool for WriteTool {
    fn id(&self) -> &str {
        "write"
    }

    fn description(&self) -> &str {
        "Writes content to a file on the local filesystem. Creates the file if it doesn't exist, overwrites if it does."
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "file_path": {
                    "type": "string",
                    "description": "The absolute path to the file to write"
                },
                "content": {
                    "type": "string",
                    "description": "The content to write to the file"
                }
            },
            "required": ["file_path", "content"]
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

        let content: String = args["content"]
            .as_str()
            .ok_or_else(|| ToolError::InvalidArguments("content is required".into()))?
            .to_string();

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

        // EditTool と同じく path_str をキーにしたファイルロックで
        // 存在確認〜書込までを直列化する（IOORDER-01）。キーが同一なら
        // Write/Edit は同じ Mutex を共有し、read→write 間の lost update を防ぐ。
        // IOORDER-01-R4: ただし権限確認（ask_permission）はロック外で行う。
        // 第1フェーズで存在確認・読取り・assert・diff を計算してロックを解放し、
        // 権限確認後に第2フェーズで stale 再検証を行ってから書込む。

        // ---- 第1フェーズ: ロック内で権限確認に必要な値だけを準備する ----
        let ctx_clone = ctx.clone();
        let path_clone = path.clone();
        let path_str_clone = path_str.clone();
        let content_clone = content.clone();

        let prepared = with_file_lock(&path_str, || async move {
            // 既存判定は内容の空・非空ではなく、実際にパスが存在するかで行う。
            // 既存の空ファイルを新規作成と誤判定すると file_time_assert（stale 検出）
            // がスキップされ、並行編集による上書きを防げなくなる。
            let exists = fs::try_exists(&path_clone).await.map_err(|e| {
                ToolError::execution_with_source(
                    format!("Failed to check file existence: {}", e),
                    e,
                )
            })?;
            // 既存ファイルが非 UTF-8 等の理由で読めない場合、空文字として扱うと
            // 「新規作成」として無警告に上書きされる。EditTool と同じく伝播する（R4）。
            let old_content = if exists {
                fs::read_to_string(&path_clone).await.map_err(|e| {
                    ToolError::execution_with_source(format!("Failed to read file: {}", e), e)
                })?
            } else {
                String::new()
            };

            if exists {
                ctx_clone
                    .do_file_time_assert(path_str_clone.clone())
                    .await?;
            }

            let diff = create_diff(&path_str_clone, &old_content, &content_clone);
            Ok::<_, ToolError>(PreparedWrite {
                existed: exists,
                diff,
            })
        })
        .await?;

        // ---- 権限確認: ロック外で行う ----
        // HTTP 経路の PermissionBroker::ask がユーザ応答を待つ間も、同一パスの
        // Write/Edit/ApplyPatch/MultiEdit はロックを取得でき、Head-of-line
        // ブロックしない。
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
        let content_clone = content.clone();
        let existed = prepared.existed;
        let diff = prepared.diff;

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

            if let Some(parent) = path_clone.parent() {
                fs::create_dir_all(parent).await.map_err(|e| {
                    ToolError::execution_with_source(
                        format!("Failed to create directory: {}", e),
                        e,
                    )
                })?;
            }

            fs::write(&path_clone, &content_clone).await.map_err(|e| {
                ToolError::execution_with_source(format!("Failed to write file: {}", e), e)
            })?;

            ctx_clone
                .do_publish_bus(
                    "file.edited",
                    serde_json::json!({
                        "file": path_str_clone
                    }),
                )
                .await;

            ctx_clone
                .do_publish_bus(
                    "file_watcher.updated",
                    serde_json::json!({
                        "file": path_str_clone,
                        "event": if existed { "change" } else { "add" }
                    }),
                )
                .await;

            ctx_clone
                .do_lsp_touch_file(path_str_clone.clone(), true)
                .await?;
            ctx_clone.do_file_time_read(path_str_clone.clone()).await?;

            let line_count = content_clone.lines().count();
            let byte_count = content_clone.len();

            let diagnostics_output = get_lsp_diagnostics(&path_clone, &ctx_clone).await;

            let output = if diagnostics_output.is_empty() {
                format!(
                    "Successfully wrote {} bytes ({} lines) to {}",
                    byte_count,
                    line_count,
                    path_clone.display()
                )
            } else {
                format!(
                    "Successfully wrote {} bytes ({} lines) to {}\n\n{}",
                    byte_count,
                    line_count,
                    path_clone.display(),
                    diagnostics_output
                )
            };

            Ok(ToolResult {
                title: title_clone,
                output,
                metadata: {
                    let mut m = Metadata::new();
                    m.insert("bytes".into(), serde_json::json!(byte_count));
                    m.insert("lines".into(), serde_json::json!(line_count));
                    m.insert("filepath".into(), serde_json::json!(path_str_clone));
                    m.insert("exists".into(), serde_json::json!(existed));
                    m.insert("diff".into(), serde_json::json!(diff));
                    m
                },
                truncated: false,
            })
        })
        .await
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

async fn get_lsp_diagnostics(path: &Path, ctx: &ToolContext) -> String {
    #[cfg(feature = "lsp")]
    {
        if let Some(lsp_registry) = &ctx.lsp_registry {
            return get_lsp_diagnostics_impl(path, lsp_registry.clone()).await;
        }
    }

    #[cfg(not(feature = "lsp"))]
    {
        let _ = (path, ctx);
    }

    String::new()
}

#[cfg(feature = "lsp")]
async fn get_lsp_diagnostics_impl(
    path: &Path,
    lsp_registry: std::sync::Arc<opencode_lsp::LspClientRegistry>,
) -> String {
    use opencode_lsp::detect_language;

    let language = detect_language(path);
    let clients = lsp_registry.list().await;

    let client = clients
        .iter()
        .find(|(id, _)| id.contains(language))
        .map(|(_, c)| c.clone());

    let Some(client) = client else {
        return String::new();
    };

    // Open/refresh the written file so the LSP re-publishes diagnostics
    if let Ok(content) = tokio::fs::read_to_string(path).await {
        let _ = client.open_document(path, &content, language).await;
    }

    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    // Collect all diagnostics from all files the LSP knows about
    let all_diagnostics = client.get_all_diagnostics().await;

    let normalized_path = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    let mut output = String::new();
    let mut project_diagnostics_count = 0;

    // First pass: the written file itself
    for (file, diagnostics) in &all_diagnostics {
        let file_canonical = file.canonicalize().unwrap_or_else(|_| file.clone());
        if file_canonical != normalized_path {
            continue;
        }

        let errors: Vec<_> = diagnostics
            .iter()
            .filter(|d| d.severity == Some(lsp_types::DiagnosticSeverity::ERROR))
            .collect();

        if errors.is_empty() {
            continue;
        }

        let total = errors.len();
        let limited: Vec<_> = errors.into_iter().take(MAX_DIAGNOSTICS_PER_FILE).collect();
        let suffix = if limited.len() < total {
            format!("\n... and {} more", total - limited.len())
        } else {
            String::new()
        };

        let lines: Vec<String> = limited
            .iter()
            .map(|d| {
                let line = d.range.start.line + 1;
                format!("  Line {}: {}", line, d.message)
            })
            .collect();

        output.push_str(&format!(
            "LSP errors detected in this file, please fix:\n<diagnostics file=\"{}\">\n{}{}\n</diagnostics>",
            path.display(),
            lines.join("\n"),
            suffix
        ));
    }

    // Second pass: up to MAX_PROJECT_DIAGNOSTICS_FILES other files
    for (file, diagnostics) in &all_diagnostics {
        if project_diagnostics_count >= MAX_PROJECT_DIAGNOSTICS_FILES {
            break;
        }

        let file_canonical = file.canonicalize().unwrap_or_else(|_| file.clone());
        if file_canonical == normalized_path {
            continue;
        }

        let errors: Vec<_> = diagnostics
            .iter()
            .filter(|d| d.severity == Some(lsp_types::DiagnosticSeverity::ERROR))
            .collect();

        if errors.is_empty() {
            continue;
        }

        project_diagnostics_count += 1;

        let total = errors.len();
        let limited: Vec<_> = errors.into_iter().take(MAX_DIAGNOSTICS_PER_FILE).collect();
        let suffix = if limited.len() < total {
            format!("\n... and {} more", total - limited.len())
        } else {
            String::new()
        };

        let lines: Vec<String> = limited
            .iter()
            .map(|d| {
                let line = d.range.start.line + 1;
                format!("  Line {}: {}", line, d.message)
            })
            .collect();

        output.push_str(&format!(
            "\n\nLSP errors detected in other files:\n<diagnostics file=\"{}\">\n{}{}\n</diagnostics>",
            file.display(),
            lines.join("\n"),
            suffix
        ));
    }

    output
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};

    fn temp_dir() -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("opencode-write-test-{}", uuid::Uuid::new_v4()));
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

    #[test]
    fn identity_and_schema() {
        let tool = WriteTool::new();
        assert_eq!(tool.id(), "write");
        assert_eq!(tool.parameters()["required"][0], "file_path");
        assert_eq!(tool.parameters()["required"][1], "content");
    }

    #[test]
    fn create_diff_for_new_file() {
        let diff = create_diff("a.rs", "", "hello");
        assert!(diff.starts_with("--- a.rs\n+++ a.rs\n"));
        assert!(diff.contains("+hello\n"));
    }

    #[tokio::test]
    async fn execute_creates_new_file() {
        let dir = temp_dir();
        let nested = dir.join("sub").join("a.txt");

        let result = WriteTool::new()
            .execute(
                serde_json::json!({
                    "file_path": nested.to_string_lossy(),
                    "content": "hello\nworld"
                }),
                context(&dir),
            )
            .await
            .unwrap();

        assert_eq!(std::fs::read_to_string(&nested).unwrap(), "hello\nworld");
        assert_eq!(result.metadata["bytes"], 11);
        assert_eq!(result.metadata["lines"], 2);
        assert_eq!(result.metadata["exists"], false);

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[tokio::test]
    async fn execute_overwrites_existing_file() {
        let dir = temp_dir();
        let file = dir.join("a.txt");
        std::fs::write(&file, "old").unwrap();

        let result = WriteTool::new()
            .execute(
                serde_json::json!({
                    "file_path": file.to_string_lossy(),
                    "content": "new"
                }),
                context(&dir),
            )
            .await
            .unwrap();

        assert_eq!(std::fs::read_to_string(&file).unwrap(), "new");
        assert_eq!(result.metadata["exists"], true);

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[tokio::test]
    async fn execute_requires_file_path() {
        let dir = temp_dir();
        let error = WriteTool::new()
            .execute(serde_json::json!({ "content": "x" }), context(&dir))
            .await
            .unwrap_err();
        assert!(matches!(error, ToolError::InvalidArguments(_)));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[tokio::test]
    async fn execute_requires_content() {
        let dir = temp_dir();
        let error = WriteTool::new()
            .execute(serde_json::json!({ "file_path": "a.txt" }), context(&dir))
            .await
            .unwrap_err();
        assert!(matches!(error, ToolError::InvalidArguments(_)));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    // 既存判定を内容の空・非空ではなくパスの存在で行うことの回帰テスト。

    #[tokio::test]
    async fn execute_existing_empty_file_asserts_file_time_and_reports_change() {
        let dir = temp_dir();
        let file = dir.join("empty.txt");
        // 既存だが内容が空のファイル（旧実装では新規作成と誤判定されていた）
        std::fs::write(&file, "").unwrap();

        let assert_calls = Arc::new(AtomicUsize::new(0));
        let events = Arc::new(Mutex::new(Vec::new()));
        let ctx = observing_context(&dir, assert_calls.clone(), events.clone());

        let result = WriteTool::new()
            .execute(
                serde_json::json!({
                    "file_path": file.to_string_lossy(),
                    "content": "new content"
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
        assert_eq!(result.metadata["exists"], true);
        {
            let events = events.lock().unwrap();
            assert_eq!(watcher_event(&events)["event"], "change");
        }

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[tokio::test]
    async fn execute_new_file_skips_file_time_assert_and_reports_add() {
        let dir = temp_dir();
        let file = dir.join("new.txt"); // まだ存在しない

        let assert_calls = Arc::new(AtomicUsize::new(0));
        let events = Arc::new(Mutex::new(Vec::new()));
        let ctx = observing_context(&dir, assert_calls.clone(), events.clone());

        let result = WriteTool::new()
            .execute(
                serde_json::json!({
                    "file_path": file.to_string_lossy(),
                    "content": "created"
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
        assert_eq!(result.metadata["exists"], false);
        {
            let events = events.lock().unwrap();
            assert_eq!(watcher_event(&events)["event"], "add");
        }

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

        let result = WriteTool::new()
            .execute(
                serde_json::json!({
                    "file_path": file.to_string_lossy(),
                    "content": "new"
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
        assert_eq!(result.metadata["exists"], true);
        {
            let events = events.lock().unwrap();
            assert_eq!(watcher_event(&events)["event"], "change");
        }

        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// IOORDER-01 / -R4 回帰: Write と Edit はフェーズ単位で同一パスの
    /// ファイルロックを共有し、各フェーズのクリティカルセクション（存在確認〜
    /// file_time_assert）は重ならないこと。ロックが無い旧実装では peak が 2 になる。
    ///
    /// 2 フェーズ化により権限待ちの間はロックが解放されるため、このテストの
    /// no-op assert では stale 検出が働かず、最後に書込んだ側の内容が残る。
    /// 権限待ち中のロック取得と stale 検出は専用テストで検証する。
    #[tokio::test]
    async fn write_and_edit_share_file_lock_on_same_path() {
        use std::time::Duration;

        let dir = temp_dir();
        let file = dir.join("shared.txt");
        // 双方が file_time_assert を通るよう既存ファイルにしておく。
        std::fs::write(&file, "AAA\nBBB\n").unwrap();

        let active = Arc::new(AtomicUsize::new(0));
        let peak = Arc::new(AtomicUsize::new(0));
        // Write がクリティカルセクションへ入ったことを親テストへ知らせる旗。
        let write_entered = Arc::new(AtomicUsize::new(0));

        let make_ctx = |entered: Arc<AtomicUsize>| {
            let active = active.clone();
            let peak = peak.clone();
            context(&dir).with_file_time_assert(move |_s, _p| {
                let active = active.clone();
                let peak = peak.clone();
                let entered = entered.clone();
                async move {
                    let now = active.fetch_add(1, Ordering::SeqCst) + 1;
                    peak.fetch_max(now, Ordering::SeqCst);
                    entered.store(1, Ordering::SeqCst);
                    // ロックが無い間に他方が入れば peak が 2 になるよう窓を広げる。
                    tokio::time::sleep(Duration::from_millis(50)).await;
                    active.fetch_sub(1, Ordering::SeqCst);
                    Ok::<(), ToolError>(())
                }
            })
        };

        let file_path = file.to_string_lossy().to_string();

        let write_ctx = make_ctx(write_entered.clone());
        let write_path = file_path.clone();
        let write = tokio::spawn(async move {
            WriteTool::new()
                .execute(
                    serde_json::json!({
                        "file_path": write_path,
                        "content": "AAA\nBBB\nCCC\n",
                    }),
                    write_ctx,
                )
                .await
                .unwrap()
        });

        // Write が file_time_assert（ロック保持中）へ入るまで待つ。
        for _ in 0..500 {
            if write_entered.load(Ordering::SeqCst) > 0 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
        assert!(
            write_entered.load(Ordering::SeqCst) > 0,
            "Write が開始すること"
        );

        // Edit は Write がロックを手放すまで file_time_assert に到達しないはず。
        let edit_ctx = make_ctx(Arc::new(AtomicUsize::new(0)));
        let edit_path = file_path.clone();
        let edit = tokio::spawn(async move {
            crate::edit::EditTool::new()
                .execute(
                    serde_json::json!({
                        "file_path": edit_path,
                        "old_string": "BBB",
                        "new_string": "BBB_EDITED",
                    }),
                    edit_ctx,
                )
                .await
                .unwrap()
        });

        write.await.unwrap();
        edit.await.unwrap();

        assert_eq!(
            peak.load(Ordering::SeqCst),
            1,
            "Write と Edit は同一パスでファイルロックを共有し直列化されること"
        );

        // 2 フェーズ化では権限待ちの間に他側が書込めるため、no-op assert の
        // このテストでは最後の書込が勝つ。いずれかの完全な結果であり、
        // 途中状態の混在や破損がないことを確認する。
        let final_content = std::fs::read_to_string(&file).unwrap();
        assert!(
            final_content == "AAA\nBBB\nCCC\n"
                || final_content == "AAA\nBBB_EDITED\n"
                || final_content == "AAA\nBBB_EDITED\nCCC\n",
            "いずれかの完全な結果であること (actual: {final_content})"
        );

        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// IOORDER-01-R4 回帰: 権限確認（ask_permission）を待つ間はファイルロックを
    /// 保持しない。別タスクが同一パスのロックを取得でき、並行度が 2 以上になる。
    #[tokio::test]
    async fn permission_wait_does_not_hold_file_lock() {
        use std::time::Duration;

        let dir = temp_dir();
        let file = dir.join("permission.txt");
        std::fs::write(&file, "old").unwrap();

        let active = Arc::new(AtomicUsize::new(0));
        let peak = Arc::new(AtomicUsize::new(0));
        let permission_entered = Arc::new(AtomicUsize::new(0));
        let permission_gate = Arc::new(AtomicUsize::new(0));

        let entered = permission_entered.clone();
        let gate = permission_gate.clone();
        let active_for_ask = active.clone();
        let peak_for_ask = peak.clone();
        let ctx = context(&dir).with_ask(move |_req| {
            let entered = entered.clone();
            let gate = gate.clone();
            let active = active_for_ask.clone();
            let peak = peak_for_ask.clone();
            async move {
                let now = active.fetch_add(1, Ordering::SeqCst) + 1;
                peak.fetch_max(now, Ordering::SeqCst);
                entered.store(1, Ordering::SeqCst);
                while gate.load(Ordering::SeqCst) == 0 {
                    tokio::time::sleep(Duration::from_millis(1)).await;
                }
                active.fetch_sub(1, Ordering::SeqCst);
                Ok::<(), ToolError>(())
            }
        });

        let file_path = file.to_string_lossy().to_string();
        let write_path = file_path.clone();
        let write = tokio::spawn(async move {
            WriteTool::new()
                .execute(
                    serde_json::json!({
                        "file_path": write_path,
                        "content": "new",
                    }),
                    ctx,
                )
                .await
                .unwrap()
        });

        // 権限待ちへ入るまで待つ（第1フェーズのロックは既に解放済み）。
        for _ in 0..500 {
            if permission_entered.load(Ordering::SeqCst) > 0 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
        assert!(
            permission_entered.load(Ordering::SeqCst) > 0,
            "権限確認コールバックが呼ばれること"
        );

        // 権限待ちの間に別タスクが同一パスのロックを取得できること。
        let other_path = file_path.clone();
        let other_active = active.clone();
        let other_peak = peak.clone();
        let other = tokio::spawn(async move {
            crate::with_file_lock(&other_path, || async move {
                let now = other_active.fetch_add(1, Ordering::SeqCst) + 1;
                other_peak.fetch_max(now, Ordering::SeqCst);
                tokio::time::sleep(Duration::from_millis(10)).await;
                other_active.fetch_sub(1, Ordering::SeqCst);
            })
            .await;
        });

        tokio::time::timeout(Duration::from_secs(5), other)
            .await
            .expect("権限待ち中に他タスクがロックを取得できること")
            .expect("他タスクが正常終了すること");

        assert!(
            peak.load(Ordering::SeqCst) > 1,
            "権限待ちと他操作が重なり peak が 2 以上になること (actual: {})",
            peak.load(Ordering::SeqCst)
        );

        permission_gate.store(1, Ordering::SeqCst);
        write.await.unwrap();
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "new");

        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// IOORDER-01-R4 回帰: 権限確認中に他操作がファイルを変更した場合、
    /// 第2フェーズの file_time_assert が固定文言のエラーを返し、書込まない。
    #[tokio::test]
    async fn stale_change_during_permission_is_rejected() {
        use std::time::Duration;

        let dir = temp_dir();
        let file = dir.join("stale.txt");
        std::fs::write(&file, "old").unwrap();

        let permission_entered = Arc::new(AtomicUsize::new(0));
        let permission_gate = Arc::new(AtomicUsize::new(0));
        let entered = permission_entered.clone();
        let gate = permission_gate.clone();

        // 2 回目（第2フェーズ）の assert で stale を検出したものとして失敗させる。
        // エラー文言は本番配線と同じ固定文言のみで、パスは含まない。
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
        let write_path = file_path.clone();
        let write = tokio::spawn(async move {
            WriteTool::new()
                .execute(
                    serde_json::json!({
                        "file_path": write_path,
                        "content": "new",
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

        // 権限待ちの間に外部変更を模擬（ロックは第1フェーズで解放済み）。
        std::fs::write(&file, "changed by other op").unwrap();
        permission_gate.store(1, Ordering::SeqCst);

        let error = tokio::time::timeout(Duration::from_secs(5), write)
            .await
            .expect("書き込みが完了すること")
            .expect("スレッドが正常終了すること")
            .expect_err("stale エラーが返ること");

        assert!(
            matches!(&error, ToolError::ExecutionError(msg) if msg.as_str() == "File changed since read"),
            "固定文言の stale エラーであること (actual: {error})"
        );
        assert_eq!(
            assert_calls.load(Ordering::SeqCst),
            2,
            "第1フェーズと第2フェーズの両方で assert が呼ばれること"
        );
        // 変更は上書きされていないこと。
        assert_eq!(
            std::fs::read_to_string(&file).unwrap(),
            "changed by other op"
        );

        std::fs::remove_dir_all(&dir).unwrap();
    }
}

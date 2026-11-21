use async_trait::async_trait;
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::{
    with_file_lock, Metadata, PermissionRequest, Tool, ToolContext, ToolError, ToolResult,
};

#[cfg(feature = "lsp")]
use crate::MAX_DIAGNOSTICS_PER_FILE;

pub struct ApplyPatchTool;

#[derive(Debug, Serialize, Deserialize)]
struct ApplyPatchInput {
    #[serde(rename = "patchText")]
    patch_text: String,
}

#[derive(Debug, Clone)]
enum PatchOperation {
    Add,
    Update,
    Delete,
    Move { move_path: String },
}

impl PatchOperation {
    fn is_add(&self) -> bool {
        matches!(self, PatchOperation::Add)
    }
}

#[derive(Debug, Clone)]
struct FilePatch {
    path: String,
    operation: PatchOperation,
    hunks: Vec<Hunk>,
}

#[derive(Debug, Clone)]
struct Hunk {
    old_start: usize,
    old_count: usize,
    new_start: usize,
    new_count: usize,
    lines: Vec<PatchLine>,
    /// `@@ -a,b +c,d @@` でカンマ付き明示された行数 (old, new)。
    /// 裸の `@@` や `@@ -a +b @@`（行数省略）は None とし、本文から算出する。
    /// Some の場合は本文から再計算した値と一致しなければならない。
    declared_counts: Option<(usize, usize)>,
    /// model patch の `*** Move To:` が `*** Begin File:` の Add コンテンツを
    /// 伴った場合に true。通常の apply_hunks は旧内容を末尾へ連結するため
    /// 「新内容 + 旧内容」となり Move 先が壊れる。この場合だけ旧内容を
    /// 完全に破棄し、追加行のみを新内容として扱う。
    replace_entire_file: bool,
}

#[derive(Debug, Clone)]
enum PatchLine {
    Context(#[allow(dead_code)] String),
    Remove(#[allow(dead_code)] String),
    Add(String),
}

#[async_trait]
impl Tool for ApplyPatchTool {
    fn id(&self) -> &str {
        "apply_patch"
    }

    fn description(&self) -> &str {
        "Apply a unified diff patch to one or more files. Supports adding new files, updating existing files, deleting files, and moving/renaming files."
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "patchText": {
                    "type": "string",
                    "description": "The full patch text that describes all changes to be made"
                }
            },
            "required": ["patchText"]
        })
    }

    async fn execute(
        &self,
        args: serde_json::Value,
        ctx: ToolContext,
    ) -> Result<ToolResult, ToolError> {
        let input: ApplyPatchInput =
            serde_json::from_value(args).map_err(|e| ToolError::InvalidArguments(e.to_string()))?;

        if input.patch_text.trim().is_empty() {
            return Err(ToolError::InvalidArguments(
                "patchText is required".to_string(),
            ));
        }

        let file_patches = parse_multi_file_patch(&input.patch_text)?;

        if file_patches.is_empty() {
            return Err(ToolError::ExecutionError(
                "No valid hunks found in patch".to_string(),
            ));
        }

        let base_path = PathBuf::from(&ctx.directory);
        let mut file_changes: Vec<FileChange> = Vec::new();
        let mut total_diff = String::new();

        for file_patch in &file_patches {
            // edit/tool.rs と同じく絶対パスはそのまま使い、相対パスのみ base_path に
            // 結合する（PathBuf::join は絶対パスを採用するため意図を明示する）。
            let file_path = resolve_patch_path(&base_path, &file_patch.path);
            let file_path_str = file_path.to_string_lossy().to_string();

            if ctx.is_external_path(&file_path_str) {
                ctx.ask_permission(
                    PermissionRequest::new("external_directory").with_pattern(&file_path_str),
                )
                .await?;
            }

            // M-54: Move の移送先 (move_path) も移送元と同じく検証する。
            // 未検証のまま `*** Move To: /etc/passwd` のようなパスを通すと、
            // 書込みフェーズで project_root 外へ任意の内容を書き込める。
            if let PatchOperation::Move { move_path } = &file_patch.operation {
                let move_full_path = resolve_patch_path(&base_path, move_path);
                let move_path_str = move_full_path.to_string_lossy().to_string();

                // 移送先はまだ存在しないことが多い。is_external_path を直接使うと
                // canonicalize 失敗が「外部」とみなされ正当な Move が拒否されるため、
                // is_move_target_external で祖先の containment まで遡って判定する。
                if is_move_target_external(&ctx, &move_full_path) {
                    let parent = move_full_path
                        .parent()
                        .map(|p| p.to_string_lossy().to_string())
                        .unwrap_or_else(|| move_path_str.clone());
                    ctx.ask_permission(
                        PermissionRequest::new("external_directory")
                            .with_pattern(format!("{}/*", parent))
                            .with_metadata("filepath", serde_json::json!(&move_path_str))
                            .with_metadata("parentDir", serde_json::json!(parent)),
                    )
                    .await?;
                }
            }

            // 読取り・差分計算の間はファイルロックを保持し、外部変更との競合を
            // process_file_patch 内の do_file_time_assert で検出する。
            let change = with_file_lock(&file_path_str, || {
                process_file_patch(&base_path, file_patch, &ctx)
            })
            .await?;
            total_diff.push_str(&change.diff);
            total_diff.push('\n');
            file_changes.push(change);
        }

        let relative_paths: Vec<String> = file_changes
            .iter()
            .map(|c| c.relative_path.clone())
            .collect();
        let files_metadata: Vec<serde_json::Value> = file_changes
            .iter()
            .map(|change| {
                let (change_type, move_path, target_relative_path) = match &change.operation {
                    PatchOperation::Add => ("add", None, change.relative_path.clone()),
                    PatchOperation::Update => ("update", None, change.relative_path.clone()),
                    PatchOperation::Delete => ("delete", None, change.relative_path.clone()),
                    PatchOperation::Move { move_path } => {
                        ("move", Some(move_path.clone()), move_path.clone())
                    }
                };

                serde_json::json!({
                    "filePath": base_path.join(&change.relative_path).to_string_lossy().to_string(),
                    "relativePath": target_relative_path,
                    "type": change_type,
                    "diff": change.diff,
                    "before": change.old_content,
                    "after": change.new_content,
                    "movePath": move_path.map(|path| base_path.join(path).to_string_lossy().to_string()),
                })
            })
            .collect();

        ctx.ask_permission(
            PermissionRequest::new("edit")
                .with_patterns(relative_paths.clone())
                .with_metadata("diff", serde_json::json!(&total_diff))
                .with_metadata("filepath", serde_json::json!(relative_paths.join(", ")))
                .with_metadata("files", serde_json::json!(&files_metadata))
                .always_allow(),
        )
        .await?;

        let mut updates: Vec<(String, String)> = Vec::new();
        let mut summary_lines: Vec<String> = Vec::new();
        let mut edited_files: Vec<String> = Vec::new();
        let mut lsp_targets: Vec<String> = Vec::new();

        for change in &file_changes {
            let file_path = resolve_patch_path(&base_path, &change.relative_path);

            match change.operation {
                PatchOperation::Add => {
                    if let Some(parent) = file_path.parent() {
                        tokio::fs::create_dir_all(parent).await.map_err(|e| {
                            ToolError::execution_with_source(
                                format!("Failed to create directory: {}", e),
                                e,
                            )
                        })?;
                    }
                    tokio::fs::write(&file_path, &change.new_content)
                        .await
                        .map_err(|e| {
                            ToolError::execution_with_source(
                                format!("Failed to write file: {}", e),
                                e,
                            )
                        })?;
                    // 書込み後の mtime を記録し、後続編集で誤った競合が出ないようにする。
                    ctx.do_file_time_read(file_path.to_string_lossy().to_string())
                        .await?;
                    updates.push((change.relative_path.clone(), "add".to_string()));
                    summary_lines.push(format!("A {}", change.relative_path));
                    edited_files.push(change.relative_path.clone());
                    lsp_targets.push(change.relative_path.clone());
                }
                PatchOperation::Update => {
                    tokio::fs::write(&file_path, &change.new_content)
                        .await
                        .map_err(|e| {
                            ToolError::execution_with_source(
                                format!("Failed to write file: {}", e),
                                e,
                            )
                        })?;
                    ctx.do_file_time_read(file_path.to_string_lossy().to_string())
                        .await?;
                    updates.push((change.relative_path.clone(), "change".to_string()));
                    summary_lines.push(format!("M {}", change.relative_path));
                    edited_files.push(change.relative_path.clone());
                    lsp_targets.push(change.relative_path.clone());
                }
                PatchOperation::Delete => {
                    tokio::fs::remove_file(&file_path).await.map_err(|e| {
                        ToolError::execution_with_source(format!("Failed to delete file: {}", e), e)
                    })?;
                    updates.push((change.relative_path.clone(), "unlink".to_string()));
                    summary_lines.push(format!("D {}", change.relative_path));
                }
                PatchOperation::Move { ref move_path } => {
                    // 検証フェーズと同じ解決結果を使い、検査したパスへ必ず書込む。
                    let move_full_path = resolve_patch_path(&base_path, move_path);
                    if let Some(parent) = move_full_path.parent() {
                        tokio::fs::create_dir_all(parent).await.map_err(|e| {
                            ToolError::execution_with_source(
                                format!("Failed to create directory: {}", e),
                                e,
                            )
                        })?;
                    }
                    tokio::fs::write(&move_full_path, &change.new_content)
                        .await
                        .map_err(|e| {
                            ToolError::execution_with_source(
                                format!("Failed to write file: {}", e),
                                e,
                            )
                        })?;
                    tokio::fs::remove_file(&file_path).await.map_err(|e| {
                        ToolError::execution_with_source(
                            format!("Failed to remove old file: {}", e),
                            e,
                        )
                    })?;
                    // 移送先の mtime を記録する（移送元は削除済みのため記録しない）。
                    ctx.do_file_time_read(move_full_path.to_string_lossy().to_string())
                        .await?;
                    updates.push((change.relative_path.clone(), "unlink".to_string()));
                    updates.push((move_path.clone(), "add".to_string()));
                    summary_lines.push(format!("M {}", move_path));
                    edited_files.push(move_path.clone());
                    lsp_targets.push(move_path.clone());
                }
            }
        }

        for relative_path in &edited_files {
            ctx.do_publish_bus(
                "file.edited",
                serde_json::json!({
                    "file": relative_path
                }),
            )
            .await;
        }

        for (relative_path, event) in &updates {
            ctx.do_publish_bus(
                "file_watcher.updated",
                serde_json::json!({
                    "file": relative_path,
                    "event": event
                }),
            )
            .await;
        }

        for relative_path in &lsp_targets {
            ctx.do_lsp_touch_file(relative_path.clone(), true).await?;
        }

        let (diagnostics_output, diagnostics_meta) =
            collect_lsp_diagnostics_for_targets(&base_path, &lsp_targets, &ctx).await;

        let mut output = format!(
            "Success. Updated the following files:\n{}",
            summary_lines.join("\n")
        );
        if !diagnostics_output.is_empty() {
            output.push_str("\n\n");
            output.push_str(&diagnostics_output);
        }

        let mut metadata = Metadata::new();
        metadata.insert("diff".to_string(), serde_json::json!(total_diff));
        metadata.insert("files".to_string(), serde_json::json!(files_metadata));
        metadata.insert(
            "diagnostics".to_string(),
            serde_json::json!(diagnostics_meta),
        );

        Ok(ToolResult {
            title: output.clone(),
            output,
            metadata,
            truncated: false,
        })
    }
}

/// パッチ内のパスを絶対パスへ解決する。
///
/// `PathBuf::join` は第 2 引数が絶対パスの場合に第 1 引数を破棄して採用するため、
/// 「絶対パスはそのまま、相対パスは base_path に結合」という意図を明示する。
/// `edit/tool.rs` / `write.rs` と同じ分岐を使う。
fn resolve_patch_path(base_path: &Path, path: &str) -> PathBuf {
    if Path::new(path).is_absolute() {
        PathBuf::from(path)
    } else {
        base_path.join(path)
    }
}

/// Move の移送先が project_root 外かどうかを判定する。
///
/// `ToolContext::is_external_path` は対象の `canonicalize()` が失敗すると
/// （＝まだ存在しないパス）安全側として「外部」を返す。しかし Move の移送先は
/// 通常まだ存在しないため、そのまま使うと project_root 内の新規ファイルへの
/// 正当な Move まで `external_directory` 権限を要求し、deny 既定の
/// PermissionBroker では拒否されてしまう（機能リグレッション）。
///
/// 存在しない場合は「存在する最も近い祖先」を `canonicalize()` して
/// containment を判定する。祖先が見つからない／project_root 外なら安全側として
/// 外部扱いにする。絶対パス・`..` を含むパス・シンボリックリンク脱出は、
/// `is_external_path` 側の正規化＋components 比較で従来どおり検出される。
fn is_move_target_external(ctx: &ToolContext, target: &Path) -> bool {
    if target.exists() {
        return ctx.is_external_path(&target.to_string_lossy());
    }

    let mut ancestor = target;
    loop {
        let Some(parent) = ancestor.parent() else {
            return true;
        };
        if parent.as_os_str().is_empty() || parent == ancestor {
            return true;
        }
        ancestor = parent;
        if ancestor.exists() {
            return ctx.is_external_path(&ancestor.to_string_lossy());
        }
    }
}

struct FileChange {
    relative_path: String,
    operation: PatchOperation,
    old_content: String,
    new_content: String,
    diff: String,
}

fn parse_multi_file_patch(patch_text: &str) -> Result<Vec<FilePatch>, ToolError> {
    let mut file_patches = Vec::new();

    let normalized = patch_text.replace("\r\n", "\n").replace('\r', "\n");

    if normalized.trim().starts_with("*** Begin Patch") {
        return parse_model_patch(&normalized);
    }

    let file_header = Regex::new(r"^---\s+(?:a/)?(.+?)(?:\s+\d{4}-\d{2}-\d{2}.*)?$").unwrap();
    let new_file_header =
        Regex::new(r"^\+\+\+\s+(?:b/)?(.+?)(?:\s+\d{4}-\d{2}-\d{2}.*)?$").unwrap();
    let hunk_header = Regex::new(r"^@@ -(\d+)(?:,(\d+))? \+(\d+)(?:,(\d+))? @@").unwrap();
    let rename_from_re = Regex::new(r"^rename from (.+)$").unwrap();
    let rename_to_re = Regex::new(r"^rename to (.+)$").unwrap();

    let mut current_file: Option<FilePatch> = None;
    let mut current_hunk: Option<Hunk> = None;
    let mut in_hunk = false;
    let mut old_path: Option<String> = None;
    let mut rename_from: Option<String> = None;
    let mut rename_to: Option<String> = None;

    for line in normalized.lines() {
        if line.starts_with("diff --git ") || line.starts_with("diff -") {
            if let Some(mut file) = current_file.take() {
                if let Some(hunk) = current_hunk.take() {
                    file.hunks.push(hunk);
                }
                if !file.hunks.is_empty() {
                    file_patches.push(file);
                }
            }
            current_file = None;
            current_hunk = None;
            in_hunk = false;
            old_path = None;
            rename_from = None;
            rename_to = None;
            continue;
        }

        // Detect rename from/to lines in git diff extended headers
        if let Some(caps) = rename_from_re.captures(line) {
            rename_from = Some(caps[1].to_string());
            continue;
        }
        if let Some(caps) = rename_to_re.captures(line) {
            rename_to = Some(caps[1].to_string());
            continue;
        }

        if let Some(caps) = file_header.captures(line) {
            if let Some(ref mut file) = current_file {
                if let Some(hunk) = current_hunk.take() {
                    file.hunks.push(hunk);
                }
            }
            old_path = Some(caps[1].to_string());
            in_hunk = false;
            continue;
        }

        if let Some(caps) = new_file_header.captures(line) {
            let new_path = caps[1].to_string();

            // Determine operation based on old/new paths
            let operation = if let (Some(ref rf), Some(ref rt)) = (&rename_from, &rename_to) {
                // Git rename detected
                let op = PatchOperation::Move {
                    move_path: rt.clone(),
                };
                current_file = Some(FilePatch {
                    path: rf.clone(),
                    operation: op,
                    hunks: Vec::new(),
                });
                continue;
            } else if old_path.as_deref() == Some("/dev/null") {
                // New file: --- /dev/null, +++ b/file
                PatchOperation::Add
            } else if new_path == "/dev/null" {
                // Deleted file: --- a/file, +++ /dev/null
                PatchOperation::Delete
            } else {
                PatchOperation::Update
            };

            let path = if operation.is_add() {
                new_path.clone()
            } else {
                old_path.clone().unwrap_or_else(|| new_path.clone())
            };

            if path == "/dev/null" || path.is_empty() {
                continue;
            }

            current_file = Some(FilePatch {
                path,
                operation,
                hunks: Vec::new(),
            });
            continue;
        }

        if let Some(caps) = hunk_header.captures(line) {
            if let Some(ref mut file) = current_file {
                if let Some(hunk) = current_hunk.take() {
                    file.hunks.push(hunk);
                }

                let parse_num = |value: &str| -> Result<usize, ToolError> {
                    value.parse::<usize>().map_err(|e| {
                        ToolError::InvalidArguments(format!("invalid line number: {}", e))
                    })
                };
                let old_start = parse_num(&caps[1])?;
                let old_count = match caps.get(2) {
                    Some(m) => parse_num(m.as_str())?,
                    None => 1,
                };
                let new_start = parse_num(&caps[3])?;
                let new_count = match caps.get(4) {
                    Some(m) => parse_num(m.as_str())?,
                    None => 1,
                };

                current_hunk = Some(Hunk {
                    old_start,
                    old_count,
                    new_start,
                    new_count,
                    lines: Vec::new(),
                    // git diff は apply_hunks 側で宣言値を検証するため再検証不要。
                    declared_counts: None,
                    replace_entire_file: false,
                });
                in_hunk = true;
            }
            continue;
        }

        if in_hunk {
            if let Some(ref mut hunk) = current_hunk {
                if line.starts_with(' ') || line.starts_with('\t') {
                    hunk.lines
                        .push(PatchLine::Context(line.chars().skip(1).collect()));
                } else if line.starts_with('-') && !line.starts_with("--- ") {
                    hunk.lines
                        .push(PatchLine::Remove(line.chars().skip(1).collect()));
                } else if line.starts_with('+') && !line.starts_with("+++") {
                    hunk.lines
                        .push(PatchLine::Add(line.chars().skip(1).collect()));
                } else if line.starts_with('\\') {
                    continue;
                }
            }
        }
    }

    if let Some(mut file) = current_file {
        if let Some(hunk) = current_hunk {
            file.hunks.push(hunk);
        }
        if !file.hunks.is_empty() {
            file_patches.push(file);
        }
    }

    Ok(file_patches)
}

/// `***`-形式（LLM が出力する apply_patch 形式）をパースする。
///
/// 従来は `Begin File` / `Delete File` / `Move To` / `End File` のみを解釈し、
/// 最も一般的な `*** Update File:` + `@@` hunk を読み飛ばしていた。ここでは
/// Update と hunk を正式に解釈し、`Move To` についても旧内容を連結しない
/// 正しい Move を生成する。
fn parse_model_patch(patch_text: &str) -> Result<Vec<FilePatch>, ToolError> {
    let mut file_patches = Vec::new();
    let begin_file = Regex::new(r"^\*\*\*\s+Begin\s+File:\s+(.+)$").unwrap();
    let update_file = Regex::new(r"(?i)^\*\*\*\s+Update\s+File:\s+(.+)$").unwrap();
    let delete_file = Regex::new(r"^\*\*\*\s+Delete\s+File:\s+(.+)$").unwrap();
    let end_patch = Regex::new(r"^\*\*\*\s+End\s+Patch$").unwrap();
    let move_to = Regex::new(r"(?i)^\*\*\*\s+Move\s+To:\s+(.+)$").unwrap();
    // `@@ -a,b +c,d @@` の他、行数を省略した裸の `@@` も受け付ける。
    // 行数は hunk 本体から算出するため（R2 の count 検証と整合する）。
    let hunk_header =
        Regex::new(r"^@@(?: -(\d+)(?:,(\d+))? \+(\d+)(?:,(\d+))? @@.*|\s*)$").unwrap();

    let mut current_file: Option<FilePatch> = None;
    // Begin File の生コンテンツ（従来どおり接頭辞を落とさず保持する）。
    let mut content_lines: Vec<String> = Vec::new();
    let mut current_hunk: Option<Hunk> = None;
    let mut in_content = false;
    let mut in_hunk = false;
    // 裸の `@@` が続く場合に使う元ファイル上のカーソル。
    let mut next_old_start: usize = 1;
    let mut next_new_start: usize = 1;

    // hunk 本体から old/new の行数を算出する。apply_hunks の厳密な
    // カウント検証（R2）と同じ定義を使う。
    fn count_lines(lines: &[PatchLine]) -> (usize, usize) {
        let old_count = lines
            .iter()
            .filter(|l| matches!(l, PatchLine::Remove(_) | PatchLine::Context(_)))
            .count();
        let new_count = lines
            .iter()
            .filter(|l| matches!(l, PatchLine::Add(_) | PatchLine::Context(_)))
            .count();
        (old_count, new_count)
    }

    // 現在の FilePatch を確定して file_patches へ移送する。
    // Add ファイルは content_lines から Add-only hunk を作る（従来どおり）。
    // Update/Move は解析済みの hunks を使い、行数をここで確定させる。
    fn finalize_file(
        file_patches: &mut Vec<FilePatch>,
        current_file: &mut Option<FilePatch>,
        current_hunk: &mut Option<Hunk>,
        content_lines: &[String],
        add_content_mode: bool,
        next_old_start: &mut usize,
        next_new_start: &mut usize,
    ) -> Result<(), ToolError> {
        let Some(mut file) = current_file.take() else {
            return Ok(());
        };
        if let Some(hunk) = current_hunk.take() {
            file.hunks.push(hunk);
        }
        if add_content_mode {
            let content = content_lines.join("\n");
            file.hunks.push(Hunk {
                old_start: 1,
                old_count: 0,
                new_start: 1,
                new_count: content.lines().count(),
                lines: content
                    .lines()
                    .map(|l| PatchLine::Add(l.to_string()))
                    .collect(),
                declared_counts: None,
                // Begin File + Move To のときは旧内容を連結しない。
                replace_entire_file: matches!(file.operation, PatchOperation::Move { .. }),
            });
        }
        for hunk in file.hunks.iter_mut() {
            if hunk.replace_entire_file {
                continue;
            }
            let (old_count, new_count) = count_lines(&hunk.lines);
            match hunk.declared_counts {
                Some((declared_old, declared_new)) => {
                    // 明示された宣言行数が本文と食い違う場合は、途中切れ等の
                    // 壊れたパッチとして拒否する。git diff 形式と検証強度を揃える。
                    if declared_old != old_count || declared_new != new_count {
                        return Err(ToolError::InvalidArguments(format!(
                            "Model patch hunk line-count mismatch: declared {} old / {} new lines, but found {} old / {} new lines",
                            declared_old, declared_new, old_count, new_count
                        )));
                    }
                }
                None => {
                    // 裸の `@@` あるいは行数省略形式は本文から算出する（従来どおり）。
                    hunk.old_count = old_count;
                    hunk.new_count = new_count;
                }
            }
        }
        // 純粋な rename（hunk なしの Move）も移送対象として残す。
        if !file.hunks.is_empty() || matches!(file.operation, PatchOperation::Move { .. }) {
            file_patches.push(file);
        }
        *next_old_start = 1;
        *next_new_start = 1;
        Ok(())
    }

    for line in patch_text.lines() {
        if line == "*** Begin Patch" {
            continue;
        }

        if let Some(caps) = begin_file.captures(line) {
            finalize_file(
                &mut file_patches,
                &mut current_file,
                &mut current_hunk,
                &content_lines,
                in_content,
                &mut next_old_start,
                &mut next_new_start,
            )?;
            let path = caps[1].trim().to_string();
            current_file = Some(FilePatch {
                path,
                operation: PatchOperation::Add,
                hunks: Vec::new(),
            });
            content_lines.clear();
            current_hunk = None;
            in_content = true;
            in_hunk = false;
            continue;
        }

        if let Some(caps) = update_file.captures(line) {
            finalize_file(
                &mut file_patches,
                &mut current_file,
                &mut current_hunk,
                &content_lines,
                in_content,
                &mut next_old_start,
                &mut next_new_start,
            )?;
            let path = caps[1].trim().to_string();
            current_file = Some(FilePatch {
                path,
                operation: PatchOperation::Update,
                hunks: Vec::new(),
            });
            content_lines.clear();
            current_hunk = None;
            in_content = false;
            in_hunk = false;
            continue;
        }

        // Detect delete file directive
        if let Some(caps) = delete_file.captures(line) {
            finalize_file(
                &mut file_patches,
                &mut current_file,
                &mut current_hunk,
                &content_lines,
                in_content,
                &mut next_old_start,
                &mut next_new_start,
            )?;
            let path = caps[1].trim().to_string();
            file_patches.push(FilePatch {
                path,
                operation: PatchOperation::Delete,
                hunks: Vec::new(),
            });
            content_lines.clear();
            current_hunk = None;
            in_content = false;
            in_hunk = false;
            continue;
        }

        // Detect move-to directive (applies to current file).
        // 標準的な apply_patch では `@@` より前に現れるため、ここでは
        // コンテンツ読み取り状態を変えずに operation だけを差し替える。
        if let Some(caps) = move_to.captures(line) {
            if let Some(ref mut file) = current_file {
                let target = caps[1].trim().to_string();
                file.operation = PatchOperation::Move { move_path: target };
            }
            continue;
        }

        if line == "*** End File" {
            finalize_file(
                &mut file_patches,
                &mut current_file,
                &mut current_hunk,
                &content_lines,
                in_content,
                &mut next_old_start,
                &mut next_new_start,
            )?;
            content_lines.clear();
            current_hunk = None;
            in_content = false;
            in_hunk = false;
            continue;
        }

        if end_patch.is_match(line) {
            finalize_file(
                &mut file_patches,
                &mut current_file,
                &mut current_hunk,
                &content_lines,
                in_content,
                &mut next_old_start,
                &mut next_new_start,
            )?;
            break;
        }

        // hunk ヘッダは Update/Move ファイルのときだけ解釈する。
        // Add ファイルのコンテンツが `@@` で始まる場合は通常行として扱う。
        let is_update_or_move = matches!(
            current_file.as_ref().map(|f| &f.operation),
            Some(PatchOperation::Update) | Some(PatchOperation::Move { .. })
        );
        if is_update_or_move {
            if let Some(caps) = hunk_header.captures(line) {
                if let Some(file) = current_file.as_mut() {
                    if let Some(hunk) = current_hunk.take() {
                        let (old_count, new_count) = count_lines(&hunk.lines);
                        next_old_start = hunk.old_start + old_count;
                        next_new_start = hunk.new_start + new_count;
                        file.hunks.push(hunk);
                    }
                }
                let parse_num = |value: &str| -> Result<usize, ToolError> {
                    value.parse::<usize>().map_err(|e| {
                        ToolError::InvalidArguments(format!("invalid line number: {}", e))
                    })
                };
                let old_start = match caps.get(1) {
                    Some(m) => parse_num(m.as_str())?,
                    None => next_old_start,
                };
                let new_start = match caps.get(3) {
                    Some(m) => parse_num(m.as_str())?,
                    None => next_new_start,
                };
                // カンマ付きで行数が明示された場合だけ検証対象にする。
                // `@@ -a +b @@` は行数省略（git diff では既定 1）だが、LLM 出力に
                // 寛容なため本文から算出する（None）として扱う。
                let declared_old = match caps.get(2) {
                    Some(m) => Some(parse_num(m.as_str())?),
                    None => None,
                };
                let declared_new = match caps.get(4) {
                    Some(m) => Some(parse_num(m.as_str())?),
                    None => None,
                };
                let declared_counts = match (declared_old, declared_new) {
                    (Some(old), Some(new)) => Some((old, new)),
                    _ => None,
                };
                let (old_count, new_count) = declared_counts.unwrap_or((0, 0));
                current_hunk = Some(Hunk {
                    old_start,
                    old_count,
                    new_start,
                    new_count,
                    lines: Vec::new(),
                    declared_counts,
                    replace_entire_file: false,
                });
                in_hunk = true;
                in_content = false;
                continue;
            }
        }

        if in_hunk {
            if let Some(ref mut hunk) = current_hunk {
                // 先頭の 1 文字だけが PatchLine の種別を表す。
                // `__pycache__/foo.pyc` のような接頭辞の無いノイズ行は無視する。
                if let Some(rest) = line.strip_prefix(' ') {
                    hunk.lines.push(PatchLine::Context(rest.to_string()));
                } else if line.starts_with('-') && !line.starts_with("--- ") {
                    hunk.lines.push(PatchLine::Remove(line[1..].to_string()));
                } else if line.starts_with('+') && !line.starts_with("+++") {
                    hunk.lines.push(PatchLine::Add(line[1..].to_string()));
                }
            }
            continue;
        }

        if in_content {
            content_lines.push(line.to_string());
        }
    }

    // `*** End Patch` が無い場合でも解析済みの内容は保存する。
    finalize_file(
        &mut file_patches,
        &mut current_file,
        &mut current_hunk,
        &content_lines,
        in_content,
        &mut next_old_start,
        &mut next_new_start,
    )?;

    Ok(file_patches)
}

async fn process_file_patch(
    base_path: &Path,
    file_patch: &FilePatch,
    ctx: &ToolContext,
) -> Result<FileChange, ToolError> {
    let file_path = resolve_patch_path(base_path, &file_patch.path);
    let relative_path = file_patch.path.clone();

    match file_patch.operation {
        PatchOperation::Add => {
            let new_content = file_patch
                .hunks
                .iter()
                .flat_map(|h| h.lines.iter())
                .filter_map(|l| match l {
                    PatchLine::Add(s) => Some(s.clone()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("\n");

            let diff = format!(
                "--- /dev/null\n+++ b/{}\n{}",
                relative_path,
                file_patch
                    .hunks
                    .iter()
                    .flat_map(|h| h.lines.iter())
                    .map(|l| match l {
                        PatchLine::Add(s) => format!("+{}", s),
                        _ => String::new(),
                    })
                    .collect::<Vec<_>>()
                    .join("\n")
            );

            Ok(FileChange {
                relative_path,
                operation: PatchOperation::Add,
                old_content: String::new(),
                new_content: new_content + "\n",
                diff,
            })
        }
        PatchOperation::Update | PatchOperation::Move { .. } => {
            let old_content = tokio::fs::read_to_string(&file_path).await.map_err(|e| {
                ToolError::execution_with_source(format!("Failed to read file: {}", e), e)
            })?;

            // 既存ファイルを更新する前に、直近の read 時に記録した mtime と突き合わせて
            // 外部からの並行変更（競合編集）を検出する。edit/tool.rs と同じ順序。
            if file_path.exists() {
                ctx.do_file_time_assert(file_path.to_string_lossy().to_string())
                    .await?;
            }

            let new_content = apply_hunks(&old_content, &file_patch.hunks)?;

            let diff = generate_diff(&relative_path, &old_content, &new_content);

            Ok(FileChange {
                relative_path,
                operation: file_patch.operation.clone(),
                old_content,
                new_content,
                diff,
            })
        }
        PatchOperation::Delete => {
            let old_content = tokio::fs::read_to_string(&file_path).await.map_err(|e| {
                ToolError::execution_with_source(format!("Failed to read file: {}", e), e)
            })?;

            if file_path.exists() {
                ctx.do_file_time_assert(file_path.to_string_lossy().to_string())
                    .await?;
            }

            let diff = format!(
                "--- a/{}\n+++ /dev/null\n{}",
                relative_path,
                old_content
                    .lines()
                    .map(|l| format!("-{}", l))
                    .collect::<Vec<_>>()
                    .join("\n")
            );

            Ok(FileChange {
                relative_path,
                operation: PatchOperation::Delete,
                old_content,
                new_content: String::new(),
                diff,
            })
        }
    }
}

fn apply_hunks(original: &str, hunks: &[Hunk]) -> Result<String, ToolError> {
    // `*** Move To:` に Add-only hunk が転用された場合、旧内容を一切残さず
    // 追加行だけを新内容にする。git diff 由来の Move は通常の hunk を持つため
    // この分岐には入らず、従来どおり apply_hunks の照合・カウント検証を通る。
    if hunks.len() == 1 && hunks[0].replace_entire_file {
        return Ok(hunks[0]
            .lines
            .iter()
            .filter_map(|line| match line {
                PatchLine::Add(s) => Some(s.clone()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n"));
    }

    let mut lines: Vec<String> = original.lines().map(|s| s.to_string()).collect();
    let mut offset = 0isize;

    for (hunk_index, hunk) in hunks.iter().enumerate() {
        let hunk_no = hunk_index + 1;

        // 宣言された行数と hunk 本体の行数が食い違う場合は stale/不正なパッチとして
        // 拒否する（従来は warn のみで黙って適用していたため、行数ずれで静かに
        // ファイルが壊れる可能性があった）。
        let actual_old = hunk
            .lines
            .iter()
            .filter(|l| matches!(l, PatchLine::Remove(_) | PatchLine::Context(_)))
            .count();
        let actual_new = hunk
            .lines
            .iter()
            .filter(|l| matches!(l, PatchLine::Add(_) | PatchLine::Context(_)))
            .count();

        if actual_old != hunk.old_count {
            return Err(ToolError::InvalidArguments(format!(
                "Hunk {} old_count mismatch: declared {} but found {} lines",
                hunk_no, hunk.old_count, actual_old
            )));
        }
        if actual_new != hunk.new_count {
            return Err(ToolError::InvalidArguments(format!(
                "Hunk {} new_count mismatch: declared {} but found {} lines",
                hunk_no, hunk.new_count, actual_new
            )));
        }

        let start_signed = hunk.old_start as isize + offset - 1;
        if start_signed < 0 || start_signed as usize > lines.len() {
            return Err(ToolError::ExecutionError(format!(
                "Hunk {} start line {} is out of range for file with {} lines",
                hunk_no,
                hunk.old_start,
                lines.len()
            )));
        }
        let start = start_signed as usize;

        // 従来どおり位置のズレはデバッグログに残す（致命エラーにはしない）。
        let expected_new_pos = (hunk.new_start as isize - 1).max(0) as usize;
        if hunk.new_start > 0 && start != expected_new_pos && offset != 0 {
            tracing::debug!(
                "Hunk position drift: old_start={} + offset={} = {}, new_start={}",
                hunk.old_start,
                offset,
                start + 1,
                hunk.new_start
            );
        }

        // Context/Remove 行は「元ファイルの対応位置」のテキストと完全一致しなければ
        // ならない。不一致・範囲外は stale パッチとしてエラーにし、ここで即座に
        // 戻ることで部分的な適用（ファイル書換え）を防ぐ。
        // 前方の hunk による差分は offset と lines に反映済みなので、
        // start 位置の lines が元ファイルの old_start 位置に対応する。
        let mut cursor = start;
        let mut rebuilt: Vec<String> = Vec::with_capacity(lines.len());
        rebuilt.extend_from_slice(&lines[..start]);

        for line in &hunk.lines {
            match line {
                PatchLine::Context(expected) | PatchLine::Remove(expected) => {
                    if cursor >= lines.len() {
                        return Err(ToolError::ExecutionError(format!(
                            "Hunk {} references line {} which is out of range for file with {} lines",
                            hunk_no,
                            cursor + 1,
                            lines.len()
                        )));
                    }
                    let actual = &lines[cursor];
                    if actual != expected {
                        return Err(ToolError::ExecutionError(format!(
                            "Hunk {} context/remove line {} mismatch: expected '{}' but found '{}'",
                            hunk_no,
                            cursor + 1,
                            expected,
                            actual
                        )));
                    }
                    // Context 行はそのまま残し、Remove 行は落とす。
                    if matches!(line, PatchLine::Context(_)) {
                        rebuilt.push(actual.clone());
                    }
                    cursor += 1;
                }
                PatchLine::Add(added_line) => {
                    rebuilt.push(added_line.clone());
                }
            }
        }
        rebuilt.extend_from_slice(&lines[cursor..]);
        lines = rebuilt;

        let added = hunk
            .lines
            .iter()
            .filter(|l| matches!(l, PatchLine::Add(_)))
            .count() as isize;
        let removed = hunk
            .lines
            .iter()
            .filter(|l| matches!(l, PatchLine::Remove(_)))
            .count() as isize;
        offset += added - removed;
    }

    Ok(lines.join("\n"))
}

fn generate_diff(path: &str, old: &str, new: &str) -> String {
    let old_lines: Vec<&str> = old.lines().collect();
    let new_lines: Vec<&str> = new.lines().collect();

    let mut diff = format!("--- a/{}\n+++ b/{}\n", path, path);

    let old_count = old_lines.len();
    let new_count = new_lines.len();

    diff.push_str(&format!(
        "@@ -1,{} +1,{} @@\n",
        old_count.max(1),
        new_count.max(1)
    ));

    for line in &old_lines {
        diff.push_str(&format!("-{}\n", line));
    }
    for line in &new_lines {
        diff.push_str(&format!("+{}\n", line));
    }

    diff
}

async fn collect_lsp_diagnostics_for_targets(
    base_path: &Path,
    targets: &[String],
    ctx: &ToolContext,
) -> (String, HashMap<String, Vec<serde_json::Value>>) {
    #[cfg(feature = "lsp")]
    {
        use opencode_lsp::detect_language;
        use std::collections::HashSet;
        use std::time::Duration;

        let Some(lsp_registry) = &ctx.lsp_registry else {
            return (String::new(), HashMap::new());
        };

        let clients = lsp_registry.list().await;
        if clients.is_empty() {
            return (String::new(), HashMap::new());
        }

        let mut unique_targets = Vec::new();
        let mut seen = HashSet::new();
        for target in targets {
            if seen.insert(target.clone()) {
                unique_targets.push(target.clone());
            }
        }

        let mut output_parts = Vec::new();
        let mut diagnostics_meta: HashMap<String, Vec<serde_json::Value>> = HashMap::new();

        for target in unique_targets {
            let path = base_path.join(&target);
            let language = detect_language(&path);
            let client = clients
                .iter()
                .find(|(id, _)| id.contains(language))
                .map(|(_, client)| client.clone());

            let Some(client) = client else {
                continue;
            };

            if let Ok(content) = tokio::fs::read_to_string(&path).await {
                let _ = client.open_document(&path, &content, language).await;
            }

            tokio::time::sleep(Duration::from_millis(100)).await;

            let diagnostics = client.get_diagnostics(&path).await;
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

            diagnostics_meta.insert(
                target.clone(),
                limited
                    .iter()
                    .map(|d| {
                        serde_json::json!({
                            "line": d.range.start.line + 1,
                            "message": d.message,
                            "severity": "error",
                        })
                    })
                    .collect(),
            );

            output_parts.push(format!(
                "LSP errors detected in {}, please fix:\n<diagnostics file=\"{}\">\n{}{}\n</diagnostics>",
                target,
                path.display(),
                lines.join("\n"),
                suffix
            ));
        }

        return (output_parts.join("\n\n"), diagnostics_meta);
    }

    #[cfg(not(feature = "lsp"))]
    {
        let _ = (base_path, targets, ctx);
        (String::new(), HashMap::new())
    }
}

impl Default for ApplyPatchTool {
    fn default() -> Self {
        Self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_and_schema() {
        let tool = ApplyPatchTool;
        assert_eq!(tool.id(), "apply_patch");
        assert_eq!(tool.parameters()["required"][0], "patchText");
    }

    #[test]
    fn parse_add_file_patch() {
        let text = "*** Begin Patch\n*** Begin File: src/new.rs\n+fn main() {}\n*** End File\n*** End Patch";
        let patches = parse_model_patch(text).unwrap();

        assert_eq!(patches.len(), 1);
        assert_eq!(patches[0].path, "src/new.rs");
        assert!(patches[0].operation.is_add());
    }

    #[test]
    fn parse_delete_file_patch() {
        let text = "*** Begin Patch\n*** Delete File: old.rs\n*** End Patch";
        let patches = parse_model_patch(text).unwrap();

        assert_eq!(patches.len(), 1);
        assert!(matches!(patches[0].operation, PatchOperation::Delete));
    }

    #[test]
    fn parse_empty_patch() {
        let patches = parse_model_patch("*** Begin Patch\n*** End Patch").unwrap();
        assert!(patches.is_empty());
    }

    #[test]
    fn parse_multi_file_patch_rejects_overflowing_line_numbers() {
        let patch = concat!(
            "--- a/file.rs\n",
            "+++ b/file.rs\n",
            "@@ -999999999999999999999999999999 +1 @@\n",
            " old\n",
            " new"
        );
        assert!(
            parse_multi_file_patch(patch).is_err(),
            "overflowing line numbers must be an error, not a silent fallback to 1"
        );
    }

    #[test]
    fn patch_operation_is_add_only_for_add() {
        assert!(PatchOperation::Add.is_add());
        assert!(!PatchOperation::Update.is_add());
        assert!(!PatchOperation::Delete.is_add());
        assert!(!PatchOperation::Move {
            move_path: "x".to_owned()
        }
        .is_add());
    }

    #[test]
    fn resolve_patch_path_keeps_absolute_and_joins_relative() {
        let base = Path::new("/project");
        assert_eq!(
            resolve_patch_path(base, "/etc/passwd"),
            PathBuf::from("/etc/passwd")
        );
        assert_eq!(
            resolve_patch_path(base, "src/main.rs"),
            base.join("src/main.rs")
        );
    }

    fn recording_context(
        directory: &Path,
    ) -> (
        ToolContext,
        std::sync::Arc<std::sync::Mutex<Vec<PermissionRequest>>>,
    ) {
        let asks = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let asks_cb = asks.clone();
        let ctx = ToolContext::new(
            "s".to_owned(),
            "m".to_owned(),
            directory.to_string_lossy().to_string(),
        )
        .with_ask(move |request| {
            let asks = asks_cb.clone();
            async move {
                asks.lock().unwrap().push(request);
                Ok(())
            }
        });
        (ctx, asks)
    }

    fn rename_patch(move_to: &str) -> String {
        format!(
            concat!(
                "diff --git a/src.txt b/src.txt\n",
                "rename from src.txt\n",
                "rename to {}\n",
                "--- a/src.txt\n",
                "+++ b/src.txt\n",
                "@@ -1 +1 @@\n",
                "-old\n",
                "+new\n"
            ),
            move_to
        )
    }

    #[tokio::test]
    async fn move_to_external_absolute_path_requests_external_directory() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("project");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("src.txt"), "old\n").unwrap();

        // project_root の外側（tmp 直下）への絶対パス Move。
        let external = tmp.path().join("outside.txt");
        let external_str = external.to_string_lossy().to_string();
        let (ctx, asks) = recording_context(&root);

        // コールバックは拒否せず記録するだけなので実行自体は成功し得るが、
        // 検証対象は external_directory 権限が要求されたことのみ。
        let _ = ApplyPatchTool
            .execute(
                serde_json::json!({ "patchText": rename_patch(&external_str) }),
                ctx,
            )
            .await;

        let asks = asks.lock().unwrap();
        let external_ask = asks
            .iter()
            .find(|r| r.permission == "external_directory")
            .unwrap_or_else(|| {
                panic!(
                    "move_path outside project_root must request external_directory: {:?}",
                    asks.iter()
                        .map(|r| r.permission.clone())
                        .collect::<Vec<_>>()
                )
            });
        assert_eq!(
            external_ask
                .metadata
                .get("filepath")
                .and_then(|v| v.as_str()),
            Some(external_str.as_str())
        );
        assert!(!external_ask.patterns.is_empty());
    }

    #[tokio::test]
    async fn move_to_internal_absolute_path_does_not_request_external_directory() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("project");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("src.txt"), "old\n").unwrap();
        // 移送先は意図的に作成しない（本番の Move と同じ状態）。

        let internal_str = root.join("dst.txt").to_string_lossy().to_string();
        let (ctx, asks) = recording_context(&root);

        ApplyPatchTool
            .execute(
                serde_json::json!({ "patchText": rename_patch(&internal_str) }),
                ctx,
            )
            .await
            .unwrap();

        let asks = asks.lock().unwrap();
        assert!(
            !asks.iter().any(|r| r.permission == "external_directory"),
            "a non-existent destination inside project_root must not request external_directory"
        );
        assert_eq!(
            std::fs::read_to_string(root.join("dst.txt")).unwrap(),
            "new"
        );
    }

    #[tokio::test]
    async fn move_to_internal_path_with_missing_parent_does_not_request_external_directory() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("project");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("src.txt"), "old\n").unwrap();

        // 親ディレクトリも存在しない移送先。祖先を辿って project_root に
        // 到達できれば external_directory は要求されないはず。
        let internal_str = root.join("newdir/dst.txt").to_string_lossy().to_string();
        let (ctx, asks) = recording_context(&root);

        ApplyPatchTool
            .execute(
                serde_json::json!({ "patchText": rename_patch(&internal_str) }),
                ctx,
            )
            .await
            .unwrap();

        let asks = asks.lock().unwrap();
        assert!(
            !asks.iter().any(|r| r.permission == "external_directory"),
            "a missing parent directory inside project_root must not request external_directory"
        );
        assert_eq!(
            std::fs::read_to_string(root.join("newdir/dst.txt")).unwrap(),
            "new"
        );
    }

    #[tokio::test]
    async fn move_to_external_path_with_missing_parent_still_requests_external_directory() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("project");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("src.txt"), "old\n").unwrap();

        // project_root 外の存在しない親ディレクトリ配下への Move は
        // フォールバック後も外部と判定され、権限が要求され続けること。
        let external = tmp.path().join("outside/nested/dst.txt");
        let external_str = external.to_string_lossy().to_string();
        let (ctx, asks) = recording_context(&root);

        let _ = ApplyPatchTool
            .execute(
                serde_json::json!({ "patchText": rename_patch(&external_str) }),
                ctx,
            )
            .await;

        let asks = asks.lock().unwrap();
        assert!(
            asks.iter().any(|r| r.permission == "external_directory"),
            "a non-existent destination outside project_root must still request external_directory"
        );
    }

    // ---- stale patch 検証 (R2 regression) ------------------------------------

    #[test]
    fn apply_hunks_applies_valid_interleaved_context() {
        // 正常系: Context を挟む hunk が従来どおり適用できること。
        let hunk = Hunk {
            old_start: 1,
            old_count: 3,
            new_start: 1,
            new_count: 3,
            lines: vec![
                PatchLine::Context("line1".into()),
                PatchLine::Remove("line2".into()),
                PatchLine::Add("NEW2".into()),
                PatchLine::Context("line3".into()),
            ],
            declared_counts: None,
            replace_entire_file: false,
        };
        assert_eq!(
            apply_hunks("line1\nline2\nline3", &[hunk]).unwrap(),
            "line1\nNEW2\nline3"
        );
    }

    #[test]
    fn apply_hunks_rejects_remove_text_mismatch_without_partial_result() {
        // 1 つ目の hunk は成功するが、2 つ目の Remove 行が食い違う。
        // エラーが返り、部分的な結果（文字列）が一切出ないことを確認する。
        let hunks = vec![
            Hunk {
                old_start: 1,
                old_count: 1,
                new_start: 1,
                new_count: 1,
                lines: vec![
                    PatchLine::Remove("line1".into()),
                    PatchLine::Add("NEW1".into()),
                ],
                declared_counts: None,
                replace_entire_file: false,
            },
            Hunk {
                old_start: 2,
                old_count: 1,
                new_start: 2,
                new_count: 1,
                lines: vec![
                    PatchLine::Remove("WRONG".into()),
                    PatchLine::Add("NEW2".into()),
                ],
                declared_counts: None,
                replace_entire_file: false,
            },
        ];
        let err = apply_hunks("line1\nline2\nline3", &hunks).unwrap_err();
        assert!(
            matches!(&err, ToolError::ExecutionError(_)),
            "stale Remove text must be an ExecutionError, got {:?}",
            err
        );
    }

    #[test]
    fn apply_hunks_rejects_context_text_mismatch() {
        let hunk = Hunk {
            old_start: 1,
            old_count: 3,
            new_start: 1,
            new_count: 3,
            lines: vec![
                PatchLine::Context("line1".into()),
                PatchLine::Remove("line2".into()),
                PatchLine::Add("NEW2".into()),
                PatchLine::Context("WRONG".into()),
            ],
            declared_counts: None,
            replace_entire_file: false,
        };
        let err = apply_hunks("line1\nline2\nline3", &[hunk]).unwrap_err();
        assert!(
            matches!(&err, ToolError::ExecutionError(_)),
            "stale Context text must be an ExecutionError, got {:?}",
            err
        );
    }

    #[test]
    fn apply_hunks_rejects_declared_count_mismatch() {
        let hunk = Hunk {
            old_start: 1,
            old_count: 5, // 実際の Remove+Context は 1 行
            new_start: 1,
            new_count: 1,
            lines: vec![
                PatchLine::Remove("line1".into()),
                PatchLine::Add("NEW".into()),
            ],
            declared_counts: None,
            replace_entire_file: false,
        };
        let err = apply_hunks("line1\nline2", &[hunk]).unwrap_err();
        assert!(
            matches!(&err, ToolError::InvalidArguments(_)),
            "declared/actual line-count mismatch must be an InvalidArguments error, got {:?}",
            err
        );
    }

    #[test]
    fn apply_hunks_rejects_out_of_range_hunk() {
        let hunk = Hunk {
            old_start: 99,
            old_count: 1,
            new_start: 99,
            new_count: 1,
            lines: vec![
                PatchLine::Remove("line1".into()),
                PatchLine::Add("NEW".into()),
            ],
            declared_counts: None,
            replace_entire_file: false,
        };
        let err = apply_hunks("line1\nline2", &[hunk]).unwrap_err();
        assert!(
            matches!(&err, ToolError::ExecutionError(_)),
            "out-of-range hunk must be an ExecutionError, got {:?}",
            err
        );
    }

    #[tokio::test]
    async fn stale_patches_are_rejected_and_leave_file_unchanged() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("project");
        std::fs::create_dir_all(&root).unwrap();
        let file = root.join("file.txt");
        let original = "alpha\nbeta\ngamma\n";

        let stale_patches = [
            // 行番号ずれ: old_start=2 だが 2 行目の Context は "alpha" ではない。
            concat!(
                "--- a/file.txt\n",
                "+++ b/file.txt\n",
                "@@ -2,3 +2,3 @@\n",
                " alpha\n",
                "-beta\n",
                "+BETA\n",
                " gamma\n"
            ),
            // テキスト不一致: 行番号は正しいが Remove 行が実ファイルと食い違う。
            concat!(
                "--- a/file.txt\n",
                "+++ b/file.txt\n",
                "@@ -2,1 +2,1 @@\n",
                "-WRONG\n",
                "+BETA\n"
            ),
            // 範囲外: ファイルの行数より後ろを指す hunk。
            concat!(
                "--- a/file.txt\n",
                "+++ b/file.txt\n",
                "@@ -90,1 +90,1 @@\n",
                "-alpha\n",
                "+BETA\n"
            ),
        ];

        for patch in stale_patches {
            std::fs::write(&file, original).unwrap();
            let (ctx, _asks) = recording_context(&root);
            let result = ApplyPatchTool
                .execute(serde_json::json!({ "patchText": patch }), ctx)
                .await;
            assert!(result.is_err(), "stale patch must be rejected: {}", patch);
            assert_eq!(
                std::fs::read_to_string(&file).unwrap(),
                original,
                "file must remain unchanged for stale patch: {}",
                patch
            );
        }
    }

    // ---- model patch: Update / Move (R2 受け入れ条件) -------------------------

    #[test]
    fn parse_update_file_patch_with_hunk() {
        let text = concat!(
            "*** Begin Patch\n",
            "*** Update File: src/main.rs\n",
            "@@ -1,3 +1,3 @@\n",
            " fn main() {\n",
            "-    println!(\"old\");\n",
            "+    println!(\"new\");\n",
            " }\n",
            "*** End Patch"
        );
        let patches = parse_model_patch(text).unwrap();

        assert_eq!(patches.len(), 1);
        assert_eq!(patches[0].path, "src/main.rs");
        assert!(matches!(patches[0].operation, PatchOperation::Update));
        assert_eq!(patches[0].hunks.len(), 1);

        let hunk = &patches[0].hunks[0];
        assert_eq!(hunk.old_start, 1);
        assert_eq!(hunk.new_start, 1);
        // 2 Context + 1 Remove / 1 Add から算出されること。
        assert_eq!(hunk.old_count, 3);
        assert_eq!(hunk.new_count, 3);
        assert_eq!(hunk.lines.len(), 4);
        assert!(matches!(&hunk.lines[0], PatchLine::Context(s) if s == "fn main() {"));
        assert!(matches!(&hunk.lines[1], PatchLine::Remove(s) if s == "    println!(\"old\");"));
        assert!(matches!(&hunk.lines[2], PatchLine::Add(s) if s == "    println!(\"new\");"));
        assert!(matches!(&hunk.lines[3], PatchLine::Context(s) if s == "}"));
    }

    #[test]
    fn parse_update_file_patch_rejects_declared_count_mismatch() {
        // 宣言は old=3/new=3 だが本文は Remove 1 / Add 1 だけ（途中切れ相当）。
        // 本文から再計算して上書きすると静かな部分適用になるため拒否する。
        let text = concat!(
            "*** Begin Patch\n",
            "*** Update File: file.txt\n",
            "@@ -1,3 +1,3 @@\n",
            "-only line\n",
            "+replacement\n",
            "*** End Patch"
        );
        let err = parse_model_patch(text).unwrap_err();
        assert!(
            matches!(&err, ToolError::InvalidArguments(_)),
            "declared line-count mismatch in model patch must be InvalidArguments, got {:?}",
            err
        );
    }

    #[test]
    fn parse_update_file_patch_bare_hunk_ignores_noise_lines() {
        let text = concat!(
            "*** Begin Patch\n",
            "*** Update File: file.txt\n",
            "@@\n",
            "-old\n",
            "+new\n",
            "__pycache__/noise.pyc\n",
            "*** End Patch"
        );
        let patches = parse_model_patch(text).unwrap();

        assert_eq!(patches.len(), 1);
        assert_eq!(patches[0].hunks.len(), 1);
        let hunk = &patches[0].hunks[0];
        // 接頭辞の無い `__pycache__/...` は PatchLine に変換されない。
        assert_eq!(hunk.lines.len(), 2);
        assert_eq!(hunk.old_count, 1);
        assert_eq!(hunk.new_count, 1);
    }

    #[test]
    fn parse_update_file_move_patch() {
        let text = concat!(
            "*** Begin Patch\n",
            "*** Update File: old.txt\n",
            "*** Move To: new.txt\n",
            "@@ -1 +1 @@\n",
            "-old\n",
            "+new\n",
            "*** End Patch"
        );
        let patches = parse_model_patch(text).unwrap();

        assert_eq!(patches.len(), 1);
        assert_eq!(patches[0].path, "old.txt");
        assert!(
            matches!(&patches[0].operation, PatchOperation::Move { move_path } if move_path == "new.txt")
        );
        assert_eq!(patches[0].hunks.len(), 1);
        assert_eq!(patches[0].hunks[0].old_count, 1);
        assert_eq!(patches[0].hunks[0].new_count, 1);
    }

    #[tokio::test]
    async fn model_patch_update_writes_expected_content_via_execute() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("project");
        std::fs::create_dir_all(root.join("src")).unwrap();
        let file = root.join("src/main.rs");
        std::fs::write(&file, "fn main() {\n    println!(\"old\");\n}\n").unwrap();

        let patch = concat!(
            "*** Begin Patch\n",
            "*** Update File: src/main.rs\n",
            "@@ -1,3 +1,3 @@\n",
            " fn main() {\n",
            "-    println!(\"old\");\n",
            "+    println!(\"new\");\n",
            " }\n",
            "*** End Patch"
        );
        let (ctx, _asks) = recording_context(&root);
        ApplyPatchTool
            .execute(serde_json::json!({ "patchText": patch }), ctx)
            .await
            .unwrap();

        // 既存仕様どおり末尾改行は付かないが、更新行は正しく反映される。
        assert_eq!(
            std::fs::read_to_string(&file).unwrap(),
            "fn main() {\n    println!(\"new\");\n}"
        );
    }

    #[tokio::test]
    async fn model_patch_move_writes_only_new_content_to_destination() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("project");
        std::fs::create_dir_all(&root).unwrap();
        let old = root.join("old.txt");
        let new = root.join("new.txt");
        std::fs::write(&old, "old first\nold second\n").unwrap();

        let patch = concat!(
            "*** Begin Patch\n",
            "*** Update File: old.txt\n",
            "*** Move To: new.txt\n",
            "@@ -1,2 +1,2 @@\n",
            "-old first\n",
            "+NEW first\n",
            " old second\n",
            "*** End Patch"
        );
        let (ctx, _asks) = recording_context(&root);
        ApplyPatchTool
            .execute(serde_json::json!({ "patchText": patch }), ctx)
            .await
            .unwrap();

        assert!(!old.exists(), "移送元は削除されること");
        assert_eq!(
            std::fs::read_to_string(&new).unwrap(),
            "NEW first\nold second",
            "移送先に旧内容が連結されていないこと"
        );
    }

    #[tokio::test]
    async fn model_patch_move_from_add_content_discards_old_content() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("project");
        std::fs::create_dir_all(&root).unwrap();
        let old = root.join("old.txt");
        let new = root.join("new.txt");
        std::fs::write(&old, "STALE CONTENT THAT MUST NOT SURVIVE\n").unwrap();

        // Add-only hunk を Move に転用した従来の壊れる形。
        let patch = concat!(
            "*** Begin Patch\n",
            "*** Begin File: old.txt\n",
            "*** Move To: new.txt\n",
            "brand new content\n",
            "*** End File\n",
            "*** End Patch"
        );
        let (ctx, _asks) = recording_context(&root);
        ApplyPatchTool
            .execute(serde_json::json!({ "patchText": patch }), ctx)
            .await
            .unwrap();

        assert!(!old.exists(), "移送元は削除されること");
        assert_eq!(
            std::fs::read_to_string(&new).unwrap(),
            "brand new content",
            "旧内容が連結されず新内容のみが配置されること"
        );
    }
}

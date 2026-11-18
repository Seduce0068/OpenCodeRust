use async_trait::async_trait;
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::{
    with_file_lock, Metadata, PermissionRequest, Tool, ToolContext, ToolError, ToolResult,
};

pub struct ApplyPatchTool;
#[cfg(feature = "lsp")]
const MAX_DIAGNOSTICS_PER_FILE: usize = 20;

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

fn parse_model_patch(patch_text: &str) -> Result<Vec<FilePatch>, ToolError> {
    let mut file_patches = Vec::new();
    let begin_file = Regex::new(r"^\*\*\*\s+Begin\s+File:\s+(.+)$").unwrap();
    let delete_file = Regex::new(r"^\*\*\*\s+Delete\s+File:\s+(.+)$").unwrap();
    let end_patch = Regex::new(r"^\*\*\*\s+End\s+Patch$").unwrap();
    let move_to = Regex::new(r"^\*\*\*\s+Move\s+To:\s+(.+)$").unwrap();

    let mut current_file: Option<FilePatch> = None;
    let mut content_lines: Vec<String> = Vec::new();
    let mut in_content = false;

    for line in patch_text.lines() {
        if line == "*** Begin Patch" {
            continue;
        }

        // Detect delete file directive
        if let Some(caps) = delete_file.captures(line) {
            if let Some(file) = current_file.take() {
                if !content_lines.is_empty() {
                    file_patches.push(file);
                }
            }
            let path = caps[1].trim().to_string();
            file_patches.push(FilePatch {
                path,
                operation: PatchOperation::Delete,
                hunks: Vec::new(),
            });
            content_lines = Vec::new();
            in_content = false;
            continue;
        }

        // Detect move-to directive (applies to current file)
        if let Some(caps) = move_to.captures(line) {
            if let Some(ref mut file) = current_file {
                let target = caps[1].trim().to_string();
                file.operation = PatchOperation::Move { move_path: target };
            }
            continue;
        }

        if let Some(caps) = begin_file.captures(line) {
            if let Some(file) = current_file.take() {
                if !content_lines.is_empty() {
                    file_patches.push(file);
                }
            }
            let path = caps[1].trim().to_string();
            current_file = Some(FilePatch {
                path: path.clone(),
                operation: PatchOperation::Add,
                hunks: Vec::new(),
            });
            content_lines = Vec::new();
            in_content = true;
            continue;
        }

        if line == "*** End File" {
            if let Some(file) = current_file.take() {
                let content = content_lines.join("\n");
                let hunk = Hunk {
                    old_start: 1,
                    old_count: 0,
                    new_start: 1,
                    new_count: content.lines().count(),
                    lines: content
                        .lines()
                        .map(|l| PatchLine::Add(l.to_string()))
                        .collect(),
                };
                let mut file = file;
                file.hunks.push(hunk);
                file_patches.push(file);
            }
            current_file = None;
            content_lines = Vec::new();
            in_content = false;
            continue;
        }

        if end_patch.is_match(line) {
            break;
        }

        if in_content {
            content_lines.push(line.to_string());
        }
    }

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
    let mut lines: Vec<String> = original.lines().map(|s| s.to_string()).collect();
    let mut offset = 0isize;

    for hunk in hunks {
        // Validate hunk line counts against declared counts
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

        if hunk.old_count > 0 && actual_old != hunk.old_count {
            tracing::warn!(
                "Hunk old_count mismatch: declared {} but found {} lines",
                hunk.old_count,
                actual_old
            );
        }
        if hunk.new_count > 0 && actual_new != hunk.new_count {
            tracing::warn!(
                "Hunk new_count mismatch: declared {} but found {} lines",
                hunk.new_count,
                actual_new
            );
        }

        let start = (hunk.old_start as isize + offset - 1).max(0) as usize;

        // Cross-check: expected new position should align with new_start
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

        let mut remove_count = 0;
        let add_lines: Vec<String> = hunk
            .lines
            .iter()
            .filter_map(|l| match l {
                PatchLine::Add(s) => Some(s.clone()),
                _ => None,
            })
            .collect();

        for line in &hunk.lines {
            if matches!(line, PatchLine::Remove(_)) {
                remove_count += 1;
            }
        }

        let insert_pos = start.min(lines.len());

        if insert_pos + remove_count <= lines.len() {
            for _ in 0..remove_count {
                lines.remove(insert_pos);
            }
        }

        for (i, add_line) in add_lines.into_iter().enumerate() {
            lines.insert(insert_pos + i, add_line);
        }

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
}

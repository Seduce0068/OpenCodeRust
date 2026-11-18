use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

use crate::{with_file_lock, PermissionRequest, Tool, ToolContext, ToolError, ToolResult};

pub struct MultiEditTool;

#[derive(Debug, Serialize, Deserialize)]
struct MultiEditInput {
    edits: Vec<FileEdit>,
}

#[derive(Debug, Serialize, Deserialize)]
struct FileEdit {
    file_path: String,
    edits: Vec<EditOperation>,
}

#[derive(Debug, Serialize, Deserialize)]
struct EditOperation {
    old_string: String,
    new_string: String,
    #[serde(default)]
    replace_all: bool,
}

#[async_trait]
impl Tool for MultiEditTool {
    fn id(&self) -> &str {
        "multiedit"
    }

    fn description(&self) -> &str {
        "Apply multiple string replacements across multiple files in a single atomic operation. Each file can have multiple edits applied in sequence."
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "edits": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "properties": {
                            "file_path": {
                                "type": "string",
                                "description": "The path to the file to edit"
                            },
                            "edits": {
                                "type": "array",
                                "items": {
                                    "type": "object",
                                    "properties": {
                                        "old_string": {
                                            "type": "string",
                                            "description": "The text to search for"
                                        },
                                        "new_string": {
                                            "type": "string",
                                            "description": "The text to replace it with"
                                        },
                                        "replace_all": {
                                            "type": "boolean",
                                            "default": false,
                                            "description": "Replace all occurrences"
                                        }
                                    },
                                    "required": ["old_string", "new_string"]
                                },
                                "description": "List of edits to apply to this file"
                            }
                        },
                        "required": ["file_path", "edits"]
                    },
                    "description": "List of files with their edits"
                }
            },
            "required": ["edits"]
        })
    }

    async fn execute(
        &self,
        args: serde_json::Value,
        ctx: ToolContext,
    ) -> Result<ToolResult, ToolError> {
        let input: MultiEditInput =
            serde_json::from_value(args).map_err(|e| ToolError::InvalidArguments(e.to_string()))?;

        let base_path = PathBuf::from(&ctx.directory);
        let mut results: Vec<String> = Vec::new();
        let mut total_edits = 0;
        let mut total_files = 0;

        for file_edit in input.edits {
            let file_label = file_edit.file_path.clone();

            // edit/tool.rs と同様に絶対パスはそのまま使い、相対パスのみ
            // ベースディレクトリへ結合する。
            let file_path = if Path::new(&file_label).is_absolute() {
                PathBuf::from(&file_label)
            } else {
                base_path.join(&file_label)
            };
            let path_str = file_path.to_string_lossy().to_string();

            if !file_path.exists() {
                return Err(ToolError::FileNotFound(format!(
                    "File not found: {}",
                    file_label
                )));
            }

            // プロジェクト外のパスは external_directory 権限を確認する。
            if ctx.is_external_path(&path_str) {
                let parent = file_path
                    .parent()
                    .map(|p| p.to_string_lossy().to_string())
                    .unwrap_or_else(|| path_str.clone());

                ctx.ask_permission(
                    PermissionRequest::new("external_directory")
                        .with_pattern(format!("{}/*", parent))
                        .with_metadata("filepath", serde_json::json!(&path_str))
                        .with_metadata("parentDir", serde_json::json!(parent)),
                )
                .await?;
            }

            let ctx_clone = ctx.clone();
            let path_clone = file_path.clone();
            let path_str_clone = path_str.clone();
            let file_label_clone = file_label.clone();
            let edits = file_edit.edits;

            let file_edits = with_file_lock(&path_str, || async move {
                let content = tokio::fs::read_to_string(&path_clone).await.map_err(|e| {
                    ToolError::execution_with_source(format!("Failed to read file: {}", e), e)
                })?;

                // 読取り後に mtime を検証し、外部変更との競合を検出する。
                ctx_clone
                    .do_file_time_assert(path_str_clone.clone())
                    .await?;

                let mut new_content = content.clone();
                let mut count_total = 0;

                for edit in edits {
                    let count = if edit.replace_all {
                        let count = new_content.matches(&edit.old_string).count();
                        new_content = new_content.replace(&edit.old_string, &edit.new_string);
                        count
                    } else {
                        if !new_content.contains(&edit.old_string) {
                            return Err(ToolError::ExecutionError(format!(
                                "Could not find '{}' in file {}",
                                edit.old_string, file_label_clone
                            )));
                        }
                        if let Some(pos) = new_content.find(&edit.old_string) {
                            let before = &new_content[..pos];
                            let after = &new_content[pos + edit.old_string.len()..];
                            new_content = format!("{}{}{}", before, edit.new_string, after);
                            1
                        } else {
                            0
                        }
                    };
                    count_total += count;
                }

                if count_total > 0 {
                    // edit/tool.rs と同じく書込み前に edit 権限を確認する。
                    let diff = create_diff(&path_str_clone, &content, &new_content);
                    ctx_clone
                        .ask_permission(
                            PermissionRequest::new("edit")
                                .with_pattern(&path_str_clone)
                                .with_metadata("diff", serde_json::json!(diff))
                                .always_allow(),
                        )
                        .await?;

                    tokio::fs::write(&path_clone, &new_content)
                        .await
                        .map_err(|e| {
                            ToolError::execution_with_source(
                                format!("Failed to write file: {}", e),
                                e,
                            )
                        })?;

                    ctx_clone
                        .do_publish_bus(
                            "file.edited",
                            serde_json::json!({ "file": file_label_clone.clone() }),
                        )
                        .await;

                    ctx_clone
                        .do_publish_bus(
                            "file_watcher.updated",
                            serde_json::json!({
                                "file": file_label_clone.clone(),
                                "event": "change"
                            }),
                        )
                        .await;

                    ctx_clone
                        .do_lsp_touch_file(file_label_clone.clone(), true)
                        .await?;
                    ctx_clone.do_file_time_read(path_str_clone.clone()).await?;
                }

                Ok::<usize, ToolError>(count_total)
            })
            .await?;

            if file_edits > 0 {
                results.push(format!("- {}: {} edit(s)", file_label, file_edits));
                total_edits += file_edits;
                total_files += 1;
            }
        }

        let output = if results.is_empty() {
            "No edits applied.".to_string()
        } else {
            format!(
                "Applied {} edit(s) across {} file(s):\n{}",
                total_edits,
                total_files,
                results.join("\n")
            )
        };

        Ok(ToolResult {
            title: format!("Multi-edit: {} edits in {} files", total_edits, total_files),
            output,
            metadata: std::collections::HashMap::new(),
            truncated: false,
        })
    }
}

impl Default for MultiEditTool {
    fn default() -> Self {
        Self
    }
}

/// `edit` 権限のメタデータに渡す最小限の差分。`edit/tool.rs` の `create_diff` と同じ形式。
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

    fn temp_dir() -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("opencode-multiedit-test-{}", uuid::Uuid::new_v4()));
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

    fn edit(file: &str, old: &str, new: &str, replace_all: bool) -> serde_json::Value {
        serde_json::json!({
            "file_path": file,
            "edits": [{ "old_string": old, "new_string": new, "replace_all": replace_all }]
        })
    }

    #[test]
    fn identity_and_schema() {
        let tool = MultiEditTool;
        assert_eq!(tool.id(), "multiedit");
        assert_eq!(tool.parameters()["required"][0], "edits");
    }

    #[tokio::test]
    async fn execute_replaces_first_occurrence() {
        let dir = temp_dir();
        std::fs::write(dir.join("a.txt"), "a a").unwrap();

        let result = MultiEditTool
            .execute(
                serde_json::json!({ "edits": [edit("a.txt", "a", "b", false)] }),
                context(&dir),
            )
            .await
            .unwrap();

        assert_eq!(std::fs::read_to_string(dir.join("a.txt")).unwrap(), "b a");
        assert!(result.output.contains("1 edit(s) across 1 file(s)"));

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[tokio::test]
    async fn execute_replace_all() {
        let dir = temp_dir();
        std::fs::write(dir.join("a.txt"), "a a").unwrap();

        MultiEditTool
            .execute(
                serde_json::json!({ "edits": [edit("a.txt", "a", "b", true)] }),
                context(&dir),
            )
            .await
            .unwrap();

        assert_eq!(std::fs::read_to_string(dir.join("a.txt")).unwrap(), "b b");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[tokio::test]
    async fn execute_multiple_files() {
        let dir = temp_dir();
        std::fs::write(dir.join("a.txt"), "x").unwrap();
        std::fs::write(dir.join("b.txt"), "x").unwrap();

        let result = MultiEditTool
            .execute(
                serde_json::json!({ "edits": [
                    edit("a.txt", "x", "1", false),
                    edit("b.txt", "x", "2", false)
                ] }),
                context(&dir),
            )
            .await
            .unwrap();

        assert_eq!(std::fs::read_to_string(dir.join("a.txt")).unwrap(), "1");
        assert_eq!(std::fs::read_to_string(dir.join("b.txt")).unwrap(), "2");
        assert!(result.output.contains("across 2 file(s)"));

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[tokio::test]
    async fn execute_file_not_found() {
        let dir = temp_dir();
        let error = MultiEditTool
            .execute(
                serde_json::json!({ "edits": [edit("missing.txt", "a", "b", false)] }),
                context(&dir),
            )
            .await
            .unwrap_err();

        assert!(matches!(error, ToolError::FileNotFound(_)));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[tokio::test]
    async fn execute_missing_old_string() {
        let dir = temp_dir();
        std::fs::write(dir.join("a.txt"), "abc").unwrap();

        let error = MultiEditTool
            .execute(
                serde_json::json!({ "edits": [edit("a.txt", "zzz", "b", false)] }),
                context(&dir),
            )
            .await
            .unwrap_err();

        assert!(matches!(error, ToolError::ExecutionError(_)));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[tokio::test]
    async fn execute_no_edits() {
        let dir = temp_dir();
        let result = MultiEditTool
            .execute(serde_json::json!({ "edits": [] }), context(&dir))
            .await
            .unwrap();

        assert_eq!(result.output, "No edits applied.");
        std::fs::remove_dir_all(&dir).unwrap();
    }
}

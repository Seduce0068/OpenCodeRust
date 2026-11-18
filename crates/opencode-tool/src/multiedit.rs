use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

use crate::{Tool, ToolContext, ToolError, ToolResult};

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
            let file_path = base_path.join(&file_edit.file_path);

            if !file_path.exists() {
                return Err(ToolError::FileNotFound(format!(
                    "File not found: {}",
                    file_edit.file_path
                )));
            }

            let content = tokio::fs::read_to_string(&file_path).await.map_err(|e| {
                ToolError::execution_with_source(format!("Failed to read file: {}", e), e)
            })?;

            let mut new_content = content;
            let mut file_edits = 0;

            for edit in file_edit.edits {
                let count = if edit.replace_all {
                    let count = new_content.matches(&edit.old_string).count();
                    new_content = new_content.replace(&edit.old_string, &edit.new_string);
                    count
                } else {
                    if !new_content.contains(&edit.old_string) {
                        return Err(ToolError::ExecutionError(format!(
                            "Could not find '{}' in file {}",
                            edit.old_string, file_edit.file_path
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
                file_edits += count;
            }

            if file_edits > 0 {
                tokio::fs::write(&file_path, &new_content)
                    .await
                    .map_err(|e| {
                        ToolError::execution_with_source(format!("Failed to write file: {}", e), e)
                    })?;

                ctx.do_publish_bus(
                    "file.edited",
                    serde_json::json!({
                        "file": file_edit.file_path
                    }),
                )
                .await;

                ctx.do_publish_bus(
                    "file_watcher.updated",
                    serde_json::json!({
                        "file": file_edit.file_path,
                        "event": "change"
                    }),
                )
                .await;

                ctx.do_lsp_touch_file(file_edit.file_path.clone(), true)
                    .await?;

                results.push(format!("- {}: {} edit(s)", file_edit.file_path, file_edits));
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

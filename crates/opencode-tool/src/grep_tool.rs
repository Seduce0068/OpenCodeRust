use async_trait::async_trait;
use regex::Regex;
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::time::SystemTime;
use walkdir::WalkDir;

use crate::{Metadata, Tool, ToolContext, ToolError, ToolResult};

const MAX_LINE_LENGTH: usize = 2000;

pub struct GrepTool {
    directory: PathBuf,
}

impl GrepTool {
    pub fn new() -> Self {
        Self {
            directory: std::env::current_dir().unwrap_or_default(),
        }
    }
}

impl Default for GrepTool {
    fn default() -> Self {
        Self::new()
    }
}

struct GrepMatch {
    path: String,
    mtime: SystemTime,
    line_num: usize,
    line_text: String,
}

/// Parse an optional boolean argument. A missing or `null` value keeps
/// `default`; any other JSON type is rejected instead of being silently
/// coerced to `false`.
fn parse_bool_arg(args: &serde_json::Value, key: &str, default: bool) -> Result<bool, ToolError> {
    let value = &args[key];
    if value.is_null() {
        return Ok(default);
    }
    value
        .as_bool()
        .ok_or_else(|| ToolError::InvalidArguments(format!("{} must be a boolean", key)))
}

/// Largest byte index `<= index` that is a valid UTF-8 character boundary in `s`.
///
/// Mirrors read.rs's private `floor_char_boundary`; kept local because tool
/// modules in this write scope do not share a helper module. `- 1` steps never
/// underflow because the loop stops at `0`, which is always a boundary.
fn floor_char_boundary(s: &str, index: usize) -> usize {
    let mut i = index.min(s.len());
    while i > 0 && !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

#[async_trait]
impl Tool for GrepTool {
    fn id(&self) -> &str {
        "grep"
    }

    fn description(&self) -> &str {
        "Fast content search tool. Searches file contents using regular expressions. Results sorted by file modification time (most recent first)."
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "pattern": {
                    "type": "string",
                    "description": "The regex pattern to search for"
                },
                "path": {
                    "type": "string",
                    "description": "The directory to search in"
                },
                "glob": {
                    "type": "string",
                    "description": "File pattern to include (e.g., '*.js')"
                },
                "ignore_case": {
                    "type": "boolean",
                    "description": "Case insensitive search"
                },
                "hidden": {
                    "type": "boolean",
                    "description": "Search hidden files and directories (default: false)"
                }
            },
            "required": ["pattern"]
        })
    }

    async fn execute(
        &self,
        args: serde_json::Value,
        ctx: ToolContext,
    ) -> Result<ToolResult, ToolError> {
        let pattern: String = args["pattern"]
            .as_str()
            .ok_or_else(|| ToolError::InvalidArguments("pattern is required".into()))?
            .to_string();

        let search_path: String = args["path"]
            .as_str()
            .map(|s| s.to_string())
            .unwrap_or_else(|| ctx.directory.clone());

        let glob_filter: Option<String> = args["glob"].as_str().map(|s| s.to_string());

        let ignore_case: bool = parse_bool_arg(&args, "ignore_case", false)?;

        let include_hidden: bool = parse_bool_arg(&args, "hidden", false)?;

        let base_dir = if search_path.is_empty() {
            &self.directory
        } else {
            Path::new(&search_path)
        };

        let base_dir_str = base_dir.to_string_lossy().to_string();

        if ctx.is_external_path(&base_dir_str) {
            ctx.ask_permission(
                crate::PermissionRequest::new("external_directory")
                    .with_pattern(format!("{}/*", base_dir_str))
                    .with_metadata("path", serde_json::json!(&base_dir_str)),
            )
            .await?;
        }

        ctx.ask_permission(
            crate::PermissionRequest::new("grep")
                .with_pattern(&pattern)
                .always_allow()
                .with_metadata("path", serde_json::json!(&base_dir_str)),
        )
        .await?;

        let regex_pattern = if ignore_case {
            format!("(?i){}", pattern)
        } else {
            pattern.clone()
        };

        let regex = Regex::new(&regex_pattern)
            .map_err(|e| ToolError::InvalidArguments(format!("Invalid regex: {}", e)))?;

        // A malformed glob must not silently degrade into "no filter" (which
        // would scan every file). Match glob_tool.rs and fail with
        // InvalidArguments instead.
        let glob_pattern = match glob_filter.as_ref() {
            Some(g) => Some(glob::Pattern::new(g).map_err(|e| {
                ToolError::InvalidArguments(format!("Invalid glob pattern: {}", e))
            })?),
            None => None,
        };

        let limit = 100;

        // 走査とファイル読み取りは同期 I/O なので、所有値を move したクロージャを
        // spawn_blocking に逃がし、tokio ワーカーを占有しないようにする。
        let base_dir_buf = base_dir.to_path_buf();
        let (mut matches, has_errors) = tokio::task::spawn_blocking(move || {
            let mut matches: Vec<GrepMatch> = Vec::new();
            let mut has_errors = false;

            // 外部ディレクトリの権限チェックは検索ルート base_dir に対して
            // 走査前に一度だけ行われる。シンボリックリンクを辿ると、
            // プロジェクト内のリンクが外部ディレクトリ／ファイルを指す場合に
            // そのチェックをすり抜けて外部の内容を読み出してしまうため、辿らない。
            // follow_links(false) は symlink cycle による無限走査も構造的に防ぐ。
            for entry in WalkDir::new(&base_dir_buf)
                .follow_links(false)
                .into_iter()
                .filter_entry(|e| {
                    if include_hidden {
                        true
                    } else {
                        let name = e.file_name().to_string_lossy();
                        !name.starts_with('.')
                    }
                })
                .filter_map(|e| e.ok())
            {
                // follow_links(false) でも WalkDir はリンク自身をエントリとして
                // 返す。path.is_file() / File::open はリンク先を辿るため、
                // 外部ファイルの内容を返さないようリンクを明示的に除外する。
                if entry.file_type().is_symlink() {
                    continue;
                }

                let path = entry.path();
                if !path.is_file() {
                    continue;
                }

                if let Some(ref gp) = glob_pattern {
                    let rel_path = path.strip_prefix(&base_dir_buf).unwrap_or(path);
                    if !gp.matches(&rel_path.to_string_lossy()) {
                        continue;
                    }
                }

                let mtime = path
                    .metadata()
                    .and_then(|m| m.modified())
                    .unwrap_or(SystemTime::UNIX_EPOCH);

                if let Ok(file) = File::open(path) {
                    let reader = BufReader::new(file);
                    let path_str = path.to_string_lossy().to_string();

                    for (line_num, line_result) in reader.lines().enumerate() {
                        if let Ok(line) = line_result {
                            if regex.is_match(&line) {
                                let truncated_line = if line.len() > MAX_LINE_LENGTH {
                                    let end = floor_char_boundary(&line, MAX_LINE_LENGTH);
                                    format!("{}...", &line[..end])
                                } else {
                                    line.clone()
                                };

                                matches.push(GrepMatch {
                                    path: path_str.clone(),
                                    mtime,
                                    line_num: line_num + 1,
                                    line_text: truncated_line,
                                });
                            }
                        }
                    }
                } else {
                    has_errors = true;
                }
            }

            (matches, has_errors)
        })
        .await
        .map_err(|e| {
            ToolError::execution_with_source(format!("grep scan task failed: {}", e), e)
        })?;

        matches.sort_by(|a, b| b.mtime.cmp(&a.mtime));

        let total_matches = matches.len();
        let truncated = matches.len() > limit;

        let title = format!("grep '{}'", pattern);
        let output = if total_matches == 0 {
            format!("No matches found for pattern '{}'", pattern)
        } else {
            let mut output_lines = vec![format!(
                "Found {} matches{}",
                total_matches,
                if truncated {
                    format!(" (showing first {})", limit)
                } else {
                    String::new()
                }
            )];

            let display_matches: Vec<&GrepMatch> = matches.iter().take(limit).collect();
            let mut current_file = "";

            for m in display_matches {
                if current_file != m.path {
                    if !current_file.is_empty() {
                        output_lines.push(String::new());
                    }
                    current_file = &m.path;
                    output_lines.push(format!("{}:", m.path));
                }
                output_lines.push(format!("  Line {}: {}", m.line_num, m.line_text));
            }

            if truncated {
                output_lines.push(String::new());
                output_lines.push(format!(
                    "(Results truncated: showing {} of {} matches ({} hidden). Consider using a more specific path or pattern.)",
                    limit, total_matches, total_matches - limit
                ));
            }

            if has_errors {
                output_lines.push(String::new());
                output_lines.push("(Some paths were inaccessible and skipped)".to_string());
            }

            output_lines.join("\n")
        };

        Ok(ToolResult {
            title,
            output,
            metadata: {
                let mut m = Metadata::new();
                m.insert("matches".into(), serde_json::json!(total_matches));
                m.insert("truncated".into(), serde_json::json!(truncated));
                m.insert("hasErrors".into(), serde_json::json!(has_errors));
                m
            },
            truncated,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("opencode-grep-test-{}", uuid::Uuid::new_v4()));
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
    fn identity_and_schema() {
        let tool = GrepTool::new();
        assert_eq!(tool.id(), "grep");
        assert!(tool.description().contains("regular expressions"));
        assert_eq!(tool.parameters()["required"][0], "pattern");
    }

    #[tokio::test]
    async fn execute_finds_matching_lines() {
        let dir = temp_dir();
        std::fs::write(dir.join("a.rs"), "hello world\nnothing\nhello again\n").unwrap();

        let result = GrepTool::new()
            .execute(serde_json::json!({ "pattern": "hello" }), context(&dir))
            .await
            .unwrap();

        assert_eq!(result.title, "grep 'hello'");
        assert_eq!(result.metadata["matches"], 2);
        assert!(result.output.contains("Line 1: hello world"));
        assert!(result.output.contains("Line 3: hello again"));
        assert!(!result.truncated);

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[tokio::test]
    async fn execute_no_matches() {
        let dir = temp_dir();
        std::fs::write(dir.join("a.txt"), "nothing here").unwrap();

        let result = GrepTool::new()
            .execute(serde_json::json!({ "pattern": "hello" }), context(&dir))
            .await
            .unwrap();

        assert_eq!(result.output, "No matches found for pattern 'hello'");
        assert_eq!(result.metadata["matches"], 0);

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[tokio::test]
    async fn execute_ignore_case() {
        let dir = temp_dir();
        std::fs::write(dir.join("a.txt"), "HELLO world").unwrap();

        let result = GrepTool::new()
            .execute(
                serde_json::json!({ "pattern": "hello", "ignore_case": true }),
                context(&dir),
            )
            .await
            .unwrap();

        assert_eq!(result.metadata["matches"], 1);

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[tokio::test]
    async fn execute_glob_filter() {
        let dir = temp_dir();
        std::fs::write(dir.join("a.rs"), "hello").unwrap();
        std::fs::write(dir.join("b.txt"), "hello").unwrap();

        let result = GrepTool::new()
            .execute(
                serde_json::json!({ "pattern": "hello", "glob": "*.rs" }),
                context(&dir),
            )
            .await
            .unwrap();

        assert_eq!(result.metadata["matches"], 1);
        assert!(result.output.contains("a.rs"));
        assert!(!result.output.contains("b.txt"));

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn floor_char_boundary_respects_multibyte_boundaries() {
        // 'あ' is 3 bytes; byte offset 2000 lands in the middle of it.
        let s = "a".repeat(1999) + "あ";
        assert!(!s.is_char_boundary(MAX_LINE_LENGTH));
        let end = floor_char_boundary(&s, MAX_LINE_LENGTH);
        assert_eq!(end, 1999);
        assert!(s.is_char_boundary(end));
        assert_eq!(&s[..end], "a".repeat(1999).as_str());
    }

    #[tokio::test]
    async fn execute_truncates_long_multibyte_line_without_panic() {
        let dir = temp_dir();
        let line = format!("{}あ{}", "a".repeat(1999), "tail");
        assert!(line.len() > MAX_LINE_LENGTH);
        assert!(!line.is_char_boundary(MAX_LINE_LENGTH));
        std::fs::write(dir.join("a.txt"), line.as_bytes()).unwrap();

        let result = GrepTool::new()
            .execute(
                serde_json::json!({ "pattern": "a", "glob": "*.txt" }),
                context(&dir),
            )
            .await
            .unwrap();

        assert_eq!(result.metadata["matches"], 1);
        assert!(result.output.contains("..."));
        assert!(result.output.contains("a".repeat(1999).as_str()));
        assert!(!result.output.contains("tail"));

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[tokio::test]
    async fn execute_rejects_invalid_regex() {
        let dir = temp_dir();
        let error = GrepTool::new()
            .execute(serde_json::json!({ "pattern": "[" }), context(&dir))
            .await
            .unwrap_err();

        assert!(matches!(error, ToolError::InvalidArguments(_)));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[tokio::test]
    async fn execute_rejects_invalid_glob() {
        let dir = temp_dir();
        let error = GrepTool::new()
            .execute(
                serde_json::json!({ "pattern": "hello", "glob": "[" }),
                context(&dir),
            )
            .await
            .unwrap_err();

        assert!(matches!(error, ToolError::InvalidArguments(_)));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[tokio::test]
    async fn execute_rejects_non_boolean_ignore_case() {
        let dir = temp_dir();
        let error = GrepTool::new()
            .execute(
                serde_json::json!({ "pattern": "hello", "ignore_case": "yes" }),
                context(&dir),
            )
            .await
            .unwrap_err();

        assert!(matches!(error, ToolError::InvalidArguments(_)));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[tokio::test]
    async fn execute_rejects_non_boolean_hidden() {
        let dir = temp_dir();
        let error = GrepTool::new()
            .execute(
                serde_json::json!({ "pattern": "hello", "hidden": 1 }),
                context(&dir),
            )
            .await
            .unwrap_err();

        assert!(matches!(error, ToolError::InvalidArguments(_)));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[tokio::test]
    async fn execute_requires_pattern() {
        let dir = temp_dir();
        let error = GrepTool::new()
            .execute(serde_json::json!({}), context(&dir))
            .await
            .unwrap_err();

        assert!(matches!(error, ToolError::InvalidArguments(_)));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// 検索ルート内の symlink がプロジェクト外のファイル／ディレクトリを
    /// 指しても、その内容を返してはならない
    /// （external_directory 権限のすり抜け回帰テスト）。
    #[cfg(unix)]
    #[tokio::test]
    async fn execute_does_not_follow_external_symlinks() {
        use std::os::unix::fs::symlink;

        let dir = temp_dir();
        let outside =
            std::env::temp_dir().join(format!("opencode-grep-outside-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("secret.txt"), "TOP_SECRET_NEVER_RETURNED\n").unwrap();

        // 外部ディレクトリを指す symlink と、外部ファイルを直接指す symlink。
        symlink(&outside, dir.join("outside_link")).unwrap();
        symlink(outside.join("secret.txt"), dir.join("secret_link.txt")).unwrap();

        let result = GrepTool::new()
            .execute(
                serde_json::json!({
                    "pattern": "TOP_SECRET_NEVER_RETURNED",
                    "path": dir.to_string_lossy()
                }),
                context(&dir),
            )
            .await
            .unwrap();

        // output には検索タイトル（パターン文字列）が含まれるため、マッチ数で検証する。
        assert_eq!(result.metadata["matches"], 0);

        std::fs::remove_dir_all(&dir).unwrap();
        std::fs::remove_dir_all(&outside).unwrap();
    }

    /// 自分自身を指す symlink（cycle）があっても走査が完了することを確認する。
    #[cfg(unix)]
    #[tokio::test]
    async fn execute_cyclic_symlink_does_not_hang() {
        use std::os::unix::fs::symlink;

        let dir = temp_dir();
        std::fs::write(dir.join("a.txt"), "hello world\n").unwrap();
        symlink(".", dir.join("self_link")).unwrap();

        let result = GrepTool::new()
            .execute(
                serde_json::json!({
                    "pattern": "hello",
                    "path": dir.to_string_lossy()
                }),
                context(&dir),
            )
            .await
            .unwrap();

        assert_eq!(result.metadata["matches"], 1);

        std::fs::remove_dir_all(&dir).unwrap();
    }
}

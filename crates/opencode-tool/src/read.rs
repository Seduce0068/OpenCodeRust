use async_trait::async_trait;
use std::path::{Path, PathBuf};
use tokio::fs;
use walkdir::WalkDir;

use crate::tool::parse_usize_arg;
use crate::truncation::MAX_BYTES;
use crate::{Metadata, Tool, ToolContext, ToolError, ToolResult};

const DEFAULT_READ_LIMIT: usize = 2000;
const MAX_LINE_LENGTH: usize = 2000;

const INSTRUCTION_FILES: &[&str] = &[
    "AGENTS.md",
    "CLAUDE.md",
    "CONTEXT.md",
    "CONTEXT.txt",
    ".context",
    ".cursorrules",
    ".opencoderules",
];

/// Largest byte index `<= index` that is a valid UTF-8 character boundary in `s`.
///
/// Equivalent to `str::floor_char_boundary`, implemented locally so the crate does
/// not depend on a specific MSRV for that method's stabilization. `- 1` steps never
/// underflow because the loop stops at `0`, which is always a character boundary.
fn floor_char_boundary(s: &str, index: usize) -> usize {
    let mut i = index.min(s.len());
    while i > 0 && !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

/// Convert a `fs::metadata` failure into a `ToolError`.
///
/// Only [`std::io::ErrorKind::NotFound`] keeps the historical "File not found"
/// response (with best-effort spelling suggestions). Every other I/O error —
/// notably `PermissionDenied` — becomes an `ExecutionWithSource` error that
/// preserves the original `io::Error` as its source, so an access failure is
/// never misreported as a missing file.
async fn map_metadata_error(err: std::io::Error, path: &Path) -> ToolError {
    if err.kind() != std::io::ErrorKind::NotFound {
        return ToolError::execution_with_source(
            format!("Failed to access {}: {}", path.display(), err),
            err,
        );
    }

    let dir = path.parent().unwrap_or(Path::new("."));
    let base = path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();

    // 候補パスの列挙は tokio::fs::read_dir に変更し、同期 read_dir で
    // tokio ワーカーをブロックしないようにする。
    let mut suggestions: Vec<String> = Vec::new();
    if let Ok(mut entries) = fs::read_dir(dir).await {
        while let Ok(Some(entry)) = entries.next_entry().await {
            let name = entry.file_name().to_string_lossy().to_lowercase();
            let target = base.to_lowercase();
            if name.contains(&target) || target.contains(&name) {
                suggestions.push(entry.path().to_string_lossy().to_string());
                if suggestions.len() >= 3 {
                    break;
                }
            }
        }
    }

    ToolError::with_suggestions(format!("File not found: {}", path.display()), &suggestions)
}

pub struct ReadTool {
    directory: PathBuf,
}

impl ReadTool {
    pub fn new() -> Self {
        Self {
            directory: std::env::current_dir().unwrap_or_default(),
        }
    }

    pub fn with_directory(directory: impl Into<PathBuf>) -> Self {
        Self {
            directory: directory.into(),
        }
    }
}

impl Default for ReadTool {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Tool for ReadTool {
    fn id(&self) -> &str {
        "read"
    }

    fn description(&self) -> &str {
        "Reads a file from the local filesystem. Can read text files, list directory contents, or handle binary files (images, PDFs) as base64."
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "file_path": {
                    "type": "string",
                    "description": "The absolute path to the file or directory to read"
                },
                "offset": {
                    "type": "integer",
                    "description": "The line number to start reading from (1-indexed)"
                },
                "limit": {
                    "type": "integer",
                    "description": "The maximum number of lines to read (defaults to 2000)"
                }
            },
            "required": ["file_path"]
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

        let offset: usize = parse_usize_arg(&args, "offset", 1)?;

        let limit: usize = parse_usize_arg(&args, "limit", DEFAULT_READ_LIMIT)?;

        if offset < 1 {
            return Err(ToolError::InvalidArguments("offset must be >= 1".into()));
        }

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
                    .with_metadata("filepath", serde_json::json!(path_str))
                    .with_metadata("parentDir", serde_json::json!(parent)),
            )
            .await?;
        }

        ctx.ask_permission(
            crate::PermissionRequest::new("read")
                .with_pattern(&path_str)
                .always_allow(),
        )
        .await?;

        let title = path
            .strip_prefix(&ctx.worktree)
            .unwrap_or(&path)
            .to_string_lossy()
            .to_string();

        let metadata = match fs::metadata(&path).await {
            Ok(metadata) => metadata,
            // エラーを破棄せず、NotFound とそれ以外（権限不足等）を区別して返す。
            Err(err) => return Err(map_metadata_error(err, &path).await),
        };

        if metadata.is_dir() {
            ctx.do_file_time_read(path_str.clone()).await?;
            ctx.do_lsp_touch_file(path_str.clone(), false).await?;
            return read_directory(&path, offset, limit, title).await;
        }

        let content = fs::read(&path).await.map_err(|e| {
            ToolError::execution_with_source(format!("Failed to read file: {}", e), e)
        })?;

        let mime = detect_mime(&path);

        if is_image_mime(&mime) || mime == "application/pdf" {
            ctx.do_file_time_read(path_str.clone()).await?;
            ctx.do_lsp_touch_file(path_str.clone(), false).await?;
            return handle_binary_file(&path, &content, &mime, title);
        }

        if is_binary(&content) {
            return Err(ToolError::BinaryFile(path.display().to_string()));
        }

        ctx.do_file_time_read(path_str.clone()).await?;
        ctx.do_lsp_touch_file(path_str.clone(), false).await?;
        read_file_content(
            &path,
            &path_str,
            &content,
            offset,
            limit,
            title,
            &ctx.project_root,
        )
        .await
    }
}

fn detect_mime(path: &Path) -> String {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_lowercase())
        .unwrap_or_default();

    match ext.as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "bmp" => "image/bmp",
        "ico" => "image/x-icon",
        "tiff" | "tif" => "image/tiff",
        "avif" => "image/avif",
        "heic" | "heif" => "image/heic",
        "pdf" => "application/pdf",
        "json" => "application/json",
        "html" | "htm" => "text/html",
        "css" => "text/css",
        "js" => "application/javascript",
        "ts" => "application/typescript",
        "md" => "text/markdown",
        "txt" => "text/plain",
        "xml" => "application/xml",
        "svg" => "image/svg+xml",
        _ => "application/octet-stream",
    }
    .to_string()
}

fn is_image_mime(mime: &str) -> bool {
    mime.starts_with("image/") && mime != "image/svg+xml" && mime != "image/vnd.fastbidsheet"
}

fn handle_binary_file(
    path: &Path,
    content: &[u8],
    mime: &str,
    title: String,
) -> Result<ToolResult, ToolError> {
    let base64_content =
        base64::Engine::encode(&base64::engine::general_purpose::STANDARD, content);
    let data_url = format!("data:{};base64,{}", mime, base64_content);

    let file_type = if mime.starts_with("image/") {
        "Image"
    } else {
        "PDF"
    };
    let msg = format!("{} read successfully ({} bytes)", file_type, content.len());

    let output = format!(
        "<path>{}</path>\n<type>binary</type>\n<mime>{}</mime>\n<size>{}</size>\n<data-url>\n{}\n</data-url>",
        path.display(),
        mime,
        content.len(),
        data_url
    );

    Ok(ToolResult {
        title,
        output,
        metadata: {
            let mut m = Metadata::new();
            m.insert("preview".into(), serde_json::json!(msg));
            m.insert("truncated".into(), serde_json::json!(false));
            m.insert("mime".into(), serde_json::json!(mime));
            m.insert("size".into(), serde_json::json!(content.len()));
            m
        },
        truncated: false,
    })
}

async fn read_directory(
    path: &Path,
    offset: usize,
    limit: usize,
    title: String,
) -> Result<ToolResult, ToolError> {
    // 走査は同期 I/O なので、所有値を move したクロージャを spawn_blocking に
    // 逃がし、tokio ワーカーを占有しないようにする。
    let path_buf = path.to_path_buf();
    let mut entries: Vec<String> = tokio::task::spawn_blocking(move || {
        let mut entries: Vec<String> = Vec::new();

        // 外部ディレクトリの権限チェックは走査対象のディレクトリに対して一度だけ
        // 行われる。シンボリックリンクを辿ると、ディレクトリ内のリンクが外部を
        // 指す場合にそのチェックをすり抜けて外部のエントリ名を返してしまうため、
        // 辿らない。follow_links(false) は symlink cycle も構造的に防ぐ。
        for entry in WalkDir::new(&path_buf)
            .max_depth(1)
            .follow_links(false)
            .into_iter()
            .filter_map(|e| e.ok())
        {
            // follow_links(false) でも WalkDir はリンク自身をエントリとして返す。
            // リンク先が外部のとき path.is_file() / File::open はリンク先を辿るため、
            // 外部の内容や名前を返さないようリンクを明示的に除外する。
            if entry.file_type().is_symlink() {
                continue;
            }

            if entry.path() == path_buf.as_path() {
                continue;
            }

            let name = entry.file_name().to_string_lossy().to_string();
            if entry.file_type().is_dir() {
                entries.push(format!("{}/", name));
            } else {
                entries.push(name);
            }
        }

        entries
    })
    .await
    .map_err(|e| {
        ToolError::execution_with_source(format!("directory scan task failed: {}", e), e)
    })?;

    entries.sort();

    let start = offset.saturating_sub(1);
    let sliced: Vec<&str> = entries
        .iter()
        .skip(start)
        .take(limit)
        .map(|s| s.as_str())
        .collect();
    let truncated = start.saturating_add(sliced.len()) < entries.len();

    let output = format!(
        "<path>{}</path>\n<type>directory</type>\n<entries>\n{}\n{}{}\n</entries>",
        path.display(),
        sliced.join("\n"),
        if truncated {
            format!(
                "\n(Showing {} of {} entries. Use 'offset' parameter to read beyond entry {})",
                sliced.len(),
                entries.len(),
                offset.saturating_add(sliced.len())
            )
        } else {
            format!("\n({} entries)", entries.len())
        },
        ""
    );

    let preview = sliced
        .iter()
        .take(20)
        .cloned()
        .collect::<Vec<_>>()
        .join("\n");

    Ok(ToolResult {
        title,
        output,
        metadata: {
            let mut m = Metadata::new();
            m.insert("preview".into(), serde_json::json!(preview));
            m.insert("truncated".into(), serde_json::json!(truncated));
            m
        },
        truncated,
    })
}

async fn read_file_content(
    path: &Path,
    path_str: &str,
    content: &[u8],
    offset: usize,
    limit: usize,
    title: String,
    project_root: &str,
) -> Result<ToolResult, ToolError> {
    let text = String::from_utf8_lossy(content);
    let lines: Vec<&str> = text.lines().collect();

    if offset > lines.len() {
        return Err(ToolError::InvalidArguments(format!(
            "Offset {} is out of range (file has {} lines)",
            offset,
            lines.len()
        )));
    }

    let start = offset.saturating_sub(1);
    let mut result_lines: Vec<String> = Vec::new();
    let mut bytes = 0;
    let mut truncated_by_bytes = false;

    // `limit` can be u64::MAX (== usize::MAX on 64-bit targets); a plain
    // `start + limit` would overflow in debug builds. Saturating is correct
    // because the loop bound is clamped by `lines.len()` anyway.
    for i in start..std::cmp::min(lines.len(), start.saturating_add(limit)) {
        let line = if lines[i].len() > MAX_LINE_LENGTH {
            let end = floor_char_boundary(lines[i], MAX_LINE_LENGTH);
            format!("{}...", &lines[i][..end])
        } else {
            lines[i].to_string()
        };

        let size = line.len() + if result_lines.is_empty() { 0 } else { 1 };
        if bytes + size > MAX_BYTES {
            truncated_by_bytes = true;
            break;
        }

        result_lines.push(format!("{}: {}", i + 1, line));
        bytes += size;
    }

    let preview = result_lines
        .iter()
        .take(20)
        .cloned()
        .collect::<Vec<_>>()
        .join("\n");
    let total_lines = lines.len();
    let last_read_line = offset.saturating_add(result_lines.len()).saturating_sub(1);
    let has_more_lines = total_lines > last_read_line;
    let truncated = has_more_lines || truncated_by_bytes;

    let truncation_msg = if result_lines.is_empty() {
        "\n\n(0 lines read)".to_string()
    } else if truncated_by_bytes {
        format!(
            "\n\n(Output truncated at {} bytes. Use 'offset' parameter to read beyond line {})",
            MAX_BYTES, last_read_line
        )
    } else if has_more_lines {
        format!(
            "\n\n(File has more lines. Use 'offset' parameter to read beyond line {})",
            last_read_line
        )
    } else {
        format!("\n\n(End of file - total {} lines)", total_lines)
    };

    let mut output = format!(
        "<path>{}</path>\n<type>file</type>\n<content>\n{}{}\n</content>",
        path.display(),
        result_lines.join("\n"),
        truncation_msg
    );

    let project_root_path = PathBuf::from(project_root);
    let instructions = resolve_instruction_prompts(path, &project_root_path).await;

    let mut loaded_files = vec![path_str.to_string()];

    if !instructions.is_empty() {
        let instruction_content: Vec<String> = instructions
            .iter()
            .map(|i| {
                loaded_files.push(i.filepath.clone());
                i.content.clone()
            })
            .collect();

        output.push_str("\n\n<system-reminder>\n");
        output.push_str(&instruction_content.join("\n\n"));
        output.push_str("\n</system-reminder>");
    }

    Ok(ToolResult {
        title,
        output,
        metadata: {
            let mut m = Metadata::new();
            m.insert("preview".into(), serde_json::json!(preview));
            m.insert("truncated".into(), serde_json::json!(truncated));
            m.insert("filepath".into(), serde_json::json!(path_str));
            m.insert("loaded".into(), serde_json::json!(loaded_files));
            m
        },
        truncated,
    })
}

fn is_binary(content: &[u8]) -> bool {
    if content.is_empty() {
        return false;
    }

    let check_len = std::cmp::min(4096, content.len());
    let bytes = &content[..check_len];

    if bytes.contains(&0) {
        return true;
    }

    let non_printable = bytes
        .iter()
        .filter(|&&b| b < 9 || (b > 13 && b < 32))
        .count();

    non_printable as f32 / check_len as f32 > 0.3
}

struct InstructionPrompt {
    filepath: String,
    content: String,
}

async fn resolve_instruction_prompts(
    file_path: &Path,
    project_root: &Path,
) -> Vec<InstructionPrompt> {
    let mut results = Vec::new();

    let target = file_path
        .canonicalize()
        .unwrap_or_else(|_| file_path.to_path_buf());
    let root = project_root
        .canonicalize()
        .unwrap_or_else(|_| project_root.to_path_buf());

    let mut current = target.parent().unwrap_or(&target).to_path_buf();

    while current.starts_with(&root) && current != root {
        if let Some(found) = find_instruction_file(&current).await {
            let canonical = found.canonicalize().unwrap_or_else(|_| found.clone());
            if canonical != target {
                if let Ok(content) = tokio::fs::read_to_string(&found).await {
                    if !content.is_empty() {
                        results.push(InstructionPrompt {
                            filepath: found.to_string_lossy().to_string(),
                            content: format!("Instructions from: {}\n{}", found.display(), content),
                        });
                    }
                }
            }
        }

        if !current.pop() {
            break;
        }
    }

    if let Some(found) = find_instruction_file(&root).await {
        let canonical = found.canonicalize().unwrap_or_else(|_| found.clone());
        if canonical != target {
            if let Ok(content) = tokio::fs::read_to_string(&found).await {
                if !content.is_empty() {
                    results.push(InstructionPrompt {
                        filepath: found.to_string_lossy().to_string(),
                        content: format!("Instructions from: {}\n{}", found.display(), content),
                    });
                }
            }
        }
    }

    results
}

async fn find_instruction_file(dir: &Path) -> Option<PathBuf> {
    for name in INSTRUCTION_FILES {
        let path = dir.join(name);
        if path.exists() && path.is_file() {
            return Some(path);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn context(directory: &Path) -> ToolContext {
        ToolContext::new(
            "session".to_owned(),
            "message".to_owned(),
            directory.to_string_lossy().to_string(),
        )
    }

    #[test]
    fn identity_and_schema() {
        let tool = ReadTool::new();
        assert_eq!(tool.id(), "read");
        assert_eq!(tool.parameters()["required"][0], "file_path");
        // The schema must advertise integers: the parser accepts only u64
        // literals, so `type: number` would mislead clients into sending 2000.0.
        assert_eq!(tool.parameters()["properties"]["offset"]["type"], "integer");
        assert_eq!(tool.parameters()["properties"]["limit"]["type"], "integer");
    }

    #[test]
    fn detect_mime_by_extension() {
        assert_eq!(detect_mime(Path::new("a.png")), "image/png");
        assert_eq!(detect_mime(Path::new("a.JPG")), "image/jpeg");
        assert_eq!(detect_mime(Path::new("a.pdf")), "application/pdf");
        assert_eq!(detect_mime(Path::new("a.md")), "text/markdown");
        assert_eq!(
            detect_mime(Path::new("a.unknown")),
            "application/octet-stream"
        );
    }

    #[test]
    fn is_image_mime_excludes_svg() {
        assert!(is_image_mime("image/png"));
        assert!(!is_image_mime("image/svg+xml"));
        assert!(!is_image_mime("text/plain"));
    }

    #[test]
    fn is_binary_detects_nul_bytes() {
        assert!(is_binary(&[0x00, 0x01]));
        assert!(!is_binary(b"plain text"));
    }

    #[test]
    fn is_binary_empty_is_text() {
        assert!(!is_binary(&[]));
    }

    #[test]
    fn is_binary_detects_mostly_non_printable() {
        let bytes: Vec<u8> = (0..100).map(|_| 1u8).collect();
        assert!(is_binary(&bytes));
    }

    #[tokio::test]
    async fn read_file_content_with_zero_limit_reports_zero_lines() {
        let content = b"first\nsecond\nthird";
        let result = read_file_content(
            std::path::Path::new("/tmp/test.txt"),
            "/tmp/test.txt",
            content,
            1,
            0,
            "test".to_string(),
            "/tmp",
        )
        .await
        .unwrap();
        // With limit=0 no lines are read; the output must say so explicitly
        // instead of claiming a line was consumed ("beyond line 0").
        assert!(result.output.contains("0 lines read"));
        assert!(!result.output.contains("beyond line 0"));
    }

    #[tokio::test]
    async fn read_file_content_with_max_limit_does_not_overflow() {
        // Regression: offset >= 2 with limit = usize::MAX overflowed the old
        // `start + limit` in debug builds. Saturating must clamp it to the
        // actual line count instead.
        let content = b"first\nsecond\nthird";
        let result = read_file_content(
            std::path::Path::new("/tmp/test.txt"),
            "/tmp/test.txt",
            content,
            2,
            usize::MAX,
            "test".to_string(),
            "/tmp",
        )
        .await
        .unwrap();

        assert!(result.output.contains("2: second"));
        assert!(result.output.contains("3: third"));
        assert!(!result.output.contains("1: first"));
        assert!(result.output.contains("End of file - total 3 lines"));
    }

    #[test]
    fn floor_char_boundary_respects_multibyte_boundaries() {
        // 'あ' is 3 bytes; place it so byte offset 2000 lands on its middle byte.
        let s = "a".repeat(1999) + "あ";
        assert!(!s.is_char_boundary(MAX_LINE_LENGTH));
        let end = floor_char_boundary(&s, MAX_LINE_LENGTH);
        assert_eq!(end, 1999);
        assert!(s.is_char_boundary(end));
        assert_eq!(&s[..end], "a".repeat(1999).as_str());
    }

    #[tokio::test]
    async fn read_file_content_truncates_long_multibyte_line_without_panic() {
        // 1999 ASCII bytes + a 3-byte 'あ' whose middle byte sits at offset 2000,
        // followed by more content. The old `&lines[i][..2000]` slice panicked here.
        let line = format!("{}あ{}", "a".repeat(1999), "tail");
        assert!(line.len() > MAX_LINE_LENGTH);
        assert!(!line.is_char_boundary(MAX_LINE_LENGTH));

        let result = read_file_content(
            std::path::Path::new("/tmp/utf8.txt"),
            "/tmp/utf8.txt",
            line.as_bytes(),
            1,
            1,
            "test".to_string(),
            "/tmp",
        )
        .await
        .unwrap();

        assert!(result.output.contains("..."));
        assert!(result.output.contains("a".repeat(1999).as_str()));
    }

    #[test]
    fn with_directory_sets_base() {
        let tool = ReadTool::with_directory("/tmp");
        assert_eq!(tool.directory, PathBuf::from("/tmp"));
    }

    #[tokio::test]
    async fn execute_rejects_negative_offset() {
        // Regression: `as_u64().unwrap_or(1)` turned `offset: -1` into `1`.
        let error = ReadTool::with_directory("/tmp")
            .execute(
                serde_json::json!({ "file_path": "a.txt", "offset": -1 }),
                context(Path::new("/tmp")),
            )
            .await
            .unwrap_err();

        assert!(matches!(error, ToolError::InvalidArguments(_)));
    }

    #[tokio::test]
    async fn execute_rejects_non_numeric_limit() {
        let error = ReadTool::with_directory("/tmp")
            .execute(
                serde_json::json!({ "file_path": "a.txt", "limit": "10" }),
                context(Path::new("/tmp")),
            )
            .await
            .unwrap_err();

        assert!(matches!(error, ToolError::InvalidArguments(_)));
    }

    #[test]
    fn parse_usize_arg_keeps_omitted_and_null_defaults() {
        let args = serde_json::json!({});
        assert_eq!(parse_usize_arg(&args, "offset", 1).unwrap(), 1);
        assert_eq!(
            parse_usize_arg(&args, "limit", DEFAULT_READ_LIMIT).unwrap(),
            DEFAULT_READ_LIMIT
        );

        let nulls = serde_json::json!({ "offset": null, "limit": null });
        assert_eq!(parse_usize_arg(&nulls, "offset", 1).unwrap(), 1);
        assert_eq!(
            parse_usize_arg(&nulls, "limit", DEFAULT_READ_LIMIT).unwrap(),
            DEFAULT_READ_LIMIT
        );
    }

    #[test]
    fn parse_usize_arg_rejects_invalid_types() {
        for bad in [
            serde_json::json!(-1),
            serde_json::json!(1.5),
            serde_json::json!("10"),
            serde_json::json!(true),
            serde_json::json!([]),
        ] {
            let args = serde_json::json!({ "offset": bad });
            assert!(
                matches!(
                    parse_usize_arg(&args, "offset", 1),
                    Err(ToolError::InvalidArguments(_))
                ),
                "unexpectedly accepted {bad}"
            );
        }
    }

    /// ディレクトリ読み取り時に内部の symlink が外部のファイル／ディレクトリを
    /// 指しても、そのエントリ名を返してはならない（権限すり抜け回帰テスト）。
    #[cfg(unix)]
    #[tokio::test]
    async fn read_directory_does_not_follow_external_symlinks() {
        use std::os::unix::fs::symlink;

        let dir = std::env::temp_dir().join(format!("opencode-read-test-{}", uuid::Uuid::new_v4()));
        let outside =
            std::env::temp_dir().join(format!("opencode-read-outside-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("secret.txt"), "TOP_SECRET").unwrap();
        std::fs::write(dir.join("normal.txt"), "normal").unwrap();

        symlink(&outside, dir.join("outside_link")).unwrap();
        symlink(outside.join("secret.txt"), dir.join("secret_link.txt")).unwrap();

        let result = read_directory(&dir, 1, 100, "test".to_string())
            .await
            .unwrap();

        // 通常ファイルはそのまま返るが、外部を指す symlink の名前は漏れない。
        assert!(result.output.contains("normal.txt"));
        assert!(!result.output.contains("outside_link"));
        assert!(!result.output.contains("secret_link.txt"));

        std::fs::remove_dir_all(&dir).unwrap();
        std::fs::remove_dir_all(&outside).unwrap();
    }

    /// 権限不足（EACCES 等）は「File not found」に丸められず、
    /// 元の I/O エラーを source として保持すること（回帰テスト）。
    #[tokio::test]
    async fn metadata_permission_error_is_not_file_not_found() {
        let err = std::io::Error::new(std::io::ErrorKind::PermissionDenied, "permission denied");
        let mapped = map_metadata_error(err, Path::new("/some/locked/file.txt")).await;

        assert!(
            !matches!(&mapped, ToolError::FileNotFound(_)),
            "PermissionDenied は FileNotFound に丸められないこと: {mapped}"
        );
        assert!(
            matches!(&mapped, ToolError::ExecutionWithSource { .. }),
            "元の I/O エラーを source として保持すること: {mapped}"
        );
    }

    /// NotFound は従来どおり FileNotFound（候補付き）として報告されること。
    #[tokio::test]
    async fn metadata_not_found_still_reports_file_not_found() {
        let err = std::io::Error::new(std::io::ErrorKind::NotFound, "no such file");
        let mapped = map_metadata_error(err, Path::new("/some/missing-file.txt")).await;

        assert!(
            matches!(&mapped, ToolError::FileNotFound(_)),
            "NotFound は従来どおり FileNotFound であること: {mapped}"
        );
    }

    /// 読み取り上限は `truncation::MAX_BYTES` を import して使うため
    /// 単一情報源であり、その値が従来どおり 50 KiB であることを固定する。
    #[test]
    fn read_byte_limit_uses_shared_default() {
        assert_eq!(MAX_BYTES, 50 * 1024);
    }
}

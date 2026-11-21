use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::fs;
use tokio::io::AsyncWriteExt;

pub const MAX_LINES: usize = 2000;
pub const MAX_BYTES: usize = 50 * 1024;
pub const CLEANUP_RETENTION_DAYS: u64 = 7;

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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TruncationResult {
    pub truncated: bool,
    pub original_lines: usize,
    pub original_bytes: usize,
    pub truncated_lines: usize,
    pub truncated_bytes: usize,
    pub saved_to: Option<PathBuf>,
}

pub struct Truncate;

impl Truncate {
    pub async fn output(content: &str, save_dir: &Path) -> std::io::Result<TruncationResult> {
        let original_bytes = content.len();
        let original_lines = content.lines().count();

        let needs_truncation = original_bytes > MAX_BYTES || original_lines > MAX_LINES;

        if !needs_truncation {
            return Ok(TruncationResult {
                truncated: false,
                original_lines,
                original_bytes,
                truncated_lines: original_lines,
                truncated_bytes: original_bytes,
                saved_to: None,
            });
        }

        let lines: Vec<&str> = content.lines().collect();
        let mut truncated_lines = lines.clone();

        if truncated_lines.len() > MAX_LINES {
            truncated_lines = truncated_lines.split_off(truncated_lines.len() - MAX_LINES);
        }

        let mut truncated_content = truncated_lines.join("\n");

        if truncated_content.len() > MAX_BYTES {
            let bytes_to_take = MAX_BYTES.saturating_sub(100);
            let start = truncated_content.len().saturating_sub(bytes_to_take);
            // Round the start down to a valid UTF-8 boundary so a multibyte character
            // is never split. Floor (rather than ceil) also keeps the tail slice
            // non-empty: `start` is strictly below `len` here, and the adjustment
            // moves it further down by at most 3 bytes.
            let start = floor_char_boundary(&truncated_content, start);
            truncated_content = format!("... [truncated]\n{}", &truncated_content[start..]);
        }

        let truncated_bytes = truncated_content.len();
        let truncated_line_count = truncated_content.lines().count();

        let saved_to = Self::save_truncated(&truncated_content, save_dir).await?;

        Ok(TruncationResult {
            truncated: true,
            original_lines,
            original_bytes,
            truncated_lines: truncated_line_count,
            truncated_bytes,
            saved_to: Some(saved_to),
        })
    }

    async fn save_truncated(content: &str, save_dir: &Path) -> std::io::Result<PathBuf> {
        fs::create_dir_all(save_dir).await?;

        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0);

        let filename = format!("truncated_{}.txt", timestamp);
        let filepath = save_dir.join(filename);

        let mut file = fs::File::create(&filepath).await?;
        file.write_all(content.as_bytes()).await?;
        file.flush().await?;

        Ok(filepath)
    }

    pub async fn cleanup(save_dir: &Path) -> std::io::Result<usize> {
        if !save_dir.exists() {
            return Ok(0);
        }

        let mut entries = fs::read_dir(save_dir).await?;
        let mut deleted = 0;
        let cutoff = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs() - CLEANUP_RETENTION_DAYS * 24 * 60 * 60)
            .unwrap_or(0);

        while let Some(entry) = entries.next_entry().await? {
            let path = entry.path();

            if path.extension().map(|e| e == "txt").unwrap_or(false) {
                if let Ok(metadata) = entry.metadata().await {
                    if let Ok(modified) = metadata.modified() {
                        if let Ok(modified_ts) = modified.duration_since(UNIX_EPOCH) {
                            if modified_ts.as_secs() < cutoff {
                                fs::remove_file(&path).await?;
                                deleted += 1;
                            }
                        }
                    }
                }
            }
        }

        Ok(deleted)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[tokio::test]
    async fn test_no_truncation_needed() {
        let content = "Hello World\n".repeat(10);
        let dir = tempdir().unwrap();

        let result = Truncate::output(&content, dir.path()).await.unwrap();

        assert!(!result.truncated);
        assert_eq!(result.original_lines, 10);
    }

    #[tokio::test]
    async fn test_truncation_by_lines() {
        let content = (0..3000)
            .map(|i| format!("Line {}", i))
            .collect::<Vec<_>>()
            .join("\n");
        let dir = tempdir().unwrap();

        let result = Truncate::output(&content, dir.path()).await.unwrap();

        assert!(result.truncated);
        assert!(result.truncated_lines <= MAX_LINES);
        assert!(result.saved_to.is_some());
    }

    #[tokio::test]
    async fn test_truncation_by_bytes() {
        let content = "x".repeat(MAX_BYTES + 1000);
        let dir = tempdir().unwrap();

        let result = Truncate::output(&content, dir.path()).await.unwrap();

        assert!(result.truncated);
        assert!(result.truncated_bytes <= MAX_BYTES);
        let saved = std::fs::read_to_string(result.saved_to.unwrap()).unwrap();
        assert!(saved.starts_with("... [truncated]\n"));
    }

    /// 共通既定である 50 KiB が変わらないことを固定する。
    /// `read` / `bash` はこの定数を直接参照する単一情報源として使う。
    #[test]
    fn max_bytes_is_pinned() {
        assert_eq!(MAX_BYTES, 50 * 1024);
    }

    #[test]
    fn floor_char_boundary_respects_multibyte_boundaries() {
        // 'あ' is 3 bytes; make the raw start index land on its continuation byte.
        let s = format!("{}あ{}", "x".repeat(100), "x".repeat(51098));
        let start = s.len() - (MAX_BYTES - 100);
        assert!(!s.is_char_boundary(start));
        let boundary = floor_char_boundary(&s, start);
        assert!(boundary < start);
        assert!(s.is_char_boundary(boundary));
        assert!(s[boundary..].starts_with('あ'));
    }

    #[tokio::test]
    async fn test_truncation_by_bytes_with_multibyte_boundary() {
        // One line of MAX_BYTES + 1 bytes with a 3-byte 'あ' placed so the
        // computed slice start falls on its middle byte. The old
        // `&truncated_content[start..]` slice panicked here.
        let content = format!("{}あ{}", "x".repeat(100), "x".repeat(51098));
        assert_eq!(content.len(), MAX_BYTES + 1);
        let start = content.len() - (MAX_BYTES - 100);
        assert!(!content.is_char_boundary(start));

        let dir = tempdir().unwrap();
        let result = Truncate::output(&content, dir.path()).await.unwrap();

        assert!(result.truncated);
        assert!(result.truncated_bytes <= MAX_BYTES);
        let saved = std::fs::read_to_string(result.saved_to.unwrap()).unwrap();
        assert!(saved.starts_with("... [truncated]\n"));
        // The multibyte character must survive intact, not be split mid-codepoint.
        assert!(saved.contains('あ'));
    }

    #[tokio::test]
    async fn test_cleanup_removes_only_old_txt_files() {
        use std::time::{Duration, SystemTime};

        let dir = tempdir().unwrap();
        let old_txt = dir.path().join("old.txt");
        let new_txt = dir.path().join("new.txt");
        let old_md = dir.path().join("old.md");
        std::fs::write(&old_txt, "old").unwrap();
        std::fs::write(&new_txt, "new").unwrap();
        std::fs::write(&old_md, "old").unwrap();

        let old_time =
            SystemTime::now() - Duration::from_secs((CLEANUP_RETENTION_DAYS + 1) * 24 * 60 * 60);
        std::fs::File::options()
            .write(true)
            .open(&old_txt)
            .unwrap()
            .set_modified(old_time)
            .unwrap();
        std::fs::File::options()
            .write(true)
            .open(&old_md)
            .unwrap()
            .set_modified(old_time)
            .unwrap();

        let deleted = Truncate::cleanup(dir.path()).await.unwrap();

        assert_eq!(deleted, 1);
        assert!(!old_txt.exists());
        assert!(new_txt.exists());
        assert!(old_md.exists());
    }

    #[tokio::test]
    async fn test_cleanup_missing_directory() {
        let dir = tempdir().unwrap();
        let missing = dir.path().join("missing");

        assert_eq!(Truncate::cleanup(&missing).await.unwrap(), 0);
    }
}

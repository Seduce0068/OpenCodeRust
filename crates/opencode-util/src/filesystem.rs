use std::path::{Path, PathBuf};
use tokio::fs;
use walkdir::WalkDir;

pub struct Filesystem;

impl Filesystem {
    pub async fn exists<P: AsRef<Path>>(p: P) -> bool {
        fs::metadata(p).await.is_ok()
    }

    pub async fn is_dir<P: AsRef<Path>>(p: P) -> bool {
        match fs::metadata(p).await {
            Ok(m) => m.is_dir(),
            Err(_) => false,
        }
    }

    pub async fn is_file<P: AsRef<Path>>(p: P) -> bool {
        match fs::metadata(p).await {
            Ok(m) => m.is_file(),
            Err(_) => false,
        }
    }

    #[cfg(windows)]
    pub fn normalize_path(p: &str) -> std::io::Result<String> {
        use std::fs;
        fs::canonicalize(p).map(|p| p.to_string_lossy().to_string())
    }

    #[cfg(not(windows))]
    pub fn normalize_path(p: &str) -> String {
        p.to_string()
    }

    pub fn overlaps(a: &str, b: &str) -> bool {
        let path_a = Path::new(a);
        let path_b = Path::new(b);

        let rel_a = path_a.strip_prefix(path_b).unwrap_or(path_a);
        let rel_b = path_b.strip_prefix(path_a).unwrap_or(path_b);

        let rel_a_str = rel_a.to_string_lossy();
        let rel_b_str = rel_b.to_string_lossy();

        !rel_a_str.starts_with("..")
            || !rel_b_str.starts_with("..")
            || rel_a_str.is_empty()
            || rel_b_str.is_empty()
    }

    pub fn contains(parent: &str, child: &str) -> bool {
        let parent_path = Path::new(parent);
        let child_path = Path::new(child);

        child_path.strip_prefix(parent_path).is_ok()
    }

    pub async fn find_up<P: AsRef<Path>>(target: &str, start: P, stop: Option<P>) -> Vec<PathBuf> {
        let mut current = start.as_ref().to_path_buf();
        let stop = stop.map(|s| s.as_ref().to_path_buf());
        let mut result = Vec::new();

        loop {
            let search = current.join(target);
            if Self::exists(&search).await {
                result.push(search);
            }
            if stop.as_ref() == Some(&current) {
                break;
            }
            let parent = current.parent().map(|p| p.to_path_buf());
            if let Some(parent) = parent {
                if parent == current {
                    break;
                }
                current = parent;
            } else {
                break;
            }
        }
        result
    }

    pub async fn glob_up<P: AsRef<Path>>(pattern: &str, start: P, stop: Option<P>) -> Vec<PathBuf> {
        let mut current = start.as_ref().to_path_buf();
        let stop = stop.map(|s| s.as_ref().to_path_buf());
        let mut result = Vec::new();

        loop {
            let glob = match glob::Pattern::new(pattern) {
                Ok(g) => g,
                Err(_) => {
                    if let Some(ref s) = stop {
                        if &current == s {
                            break;
                        }
                    }
                    if let Some(parent) = current.parent() {
                        if parent == current {
                            break;
                        }
                        current = parent.to_path_buf();
                    }
                    continue;
                }
            };

            for entry in WalkDir::new(&current)
                .max_depth(1)
                .follow_links(false)
                .into_iter()
                .filter_map(|e| e.ok())
            {
                // 外部ディレクトリの権限チェックなしで検索ルートを走査するため、
                // シンボリックリンクを辿ると外部のファイル名を返してしまう。
                // follow_links(false) はリンクを traverse しないが、リンク自身は
                // エントリとして返る。path.is_file() はリンク先を辿るため、
                // リンク自身を明示的に除外する。symlink cycle も構造的に防ぐ。
                if entry.file_type().is_symlink() {
                    continue;
                }

                let path = entry.path();
                if path.is_file() {
                    if let Ok(rel) = path.strip_prefix(&current) {
                        if glob.matches(&rel.to_string_lossy()) {
                            result.push(path.to_path_buf());
                        }
                    }
                }
            }

            if stop.as_ref() == Some(&current) {
                break;
            }
            if let Some(parent) = current.parent() {
                if parent == current {
                    break;
                }
                current = parent.to_path_buf();
            } else {
                break;
            }
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn unique_temp_dir(prefix: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock is before UNIX epoch")
            .as_nanos();
        std::env::temp_dir().join(format!("{}-{}-{}", prefix, std::process::id(), nanos))
    }

    #[tokio::test]
    async fn test_exists() {
        assert!(Filesystem::exists(".").await);
        assert!(!Filesystem::exists("/nonexistent_path_12345").await);
    }

    #[tokio::test]
    async fn test_is_dir() {
        assert!(Filesystem::is_dir(".").await);
        assert!(!Filesystem::is_dir("Cargo.toml").await);
    }

    #[tokio::test]
    async fn test_find_up() {
        let results = Filesystem::find_up("Cargo.toml", ".", None).await;
        assert!(!results.is_empty());
    }

    /// 通常ファイルの glob マッチが従来どおり機能すること。
    #[tokio::test]
    async fn glob_up_matches_regular_files() {
        let dir = unique_temp_dir("opencode-util-glob");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a.rs"), "fn a() {}").unwrap();
        std::fs::write(dir.join("b.txt"), "text").unwrap();

        let result = Filesystem::glob_up("*.rs", dir.as_path(), Some(dir.as_path())).await;
        assert_eq!(result, vec![dir.join("a.rs")]);

        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// 検索ルート内の symlink が外部のファイル／ディレクトリを指しても、
    /// 外部のファイル名を返してはならない（権限すり抜け回帰テスト）。
    /// 同一ディレクトリ内の正当な symlink も非追跡となるのは、前回の
    /// glob/read/ls ツールと同じトレードオフである。
    #[cfg(unix)]
    #[tokio::test]
    async fn glob_up_does_not_follow_symlinks() {
        use std::os::unix::fs::symlink;

        let dir = unique_temp_dir("opencode-util-glob");
        let outside = unique_temp_dir("opencode-util-outside");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("leaked.rs"), "fn leaked() {}").unwrap();
        std::fs::write(dir.join("normal.rs"), "fn normal() {}").unwrap();

        symlink(&outside, dir.join("outside_link")).unwrap();
        symlink(outside.join("leaked.rs"), dir.join("leaked_link.rs")).unwrap();
        symlink(dir.join("normal.rs"), dir.join("local_link.rs")).unwrap();

        // path.is_file() はリンク先を辿るため、follow_links(false) だけでは
        // leaked_link.rs / local_link.rs が結果に残る。リンク自身を除外する
        // ことで、外部への漏洩と重複をともに防ぐ。
        let result = Filesystem::glob_up("*.rs", dir.as_path(), Some(dir.as_path())).await;
        assert_eq!(result, vec![dir.join("normal.rs")]);

        std::fs::remove_dir_all(&dir).unwrap();
        std::fs::remove_dir_all(&outside).unwrap();
    }
}

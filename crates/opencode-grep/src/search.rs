use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs::File;
use std::io::{self, BufRead, BufReader};
use std::path::{Path, PathBuf};
use walkdir::WalkDir;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MatchResult {
    pub path: String,
    pub line_number: usize,
    pub lines: String,
    pub absolute_offset: usize,
    pub submatches: Vec<SubMatch>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubMatch {
    pub text: String,
    pub start: usize,
    pub end: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Stats {
    pub elapsed: String,
    pub searches: usize,
    pub bytes_searched: usize,
    pub matched_lines: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileSearchOptions {
    pub glob: Vec<String>,
    pub hidden: bool,
    pub follow: bool,
    pub max_depth: Option<usize>,
}

impl Default for FileSearchOptions {
    fn default() -> Self {
        Self {
            glob: vec![],
            hidden: true,
            follow: false,
            max_depth: None,
        }
    }
}

pub struct Ripgrep;

impl Ripgrep {
    pub fn search<P: AsRef<Path>>(
        path: P,
        pattern: &str,
    ) -> Result<Vec<MatchResult>, Box<dyn std::error::Error>> {
        Self::search_with_limit(path, pattern, usize::MAX)
    }

    pub fn search_with_limit<P: AsRef<Path>>(
        path: P,
        pattern: &str,
        limit: usize,
    ) -> Result<Vec<MatchResult>, Box<dyn std::error::Error>> {
        let regex = regex::Regex::new(pattern)?;
        let path = path.as_ref();
        let mut matches = Vec::new();

        if path.is_file() {
            search_file(path, &regex, &mut matches, limit)?;
        } else if path.is_dir() {
            let files = Self::files(path, FileSearchOptions::default())?;
            let mut errors: Vec<io::Error> = Vec::new();
            for file in files {
                if matches.len() >= limit {
                    break;
                }
                if let Err(e) = search_file(&file, &regex, &mut matches, limit) {
                    errors.push(e);
                }
            }
            // ERRPROP-02: ディレクトリ検索では一部ファイルの読み取り失敗を握りつぶさず、
            // 一致が存在しても最初のエラーを返す。全成功時は errors が空のため
            // 従来どおり Ok(matches) を返し、後方互換性は維持される。
            // limit 到達で break した場合は「最初の limit 件の一致が確定した時点で
            // 打ち切り、その時点までに集めた errors のみを伝播する」仕様（R4）。
            // 未走査ファイルの読み取り失敗は検索範囲外のため報告しない。
            if let Some(e) = errors.into_iter().next() {
                return Err(Box::new(e));
            }
        }

        Ok(matches)
    }

    pub fn files<P: AsRef<Path>>(
        path: P,
        options: FileSearchOptions,
    ) -> Result<Vec<PathBuf>, io::Error> {
        let path = path.as_ref();
        if !path.is_dir() {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                format!("No such directory: '{}'", path.display()),
            ));
        }

        let mut result = Vec::new();
        let mut walk = WalkDir::new(path);

        if let Some(depth) = options.max_depth {
            walk = walk.max_depth(depth);
        }

        for entry in walk.into_iter() {
            let entry = entry.map_err(|e| {
                io::Error::other(format!("failed to walk {}: {}", path.display(), e))
            })?;
            let entry_path = entry.path();

            if !entry.file_type().is_file() {
                continue;
            }

            let file_name = entry.file_name().to_string_lossy();
            if !options.hidden && file_name.starts_with('.') {
                continue;
            }

            if file_name == ".git" || entry_path.to_string_lossy().contains(".git/") {
                continue;
            }

            if !options.glob.is_empty() {
                let matches_glob = options.glob.iter().any(|g| {
                    if let Some(stripped) = g.strip_prefix('!') {
                        !glob_match::glob_match(stripped, entry_path)
                    } else {
                        glob_match::glob_match(g, entry_path)
                    }
                });
                if !matches_glob {
                    continue;
                }
            }

            result.push(entry_path.to_path_buf());
        }

        Ok(result)
    }

    pub fn tree<P: AsRef<Path>>(path: P, limit: Option<usize>) -> Result<String, io::Error> {
        let path = path.as_ref();
        let files = Self::files(path, FileSearchOptions::default())?;

        let mut root: BTreeMap<String, TreeNode> = BTreeMap::new();

        for file in &files {
            let rel_path = file.strip_prefix(path).unwrap_or(file);
            let rel_str = rel_path.to_string_lossy();

            if rel_str.contains(".opencode") {
                continue;
            }

            let parts: Vec<&str> = rel_str.split(std::path::MAIN_SEPARATOR).collect();
            if parts.len() < 2 {
                continue;
            }

            let mut current = &mut root;
            for part in parts.iter().take(parts.len() - 1) {
                let node = current.entry(part.to_string()).or_insert(TreeNode {
                    name: part.to_string(),
                    children: BTreeMap::new(),
                });
                current = &mut node.children;
            }
        }

        let total = count_nodes(&root);
        let limit = limit.unwrap_or(total);
        let mut lines: Vec<String> = Vec::new();
        let mut queue: Vec<(String, String)> = Vec::new();

        for node in root.values() {
            let node_json = serde_json::to_string(node).unwrap_or_default();
            queue.push((node.name.clone(), node_json));
        }

        let mut used = 0;
        let mut i = 0;
        while i < queue.len() && used < limit {
            let (path_str, node_json) = &queue[i];
            let node: TreeNode = match serde_json::from_str(node_json) {
                Ok(n) => n,
                Err(_) => {
                    i += 1;
                    continue;
                }
            };
            lines.push(path_str.clone());
            used += 1;

            let children_to_add: Vec<(String, String)> = node
                .children
                .values()
                .map(|child| {
                    let child_json = serde_json::to_string(child).unwrap_or_default();
                    (format!("{}/{}", path_str, child.name), child_json)
                })
                .collect();

            queue.extend(children_to_add);
            i += 1;
        }

        if total > used {
            lines.push(format!("[{} truncated]", total - used));
        }

        Ok(lines.join("\n"))
    }
}

fn search_file(
    path: &Path,
    regex: &regex::Regex,
    matches: &mut Vec<MatchResult>,
    limit: usize,
) -> Result<(), io::Error> {
    let file = File::open(path)?;
    let reader = BufReader::new(file);
    let path_str = path.to_string_lossy().to_string();
    let mut offset = 0;

    for (line_num, line_result) in reader.lines().enumerate() {
        if matches.len() >= limit {
            break;
        }

        let line = line_result?;
        let line_len = line.len() + 1;

        if regex.is_match(&line) {
            let mut submatches = Vec::new();
            for cap in regex.find_iter(&line) {
                submatches.push(SubMatch {
                    text: cap.as_str().to_string(),
                    start: cap.start(),
                    end: cap.end(),
                });
            }

            matches.push(MatchResult {
                path: path_str.clone(),
                line_number: line_num + 1,
                lines: line,
                absolute_offset: offset,
                submatches,
            });
        }

        offset += line_len;
    }

    Ok(())
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct TreeNode {
    name: String,
    children: BTreeMap<String, TreeNode>,
}

fn count_nodes(node: &BTreeMap<String, TreeNode>) -> usize {
    let mut total = 0;
    for child in node.values() {
        total += 1 + count_nodes(&child.children);
    }
    total
}

mod glob_match {
    use std::path::Path;

    pub fn glob_match(pattern: &str, path: &Path) -> bool {
        let path_str = path.to_string_lossy();
        let pattern = pattern.trim_start_matches("./");

        if pattern.contains('*') {
            match glob_to_regex(pattern) {
                Some(regex) => regex.is_match(&path_str),
                None => false,
            }
        } else {
            path_str.ends_with(pattern) || path_str.contains(pattern)
        }
    }

    /// glob パターンを正規表現へ変換する。
    ///
    /// `*` / `?` / `.` は従来どおり変換し、それ以外の文字は `regex::escape`
    /// でリテラルとしてエスケープする。コンパイル失敗時は `None` を返し、
    /// 呼び出し側で「非マッチ」として扱う（全マッチへのフォールバックは行わない）。
    fn glob_to_regex(pattern: &str) -> Option<regex::Regex> {
        let mut regex_str = String::from("^.*");
        for c in pattern.chars() {
            match c {
                '*' => regex_str.push_str("[^/]*"),
                '?' => regex_str.push_str("[^/]"),
                '.' => regex_str.push_str("\\."),
                _ => regex_str.push_str(&regex::escape(&c.to_string())),
            }
        }
        regex_str.push('$');
        regex::Regex::new(&regex_str).ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_search() {
        let result = Ripgrep::search(".", "fn main").unwrap();
        assert!(!result.is_empty());
    }

    #[test]
    fn test_search_with_limit() {
        let result = Ripgrep::search_with_limit(".", "fn", 5).unwrap();
        assert!(result.len() <= 5);
    }

    #[test]
    fn test_files() {
        let result = Ripgrep::files(".", FileSearchOptions::default()).unwrap();
        assert!(!result.is_empty());
    }

    #[test]
    fn test_tree() {
        let result = Ripgrep::tree(".", Some(10)).unwrap();
        assert!(!result.is_empty());
    }

    fn tree_node(name: &str, children: BTreeMap<String, TreeNode>) -> TreeNode {
        TreeNode {
            name: name.to_owned(),
            children,
        }
    }

    #[test]
    fn count_nodes_empty_tree() {
        assert_eq!(count_nodes(&BTreeMap::new()), 0);
    }

    #[test]
    fn count_nodes_nested_tree() {
        let mut root = BTreeMap::new();
        let mut child = BTreeMap::new();
        child.insert("leaf".to_owned(), tree_node("leaf", BTreeMap::new()));
        root.insert("dir".to_owned(), tree_node("dir", child));
        root.insert("top".to_owned(), tree_node("top", BTreeMap::new()));
        assert_eq!(count_nodes(&root), 3);
    }

    #[test]
    fn glob_match_plain_suffix() {
        assert!(glob_match::glob_match(
            "src/main.rs",
            std::path::Path::new("/work/src/main.rs")
        ));
        assert!(!glob_match::glob_match(
            "src/main.rs",
            std::path::Path::new("/work/src/lib.rs")
        ));
    }

    #[test]
    fn glob_match_with_wildcards() {
        assert!(glob_match::glob_match(
            "*.rs",
            std::path::Path::new("/work/src/main.rs")
        ));
        assert!(glob_match::glob_match(
            "src/*.rs",
            std::path::Path::new("/work/src/main.rs")
        ));
        assert!(!glob_match::glob_match(
            "src/*.rs",
            std::path::Path::new("/work/src/nested/main.rs")
        ));
    }

    #[test]
    fn glob_match_strips_leading_dot_slash() {
        assert!(glob_match::glob_match(
            "./src/main.rs",
            std::path::Path::new("/work/src/main.rs")
        ));
    }

    #[test]
    fn glob_match_invalid_regex_metacharacter_does_not_match_all() {
        // ERRPROP-01 回帰: '(' がエスケープされず不正 regex となり、
        // フォールバックの `.*` で無関係なファイルまでマッチしていた問題。
        assert!(!glob_match::glob_match(
            "src/*.rs(",
            std::path::Path::new("notes.txt")
        ));
        assert!(!glob_match::glob_match(
            "src/*.rs(",
            std::path::Path::new("/work/src/main.rs")
        ));
    }

    #[test]
    fn glob_match_regex_metacharacters_are_literals() {
        // regex メタ文字は glob ではリテラル。エスケープ後も字面どおりにマッチする。
        assert!(glob_match::glob_match(
            "src/*/[1]+.rs",
            std::path::Path::new("/work/src/sub/[1]+.rs")
        ));
        assert!(!glob_match::glob_match(
            "src/*/[1]+.rs",
            std::path::Path::new("/work/src/sub/1.rs")
        ));
    }

    /// テスト専用の一意な一時ディレクトリを作成する（外部クレート依存なし）。
    fn unique_temp_dir(label: &str) -> std::path::PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let dir = std::env::temp_dir().join(format!(
            "opencode-grep-{}-{}-{}",
            label,
            std::process::id(),
            nanos
        ));
        std::fs::create_dir_all(&dir).expect("failed to create temp dir");
        dir
    }

    #[test]
    fn directory_search_returns_error_when_partial_file_is_invalid_utf8() {
        // ERRPROP-02 回帰: 一致する正常ファイルと不正 UTF-8 バイトを含むファイルが
        // 混在する場合、後者の読み取りエラーが握りつぶされ不完全な Ok が返っていた。
        let dir = unique_temp_dir("errprop02-partial");

        let good = dir.join("good.txt");
        std::fs::write(&good, b"needle in a haystack\n").expect("write good file");

        // 不正 UTF-8 バイト (0xff) を含む行は BufRead::lines() が InvalidData を返す。
        let bad = dir.join("bad.txt");
        std::fs::write(&bad, b"\xff\xfe\n").expect("write bad file");

        let result = Ripgrep::search_with_limit(&dir, "needle", 100);
        assert!(
            result.is_err(),
            "a partial read failure must not be swallowed when other files matched"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn directory_search_all_success_remains_ok() {
        // 全ファイルが正常な既存ケースが不変であることを確認する。
        let dir = unique_temp_dir("errprop02-ok");

        let first = dir.join("first.txt");
        std::fs::write(&first, b"needle one\n").expect("write first file");
        let second = dir.join("second.txt");
        std::fs::write(&second, b"needle two\nplain line\n").expect("write second file");

        let result = Ripgrep::search_with_limit(&dir, "needle", 100);
        assert!(
            result.is_ok(),
            "all-success directory search must return Ok"
        );
        assert_eq!(result.expect("ok result").len(), 2);

        let _ = std::fs::remove_dir_all(&dir);
    }
}

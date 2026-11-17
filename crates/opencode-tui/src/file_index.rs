use std::cmp::Ordering;
use std::collections::BinaryHeap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use nucleo_matcher::pattern::{CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Config, Matcher, Utf32Str};
use walkdir::{DirEntry, WalkDir};

const REFRESH_INTERVAL: Duration = Duration::from_secs(5);
const DEFAULT_MAX_DEPTH: usize = 8;

#[derive(Debug)]
pub struct FileIndex {
    entries: Vec<String>,
    last_refresh: Option<Instant>,
    root: Option<PathBuf>,
    max_depth: usize,
}

impl Default for FileIndex {
    fn default() -> Self {
        Self {
            entries: Vec::new(),
            last_refresh: None,
            root: None,
            max_depth: DEFAULT_MAX_DEPTH,
        }
    }
}

/// A scored entry kept in the bounded top-K heap used by [`FileIndex::search`].
///
/// The ordering mirrors the former full sort (score descending, then path
/// length ascending, then path ascending) but is inverted so that a
/// [`BinaryHeap`] max-heap exposes the *worst* candidate at its top.
#[derive(Debug)]
struct RankedEntry<'a> {
    score: u32,
    path: &'a String,
}

impl RankedEntry<'_> {
    /// `true` when `self` should appear before `other` in the final result.
    fn is_better_than(&self, other: &Self) -> bool {
        self.score > other.score
            || (self.score == other.score && self.path.len() < other.path.len())
            || (self.score == other.score
                && self.path.len() == other.path.len()
                && self.path < other.path)
    }
}

impl Ord for RankedEntry<'_> {
    fn cmp(&self, other: &Self) -> Ordering {
        // Inverted: a better entry compares *less*, so the heap top is the
        // worst entry and can be evicted first.
        if self.is_better_than(other) {
            Ordering::Less
        } else if other.is_better_than(self) {
            Ordering::Greater
        } else {
            Ordering::Equal
        }
    }
}

impl PartialOrd for RankedEntry<'_> {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl PartialEq for RankedEntry<'_> {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other) == Ordering::Equal
    }
}

impl Eq for RankedEntry<'_> {}

impl FileIndex {
    pub fn refresh(&mut self, root: &Path, max_depth: usize) {
        let depth = max_depth.max(1);
        let root_buf = root.to_path_buf();
        let now = Instant::now();
        if self.root.as_ref() == Some(&root_buf)
            && self.max_depth == depth
            && self
                .last_refresh
                .is_some_and(|last| now.duration_since(last) < REFRESH_INTERVAL)
        {
            return;
        }

        let mut entries = Vec::new();
        for entry in WalkDir::new(root)
            .follow_links(false)
            .max_depth(depth)
            .into_iter()
            .filter_entry(should_descend)
            .filter_map(Result::ok)
            .filter(|entry| entry.file_type().is_file())
        {
            if let Ok(relative) = entry.path().strip_prefix(root) {
                let value = relative.to_string_lossy().replace('\\', "/");
                if !value.is_empty() {
                    entries.push(value);
                }
            }
        }

        entries.sort();
        entries.dedup();
        self.entries = entries;
        self.root = Some(root_buf);
        self.max_depth = depth;
        self.last_refresh = Some(now);
    }

    pub fn search(&self, query: &str, limit: usize) -> Vec<(String, u32)> {
        if limit == 0 || self.entries.is_empty() {
            return Vec::new();
        }
        let trimmed = query.trim();
        if trimmed.is_empty() {
            return self
                .entries
                .iter()
                .take(limit)
                .map(|item| (item.clone(), 0))
                .collect();
        }

        let pattern = Pattern::parse(trimmed, CaseMatching::Smart, Normalization::Smart);
        let mut matcher = Matcher::new(Config::DEFAULT.match_paths());
        let mut utf32_buf = Vec::new();

        // Keep only the best `limit` entries. Scoring still visits every
        // entry, but we no longer clone (and later drop) all matches: a
        // bounded heap holds references and only the survivors are cloned.
        // This is O(n log k) with k = limit instead of cloning every match
        // and sorting all of them with O(n log n).
        let mut heap = BinaryHeap::with_capacity(limit.min(self.entries.len()));
        for entry in &self.entries {
            if let Some(score) = pattern.score(Utf32Str::new(entry, &mut utf32_buf), &mut matcher) {
                let candidate = RankedEntry { score, path: entry };
                if heap.len() < limit {
                    heap.push(candidate);
                } else if heap
                    .peek()
                    .is_some_and(|worst| candidate.is_better_than(worst))
                {
                    heap.pop();
                    heap.push(candidate);
                }
            }
        }

        // Ascending heap order == best-first result order.
        let mut ranked = heap.into_vec();
        ranked.sort();
        ranked
            .into_iter()
            .map(|entry| (entry.path.clone(), entry.score))
            .collect()
    }
}

fn should_descend(entry: &DirEntry) -> bool {
    if !entry.file_type().is_dir() {
        return true;
    }
    let name = entry.file_name().to_string_lossy();
    if entry.depth() == 0 {
        return true;
    }
    !matches!(
        name.as_ref(),
        ".git"
            | ".jj"
            | ".svn"
            | "node_modules"
            | "target"
            | "dist"
            | "build"
            | ".next"
            | ".nuxt"
            | ".turbo"
            | ".cache"
            | ".idea"
            | ".vscode"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refresh_skips_ignored_directories() {
        let root =
            std::env::temp_dir().join(format!("opencode-file-index-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(root.join("src")).expect("create src");
        std::fs::create_dir_all(root.join("target")).expect("create target");
        std::fs::write(root.join("src/main.rs"), "fn main() {}\n").expect("write src file");
        std::fs::write(root.join("target/ignored.rs"), "fn ignored() {}\n")
            .expect("write target file");

        let mut index = FileIndex::default();
        index.refresh(&root, 8);
        let results = index.search("main", 10);
        assert!(results.iter().any(|(path, _)| path == "src/main.rs"));
        assert!(!results.iter().any(|(path, _)| path == "target/ignored.rs"));

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn search_top_k_matches_full_sort_order() {
        let index = FileIndex {
            entries: vec![
                "src/main.rs".to_string(),
                "a/main1.rs".to_string(),
                "a/main22.rs".to_string(),
                "b/main333.rs".to_string(),
                "c/main4444.rs".to_string(),
                "zzz_unrelated.txt".to_string(),
            ],
            ..Default::default()
        };

        let query = "main";
        let limit = 3;

        // Reference: the former "score everything, clone, sort, truncate".
        let pattern = Pattern::parse(query, CaseMatching::Smart, Normalization::Smart);
        let mut matcher = Matcher::new(Config::DEFAULT.match_paths());
        let mut buf = Vec::new();
        let mut full = index
            .entries
            .iter()
            .filter_map(|entry| {
                pattern
                    .score(Utf32Str::new(entry, &mut buf), &mut matcher)
                    .map(|score| (entry.clone(), score))
            })
            .collect::<Vec<_>>();
        full.sort_by(|a, b| {
            b.1.cmp(&a.1)
                .then_with(|| a.0.len().cmp(&b.0.len()))
                .then_with(|| a.0.cmp(&b.0))
        });
        let expected = full.into_iter().take(limit).collect::<Vec<_>>();

        assert!(expected.len() == limit, "fixture must have > limit matches");
        assert_eq!(index.search(query, limit), expected);
    }
}

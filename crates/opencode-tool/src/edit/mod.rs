mod replacers;
mod tool;

pub use replacers::*;
pub use tool::EditTool;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reexported_normalize_line_endings_converts_crlf() {
        assert_eq!(normalize_line_endings("a\r\nb\r\nc"), "a\nb\nc");
        assert_eq!(normalize_line_endings("a\nb"), "a\nb");
    }

    #[test]
    fn trim_diff_removes_common_indent() {
        let diff = "--- a.rs\n+++ a.rs\n-    old\n+    new\n";
        assert_eq!(trim_diff(diff), "--- a.rs\n+++ a.rs\n-old\n+new\n");
    }

    #[test]
    fn trim_diff_keeps_diff_without_common_indent() {
        let diff = "--- a.rs\n+++ a.rs\n-old\n+new\n";
        assert_eq!(trim_diff(diff), diff);
    }

    #[test]
    fn trim_diff_keeps_header_only_diff() {
        let diff = "--- a.rs\n+++ a.rs\n";
        assert_eq!(trim_diff(diff), diff);
    }

    #[test]
    fn generate_unified_diff_emits_headers_and_hunks() {
        let diff = generate_unified_diff("a.rs", "old\nkeep\n", "new\nkeep\nextra\n");
        assert_eq!(
            diff,
            "--- a.rs\n+++ a.rs\n@@ -1,2 +1,3 @@\n-old\n-keep\n+new\n+keep\n+extra\n"
        );
    }

    #[test]
    fn file_diff_from_contents_counts_additions_and_deletions() {
        let added = FileDiff::from_contents("a\nb\n", "a\nb\nc\n");
        assert_eq!(added.path, "");
        assert_eq!(added.additions, 1);
        assert_eq!(added.deletions, 0);

        let removed = FileDiff::from_contents("a\nb\nc\n", "a\n");
        assert_eq!(removed.additions, 0);
        assert_eq!(removed.deletions, 2);

        let unchanged = FileDiff::from_contents("a\n", "a\n");
        assert_eq!(unchanged.additions, 0);
        assert_eq!(unchanged.deletions, 0);
    }

    #[test]
    fn composite_replacer_rejects_identical_strings() {
        let replacer = CompositeReplacer::new();
        let err = replacer
            .replace("hello", "hello", "hello", false)
            .unwrap_err();
        assert!(err.contains("No changes to apply"));
    }
}
